const USAGE: &str = "\
Usage: cargo dev

Builds the app on every saved change and reruns it from an isolated copy, so a running
app never locks the next build. A failed build keeps the previous run's output.

While running: ⏎ runs the app again, q⏎ or Ctrl+C quits.";

fn main() {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return;
        }
        Some(other) => {
            eprintln!("unknown argument {other:?}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
    if let Err(error) = tokenbubble_dev::run() {
        eprintln!("cargo dev: {error}");
        std::process::exit(1);
    }
}
