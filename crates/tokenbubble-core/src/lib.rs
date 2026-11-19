//! Subscription usage collection.
//!
//! Credentials are read from the stores their owning CLI maintains and are never serialized
//! or logged. The one write: an expired Claude sign-in is renewed with Claude Code's refresh
//! token and saved back to Claude Code's store, so Claude Code keeps working with it.
//! Collected data is normalized into [`model::UsageSnapshot`].

pub mod activity;
pub mod config;
pub mod model;
pub mod providers;
pub mod secret;
pub mod service;
pub mod transport;
