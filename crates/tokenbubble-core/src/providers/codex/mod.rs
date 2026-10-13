//! Codex subscription usage via the ChatGPT backend, authenticated with Codex CLI's sign-in.
//!
//! Request construction and URL resolution are adapted from CodexBar
//! `CodexOAuthUsageFetcher.swift` (MIT; see THIRD_PARTY_NOTICES.md).

mod credentials;
mod usage;

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use jiff::Timestamp;

pub use credentials::{AUTH_FILE, CodexCredentials, load as load_credentials};

use crate::model::{Account, ProviderId, UsageSnapshot};
use crate::providers::{AuthReason, CollectError};
use crate::transport::{Request, Response, Transport};

pub const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api/";
pub const CONFIG_FILE: &str = "config.toml";

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// A Codex home directory on the machine running the collector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexSource {
    pub codex_home: PathBuf,
}

impl CodexSource {
    /// `CODEX_HOME` when set and non-empty, otherwise `~/.codex`.
    pub fn from_env() -> Result<Self, CollectError> {
        if let Some(home) =
            std::env::var_os("CODEX_HOME").filter(|home| !home.to_string_lossy().trim().is_empty())
        {
            return Ok(Self {
                codex_home: PathBuf::from(home),
            });
        }
        let home = std::env::home_dir().ok_or_else(|| CollectError::Config {
            detail: "home directory is unknown; set CODEX_HOME".to_owned(),
        })?;
        Ok(Self {
            codex_home: home.join(".codex"),
        })
    }

    /// Usage URL honoring a top-level `chatgpt_base_url` in `config.toml`. A missing, unreadable,
    /// or unparsable config falls back to the default base URL.
    pub fn usage_url(&self) -> Result<String, CollectError> {
        resolve_usage_url(self.configured_base_url().as_deref())
    }

    fn configured_base_url(&self) -> Option<String> {
        let mut contents = String::new();
        File::open(self.codex_home.join(CONFIG_FILE))
            .ok()?
            .take(MAX_CONFIG_BYTES)
            .read_to_string(&mut contents)
            .ok()?;
        let table: toml::Table = contents.parse().ok()?;
        table.get("chatgpt_base_url")?.as_str().map(str::to_owned)
    }
}

/// Mirrors Codex's path selection: `/wham/usage` under `/backend-api`, else `/api/codex/usage`.
/// Bearer tokens are only ever sent over HTTPS.
pub fn resolve_usage_url(base_url: Option<&str>) -> Result<String, CollectError> {
    let base = base_url
        .map(str::trim)
        .filter(|base| !base.is_empty())
        .unwrap_or(DEFAULT_BASE_URL)
        .trim_end_matches('/');
    let is_https = base
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"));
    if !is_https || base.len() == 8 {
        return Err(CollectError::Config {
            detail: "chatgpt_base_url must be an https:// URL".to_owned(),
        });
    }
    let mut url = base.to_owned();
    if (url.starts_with("https://chatgpt.com") || url.starts_with("https://chat.openai.com"))
        && !url.contains("/backend-api")
    {
        url.push_str("/backend-api");
    }
    url.push_str(if url.contains("/backend-api") {
        "/wham/usage"
    } else {
        "/api/codex/usage"
    });
    Ok(url)
}

pub fn usage_request(credentials: &CodexCredentials, url: String) -> Request {
    let mut headers = vec![("Accept", "application/json".to_owned())];
    if let Some(account_id) = &credentials.account_id {
        headers.push(("ChatGPT-Account-Id", account_id.clone()));
    }
    Request {
        url,
        bearer: Some(credentials.access_token.clone()),
        headers,
    }
}

/// One read-only collection. Never refreshes tokens or writes Codex files.
pub fn collect(
    source: &CodexSource,
    transport: &dyn Transport,
    now: Timestamp,
) -> Result<UsageSnapshot, CollectError> {
    let credentials = credentials::load(&source.codex_home)?;
    credentials.ensure_fresh(now)?;
    let request = usage_request(&credentials, source.usage_url()?);
    let parsed = interpret(transport.get(&request)?)?;
    Ok(UsageSnapshot {
        provider: ProviderId::Codex,
        measured_at: now,
        account: Account {
            id: credentials.account_id,
            email: credentials.email,
            plan: parsed.plan.or(credentials.plan_claim),
        },
        windows: parsed.windows,
        credits: parsed.credits,
        spend_limit: parsed.spend_limit,
        issues: parsed.issues,
    })
}

fn interpret(response: Response) -> Result<usage::ParsedUsage, CollectError> {
    match response.status {
        200..=299 => usage::parse(&response.body),
        401 => Err(CollectError::AuthRequired {
            reason: AuthReason::Rejected,
        }),
        429 => Err(CollectError::RateLimited {
            retry_after: response.retry_after,
        }),
        status => Err(CollectError::Server { status }),
    }
}
