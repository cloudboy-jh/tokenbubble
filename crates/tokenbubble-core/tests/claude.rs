mod common;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{MockTransport, NOW_SECS, RecordedRequest, fixture, now};
use jiff::Timestamp;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokenbubble_core::model::{ProviderId, UsageSnapshot, WindowKind};
use tokenbubble_core::providers::claude::{
    self, AUTH_FILE, ClaudeCredentials, ClaudeSource, TOKEN_URL, USAGE_URL,
};
use tokenbubble_core::providers::{AuthReason, CollectError};
use tokenbubble_core::secret::Secret;
use tokenbubble_core::transport::{Request, Response, Transport, TransportError};

const ACCESS: &str = "synthetic-claude-access-secret";
const REFRESH: &str = "synthetic-claude-refresh-secret";
const NEW_ACCESS: &str = "synthetic-renewed-access-secret";
const NEW_REFRESH: &str = "synthetic-renewed-refresh-secret";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

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
        // Expired or not, credentials that cannot report usage are never renewed.
        for expires_at in [(NOW_SECS + 3600) * 1000, (NOW_SECS - 3600) * 1000] {
            let mut value = auth();
            value["claudeAiOauth"]["scopes"] = scopes.clone();
            value["claudeAiOauth"]["expiresAt"] = json!(expires_at);
            let (_directory, source) = source_with(&value);
            let transport = MockTransport::ok(fixture("claude/usage_full.json"));
            assert!(matches!(
                claude::collect(&source, &transport, now()),
                Err(CollectError::Unsupported { .. })
            ));
            assert_eq!(transport.request_count(), 0);
            assert_eq!(transport.post_count(), 0);
        }
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

type Hook = Box<dyn Fn(usize) + Send + Sync>;

/// Answers GETs and POSTs from one ordered queue; `hook` runs with each call's index
/// before it is answered, to simulate Claude Code touching the store meanwhile.
struct SequenceTransport {
    responses: Mutex<VecDeque<Result<Response, TransportError>>>,
    requests: Mutex<Vec<Request>>,
    posts: Mutex<Vec<RecordedRequest>>,
    calls: AtomicUsize,
    hook: Option<Hook>,
}

impl SequenceTransport {
    fn new(responses: Vec<Result<Response, TransportError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
            posts: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            hook: None,
        }
    }

    fn with_hook(mut self, hook: impl Fn(usize) + Send + Sync + 'static) -> Self {
        self.hook = Some(Box::new(hook));
        self
    }

    fn next(&self) -> Result<Response, TransportError> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(hook) = &self.hook {
            hook(index);
        }
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected retry")
    }

    fn bearers(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.bearer.as_ref().unwrap().expose().to_owned())
            .collect()
    }
}

impl Transport for SequenceTransport {
    fn get(&self, request: &Request) -> Result<Response, TransportError> {
        self.requests.lock().unwrap().push(request.clone());
        self.next()
    }

    fn post_json(&self, request: &Request, body: &Secret) -> Result<Response, TransportError> {
        self.posts
            .lock()
            .unwrap()
            .push(RecordedRequest::new(request, Some(body)));
        self.next()
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
    assert_eq!(
        credentials.refresh_token.as_ref().unwrap().expose(),
        REFRESH
    );
    let request = claude::usage_request(&credentials);
    let (_directory, source) = source_with(&expired_auth());
    let transport =
        MockTransport::ok(fixture("claude/usage_full.json")).with_post(token_response(renewal()));
    let snapshot = claude::collect(&source, &transport, now()).unwrap();
    let renewed = claude::load_credentials(&source.claude_home).unwrap();
    let visible = format!(
        "{credentials:?} {renewed:?} {request:?} {snapshot:?} {}",
        serde_json::to_string(&snapshot).unwrap()
    );
    for secret in [ACCESS, REFRESH, NEW_ACCESS, NEW_REFRESH] {
        assert!(!visible.contains(secret), "{secret} leaked");
    }
    let serialized = serde_json::to_value(snapshot).unwrap();
    assert!(serialized.get("access_token").is_none());
    assert!(serialized.get("refresh_token").is_none());
}

fn expired_auth() -> Value {
    let mut value = auth();
    value["claudeAiOauth"]["expiresAt"] = json!((NOW_SECS - 60) * 1000);
    value
}

/// Expired credentials with sibling keys and fields TokenBubble does not know about.
fn stored_auth() -> Value {
    json!({
        "mcpOAuth": {"server|1234": {"accessToken": "mcp-secret", "expiresAt": 0}},
        "claudeAiOauth": {
            "accessToken": ACCESS,
            "refreshToken": REFRESH,
            "expiresAt": (NOW_SECS - 60) * 1000,
            "scopes": ["user:inference", "user:profile"],
            "subscriptionType": "max",
            "rateLimitTier": "default_claude_max_5x",
            "futureField": {"nested": [1, 2.5, null, "text"]}
        },
        "zzUnknown": true
    })
}

fn renewal() -> Value {
    json!({
        "token_type": "Bearer",
        "access_token": NEW_ACCESS,
        "refresh_token": NEW_REFRESH,
        "expires_in": 28800,
        "scope": "user:inference user:profile user:sessions:claude_code",
        "account": {"uuid": "synthetic", "email_address": "user@example.com"}
    })
}

fn token_response(body: Value) -> Result<Response, TransportError> {
    response(200, body.to_string().as_bytes())
}

fn healthy_response() -> Result<Response, TransportError> {
    response(200, healthy().to_string().as_bytes())
}

fn stored(source: &ClaudeSource) -> Value {
    serde_json::from_slice(&std::fs::read(source.credential_path()).unwrap()).unwrap()
}

fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn write_store(path: &Path, value: &Value) {
    std::fs::write(path, value.to_string()).unwrap();
}

#[test]
fn expired_token_is_renewed_saved_in_place_and_used() {
    let original = stored_auth();
    let (directory, source) = source_with(&original);
    let transport =
        MockTransport::ok(healthy().to_string().into_bytes()).with_post(token_response(renewal()));
    let snapshot = claude::collect(&source, &transport, now()).unwrap();
    assert_eq!(snapshot.account.plan.as_deref(), Some("max"));

    let posts = transport.posts.lock();
    assert_eq!(posts.len(), 1);
    assert_eq!(TOKEN_URL, "https://platform.claude.com/v1/oauth/token");
    assert_eq!(posts[0].url, TOKEN_URL);
    assert_eq!(posts[0].bearer, None);
    assert_eq!(posts[0].header("Content-Type"), Some("application/json"));
    assert_eq!(posts[0].header("Accept"), Some("application/json"));
    assert_eq!(posts[0].header("anthropic-beta"), Some("oauth-2025-04-20"));
    let body = posts[0].json_body();
    assert_eq!(
        body,
        json!({"grant_type": "refresh_token", "refresh_token": REFRESH, "client_id": CLIENT_ID})
    );
    assert_eq!(keys(&body), ["grant_type", "refresh_token", "client_id"]);
    let requests = transport.requests.lock();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, USAGE_URL);
    assert_eq!(requests[0].bearer.as_deref(), Some(NEW_ACCESS));

    let mut expected = original.clone();
    let oauth = &mut expected["claudeAiOauth"];
    oauth["accessToken"] = json!(NEW_ACCESS);
    oauth["refreshToken"] = json!(NEW_REFRESH);
    oauth["expiresAt"] = json!((NOW_SECS + 28800) * 1000);
    oauth["scopes"] = json!([
        "user:inference",
        "user:profile",
        "user:sessions:claude_code"
    ]);
    let saved = stored(&source);
    assert_eq!(saved, expected);
    assert_eq!(keys(&saved), keys(&original));
    assert_eq!(
        keys(&saved["claudeAiOauth"]),
        keys(&original["claudeAiOauth"])
    );
    assert!(saved["claudeAiOauth"]["expiresAt"].is_i64());
    assert_eq!(entries(directory.path()), [AUTH_FILE]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(source.credential_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // The saved sign-in is fresh: the next reading needs no renewal.
    let next = MockTransport::ok(healthy().to_string().into_bytes());
    claude::collect(&source, &next, now()).unwrap();
    assert_eq!(next.requests.lock()[0].bearer.as_deref(), Some(NEW_ACCESS));
}

#[test]
fn renewal_without_rotation_or_scope_keeps_the_stored_values() {
    let original = stored_auth();
    let (_directory, source) = source_with(&original);
    let transport = MockTransport::ok(healthy().to_string().into_bytes()).with_post(
        token_response(json!({"access_token": NEW_ACCESS, "expires_in": 3600})),
    );
    claude::collect(&source, &transport, now()).unwrap();
    let mut expected = original.clone();
    expected["claudeAiOauth"]["accessToken"] = json!(NEW_ACCESS);
    expected["claudeAiOauth"]["expiresAt"] = json!((NOW_SECS + 3600) * 1000);
    assert_eq!(stored(&source), expected);
    assert_eq!(
        transport.requests.lock()[0].bearer.as_deref(),
        Some(NEW_ACCESS)
    );
}

#[test]
fn renewal_saved_by_claude_code_meanwhile_is_kept_and_used() {
    let mut theirs = stored_auth();
    theirs["claudeAiOauth"]["accessToken"] = json!("claude-code-access");
    theirs["claudeAiOauth"]["refreshToken"] = json!("claude-code-refresh");
    theirs["claudeAiOauth"]["expiresAt"] = json!((NOW_SECS + 3600) * 1000);
    // Ours succeeded but Claude Code saved first, or Claude Code redeemed the shared
    // refresh token first so ours was refused: either way, its store wins.
    for answer in [
        token_response(renewal()),
        response(400, br#"{"error":"invalid_grant"}"#),
    ] {
        let (_directory, source) = source_with(&stored_auth());
        let path = source.credential_path();
        let written = theirs.clone();
        let transport =
            SequenceTransport::new(vec![answer, healthy_response()]).with_hook(move |index| {
                if index == 0 {
                    write_store(&path, &written);
                }
            });
        claude::collect(&source, &transport, now()).unwrap();
        assert_eq!(transport.posts.lock().unwrap().len(), 1);
        assert_eq!(
            std::fs::read_to_string(source.credential_path()).unwrap(),
            theirs.to_string()
        );
        assert_eq!(transport.bearers(), ["claude-code-access"]);
    }
}

#[test]
fn renewal_already_saved_by_claude_code_needs_no_request() {
    // The token looked fresh but was revoked; Claude Code renews before we do.
    let (_directory, source) = source_with(&auth());
    let mut theirs = auth();
    theirs["claudeAiOauth"]["accessToken"] = json!("claude-code-access");
    theirs["claudeAiOauth"]["refreshToken"] = json!("claude-code-refresh");
    let path = source.credential_path();
    let written = theirs.clone();
    let transport = SequenceTransport::new(vec![response(401, b""), healthy_response()]).with_hook(
        move |index| {
            if index == 0 {
                write_store(&path, &written);
            }
        },
    );
    claude::collect(&source, &transport, now()).unwrap();
    assert!(transport.posts.lock().unwrap().is_empty());
    assert_eq!(transport.bearers(), [ACCESS, "claude-code-access"]);
    assert_eq!(stored(&source), theirs);
}

#[test]
fn rejected_or_failed_renewal_leaves_the_store_untouched() {
    let rate_limited = Ok(Response {
        status: 429,
        body: Vec::new(),
        retry_after: Some(Duration::from_secs(30)),
    });
    let rejected = CollectError::AuthRequired {
        reason: AuthReason::Rejected,
    };
    for (answer, expected) in [
        (
            response(
                400,
                br#"{"error":"invalid_grant","detail":"upstream-private"}"#,
            ),
            rejected.clone(),
        ),
        (response(401, b"upstream-private"), rejected),
        (
            rate_limited,
            CollectError::RateLimited {
                retry_after: Some(Duration::from_secs(30)),
            },
        ),
        (
            response(503, b"upstream-private"),
            CollectError::Server { status: 503 },
        ),
        (
            Err(TransportError::Timeout),
            CollectError::Transport(TransportError::Timeout),
        ),
        (
            response(200, b"upstream-private"),
            CollectError::InvalidResponse,
        ),
        (
            token_response(json!({"refresh_token": NEW_REFRESH})),
            CollectError::InvalidResponse,
        ),
    ] {
        let (directory, source) = source_with(&stored_auth());
        let before = std::fs::read(source.credential_path()).unwrap();
        let transport = MockTransport::ok(healthy().to_string().into_bytes()).with_post(answer);
        let error = claude::collect(&source, &transport, now()).unwrap_err();
        assert_eq!(error, expected);
        let visible = format!("{error:?} {error}");
        for secret in ["upstream-private", REFRESH, NEW_REFRESH] {
            assert!(!visible.contains(secret));
        }
        assert_eq!(transport.post_count(), 1);
        assert_eq!(transport.request_count(), 0);
        assert_eq!(std::fs::read(source.credential_path()).unwrap(), before);
        assert_eq!(entries(directory.path()), [AUTH_FILE]);
    }
}

#[test]
fn expired_token_without_refresh_token_is_not_renewed() {
    for refresh in [None, Some(json!("")), Some(json!("  "))] {
        let mut value = expired_auth();
        let oauth = value["claudeAiOauth"].as_object_mut().unwrap();
        match refresh {
            Some(token) => oauth.insert("refreshToken".to_owned(), token),
            None => oauth.remove("refreshToken"),
        };
        let (_directory, source) = source_with(&value);
        let transport = MockTransport::ok(healthy().to_string().into_bytes());
        assert_eq!(
            claude::collect(&source, &transport, now()),
            Err(CollectError::AuthRequired {
                reason: AuthReason::Expired
            })
        );
        assert_eq!(transport.post_count(), 0);
        assert_eq!(transport.request_count(), 0);
    }
}

#[test]
fn fresh_token_is_used_without_renewal() {
    let (_directory, source) = source_with(&auth());
    let before = std::fs::read(source.credential_path()).unwrap();
    let transport = MockTransport::ok(healthy().to_string().into_bytes());
    claude::collect(&source, &transport, now()).unwrap();
    assert_eq!(transport.post_count(), 0);
    assert_eq!(transport.requests.lock()[0].bearer.as_deref(), Some(ACCESS));
    assert_eq!(std::fs::read(source.credential_path()).unwrap(), before);
}

#[test]
fn rejected_fresh_token_is_renewed_and_retried_once() {
    let (_directory, source) = source_with(&auth());
    let transport = SequenceTransport::new(vec![
        response(401, b""),
        token_response(renewal()),
        healthy_response(),
    ]);
    claude::collect(&source, &transport, now()).unwrap();
    assert_eq!(transport.posts.lock().unwrap().len(), 1);
    assert_eq!(transport.bearers(), [ACCESS, NEW_ACCESS]);
    assert_eq!(
        stored(&source)["claudeAiOauth"]["refreshToken"],
        NEW_REFRESH
    );

    let rejected = Err(CollectError::AuthRequired {
        reason: AuthReason::Rejected,
    });
    // Still rejected after renewal: no second renewal.
    let (_directory, source) = source_with(&auth());
    let transport = SequenceTransport::new(vec![
        response(401, b""),
        token_response(renewal()),
        response(401, b""),
    ]);
    assert_eq!(claude::collect(&source, &transport, now()), rejected);
    assert_eq!(transport.posts.lock().unwrap().len(), 1);
    assert_eq!(transport.requests.lock().unwrap().len(), 2);

    // An expired token renewed once is not renewed again on 401.
    let (_directory, source) = source_with(&expired_auth());
    let transport = SequenceTransport::new(vec![token_response(renewal()), response(401, b"")]);
    assert_eq!(claude::collect(&source, &transport, now()), rejected);
    assert_eq!(transport.posts.lock().unwrap().len(), 1);
    assert_eq!(transport.requests.lock().unwrap().len(), 1);

    // Nothing to renew with: rejected as before.
    let mut value = auth();
    value["claudeAiOauth"]
        .as_object_mut()
        .unwrap()
        .remove("refreshToken");
    let (_directory, source) = source_with(&value);
    let transport = SequenceTransport::new(vec![response(401, b"upstream-private")]);
    assert_eq!(claude::collect(&source, &transport, now()), rejected);
    assert!(transport.posts.lock().unwrap().is_empty());
    assert_eq!(transport.requests.lock().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn renewal_writes_through_a_symlinked_store() {
    let directory = tempfile::tempdir().unwrap();
    let real = directory.path().join("dotfiles");
    std::fs::create_dir(&real).unwrap();
    let target = real.join("claude-credentials.json");
    write_store(&target, &stored_auth());
    let home = directory.path().join("home");
    std::fs::create_dir(&home).unwrap();
    std::os::unix::fs::symlink(&target, home.join(AUTH_FILE)).unwrap();
    let source = ClaudeSource { claude_home: home };
    let transport =
        MockTransport::ok(healthy().to_string().into_bytes()).with_post(token_response(renewal()));
    claude::collect(&source, &transport, now()).unwrap();
    assert!(
        std::fs::symlink_metadata(source.credential_path())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(stored(&source)["claudeAiOauth"]["accessToken"], NEW_ACCESS);
    assert_eq!(entries(&real), ["claude-credentials.json"]);
}

#[cfg(unix)]
#[test]
fn unsavable_renewal_is_reported_and_leaves_no_temp_files() {
    use std::os::unix::fs::PermissionsExt;

    let (directory, source) = source_with(&stored_auth());
    let before = std::fs::read(source.credential_path()).unwrap();
    let set_mode = |mode| {
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(mode)).unwrap()
    };
    set_mode(0o500);
    let probe = directory.path().join("probe");
    if std::fs::write(&probe, b"").is_ok() {
        // Running as root: directory permissions cannot make the write fail.
        set_mode(0o700);
        return;
    }
    let transport =
        MockTransport::ok(healthy().to_string().into_bytes()).with_post(token_response(renewal()));
    let error = claude::collect(&source, &transport, now()).unwrap_err();
    set_mode(0o700);
    assert_eq!(
        error,
        CollectError::CredentialsUnwritable {
            path: source.credential_path()
        }
    );
    assert_eq!(error.kind(), "credentials_unwritable");
    assert!(!error.is_transient());
    let visible = format!("{error:?} {error}");
    for secret in [ACCESS, REFRESH, NEW_ACCESS, NEW_REFRESH] {
        assert!(!visible.contains(secret));
    }
    assert_eq!(transport.request_count(), 0);
    assert_eq!(std::fs::read(source.credential_path()).unwrap(), before);
    assert_eq!(entries(directory.path()), [AUTH_FILE]);
}
