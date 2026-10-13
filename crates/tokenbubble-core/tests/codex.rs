mod common;

use std::time::Duration;

use common::{MockTransport, fixture, fresh_access_token, jwt, now, standard_auth, write_auth};
use jiff::Timestamp;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokenbubble_core::model::{QuotaWindow, UsageSnapshot, WindowKind};
use tokenbubble_core::providers::codex::{self, CodexSource, resolve_usage_url};
use tokenbubble_core::providers::{AuthReason, CollectError};
use tokenbubble_core::transport::{Response, TransportError};

fn source_with(auth: &Value) -> (TempDir, CodexSource) {
    let dir = tempfile::tempdir().unwrap();
    write_auth(dir.path(), auth);
    let source = CodexSource {
        codex_home: dir.path().to_owned(),
    };
    (dir, source)
}

fn collect_body(body: Value) -> Result<UsageSnapshot, CollectError> {
    let (_dir, source) = source_with(&standard_auth());
    codex::collect(
        &source,
        &MockTransport::ok(body.to_string().into_bytes()),
        now(),
    )
}

fn window(used: f64, reset_at: i64, secs: i64) -> Value {
    json!({ "used_percent": used, "reset_at": reset_at, "limit_window_seconds": secs })
}

fn ids(windows: &[QuotaWindow]) -> Vec<&str> {
    windows.iter().map(|w| w.id.as_str()).collect()
}

#[test]
fn usage_url_follows_codex_path_rules_and_requires_https() {
    let url = |base| resolve_usage_url(base).unwrap();
    assert_eq!(url(None), "https://chatgpt.com/backend-api/wham/usage");
    assert_eq!(
        url(Some("  ")),
        "https://chatgpt.com/backend-api/wham/usage"
    );
    assert_eq!(
        url(Some("https://chatgpt.com/")),
        "https://chatgpt.com/backend-api/wham/usage"
    );
    assert_eq!(
        url(Some("https://chat.openai.com//")),
        "https://chat.openai.com/backend-api/wham/usage"
    );
    assert_eq!(
        url(Some("https://proxy.example/backend-api/")),
        "https://proxy.example/backend-api/wham/usage"
    );
    assert_eq!(
        url(Some("https://proxy.example")),
        "https://proxy.example/api/codex/usage"
    );
    for insecure in ["http://chatgpt.com", "chatgpt.com", "https://"] {
        assert!(
            matches!(
                resolve_usage_url(Some(insecure)),
                Err(CollectError::Config { .. })
            ),
            "{insecure}"
        );
    }
}

#[test]
fn usage_url_reads_only_top_level_config_key() {
    let (dir, source) = source_with(&standard_auth());
    let config = dir.path().join("config.toml");

    std::fs::write(
        &config,
        "[profiles.work]\nchatgpt_base_url = \"https://ignored.example\"\n",
    )
    .unwrap();
    assert_eq!(
        source.usage_url().unwrap(),
        "https://chatgpt.com/backend-api/wham/usage"
    );

    std::fs::write(
        &config,
        "chatgpt_base_url = \"https://proxy.example\" # comment\n",
    )
    .unwrap();
    assert_eq!(
        source.usage_url().unwrap(),
        "https://proxy.example/api/codex/usage"
    );
}

#[test]
fn collects_normalized_snapshot_with_scoped_request() {
    let (_dir, source) = source_with(&standard_auth());
    let transport = MockTransport::ok(fixture("codex/usage_full.json"));

    let snapshot = codex::collect(&source, &transport, now()).unwrap();

    let requests = transport.requests.lock();
    let [request] = requests.as_slice() else {
        panic!("expected one request")
    };
    assert_eq!(request.url, "https://chatgpt.com/backend-api/wham/usage");
    assert_eq!(
        request.bearer.as_deref(),
        Some(fresh_access_token().as_str())
    );
    assert_eq!(
        request.header("ChatGPT-Account-Id"),
        Some("account-fixture")
    );
    assert_eq!(request.header("Accept"), Some("application/json"));

    assert_eq!(snapshot.measured_at, now());
    assert_eq!(snapshot.account.id.as_deref(), Some("account-fixture"));
    assert_eq!(snapshot.account.email.as_deref(), Some("user@example.com"));
    assert_eq!(
        snapshot.account.plan.as_deref(),
        Some("pro"),
        "response plan wins over claim"
    );
    assert_eq!(
        ids(&snapshot.windows),
        [
            "session",
            "weekly",
            "codex-spark-session",
            "codex-spark-weekly"
        ]
    );
    let session = &snapshot.windows[0];
    assert_eq!(session.kind, WindowKind::Session);
    assert_eq!(session.scope, None);
    assert_eq!(session.used_percent, 7.0);
    assert_eq!(session.duration_secs, Some(18_000));
    assert_eq!(
        session.resets_at,
        Some(Timestamp::from_second(1_766_948_068).unwrap())
    );
    assert_eq!(
        snapshot.windows[2].scope.as_deref(),
        Some("GPT-5.3-Codex-Spark")
    );
    assert_eq!(snapshot.windows[3].used_percent, 11.0);

    let credits = snapshot.credits.unwrap();
    assert!(credits.has_credits && !credits.unlimited);
    assert_eq!(credits.balance, Some(150.5));
    let spend = snapshot.spend_limit.unwrap();
    assert_eq!(
        (spend.limit, spend.used, spend.remaining_percent),
        (Some(200.0), Some(42.25), Some(78.875))
    );
    assert_eq!(
        spend.resets_at,
        Some(Timestamp::from_second(1_769_904_000).unwrap())
    );
    assert!(snapshot.issues.is_empty());
}

#[test]
fn omits_account_header_when_account_is_unknown() {
    let (_dir, source) =
        source_with(&json!({ "tokens": { "access_token": fresh_access_token() } }));
    let transport = MockTransport::ok(fixture("codex/usage_full.json"));
    let snapshot = codex::collect(&source, &transport, now()).unwrap();
    assert_eq!(snapshot.account.id, None);
    assert_eq!(
        transport.requests.lock()[0].header("ChatGPT-Account-Id"),
        None
    );
}

#[test]
fn window_roles_come_from_duration_not_position() {
    let swapped = collect_body(json!({ "rate_limit": {
        "primary_window": window(40.0, 1_767_800_000, 604_800),
        "secondary_window": window(5.0, 1_767_240_000, 18_000),
    }}))
    .unwrap();
    assert_eq!(ids(&swapped.windows), ["session", "weekly"]);
    assert_eq!(swapped.windows[0].used_percent, 5.0);

    let unknown_first = collect_body(json!({ "rate_limit": {
        "primary_window": window(10.0, 1_767_240_000, 3_600),
    }}))
    .unwrap();
    assert_eq!(unknown_first.windows[0].kind, WindowKind::Other);
    assert_eq!(unknown_first.windows[0].id, "window-3600s");
}

#[test]
fn keeps_raw_over_quota_and_fractional_percentages() {
    let snapshot = collect_body(json!({ "rate_limit": {
        "primary_window": window(137.5, 1_767_240_000, 18_000),
        "secondary_window": window(0.0, 0, 604_800),
    }}))
    .unwrap();
    assert_eq!(snapshot.windows[0].used_percent, 137.5);
    assert_eq!(
        snapshot.windows[1].used_percent, 0.0,
        "zero is a reading, not unknown"
    );
    assert_eq!(
        snapshot.windows[1].resets_at, None,
        "non-positive reset is unknown"
    );
}

#[test]
fn malformed_optional_parts_are_reported_without_dropping_siblings() {
    let snapshot = collect_body(json!({
        "rate_limit": {
            "primary_window": window(20.0, 1_767_240_000, 18_000),
            "secondary_window": { "used_percent": "lots" },
        },
        "additional_rate_limits": [
            "garbage",
            { "metered_feature": "codex_mini", "limit_name": "Codex Mini", "rate_limit": {
                "primary_window": window(3.0, 1_767_240_000, 18_000),
                "secondary_window": window(99.0, 1_767_800_000, 604_800),
            }},
            { "limit_name": "Broken", "rate_limit": { "primary_window": { "reset_at": 1 } } },
        ],
        "credits": "n/a",
    }))
    .unwrap();

    assert_eq!(
        ids(&snapshot.windows),
        ["session", "codex-codex-mini-session"]
    );
    assert_eq!(
        snapshot.windows[1].used_percent, 3.0,
        "non-Spark extras use only the primary window"
    );
    assert_eq!(snapshot.windows[1].scope.as_deref(), Some("Codex Mini"));
    assert_eq!(snapshot.credits, None);
    assert_eq!(
        snapshot.issues,
        [
            "rate_limit.secondary_window",
            "additional_rate_limits[0]",
            "additional_rate_limits[2].rate_limit.primary_window",
            "credits",
        ]
    );
}

#[test]
fn response_without_account_windows_is_no_usage_data() {
    let result = collect_body(json!({
        "plan_type": "free",
        "additional_rate_limits": [{ "limit_name": "Spark", "rate_limit": {
            "primary_window": window(1.0, 1_767_240_000, 18_000),
        }}],
    }));
    assert_eq!(result, Err(CollectError::NoUsageData));
    assert_eq!(collect_body(json!([])), Err(CollectError::InvalidResponse));
}

#[test]
fn plan_falls_back_to_id_token_claim() {
    let snapshot = collect_body(json!({ "rate_limit": {
        "primary_window": window(1.0, 1_767_240_000, 18_000),
    }}))
    .unwrap();
    assert_eq!(snapshot.account.plan.as_deref(), Some("plus"));
}

#[test]
fn http_failures_map_to_actionable_errors_without_bodies() {
    let (_dir, source) = source_with(&standard_auth());
    let run = |transport: MockTransport| codex::collect(&source, &transport, now());

    assert_eq!(
        run(MockTransport::status(401, b"{}".to_vec())),
        Err(CollectError::AuthRequired {
            reason: AuthReason::Rejected
        })
    );
    assert_eq!(
        run(MockTransport::new(Ok(Response {
            status: 429,
            body: Vec::new(),
            retry_after: Some(Duration::from_secs(90)),
        }))),
        Err(CollectError::RateLimited {
            retry_after: Some(Duration::from_secs(90))
        })
    );
    let server = run(MockTransport::status(
        503,
        b"secret-ish upstream detail".to_vec(),
    ))
    .unwrap_err();
    assert_eq!(server, CollectError::Server { status: 503 });
    assert!(!server.to_string().contains("upstream detail"));
    assert_eq!(
        run(MockTransport::new(Err(TransportError::Timeout))),
        Err(CollectError::Transport(TransportError::Timeout))
    );
}

#[test]
fn expired_credentials_never_reach_the_network() {
    let (_dir, source) = source_with(&json!({ "tokens": {
        "access_token": jwt(json!({ "exp": common::NOW_SECS + 60 })),
        "account_id": "account-fixture",
    }}));
    let transport = MockTransport::ok(fixture("codex/usage_full.json"));
    assert_eq!(
        codex::collect(&source, &transport, now()),
        Err(CollectError::AuthRequired {
            reason: AuthReason::Expired
        })
    );
    assert_eq!(transport.request_count(), 0);
}

#[test]
fn serialized_snapshot_contains_no_credentials() {
    let (_dir, source) = source_with(&standard_auth());
    let snapshot = codex::collect(
        &source,
        &MockTransport::ok(fixture("codex/usage_full.json")),
        now(),
    )
    .unwrap();
    let text = serde_json::to_string(&snapshot).unwrap();
    assert!(!text.contains(&fresh_access_token()));
    assert!(!text.contains("refresh-must-not-be-read"));
}
