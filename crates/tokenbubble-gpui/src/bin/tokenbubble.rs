// Release builds are GUI-only; debug builds keep a console so `cargo dev` can show logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(any(windows, target_os = "macos"))]
fn main() {
    tokenbubble_gpui::standalone::run();
}

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    eprintln!("tokenbubble runs on Windows and macOS");
    std::process::exit(1);
}
