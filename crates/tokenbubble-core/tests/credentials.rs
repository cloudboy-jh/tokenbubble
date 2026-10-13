mod common;

use common::{NOW_SECS, fresh_access_token, jwt, jwt_raw, now, standard_auth, write_auth};
use serde_json::json;
use tokenbubble_core::providers::codex::{CodexCredentials, load_credentials};
use tokenbubble_core::providers::{AuthReason, CollectError};

fn parse(auth: serde_json::Value) -> Result<CodexCredentials, CollectError> {
    CodexCredentials::parse(auth.to_string().as_bytes())
}

fn expired() -> Result<(), CollectError> {
    Err(CollectError::AuthRequired {
        reason: AuthReason::Expired,
    })
}

#[test]
fn reads_identity_without_exposing_token_in_debug() {
    let credentials = parse(standard_auth()).unwrap();

    assert_eq!(credentials.account_id.as_deref(), Some("account-fixture"));
    assert_eq!(credentials.email.as_deref(), Some("user@example.com"));
    assert_eq!(credentials.plan_claim.as_deref(), Some("plus"));
    assert_eq!(credentials.access_token.expose(), fresh_access_token());
    let debug = format!("{credentials:?}");
    assert!(!debug.contains(&fresh_access_token()), "{debug}");
    assert!(!debug.contains("refresh-must-not-be-read"), "{debug}");
}

#[test]
fn account_id_falls_back_to_id_token_then_access_token_claims() {
    let from_id_token = parse(json!({ "tokens": {
        "access_token": fresh_access_token(),
        "account_id": "  ",
        "id_token": jwt(json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct-id" } })),
    }}))
    .unwrap();
    assert_eq!(from_id_token.account_id.as_deref(), Some("acct-id"));

    let from_access_token = parse(json!({ "tokens": {
        "accessToken": jwt(json!({ "exp": NOW_SECS + 3600, "organizations": [{ "id": "" }, { "id": "org-2" }] })),
    }}))
    .unwrap();
    assert_eq!(from_access_token.account_id.as_deref(), Some("org-2"));
}

#[test]
fn token_inside_expiry_margin_requires_owner_refresh() {
    let auth = |exp: i64| json!({ "tokens": { "access_token": jwt(json!({ "exp": exp })) } });

    assert_eq!(
        parse(auth(NOW_SECS + 301)).unwrap().ensure_fresh(now()),
        Ok(())
    );
    assert_eq!(
        parse(auth(NOW_SECS + 300)).unwrap().ensure_fresh(now()),
        expired()
    );
    assert_eq!(
        parse(auth(NOW_SECS - 1)).unwrap().ensure_fresh(now()),
        expired()
    );
}

#[test]
fn non_integer_or_duplicate_exp_falls_back_to_refresh_age() {
    let far_future = NOW_SECS + 86_400;
    for payload in [
        format!(r#"{{"exp":{far_future}.0}}"#),
        format!(r#"{{"exp":"{far_future}"}}"#),
        format!(r#"{{"exp":{far_future},"exp":{far_future}}}"#),
    ] {
        let token = jwt_raw(&payload);
        let stale_refresh =
            json!({ "tokens": { "access_token": token }, "last_refresh": "2025-12-20T00:00:00Z" });
        let credentials = parse(stale_refresh).unwrap();
        assert_eq!(credentials.expires_at, None, "{payload}");
        assert_eq!(credentials.ensure_fresh(now()), expired(), "{payload}");

        let recent_refresh =
            json!({ "tokens": { "access_token": token }, "last_refresh": "2025-12-30T00:00:00Z" });
        assert_eq!(
            parse(recent_refresh).unwrap().ensure_fresh(now()),
            Ok(()),
            "{payload}"
        );
    }
}

#[test]
fn opaque_token_without_refresh_time_requires_refresh() {
    let credentials = parse(json!({ "tokens": { "access_token": "opaque" } })).unwrap();
    assert_eq!(credentials.ensure_fresh(now()), expired());
}

#[test]
fn api_key_sign_in_is_unsupported_but_tokens_take_precedence() {
    let api_key_only = parse(json!({ "OPENAI_API_KEY": "sk-fixture", "tokens": null }));
    assert!(matches!(
        api_key_only,
        Err(CollectError::Unsupported { .. })
    ));

    let both =
        parse(json!({ "OPENAI_API_KEY": "sk-fixture", "tokens": { "access_token": "tok" } }));
    assert_eq!(both.unwrap().access_token.expose(), "tok");
}

#[test]
fn missing_and_malformed_files_are_distinct() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        load_credentials(dir.path()),
        Err(CollectError::CredentialsMissing { path }) if path == dir.path().join("auth.json")
    ));

    std::fs::write(dir.path().join("auth.json"), "{not json").unwrap();
    assert!(matches!(
        load_credentials(dir.path()),
        Err(CollectError::CredentialsMalformed { .. })
    ));

    write_auth(dir.path(), &json!({ "tokens": { "refresh_token": "r" } }));
    assert!(matches!(
        load_credentials(dir.path()),
        Err(CollectError::CredentialsMalformed { .. })
    ));

    write_auth(dir.path(), &standard_auth());
    assert!(load_credentials(dir.path()).is_ok());
}
