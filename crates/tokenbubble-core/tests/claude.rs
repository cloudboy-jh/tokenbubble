mod common;

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use common::{MockTransport, NOW_SECS, fixture, now};
use jiff::Timestamp;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokenbubble_core::model::{ProviderId, UsageSnapshot, WindowKind};
use tokenbubble_core::providers::claude::{
    self, AUTH_FILE, ClaudeCredentials, ClaudeSource, USAGE_URL,
};
use tokenbubble_core::providers::{AuthReason, CollectError};
use tokenbubble_core::transport::{Request, Response, Transport, TransportError};

const ACCESS: &str = "synthetic-claude-access-secret";
const REFRESH: &str = "synthetic-claude-refresh-secret";

fn auth() -> Value {
    json!({"claudeAiOauth": {
        "accessToken": ACCESS,
        "refreshToken": REFRESH,
        "expiresAt": (NOW_SECS + 3600) * 1000,
        "scopes": ["user:inference", "user:profile"],
        "subscriptionType": "max",
        "rateLimitTier": "default_claude_max_5x"
    }})
}

fn parse(value: &Value) -> Result<ClaudeCredentials, CollectError> {
    ClaudeCredentials::parse(value.to_string().as_bytes())
}

fn source_with(value: &Value) -> (TempDir, ClaudeSource) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join(AUTH_FILE), value.to_string()).unwrap();
    let source = ClaudeSource {
        claude_home: directory.path().to_owned(),
    };
    (directory, source)
}

fn collect_body(body: Value) -> Result<UsageSnapshot, CollectError> {
    let (_directory, source) = source_with(&auth());
    claude::collect(
        &source,
        &MockTransport::ok(body.to_string().into_bytes()),
        now(),
    )
}

fn healthy() -> Value {
    json!({"five_hour": {"utilization": 19}})
}

#[test]
fn expiry_uses_milliseconds_and_expires_at_the_exact_boundary() {
    let mut value = auth();
    let milliseconds = NOW_SECS * 1000;
    value["claudeAiOauth"]["expiresAt"] = json!(milliseconds + 1);
    let credentials = parse(&value).unwrap();
    assert_eq!(
        credentials.expires_at,
        Some("2026-01-01T00:00:00.001Z".parse::<Timestamp>().unwrap())
    );
    assert!(credentials.ensure_usable(now()).is_ok());
    assert_eq!(
        credentials.ensure_usable(credentials.expires_at.unwrap()),
        Err(CollectError::AuthRequired {
            reason: AuthReason::Expired
        })
    );
    for expiry in [
        json!(milliseconds),
        json!(milliseconds - 1),
        Value::Null,
        json!(NOW_SECS),
    ] {
        value["claudeAiOauth"]["expiresAt"] = expiry;
        assert_eq!(
            parse(&value).unwrap().ensure_usable(now()),
            Err(CollectError::AuthRequired {
                reason: AuthReason::Expired
            })
        );
    }
    value["claudeAiOauth"]
        .as_object_mut()
        .unwrap()
        .remove("expiresAt");
    assert!(matches!(
        parse(&value).unwrap().ensure_usable(now()),
        Err(CollectError::AuthRequired { .. })
    ));
}

#[test]
fn missing_profile_scope_and_mcp_tokens_never_reach_transport() {
    for scopes in [
        json!([]),
        json!(["user:inference"]),
        json!(["user:profile:other"]),
    ] {
        let mut value = auth();
        value["claudeAiOauth"]["scopes"] = scopes;
        let (_directory, source) = source_with(&value);
        let transport = MockTransport::ok(fixture("claude/usage_full.json"));
        assert!(matches!(
            claude::collect(&source, &transport, now()),
            Err(CollectError::Unsupported { .. })
        ));
        assert_eq!(transport.request_count(), 0);
    }
    assert!(matches!(
        parse(&json!({"mcpOAuth": {"accessToken": ACCESS}})),
        Err(CollectError::Unsupported { .. })
    ));
    let mut value = auth();
    value["mcpOAuth"] = json!({"accessToken": "not-the-subscription-token"});
    assert_eq!(parse(&value).unwrap().access_token.expose(), ACCESS);
}

#[test]
fn missing_or_blank_tokens_and_malformed_credentials_are_rejected() {
    for token in [Value::Null, json!(""), json!("  "), json!(42)] {
        let mut value = auth();
        value["claudeAiOauth"]["accessToken"] = token;
        assert!(matches!(
            parse(&value),
            Err(CollectError::CredentialsMalformed { .. })
        ));
    }
    for bytes in [b"[]".as_slice(), b"{not json", b"{}"] {
        assert!(matches!(
            ClaudeCredentials::parse(bytes),
            Err(CollectError::CredentialsMalformed { .. })
        ));
    }
}

#[test]
fn credential_reads_are_bounded_and_do_not_modify_the_file() {
    let (_directory, source) = source_with(&auth());
    let before = std::fs::read(source.credential_path()).unwrap();
    claude::load_credentials(&source.claude_home).unwrap();
    assert_eq!(std::fs::read(source.credential_path()).unwrap(), before);
    std::fs::write(source.credential_path(), vec![b' '; 1024 * 1024 + 1]).unwrap();
    assert!(matches!(
        claude::load_credentials(&source.claude_home),
        Err(CollectError::CredentialsMalformed { .. })
    ));
}

#[test]
fn missing_selected_source_does_not_fall_back_to_another_profile() {
    let directory = tempfile::tempdir().unwrap();
    let source = ClaudeSource {
        claude_home: directory.path().to_owned(),
    };
    let transport = MockTransport::ok(fixture("claude/usage_full.json"));
    assert_eq!(
        claude::collect(&source, &transport, now()),
        Err(CollectError::CredentialsMissing {
            path: source.credential_path(),
        })
    );
    assert_eq!(transport.request_count(), 0);
}

#[test]
fn maps_all_windows_raw_overquota_and_separate_cents_spend() {
    let (_directory, source) = source_with(&auth());
    let transport = MockTransport::ok(fixture("claude/usage_full.json"));
    let snapshot = claude::collect(&source, &transport, now()).unwrap();
    assert_eq!(snapshot.provider, ProviderId::Claude);
    assert_eq!(snapshot.account.plan.as_deref(), Some("max"));
    assert_eq!(snapshot.account.email, None);
    assert_eq!(snapshot.account.id, None);
    assert_eq!(snapshot.windows.len(), 7);
    assert_eq!(snapshot.windows[0].kind, WindowKind::Session);
    assert_eq!(snapshot.windows[0].duration_secs, Some(18_000));
    assert_eq!(snapshot.windows[0].used_percent, 112.75);
    assert_eq!(
        snapshot.windows[0].resets_at,
        Some("2026-01-01T05:00:00.123Z".parse::<Timestamp>().unwrap())
    );
    assert!(
        snapshot.windows[1..]
            .iter()
            .all(|window| window.kind == WindowKind::Weekly)
    );
    let scoped = snapshot
        .windows
        .iter()
        .find(|window| window.id == "claude-weekly-scoped-claude-example")
        .unwrap();
    assert_eq!(scoped.used_percent, 135.5); // is_active=false still reports an enforceable quota.
    assert_eq!(scoped.scope.as_deref(), Some("Example model"));
    let spend = snapshot.spend_limit.unwrap();
    assert_eq!(spend.limit, Some(50.0));
    assert_eq!(spend.used, Some(12.5));
    assert_eq!(spend.remaining_percent, Some(75.0));
    assert_eq!(spend.currency.as_deref(), Some("USD"));
    assert_eq!(snapshot.credits, None);
    assert!(snapshot.issues.is_empty());
    let requests = transport.requests.lock();
    assert_eq!(requests[0].url, USAGE_URL);
    assert_eq!(requests[0].bearer.as_deref(), Some(ACCESS));
    assert_eq!(
        requests[0].header("anthropic-beta"),
        Some("oauth-2025-04-20")
    );
    assert_eq!(
        requests[0].header("User-Agent"),
        Some("claude-cli/2.1.0 (external, cli)")
    );
    assert_eq!(requests[0].header("Accept"), Some("application/json"));
    assert_eq!(requests[0].header("Content-Type"), Some("application/json"));
}

#[test]
fn malformed_optional_parts_do_not_hide_healthy_readings_or_invent_zero() {
    let snapshot = collect_body(json!({
        "five_hour": {"utilization": 0, "resets_at": "broken"},
        "seven_day": {"utilization": null},
        "seven_day_opus": "broken",
        "seven_day_sonnet": {"utilization": 43},
        "extra_usage": {"is_enabled": true, "monthly_limit": "broken", "used_credits": 125, "currency": "EUR"},
        "limits": [false, {"kind": "weekly_scoped", "group": "weekly", "percent": "bad"}]
    })).unwrap();
    assert_eq!(snapshot.windows.len(), 2);
    assert_eq!(snapshot.windows[0].used_percent, 0.0);
    assert_eq!(snapshot.windows[0].resets_at, None);
    let spend = snapshot.spend_limit.unwrap();
    assert_eq!(spend.limit, None);
    assert_eq!(spend.used, Some(1.25));
    assert_eq!(spend.remaining_percent, None);
    assert_eq!(spend.currency.as_deref(), Some("EUR"));
    assert!(!snapshot.issues.is_empty());
    assert_eq!(
        collect_body(json!({"five_hour": {"resets_at": null}})),
        Err(CollectError::NoUsageData)
    );
    assert_eq!(collect_body(json!([])), Err(CollectError::InvalidResponse));
}

#[test]
fn feature_aliases_and_scoped_models_keep_stable_deduplicated_windows() {
    let snapshot = collect_body(json!({
        "seven_day_routines": "broken",
        "cowork": {"utilization": 12},
        "limits": [
            {"kind":"weekly_scoped", "group":"weekly", "percent":30, "scope":{"model":{"id":"fable", "display_name":"Fable"}}},
            {"kind":"weekly_scoped", "group":"weekly", "percent":80, "scope":{"model":{"id":"fable", "display_name":"Fable"}}},
            {"kind":"weekly_scoped", "group":"weekly", "percent":40, "scope":{"model":{"id":"all-models", "display_name":"All models"}}}
        ]
    })).unwrap();
    assert_eq!(snapshot.windows.len(), 2);
    assert_eq!(snapshot.windows[0].id, "claude-routines");
    assert_eq!(snapshot.windows[1].id, "claude-weekly-scoped-fable");
    assert_eq!(snapshot.windows[1].used_percent, 30.0);
}

#[test]
fn spend_only_unknown_metadata_and_disabled_extra_usage_remain_distinct() {
    let snapshot = collect_body(
        json!({"extra_usage":{"is_enabled":true, "used_credits":300, "monthly_limit":1000}}),
    )
    .unwrap();
    assert!(snapshot.windows.is_empty());
    assert_eq!(
        snapshot.spend_limit.unwrap().currency.as_deref(),
        Some("USD")
    );
    let mut value = auth();
    value["claudeAiOauth"]
        .as_object_mut()
        .unwrap()
        .remove("subscriptionType");
    value["claudeAiOauth"]
        .as_object_mut()
        .unwrap()
        .remove("rateLimitTier");
    let (_directory, source) = source_with(&value);
    let snapshot = claude::collect(
        &source,
        &MockTransport::ok(healthy().to_string().into_bytes()),
        now(),
    )
    .unwrap();
    assert_eq!(snapshot.account.plan, None);
    for enabled in [Value::Null, json!(false)] {
        let mut body = healthy();
        body["extra_usage"] = json!({"is_enabled": enabled, "used_credits":300});
        assert_eq!(collect_body(body).unwrap().spend_limit, None);
    }
}

struct SequenceTransport {
    responses: Mutex<VecDeque<Result<Response, TransportError>>>,
    requests: Mutex<Vec<Request>>,
}

impl SequenceTransport {
    fn new(responses: Vec<Result<Response, TransportError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl Transport for SequenceTransport {
    fn get(&self, request: &Request) -> Result<Response, TransportError> {
        self.requests.lock().unwrap().push(request.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected retry")
    }
}

fn response(status: u16, body: &[u8]) -> Result<Response, TransportError> {
    Ok(Response {
        status,
        body: body.to_vec(),
        retry_after: None,
    })
}

#[test]
fn compatibility_retries_only_inventory_rejection_once_with_same_credential() {
    for status in [400, 403] {
        let (_directory, source) = source_with(&auth());
        let transport = SequenceTransport::new(vec![
            response(status, b"optional inventory rejected"),
            response(200, healthy().to_string().as_bytes()),
        ]);
        claude::collect(&source, &transport, now()).unwrap();
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].url, USAGE_URL);
        assert_eq!(requests[1].url, "https://api.anthropic.com/api/oauth/usage");
        assert_eq!(requests[1].header("User-Agent"), Some("claude-code/2.1.0"));
        assert_eq!(requests[1].bearer, requests[0].bearer);
        assert_eq!(
            requests[1].header("anthropic-beta"),
            requests[0].header("anthropic-beta")
        );
    }
    let (_directory, source) = source_with(&auth());
    let transport = SequenceTransport::new(vec![response(400, b"first"), response(400, b"second")]);
    assert_eq!(
        claude::collect(&source, &transport, now()),
        Err(CollectError::Server { status: 400 })
    );
    assert_eq!(transport.requests.lock().unwrap().len(), 2);
}

#[test]
fn maps_auth_scope_rate_limit_server_and_transport_errors_without_body_leaks() {
    let (_directory, source) = source_with(&auth());
    for (status, body, expected) in [
        (
            401,
            "upstream-private",
            CollectError::AuthRequired {
                reason: AuthReason::Rejected,
            },
        ),
        (
            503,
            "upstream-private",
            CollectError::Server { status: 503 },
        ),
        (
            201,
            "upstream-private",
            CollectError::Server { status: 201 },
        ),
    ] {
        let transport = SequenceTransport::new(vec![response(status, body.as_bytes())]);
        let error = claude::collect(&source, &transport, now()).unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains("upstream-private"));
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
    }
    let scope = SequenceTransport::new(vec![response(
        403,
        b"missing user:profile upstream-private",
    )]);
    let error = claude::collect(&source, &scope, now()).unwrap_err();
    assert!(matches!(error, CollectError::Unsupported { .. }));
    assert!(!error.to_string().contains("upstream-private"));
    assert_eq!(scope.requests.lock().unwrap().len(), 1);
    let rate_limit = MockTransport::new(Ok(Response {
        status: 429,
        body: Vec::new(),
        retry_after: Some(Duration::from_secs(75)),
    }));
    assert_eq!(
        claude::collect(&source, &rate_limit, now()),
        Err(CollectError::RateLimited {
            retry_after: Some(Duration::from_secs(75))
        })
    );
    assert_eq!(rate_limit.request_count(), 1);
    let transport = SequenceTransport::new(vec![Err(TransportError::Timeout)]);
    assert_eq!(
        claude::collect(&source, &transport, now()),
        Err(CollectError::Transport(TransportError::Timeout))
    );
    assert_eq!(transport.requests.lock().unwrap().len(), 1);
}

#[test]
fn snapshots_and_debug_never_serialize_credential_material() {
    let credentials = parse(&auth()).unwrap();
    let request = claude::usage_request(&credentials);
    let (_directory, source) = source_with(&auth());
    let transport = MockTransport::ok(fixture("claude/usage_full.json"));
    let snapshot = claude::collect(&source, &transport, now()).unwrap();
    let visible = format!(
        "{credentials:?} {request:?} {snapshot:?} {}",
        serde_json::to_string(&snapshot).unwrap()
    );
    for secret in [ACCESS, REFRESH] {
        assert!(!visible.contains(secret));
    }
    let serialized = serde_json::to_value(snapshot).unwrap();
    assert!(serialized.get("access_token").is_none());
    assert!(serialized.get("refresh_token").is_none());
}
