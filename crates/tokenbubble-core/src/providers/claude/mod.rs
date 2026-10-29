//! Read-only Claude Code subscription usage via Anthropic OAuth.
//!
//! Request compatibility follows CodexBar ClaudeOAuthUsageFetcher at
//! d554d44931807e568471c5ff0d7f786b9c33053b (MIT; see THIRD_PARTY_NOTICES.md).

mod credentials;
mod usage;

use std::path::PathBuf;

use jiff::Timestamp;

pub use credentials::{AUTH_FILE, ClaudeCredentials, load as load_credentials};

use crate::model::{Account, ProviderId, UsageSnapshot};
use crate::providers::{AuthReason, CollectError};
use crate::transport::{Request, Response, Transport};

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage?cedar_ember=1";
const COMPATIBILITY_URL: &str = "https://api.anthropic.com/api/oauth/usage";
// The primary reference's fallback version; never run Claude CLI merely to detect a version.
const USER_AGENT: &str = "claude-cli/2.1.0 (external, cli)";
const COMPATIBILITY_USER_AGENT: &str = "claude-code/2.1.0";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeSource {
    pub claude_home: PathBuf,
}

impl ClaudeSource {
    /// A nonempty CLAUDE_CONFIG_DIR is one literal path, including spaces or a literal ~.
    pub fn from_env() -> Result<Self, CollectError> {
        if let Some(home) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|home| !home.is_empty()) {
            return Ok(Self {
                claude_home: PathBuf::from(home),
            });
        }
        let home = std::env::home_dir().ok_or_else(|| CollectError::Config {
            detail: "home directory is unknown; set CLAUDE_CONFIG_DIR".to_owned(),
        })?;
        Ok(Self {
            claude_home: home.join(".claude"),
        })
    }

    pub fn credential_path(&self) -> PathBuf {
        self.claude_home.join(AUTH_FILE)
    }
}

pub fn usage_request(credentials: &ClaudeCredentials) -> Request {
    Request {
        url: USAGE_URL.to_owned(),
        bearer: Some(credentials.access_token.clone()),
        headers: vec![
            ("Accept", "application/json".to_owned()),
            ("Content-Type", "application/json".to_owned()),
            ("anthropic-beta", "oauth-2025-04-20".to_owned()),
            ("User-Agent", USER_AGENT.to_owned()),
        ],
    }
}

/// A read-only collection: no refresh, credential writes, or Claude CLI subprocesses.
pub fn collect(
    source: &ClaudeSource,
    transport: &dyn Transport,
    now: Timestamp,
) -> Result<UsageSnapshot, CollectError> {
    let credentials = credentials::load(&source.claude_home)?;
    credentials.ensure_usable(now)?;
    let mut request = usage_request(&credentials);
    let mut response = transport.get(&request)?;
    // Exact inventory-compatibility retry from the primary reference: once for 400,
    // or 403 whose UTF-8 body does not mention the required scope. Same credential.
    if response.status == 400 || (response.status == 403 && !mentions_profile_scope(&response.body))
    {
        request.url = COMPATIBILITY_URL.to_owned();
        if let Some((_, value)) = request
            .headers
            .iter_mut()
            .find(|(key, _)| *key == "User-Agent")
        {
            *value = COMPATIBILITY_USER_AGENT.to_owned();
        }
        response = transport.get(&request)?;
    }
    let parsed = interpret(response)?;
    Ok(UsageSnapshot {
        provider: ProviderId::Claude,
        measured_at: now,
        account: Account {
            plan: credentials.subscription_type,
            ..Account::default()
        },
        windows: parsed.windows,
        credits: None,
        spend_limit: parsed.spend_limit,
        issues: parsed.issues,
    })
}

fn mentions_profile_scope(body: &[u8]) -> bool {
    std::str::from_utf8(body).is_ok_and(|text| text.contains("user:profile"))
}

fn interpret(response: Response) -> Result<usage::ParsedUsage, CollectError> {
    match response.status {
        200 => usage::parse(&response.body),
        401 => Err(CollectError::AuthRequired {
            reason: AuthReason::Rejected,
        }),
        403 if mentions_profile_scope(&response.body) => Err(CollectError::Unsupported {
            detail: "Claude subscription usage requires the user:profile OAuth scope; sign in with Claude Code",
        }),
        429 => Err(CollectError::RateLimited {
            retry_after: response.retry_after,
        }),
        status => Err(CollectError::Server { status }),
    }
}
