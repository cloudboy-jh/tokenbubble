//! Decides what a source change means for the running app.
//!
//! Invariants:
//! - Build first, replace second: a failed build never touches the running app.
//! - The same inputs are built once; a build that cannot start is not retried until the
//!   inputs change.

use crate::APP_BINARY;
use crate::watch::{Digest, Fingerprints};
use std::time::Duration;

#[derive(Debug)]
pub enum BuildOutcome {
    Built { elapsed: Duration, warnings: usize },
    Failed(String),
    Interrupted,
}

/// Side effects, separated so the decision logic is testable without processes.
pub trait Effects {
    fn say(&mut self, line: &str);
    fn build(&mut self) -> BuildOutcome;
    /// Stop any running app, first installing the newest build when `install`, then start it.
    fn launch(&mut self, install: bool) -> Result<(), String>;
    fn stop_app(&mut self);
}

pub struct Controller {
    runner_at_start: Digest,
    /// Inputs of the last build attempt, successful or not.
    attempted: Option<Digest>,
    /// Inputs of the newest successful build.
    built: Option<Digest>,
    /// Inputs of the build installed in the dev runtime.
    live: Option<Digest>,
    /// A build that compiled but could not start.
    rejected: Option<Digest>,
    runner_notice: bool,
}

impl Controller {
    pub fn new(fingerprints: &Fingerprints) -> Self {
        Self {
            runner_at_start: fingerprints.runner,
            attempted: None,
            built: None,
            live: None,
            rejected: None,
            runner_notice: false,
        }
    }

    /// Bring the build and the running app up to date with the sources.
    pub fn sync(&mut self, current: &Fingerprints, effects: &mut impl Effects) {
        if current.runner != self.runner_at_start && !self.runner_notice {
            self.runner_notice = true;
            effects.say("Runner sources changed — restart `cargo dev` to use them");
        }
        if self.attempted != Some(current.app) {
            match self.build(effects) {
                Some(true) => self.built = Some(current.app),
                Some(false) => {}
                None => return,
            }
            self.attempted = Some(current.app);
        }
        if self.fresh_build() {
            self.launch(true, effects);
        }
    }

    /// Run the app again without rebuilding.
    pub fn rerun(&mut self, effects: &mut impl Effects) {
        let install = self.fresh_build();
        if !install && self.live.is_none() {
            effects.say("No successful build yet");
            return;
        }
        self.launch(install, effects);
    }

    pub fn shutdown(&mut self, effects: &mut impl Effects) {
        effects.stop_app();
    }

    fn launch(&mut self, install: bool, effects: &mut impl Effects) {
        effects.say(&format!("Running {APP_BINARY}…"));
        match effects.launch(install) {
            Ok(()) => {
                if install {
                    self.live = self.built;
                }
            }
            Err(error) => {
                if install {
                    self.rejected = self.built;
                }
                effects.say(&format!("{APP_BINARY} failed to start: {error}"));
            }
        }
    }

    /// `None` when the build was interrupted, so the same inputs are attempted again.
    fn build(&mut self, effects: &mut impl Effects) -> Option<bool> {
        effects.say(&format!("Building {APP_BINARY}…"));
        match effects.build() {
            BuildOutcome::Built { elapsed, warnings } => {
                let warnings = match warnings {
                    0 => String::new(),
                    1 => " (1 warning)".to_owned(),
                    count => format!(" ({count} warnings)"),
                };
                effects.say(&format!("Built in {}{warnings}", seconds(elapsed)));
                Some(true)
            }
            BuildOutcome::Failed(diagnostics) => {
                effects.say("Build failed — keeping the previous build");
                effects.say(diagnostics.trim_end());
                Some(false)
            }
            BuildOutcome::Interrupted => None,
        }
    }

    /// A successful build that is neither installed nor known to fail at launch.
    fn fresh_build(&self) -> bool {
        self.built.is_some() && self.built != self.live && self.built != self.rejected
    }
}

pub fn seconds(duration: Duration) -> String {
    format!("{:.1}s", duration.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Process stand-in that records every side effect.
    #[derive(Default)]
    struct Fake {
        log: Vec<String>,
        said: Vec<String>,
        outcomes: VecDeque<BuildOutcome>,
        launch_fails: bool,
    }

    impl Fake {
        fn take(&mut self) -> Vec<String> {
            std::mem::take(&mut self.log)
        }
        fn heard(&self, text: &str) -> bool {
            self.said.iter().any(|line| line.contains(text))
        }
    }

    impl Effects for Fake {
        fn say(&mut self, line: &str) {
            self.said.push(line.to_owned());
        }
        fn build(&mut self) -> BuildOutcome {
            self.log.push("build".into());
            self.outcomes.pop_front().unwrap_or(BuildOutcome::Built {
                elapsed: Duration::from_millis(1200),
                warnings: 0,
            })
        }
        fn launch(&mut self, install: bool) -> Result<(), String> {
            self.log.push(format!("launch install={install}"));
            if std::mem::take(&mut self.launch_fails) {
                return Err("access denied".into());
            }
            Ok(())
        }
        fn stop_app(&mut self) {
            self.log.push("stop".into());
        }
    }

    fn sources(app: u8, runner: u8) -> Fingerprints {
        Fingerprints {
            app: Digest::of(app),
            runner: Digest::of(runner),
        }
    }

    fn failed() -> BuildOutcome {
        BuildOutcome::Failed("error[E0308]: mismatched types".into())
    }

    #[test]
    fn cold_start_builds_then_runs() {
        let mut fake = Fake::default();
        let start = sources(1, 0);
        Controller::new(&start).sync(&start, &mut fake);
        assert_eq!(fake.take(), ["build", "launch install=true"]);
    }

    #[test]
    fn failed_build_keeps_previous_and_is_not_retried_for_same_inputs() {
        let mut fake = Fake::default();
        let mut controller = Controller::new(&sources(1, 0));
        controller.sync(&sources(1, 0), &mut fake);
        fake.take();

        fake.outcomes.push_back(failed());
        controller.sync(&sources(2, 0), &mut fake);
        assert_eq!(fake.take(), ["build"]);
        assert!(fake.heard("error[E0308]"));

        controller.sync(&sources(2, 0), &mut fake);
        assert!(
            fake.take().is_empty(),
            "same failing inputs are not rebuilt"
        );

        controller.sync(&sources(3, 0), &mut fake);
        assert_eq!(fake.take(), ["build", "launch install=true"]);
    }

    #[test]
    fn rerun_uses_installed_build_and_needs_one_success() {
        let mut fake = Fake::default();
        let mut controller = Controller::new(&sources(1, 0));
        fake.outcomes.push_back(failed());
        controller.sync(&sources(1, 0), &mut fake);
        fake.take();

        controller.rerun(&mut fake);
        assert!(fake.take().is_empty());
        assert!(fake.heard("No successful build yet"));

        controller.sync(&sources(2, 0), &mut fake);
        fake.take();
        controller.rerun(&mut fake);
        assert_eq!(fake.take(), ["launch install=false"]);
    }

    #[test]
    fn interrupted_build_is_attempted_again() {
        let mut fake = Fake::default();
        let mut controller = Controller::new(&sources(1, 0));
        fake.outcomes.push_back(BuildOutcome::Interrupted);
        controller.sync(&sources(1, 0), &mut fake);
        assert_eq!(fake.take(), ["build"]);

        controller.sync(&sources(1, 0), &mut fake);
        assert_eq!(fake.take(), ["build", "launch install=true"]);
    }

    #[test]
    fn build_that_cannot_start_waits_for_new_inputs() {
        let mut fake = Fake::default();
        let mut controller = Controller::new(&sources(1, 0));
        fake.launch_fails = true;
        controller.sync(&sources(1, 0), &mut fake);
        assert_eq!(fake.take(), ["build", "launch install=true"]);
        assert!(fake.heard("failed to start: access denied"));

        controller.sync(&sources(1, 0), &mut fake);
        assert!(fake.take().is_empty());

        controller.sync(&sources(2, 0), &mut fake);
        assert_eq!(fake.take(), ["build", "launch install=true"]);
    }

    #[test]
    fn runner_change_is_announced_once_and_does_not_rebuild_the_app() {
        let mut fake = Fake::default();
        let mut controller = Controller::new(&sources(1, 0));
        controller.sync(&sources(1, 0), &mut fake);
        fake.take();

        controller.sync(&sources(1, 1), &mut fake);
        controller.sync(&sources(1, 2), &mut fake);
        assert!(fake.take().is_empty());
        let notices = fake
            .said
            .iter()
            .filter(|line| line.contains("restart `cargo dev`"))
            .count();
        assert_eq!(notices, 1);
    }
}
