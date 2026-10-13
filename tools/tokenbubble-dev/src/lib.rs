//! `cargo dev`: rebuild the app on save and rerun it from a copy outside Cargo's output,
//! mirroring Compi's `compi-dev` loop without its daemon handling.

mod cargo;
pub mod classify;
pub mod controller;
pub mod layout;
mod process;
mod runtime;
pub mod watch;

pub use runtime::run;

/// What `cargo dev` builds and runs.
pub const APP_PACKAGE: &str = "tokenbubble-gpui";
pub const APP_BINARY: &str = "tokenbubble";
pub const APP_ARGS: &[&str] = &[];
