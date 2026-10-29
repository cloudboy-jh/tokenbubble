pub mod claude;
pub mod codex;

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::transport::TransportError;

/// Why a collection produced no snapshot. Messages never contain credentials or response bodies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollectError {
    CredentialsMissing {
        path: PathBuf,
    },
    CredentialsUnreadable {
        path: PathBuf,
    },
    CredentialsMalformed {
        detail: &'static str,
    },
    /// The owning CLI must sign in or refresh; TokenBubble never redeems refresh tokens.
    AuthRequired {
        reason: AuthReason,
    },
    /// Credentials exist but cannot report subscription usage.
    Unsupported {
        detail: &'static str,
    },
    Config {
        detail: String,
    },
    RateLimited {
        retry_after: Option<Duration>,
    },
    Server {
        status: u16,
    },
    Transport(TransportError),
    InvalidResponse,
    /// The endpoint answered but reported no quota windows.
    NoUsageData,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthReason {
    /// The stored access token is expired or about to expire.
    Expired,
    /// The service rejected the token.
    Rejected,
}

impl CollectError {
    /// Stable machine-readable category.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::CredentialsMissing { .. } => "credentials_missing",
            Self::CredentialsUnreadable { .. } => "credentials_unreadable",
            Self::CredentialsMalformed { .. } => "credentials_malformed",
            Self::AuthRequired { .. } => "auth_required",
            Self::Unsupported { .. } => "unsupported",
            Self::Config { .. } => "config",
            Self::RateLimited { .. } => "rate_limited",
            Self::Server { .. } => "server",
            Self::Transport(_) => "transport",
            Self::InvalidResponse => "invalid_response",
            Self::NoUsageData => "no_usage_data",
        }
    }

    /// A failure that says nothing about the account: the previous reading stays valid
    /// and a retry may succeed without user action.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. }
                | Self::Server { .. }
                | Self::Transport(_)
                | Self::InvalidResponse
        )
    }
}

impl fmt::Display for CollectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CredentialsMissing { path } => {
                write!(
                    f,
                    "{} not found; sign in with the provider's CLI",
                    path.display()
                )
            }
            Self::CredentialsUnreadable { path } => {
                write!(
                    f,
                    "{} could not be read; check its permissions",
                    path.display()
                )
            }
            Self::CredentialsMalformed { detail } => {
                write!(f, "credentials are malformed: {detail}")
            }
            Self::AuthRequired {
                reason: AuthReason::Expired,
            } => f.write_str("stored sign-in has expired; run the provider's CLI to refresh it"),
            Self::AuthRequired {
                reason: AuthReason::Rejected,
            } => f.write_str(
                "the service rejected the stored sign-in; sign in again with the provider's CLI",
            ),
            Self::Unsupported { detail } => f.write_str(detail),
            Self::Config { detail } => write!(f, "configuration error: {detail}"),
            Self::RateLimited {
                retry_after: Some(after),
            } => write!(f, "rate limited; retry after {}s", after.as_secs()),
            Self::RateLimited { retry_after: None } => f.write_str("rate limited"),
            Self::Server { status } => write!(f, "usage endpoint returned HTTP {status}"),
            Self::Transport(error) => error.fmt(f),
            Self::InvalidResponse => f.write_str("usage endpoint returned an unreadable response"),
            Self::NoUsageData => f.write_str("usage endpoint reported no quota windows"),
        }
    }
}

impl std::error::Error for CollectError {}

impl From<TransportError> for CollectError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}
