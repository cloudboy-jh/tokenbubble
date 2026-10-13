//! Read-only view of Codex CLI's `auth.json`.
//!
//! Adapted from CodexBar `CodexOAuthCredentials.swift` (MIT; see THIRD_PARTY_NOTICES.md). The
//! refresh token is never read: Codex CLI owns refresh and persistence.

use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::ops::RangeInclusive;
use std::path::Path;
use std::thread;
use std::time::Duration;

use base64::Engine;
use base64::alphabet::URL_SAFE;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use jiff::{SignedDuration, Timestamp};
use serde::Deserializer;
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde_json::{Map, Value};

use crate::providers::{AuthReason, CollectError};
use crate::secret::Secret;

pub const AUTH_FILE: &str = "auth.json";

const MAX_AUTH_BYTES: u64 = 1024 * 1024;
/// Codex treats access tokens within five minutes of expiry as needing refresh.
const EXPIRY_MARGIN: SignedDuration = SignedDuration::from_mins(5);
/// Without a JWT expiry, Codex refreshes tokens older than eight days.
const MAX_REFRESH_AGE: SignedDuration = SignedDuration::from_hours(8 * 24);
/// UTC seconds accepted by Codex's chrono-based claim decoder.
const EXP_RANGE: RangeInclusive<i64> = -8_334_601_228_800..=8_210_266_876_799;
/// Attempts to observe a consistent file while Codex CLI may be rewriting it.
const READ_ATTEMPTS: u32 = 3;
const READ_RETRY_DELAY: Duration = Duration::from_millis(50);

const JWT_BASE64: GeneralPurpose = GeneralPurpose::new(
    &URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

#[derive(Clone, PartialEq, Eq)]
pub struct CodexCredentials {
    pub access_token: Secret,
    pub account_id: Option<String>,
    pub email: Option<String>,
    /// `chatgpt_plan_type` from the ID token; the usage response's plan takes precedence.
    pub plan_claim: Option<String>,
    /// Best-effort JWT `exp`; used only to decide whether the owner must refresh first.
    pub expires_at: Option<Timestamp>,
    pub last_refresh: Option<Timestamp>,
}

impl fmt::Debug for CodexCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodexCredentials")
            .field("access_token", &self.access_token)
            .field("account_id", &self.account_id)
            .field("expires_at", &self.expires_at)
            .field("last_refresh", &self.last_refresh)
            .finish_non_exhaustive()
    }
}

impl CodexCredentials {
    pub fn parse(bytes: &[u8]) -> Result<Self, CollectError> {
        let root: Map<String, Value> =
            serde_json::from_slice(bytes).map_err(|_| CollectError::CredentialsMalformed {
                detail: "auth.json is not a JSON object",
            })?;
        let tokens = root.get("tokens").and_then(Value::as_object);
        let Some(access_token) =
            tokens.and_then(|tokens| string_field(tokens, "access_token", "accessToken"))
        else {
            if non_empty(root.get("OPENAI_API_KEY").and_then(Value::as_str)).is_some() {
                return Err(CollectError::Unsupported {
                    detail: "Codex is signed in with an API key; subscription usage requires a ChatGPT sign-in",
                });
            }
            return Err(CollectError::CredentialsMalformed {
                detail: "auth.json contains no access token",
            });
        };

        let id_claims = tokens
            .and_then(|tokens| string_field(tokens, "id_token", "idToken"))
            .and_then(jwt_payload)
            .and_then(|payload| serde_json::from_slice::<Map<String, Value>>(&payload).ok());
        let access_payload = jwt_payload(access_token);
        let access_claims = access_payload
            .as_deref()
            .and_then(|payload| serde_json::from_slice::<Map<String, Value>>(payload).ok());

        let account_id = tokens
            .and_then(|tokens| non_empty(string_field(tokens, "account_id", "accountId")))
            .or_else(|| id_claims.as_ref().and_then(account_from_claims))
            .or_else(|| access_claims.as_ref().and_then(account_from_claims));
        let email = id_claims.as_ref().and_then(|claims| {
            non_empty(claims.get("email").and_then(Value::as_str)).or_else(|| {
                non_empty(
                    claims
                        .get("https://api.openai.com/profile")
                        .and_then(|profile| profile.get("email"))
                        .and_then(Value::as_str),
                )
            })
        });
        let plan_claim = id_claims.as_ref().and_then(|claims| {
            non_empty(
                claims
                    .get("https://api.openai.com/auth")
                    .and_then(|auth| auth.get("chatgpt_plan_type"))
                    .and_then(Value::as_str),
            )
            .or_else(|| non_empty(claims.get("chatgpt_plan_type").and_then(Value::as_str)))
        });
        let expires_at = has_three_nonempty_segments(access_token)
            .then_some(access_payload.as_deref())
            .flatten()
            .and_then(exp_claim)
            .map(timestamp_saturating);
        let last_refresh = root
            .get("last_refresh")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<Timestamp>().ok());

        Ok(Self {
            access_token: Secret::new(access_token),
            account_id,
            email,
            plan_claim,
            expires_at,
            last_refresh,
        })
    }

    /// Fails when Codex CLI must refresh the token before it can be used.
    pub fn ensure_fresh(&self, now: Timestamp) -> Result<(), CollectError> {
        let stale = match (self.expires_at, self.last_refresh) {
            (Some(expires_at), _) => expires_at.duration_since(now) <= EXPIRY_MARGIN,
            (None, Some(last_refresh)) => now.duration_since(last_refresh) > MAX_REFRESH_AGE,
            (None, None) => true,
        };
        if stale {
            Err(CollectError::AuthRequired {
                reason: AuthReason::Expired,
            })
        } else {
            Ok(())
        }
    }
}

/// Read and parse `<codex_home>/auth.json`, retrying briefly while the owner may be rewriting it.
pub fn load(codex_home: &Path) -> Result<CodexCredentials, CollectError> {
    let path = codex_home.join(AUTH_FILE);
    let mut attempt = 1;
    loop {
        let result = read_bounded(&path).and_then(|bytes| CodexCredentials::parse(&bytes));
        match result {
            Err(
                CollectError::CredentialsMissing { .. } | CollectError::CredentialsMalformed { .. },
            ) if attempt < READ_ATTEMPTS => {
                attempt += 1;
                thread::sleep(READ_RETRY_DELAY);
            }
            result => return result,
        }
    }
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
            detail: "auth.json is unexpectedly large",
        });
    }
    Ok(bytes)
}

fn string_field<'a>(map: &'a Map<String, Value>, snake: &str, camel: &str) -> Option<&'a str> {
    [snake, camel]
        .into_iter()
        .filter_map(|key| map.get(key).and_then(Value::as_str))
        .find(|value| !value.is_empty())
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn has_three_nonempty_segments(token: &str) -> bool {
    let mut segments = token.split('.');
    let valid = segments.by_ref().take(3).filter(|s| !s.is_empty()).count() == 3;
    valid && segments.next().is_none()
}

fn jwt_payload(token: &str) -> Option<Vec<u8>> {
    let mut segments = token.split('.');
    let (_, payload, _) = (segments.next()?, segments.next()?, segments.next()?);
    if segments.next().is_some() {
        return None;
    }
    JWT_BASE64.decode(payload).ok()
}

fn account_from_claims(claims: &Map<String, Value>) -> Option<String> {
    non_empty(claims.get("chatgpt_account_id").and_then(Value::as_str))
        .or_else(|| {
            non_empty(
                claims
                    .get("https://api.openai.com/auth")
                    .and_then(|auth| auth.get("chatgpt_account_id"))
                    .and_then(Value::as_str),
            )
        })
        .or_else(|| {
            claims
                .get("organizations")?
                .as_array()?
                .iter()
                .find_map(|org| non_empty(org.get("id").and_then(Value::as_str)))
        })
}

/// The top-level `exp` claim when it is spelled as a JSON integer, appears exactly once, and lies
/// within Codex's supported range. Anything else falls back to the `last_refresh` age rule.
fn exp_claim(payload: &[u8]) -> Option<i64> {
    struct ExpVisitor;

    impl<'de> Visitor<'de> for ExpVisitor {
        type Value = Option<i64>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a JWT claims object")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut exp: Option<Value> = None;
            let mut duplicate = false;
            while let Some(key) = map.next_key::<String>()? {
                if key == "exp" {
                    duplicate |= exp.is_some();
                    exp = Some(map.next_value()?);
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            // `as_i64` is `None` for fractional and exponent spellings, which parse as floats.
            Ok(exp.filter(|_| !duplicate).and_then(|value| value.as_i64()))
        }
    }

    let mut deserializer = serde_json::Deserializer::from_slice(payload);
    let exp = deserializer.deserialize_map(ExpVisitor).ok()??;
    deserializer.end().ok()?;
    EXP_RANGE.contains(&exp).then_some(exp)
}

/// Codex's claim range exceeds jiff's; saturate so far-future tokens stay fresh.
fn timestamp_saturating(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).unwrap_or(if seconds > 0 {
        Timestamp::MAX
    } else {
        Timestamp::MIN
    })
}
