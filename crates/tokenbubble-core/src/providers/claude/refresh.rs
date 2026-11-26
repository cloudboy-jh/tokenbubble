//! Renews an expired Claude Code sign-in the way Claude Code does, and saves the renewed
//! tokens back to Claude Code's store so Claude Code keeps working with them.
//!
//! Refresh tokens rotate: once redeemed, the stored one is spent. The renewal is therefore
//! skipped whenever Claude Code has already renewed, and never overwrites a sign-in that
//! Claude Code saved while the request was in flight.

use std::path::Path;

use jiff::Timestamp;
use serde_json::{Value, json};

use super::credentials::{ClaudeCredentials, Document};
use crate::providers::{AuthReason, CollectError};
use crate::secret::Secret;
use crate::transport::{Request, Transport};

pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Claude Code's public OAuth client.
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// A successful token response. Holds only secrets and metadata; no `Debug`.
pub(super) struct Renewal {
    pub access_token: Secret,
    /// `None` when the server kept the current refresh token.
    pub refresh_token: Option<Secret>,
    pub expires_in: Option<u64>,
    pub scopes: Option<Vec<String>>,
}

/// Returns usable credentials in place of `stale`, renewing them if Claude Code has not.
///
/// `unrenewable` is returned when the store holds no refresh token.
pub(super) fn renew(
    claude_home: &Path,
    transport: &dyn Transport,
    stale: &ClaudeCredentials,
    now: Timestamp,
    unrenewable: CollectError,
) -> Result<ClaudeCredentials, CollectError> {
    let current = Document::read(claude_home, now)?.credentials()?;
    match current.ensure_usable(now) {
        Ok(()) if current.access_token != stale.access_token => return Ok(current),
        Ok(()) | Err(CollectError::AuthRequired { .. }) => {}
        Err(error) => return Err(error),
    }
    let Some(refresh_token) = current.refresh_token else {
        return Err(unrenewable);
    };
    let redeemed = redeem(transport, &refresh_token);
    let latest = Document::read(claude_home, now)?;
    let stored = latest.credentials()?;
    if stored.refresh_token.as_ref() != Some(&refresh_token) {
        // Claude Code (or another reading) saved its own renewal meanwhile; that store is
        // authoritative, and also explains a refusal of the token it already redeemed.
        return Ok(stored);
    }
    latest.save(&redeemed?, now)
}

fn token_request() -> Request {
    Request {
        url: TOKEN_URL.to_owned(),
        bearer: None,
        headers: vec![
            ("Content-Type", "application/json".to_owned()),
            ("Accept", "application/json".to_owned()),
            ("anthropic-beta", "oauth-2025-04-20".to_owned()),
        ],
    }
}

fn redeem(transport: &dyn Transport, refresh_token: &Secret) -> Result<Renewal, CollectError> {
    let body = Secret::new(
        json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token.expose(),
            "client_id": CLIENT_ID,
        })
        .to_string(),
    );
    let response = transport.post_json(&token_request(), &body)?;
    match response.status {
        200 => parse(&response.body),
        // invalid_grant and friends: the refresh token is dead; only a new sign-in helps.
        400 | 401 => Err(CollectError::AuthRequired {
            reason: AuthReason::Rejected,
        }),
        429 => Err(CollectError::RateLimited {
            retry_after: response.retry_after,
        }),
        status => Err(CollectError::Server { status }),
    }
}

fn parse(body: &[u8]) -> Result<Renewal, CollectError> {
    let root: Value = serde_json::from_slice(body).map_err(|_| CollectError::InvalidResponse)?;
    let token = |key: &str| {
        root.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(Secret::new)
    };
    Ok(Renewal {
        access_token: token("access_token").ok_or(CollectError::InvalidResponse)?,
        refresh_token: token("refresh_token"),
        expires_in: root.get("expires_in").and_then(Value::as_u64),
        scopes: root
            .get("scope")
            .and_then(Value::as_str)
            .map(|scope| {
                scope
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .filter(|scopes| !scopes.is_empty()),
    })
}
