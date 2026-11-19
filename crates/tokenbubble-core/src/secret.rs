use std::fmt;

/// Credential material that must never reach logs, snapshots, or config.
///
/// Deliberately implements neither `Display` nor `Serialize`; `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the raw value only to send it (request headers and token bodies) or to save a
    /// renewed sign-in back to its owner's store.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}
