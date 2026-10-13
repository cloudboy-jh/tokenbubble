//! Real side effects and the watch loop behind `cargo dev`.

use crate::classify::Component;
use crate::controller::{BuildOutcome, Controller, Effects};
use crate::layout::{Layout, executable};
use crate::watch::{Debouncer, Tree};
use crate::{APP_ARGS, APP_BINARY, cargo, process};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(150);
const QUIET: Duration = Duration::from_millis(250);
const BURST_LIMIT: Duration = Duration::from_secs(2);
const HINT: &str = "⏎ run again · q⏎ quit";

pub fn run() -> Result<(), String> {
    let layout = Layout::discover()?;
    layout.prepare()?;
    let _runner_lock = layout.lock_runner()?;
    process::install_interrupt_handler();

    println!(
        "TokenBubble dev · {} · runtime {}",
        command_line(),
        relative(&layout.root, &layout.workspace)
    );
    let mut dev = Dev {
        layout,
        build: None,
        app: None,
    };
    let mut tree = Tree::scan(&dev.layout.workspace);
    let mut controller = Controller::new(&tree.fingerprints());
    controller.sync(&tree.fingerprints(), &mut dev);
    if !process::interrupted() {
        println!("Watching sources · {HINT}");
    }

    let commands = spawn_command_reader();
    let mut debouncer = Debouncer::new(QUIET, BURST_LIMIT);
    let mut batch = Vec::new();
    while !process::interrupted() {
        match commands.recv_timeout(POLL) {
            Ok(Request::Quit) => break,
            Ok(Request::Rerun) => controller.rerun(&mut dev),
            Ok(Request::Unknown(text)) => println!("Unknown command {text:?} · {HINT}"),
            Err(_) => {}
        }
        let changes = tree.refresh();
        if !changes.paths.is_empty() {
            debouncer.record(Instant::now());
            batch.extend(changes.paths);
        }
        if debouncer.due(Instant::now()) {
            debouncer.reset();
            if let Some(summary) = describe(&batch) {
                println!("{summary}");
            }
            batch.clear();
            controller.sync(&tree.fingerprints(), &mut dev);
        }
        dev.notice_exit();
    }

    controller.shutdown(&mut dev);
    println!("Stopped.");
    process::shutdown_done();
    Ok(())
}

struct Dev {
    layout: Layout,
    /// Executable from the newest successful build, inside Cargo's target directory.
    build: Option<PathBuf>,
    app: Option<Child>,
}

impl Dev {
    fn notice_exit(&mut self) {
        let Some(app) = &mut self.app else {
            return;
        };
        if let Some(status) = app.try_wait().ok().flatten() {
            let outcome = match status.code() {
                Some(0) => "exited".to_owned(),
                Some(code) => format!("exited with code {code}"),
                None => format!("ended ({status})"),
            };
            self.app = None;
            println!("{APP_BINARY} {outcome} · {HINT}");
        }
    }

    fn spawn(&mut self) -> Result<Child, String> {
        let program = self.layout.app_exe();
        let mut child = Command::new(&program)
            .args(APP_ARGS)
            .current_dir(&self.layout.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("{}: {error}", program.display()))?;
        if let Some(stdout) = child.stdout.take() {
            forward(stdout);
        }
        if let Some(stderr) = child.stderr.take() {
            forward(stderr);
        }
        Ok(child)
    }
}

impl Effects for Dev {
    fn say(&mut self, line: &str) {
        println!("{line}");
    }

    fn build(&mut self) -> BuildOutcome {
        let (outcome, built) = cargo::build(&self.layout.workspace, process::interrupted);
        if built.is_some() {
            self.build = built;
        }
        outcome
    }

    fn launch(&mut self, install_build: bool) -> Result<(), String> {
        let next = self
            .layout
            .bin
            .join(executable(&format!("{APP_BINARY}.next")));
        if install_build {
            // Copy while the old app may still run, so the swap itself is instant.
            let built = self.build.as_ref().ok_or("no build")?;
            install(built, &next)?;
        }
        self.stop_app();
        if install_build {
            replace(&next, &self.layout.app_exe())?;
        }
        self.app = Some(self.spawn()?);
        Ok(())
    }

    fn stop_app(&mut self) {
        if let Some(mut app) = self.app.take() {
            process::stop_child(&mut app);
        }
    }
}

impl Drop for Dev {
    /// Never leave an orphaned app, even on an unexpected error or panic.
    fn drop(&mut self) {
        self.stop_app();
    }
}

/// Echo a child stream line by line under an `app │` gutter.
fn forward(stream: impl Read + Send + 'static) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            println!("  app │ {line}");
        }
    });
}

/// Copy beside the destination, then rename over it: a reader never sees a partial file.
fn install(from: &Path, to: &Path) -> Result<(), String> {
    let staging = to.with_extension("partial");
    retry(|| std::fs::copy(from, &staging).map(drop))
        .map_err(|error| format!("copy {} → {}: {error}", from.display(), staging.display()))?;
    replace(&staging, to)
}

/// Rename with retries: antivirus and sync clients briefly hold new executables open.
fn replace(from: &Path, to: &Path) -> Result<(), String> {
    retry(|| std::fs::rename(from, to))
        .map_err(|error| format!("replace {}: {error}", to.display()))
}

fn retry(mut action: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match action() {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Request {
    Rerun,
    Quit,
    Unknown(String),
}

fn spawn_command_reader() -> mpsc::Receiver<Request> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if sender.send(parse_request(&line)).is_err() {
                return;
            }
        }
    });
    receiver
}

/// Terminals can inject escape sequences (focus or key reports) into the line; only the
/// typed text counts.
fn parse_request(line: &str) -> Request {
    let mut text = String::new();
    let mut chars = line.chars();
    while let Some(char) = chars.next() {
        if char != '\u{1b}' {
            if !char.is_control() {
                text.push(char);
            }
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte in @..~.
            Some('[') => {
                for char in chars.by_ref() {
                    if ('@'..='~').contains(&char) {
                        break;
                    }
                }
            }
            // SS3: exactly one more character (e.g. ESC O R).
            Some('O') => {
                chars.next();
            }
            _ => {}
        }
    }
    match text.trim() {
        "" => Request::Rerun,
        "q" | "Q" => Request::Quit,
        other => Request::Unknown(other.to_owned()),
    }
}

/// One line naming what changed, e.g. `Source change: usage.rs (+1 more)`. Runner-only
/// edits get none: the controller's one-time restart notice covers them.
fn describe(paths: &[String]) -> Option<String> {
    let unique: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    let app = unique
        .iter()
        .any(|path| crate::classify::classify(path).contains(&Component::App));
    if !app {
        return None;
    }
    let first = unique
        .iter()
        .next()
        .map(|path| path.rsplit('/').next().unwrap_or(path))
        .unwrap_or_default();
    Some(match unique.len() {
        0 | 1 => format!("Source change: {first}"),
        count => format!("Source change: {first} (+{} more)", count - 1),
    })
}

fn command_line() -> String {
    std::iter::once(APP_BINARY)
        .chain(APP_ARGS.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

fn relative(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{Request, describe, parse_request};

    #[test]
    fn change_summary_ignores_runner_only_edits() {
        let paths = |items: &[&str]| {
            items
                .iter()
                .map(|item| (*item).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            describe(&paths(&[
                "crates/tokenbubble-core/src/model.rs",
                "crates/tokenbubble-core/src/model.rs",
                "crates/tokenbubble-core/src/lib.rs",
            ]))
            .unwrap(),
            "Source change: lib.rs (+1 more)"
        );
        assert_eq!(
            describe(&paths(&["tools/tokenbubble-dev/src/runtime.rs"])),
            None
        );
    }

    #[test]
    fn commands_ignore_terminal_injected_escape_sequences() {
        assert_eq!(parse_request("\u{1b}[Iq\r"), Request::Quit);
        assert_eq!(parse_request("  \r"), Request::Rerun);
        assert_eq!(parse_request("\u{1b}[O"), Request::Rerun);
        assert_eq!(parse_request("\u{1b}OR"), Request::Rerun);
        assert_eq!(parse_request("restart"), Request::Unknown("restart".into()));
    }
}
