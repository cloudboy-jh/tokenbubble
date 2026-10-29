//! The TokenBubble usage panel, shared by Compi and the standalone app.
//!
//! [`presentation`] and [`placement`] are renderer-independent; the GPUI
//! modules exist only where GPUI runs.

pub mod placement;
pub mod presentation;

#[cfg(any(windows, target_os = "macos"))]
pub mod panel;
#[cfg(any(windows, target_os = "macos"))]
mod provider_icon;
#[cfg(any(windows, target_os = "macos"))]
pub mod settings_view;
#[cfg(any(windows, target_os = "macos"))]
pub mod standalone;
#[cfg(any(windows, target_os = "macos"))]
pub mod theme;
