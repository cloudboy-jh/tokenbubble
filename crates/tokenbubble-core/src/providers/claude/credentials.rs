//! Claude Code OAuth credentials and the store Claude Code keeps them in.
//!
//! Both tokens are held only as [`Secret`]s. The store is written solely to save a sign-in
//! TokenBubble renewed on Claude Code's behalf; every other key is written back as read.
//!
//! Adapted from CodexBar's ClaudeOAuthCredentialModels and SecurityCLIReader (MIT;
//! see THIRD_PARTY_NOTICES.md).

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use jiff::Timestamp;
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::{Value, json};

use super::refresh::Renewal;
use crate::providers::{AuthReason, CollectError};
use crate::secret::Secret;

pub const AUTH_FILE: &str = ".credentials.json";
const MAX_AUTH_BYTES: u64 = 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct ClaudeCredentials {
    pub access_token: Secret,
    /// Present when Claude Code stored one; redeemed only by [`super::refresh`].
    pub refresh_token: Option<Secret>,
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
    refresh_token: Option<String>,
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
        let access_token =
            nonempty(oauth.access_token).ok_or(CollectError::CredentialsMalformed {
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
            refresh_token: nonempty(oauth.refresh_token).map(Secret::new),
            expires_at,
            scopes: oauth.scopes,
            subscription_type: nonempty(oauth.subscription_type),
            rate_limit_tier: nonempty(oauth.rate_limit_tier),
        })
    }

    /// Scope is checked before expiry: credentials that cannot report usage are never renewed.
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

/// Loads the stored credentials; see [`Document::read`] for which store is chosen at `now`.
pub fn load(claude_home: &Path, now: Timestamp) -> Result<ClaudeCredentials, CollectError> {
    Document::read(claude_home, now)?.credentials()
}

/// Where Claude Code keeps the credentials TokenBubble read.
enum Store {
    File(PathBuf),
    /// The macOS Keychain item. The path is only named in errors.
    #[cfg(target_os = "macos")]
    Keychain(PathBuf),
}

/// The raw stored credential document. It holds both tokens, so it has no `Debug`.
pub(super) struct Document {
    store: Store,
    bytes: Vec<u8>,
}

impl Document {
    /// Reads `.credentials.json`. On macOS, Claude Code keeps the default profile's sign-in
    /// in the Keychain, and a file beside it may be a missing, scope-less, or stale leftover:
    /// unless the file is usable at `now`, a Keychain sign-in that can report usage (now or
    /// after renewal) is read instead.
    pub(super) fn read(
        claude_home: &Path,
        // Only macOS chooses between stores.
        #[cfg_attr(not(target_os = "macos"), expect(unused_variables))] now: Timestamp,
    ) -> Result<Self, CollectError> {
        let path = claude_home.join(AUTH_FILE);
        let file = read_bounded(&path).map(|bytes| Self {
            store: Store::File(path),
            bytes,
        });
        #[cfg(target_os = "macos")]
        if !file
            .as_ref()
            .is_ok_and(|document| document.check(now).is_ok())
            && std::env::home_dir().is_some_and(|home| claude_home == home.join(".claude"))
        {
            // The shared Keychain item belongs only to the default CLI profile.
            // An explicitly selected directory must never fall back to another account.
            let path = claude_home.join(AUTH_FILE);
            let keychain = keychain::load(&path).map(|bytes| Self {
                store: Store::Keychain(path),
                bytes,
            });
            return match (file, keychain) {
                (_, Ok(keychain)) if keychain.renewable(now) => Ok(keychain),
                (Err(CollectError::CredentialsMissing { .. }), keychain) => keychain,
                (file, _) => file,
            };
        }
        file
    }

    /// Fails unless these credentials can report usage at `now`.
    #[cfg(target_os = "macos")]
    fn check(&self, now: Timestamp) -> Result<(), CollectError> {
        self.credentials()?.ensure_usable(now)
    }

    /// Whether these credentials can report usage at `now`, possibly after a renewal.
    #[cfg(target_os = "macos")]
    fn renewable(&self, now: Timestamp) -> bool {
        matches!(
            self.check(now),
            Ok(()) | Err(CollectError::AuthRequired { .. })
        )
    }

    pub(super) fn credentials(&self) -> Result<ClaudeCredentials, CollectError> {
        ClaudeCredentials::parse(&self.bytes)
    }

    /// Stores `renewal` where this document came from and returns the stored credentials.
    /// Only `accessToken`, `refreshToken` (when the server rotated it), `expiresAt`, and
    /// `scopes` (when the server reported them) change; every other key keeps its value
    /// and position. A failed write is retried once.
    pub(super) fn save(
        &self,
        renewal: &Renewal,
        now: Timestamp,
    ) -> Result<ClaudeCredentials, CollectError> {
        let bytes = renewed(&self.bytes, renewal, now)?;
        let credentials = ClaudeCredentials::parse(&bytes)?;
        self.store
            .write(&bytes)
            .or_else(|_| self.store.write(&bytes))?;
        Ok(credentials)
    }
}

fn renewed(bytes: &[u8], renewal: &Renewal, now: Timestamp) -> Result<Vec<u8>, CollectError> {
    let malformed = || CollectError::CredentialsMalformed {
        detail: "claudeAiOauth is missing",
    };
    let mut root: Value = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    let oauth = root
        .get_mut("claudeAiOauth")
        .and_then(Value::as_object_mut)
        .ok_or_else(malformed)?;
    oauth.insert(
        "accessToken".to_owned(),
        json!(renewal.access_token.expose()),
    );
    if let Some(refresh_token) = &renewal.refresh_token {
        oauth.insert("refreshToken".to_owned(), json!(refresh_token.expose()));
    }
    // Without a reported lifetime the token is stored as already expired, so the next
    // reading renews it again with the refresh token saved here.
    let lifetime_ms = renewal.expires_in.map_or(0, |seconds| {
        i64::try_from(seconds)
            .unwrap_or(i64::MAX)
            .saturating_mul(1000)
    });
    oauth.insert(
        "expiresAt".to_owned(),
        json!(now.as_millisecond().saturating_add(lifetime_ms)),
    );
    if let Some(scopes) = &renewal.scopes {
        oauth.insert("scopes".to_owned(), json!(scopes));
    }
    serde_json::to_vec(&root).map_err(|_| malformed())
}

impl Store {
    fn write(&self, bytes: &[u8]) -> Result<(), CollectError> {
        match self {
            Self::File(path) => {
                write_atomically(path, bytes).map_err(|_| CollectError::CredentialsUnwritable {
                    path: path.to_owned(),
                })
            }
            #[cfg(target_os = "macos")]
            Self::Keychain(path) => keychain::save(path, bytes),
        }
    }
}

/// Replaces the file through a same-directory temp file and a rename, so Claude Code never
/// sees a partial document. A symlinked store is written through to its target. The temp
/// file is owner-only and never outlives a failed write.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let target = std::fs::canonicalize(path)?;
    let (Some(directory), Some(name)) = (target.parent(), target.file_name()) else {
        return Err(ErrorKind::InvalidInput.into());
    };
    let mut temp_name = name.to_owned();
    temp_name.push(format!(
        ".tokenbubble-{}-{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = directory.join(temp_name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(&temp)?;
    let result = write_and_close(file, bytes).and_then(|()| std::fs::rename(&temp, &target));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Flushes to disk and closes, since Windows cannot rename a file that is still open.
fn write_and_close(mut file: File, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
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

    /// Exact Claude Code generic-password service from the primary reference.
    const SERVICE: &str = "Claude Code-credentials";
    const SECURITY: &str = "/usr/bin/security";
    /// Conservative bound for one `security -i` command line.
    const MAX_INTERACTIVE_LINE: usize = 4000;

    pub(super) fn load(path: &Path) -> Result<Vec<u8>, CollectError> {
        let unreadable = || CollectError::CredentialsUnreadable {
            path: path.to_owned(),
        };
        // No shell, Claude CLI invocation, stderr capture, or credential-bearing diagnostics.
        let bytes = run(
            &["find-generic-password", "-s", SERVICE, "-w"],
            None,
            Duration::from_millis(1500),
        )
        .ok_or_else(unreadable)?;
        if bytes.len() as u64 > MAX_AUTH_BYTES {
            return Err(CollectError::CredentialsMalformed {
                detail: "Claude Keychain credential is unexpectedly large",
            });
        }
        Ok(bytes)
    }

    /// Updates the existing item in place (`-U`) under its own account so Claude Code's
    /// account-less lookup still finds exactly one item. Success is judged by reading the
    /// item back, not by `security`'s exit status, which `-i` does not tie to its commands.
    pub(super) fn save(path: &Path, bytes: &[u8]) -> Result<(), CollectError> {
        let unwritable = || CollectError::CredentialsUnwritable {
            path: path.to_owned(),
        };
        let account = account().ok_or_else(unwritable)?;
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let expected = ClaudeCredentials::parse(bytes)?;
        let stored = || {
            load(path)
                .ok()
                .and_then(|bytes| ClaudeCredentials::parse(&bytes).ok())
                .is_some_and(|stored| stored == expected)
        };
        let deadline = Duration::from_secs(5);
        // `security -i` reads the command from stdin, keeping the credential out of the
        // process list. Its line buffer is bounded and the account is quoted, so oversized
        // documents, accounts with quoting characters, or a rejected interactive command
        // fall back to argv. That fallback exposes the hex-encoded credential to other
        // local users' `ps` while `security` runs; `security` has no other non-prompting
        // way to take the secret.
        let line = format!("add-generic-password -U -a \"{account}\" -s \"{SERVICE}\" -X {hex}\n");
        if !account.contains(['"', '\\', '\n', '\r'])
            && line.len() <= MAX_INTERACTIVE_LINE
            && run(&["-i"], Some(line.into_bytes()), deadline).is_some()
            && stored()
        {
            return Ok(());
        }
        let args = [
            "add-generic-password",
            "-U",
            "-a",
            account.as_str(),
            "-s",
            SERVICE,
            "-X",
            hex.as_str(),
        ];
        if run(&args, None, deadline).is_some() && stored() {
            return Ok(());
        }
        Err(unwritable())
    }

    /// The existing item's account, read from its attributes (never its secret); Claude
    /// Code itself files the item under the login user name.
    fn account() -> Option<String> {
        let attributes = run(
            &["find-generic-password", "-s", SERVICE],
            None,
            Duration::from_millis(1500),
        )?;
        String::from_utf8_lossy(&attributes)
            .lines()
            .find_map(|line| {
                line.trim()
                    .strip_prefix("\"acct\"<blob>=\"")?
                    .strip_suffix('"')
                    .map(str::to_owned)
            })
            .or_else(|| std::env::var("USER").ok())
            .filter(|account| !account.is_empty())
    }

    /// Runs `security` with a deadline, returning bounded stdout on success.
    fn run(args: &[&str], stdin: Option<Vec<u8>>, deadline: Duration) -> Option<Vec<u8>> {
        let mut child = Command::new(SECURITY)
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let writer = child.stdin.take().zip(stdin).map(|(mut pipe, input)| {
            // Dropping the pipe at the end sends EOF, ending `security -i`.
            std::thread::spawn(move || pipe.write_all(&input))
        });
        let stdout = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(MAX_AUTH_BYTES + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let end = Instant::now() + deadline;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let written = writer.is_none_or(|writer| writer.join().is_ok_and(|result| result.is_ok()));
        let bytes = reader.join().ok()?.ok()?;
        (status?.success() && written).then_some(bytes)
    }
}
