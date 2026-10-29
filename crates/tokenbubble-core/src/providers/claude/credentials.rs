//! Read-only Claude Code OAuth credentials. Refresh tokens are deliberately not decoded.
//!
//! Adapted from CodexBar's ClaudeOAuthCredentialModels and SecurityCLIReader (MIT;
//! see THIRD_PARTY_NOTICES.md).

use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

use jiff::Timestamp;
use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::providers::{AuthReason, CollectError};
use crate::secret::Secret;

pub const AUTH_FILE: &str = ".credentials.json";
const MAX_AUTH_BYTES: u64 = 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct ClaudeCredentials {
    pub access_token: Secret,
    pub expires_at: Option<Timestamp>,
    pub scopes: Vec<String>,
    pub subscription_type: Option<String>,
    pub rate_limit_tier: Option<String>,
}

impl fmt::Debug for ClaudeCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeCredentials")
            .field("access_token", &self.access_token)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Root {
    claude_ai_oauth: Option<OAuth>,
    #[serde(rename = "mcpOAuth")]
    mcp_oauth: Option<IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuth {
    access_token: Option<String>,
    expires_at: Option<f64>,
    #[serde(default)]
    scopes: Vec<String>,
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
}

impl ClaudeCredentials {
    pub fn parse(bytes: &[u8]) -> Result<Self, CollectError> {
        let root: Root =
            serde_json::from_slice(bytes).map_err(|_| CollectError::CredentialsMalformed {
                detail: ".credentials.json is not a valid Claude OAuth credential object",
            })?;
        let Some(oauth) = root.claude_ai_oauth else {
            return Err(if root.mcp_oauth.is_some() {
                CollectError::Unsupported {
                    detail: "MCP OAuth tokens cannot report Claude subscription usage",
                }
            } else {
                CollectError::CredentialsMalformed {
                    detail: "claudeAiOauth is missing",
                }
            });
        };
        let access_token = oauth
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or(CollectError::CredentialsMalformed {
                detail: "Claude OAuth access token is missing",
            })?;
        // Claude stores epoch milliseconds, not JWT seconds. Missing or invalid expiry is
        // unknown freshness and must never be treated as a permanent token.
        let expires_at = oauth.expires_at.and_then(|millis| {
            let seconds = (millis / 1000.0).floor();
            if !seconds.is_finite() || seconds < i64::MIN as f64 || seconds >= i64::MAX as f64 {
                return None;
            }
            let nanos = ((millis - seconds * 1000.0) * 1_000_000.0).round() as i32;
            Timestamp::new(seconds as i64, nanos).ok()
        });
        Ok(Self {
            access_token: Secret::new(access_token),
            expires_at,
            scopes: oauth.scopes,
            subscription_type: nonempty(oauth.subscription_type),
            rate_limit_tier: nonempty(oauth.rate_limit_tier),
        })
    }

    pub fn ensure_usable(&self, now: Timestamp) -> Result<(), CollectError> {
        if !self.scopes.iter().any(|scope| scope == "user:profile") {
            return Err(CollectError::Unsupported {
                detail: "Claude subscription usage requires the user:profile OAuth scope; sign in with Claude Code",
            });
        }
        if self.expires_at.is_none_or(|expiry| now >= expiry) {
            return Err(CollectError::AuthRequired {
                reason: AuthReason::Expired,
            });
        }
        Ok(())
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub fn load(claude_home: &Path) -> Result<ClaudeCredentials, CollectError> {
    let path = claude_home.join(AUTH_FILE);
    let result = read_bounded(&path).and_then(|bytes| ClaudeCredentials::parse(&bytes));
    #[cfg(target_os = "macos")]
    if matches!(result, Err(CollectError::CredentialsMissing { .. }))
        && std::env::home_dir().is_some_and(|home| claude_home == home.join(".claude"))
    {
        // The shared Keychain item belongs only to the default CLI profile.
        // An explicitly selected directory must never fall back to another account.
        return keychain::load(&path);
    }
    result
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, CollectError> {
    let file = File::open(path).map_err(|error| match error.kind() {
        ErrorKind::NotFound => CollectError::CredentialsMissing {
            path: path.to_owned(),
        },
        _ => CollectError::CredentialsUnreadable {
            path: path.to_owned(),
        },
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_AUTH_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CollectError::CredentialsUnreadable {
            path: path.to_owned(),
        })?;
    if bytes.len() as u64 > MAX_AUTH_BYTES {
        return Err(CollectError::CredentialsMalformed {
            detail: ".credentials.json is unexpectedly large",
        });
    }
    Ok(bytes)
}

#[cfg(target_os = "macos")]
mod keychain {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    pub(super) fn load(path: &Path) -> Result<ClaudeCredentials, CollectError> {
        let unreadable = || CollectError::CredentialsUnreadable {
            path: path.to_owned(),
        };
        // Exact Claude Code generic-password service from the primary reference. No shell,
        // CLI invocation, Keychain writes, stderr capture, or credential-bearing diagnostics.
        let mut child = Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| unreadable())?;
        let stdout = child.stdout.take().ok_or_else(unreadable)?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(MAX_AUTH_BYTES + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let deadline = Instant::now() + Duration::from_millis(1500);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(unreadable());
                }
            }
        };
        let bytes = reader
            .join()
            .map_err(|_| unreadable())?
            .map_err(|_| unreadable())?;
        let status = status?;
        if !status.success() {
            return Err(unreadable());
        }
        if bytes.len() as u64 > MAX_AUTH_BYTES {
            return Err(CollectError::CredentialsMalformed {
                detail: "Claude Keychain credential is unexpectedly large",
            });
        }
        ClaudeCredentials::parse(&bytes)
    }
}
