//! The TokenBubble usage panel, shared by Compi and the standalone app.
//!
//! [`presentation`] is renderer-independent; the GPUI modules exist only where GPUI runs.

pub mod presentation;

#[cfg(any(windows, target_os = "macos"))]
pub mod panel;
#[cfg(any(windows, target_os = "macos"))]
pub mod standalone;
#[cfg(any(windows, target_os = "macos"))]
pub mod theme;
