//! Read-only subscription usage collection.
//!
//! Credentials are read from the files their owning CLI maintains and are never refreshed,
//! written, serialized, or logged. Collected data is normalized into [`model::UsageSnapshot`].

pub mod model;
pub mod providers;
pub mod secret;
pub mod transport;
