#![allow(dead_code)]

use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jiff::Timestamp;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokenbubble_core::transport::{Request, Response, Transport, TransportError};

/// Fixed clock for deterministic freshness checks: 2026-01-01T00:00:00Z.
pub const NOW_SECS: i64 = 1_767_225_600;

pub fn now() -> Timestamp {
    Timestamp::from_second(NOW_SECS).unwrap()
}

/// Unsigned synthetic JWT with the given raw JSON payload text.
pub fn jwt_raw(payload: &str) -> String {
    format!(
        "{}.{}.sig",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
        URL_SAFE_NO_PAD.encode(payload.as_bytes())
    )
}

pub fn jwt(payload: Value) -> String {
    jwt_raw(&payload.to_string())
}

/// An access token that stays fresh relative to [`now`].
pub fn fresh_access_token() -> String {
    jwt(json!({ "exp": NOW_SECS + 3600 }))
}

pub fn write_auth(codex_home: &Path, auth: &Value) {
    std::fs::write(codex_home.join("auth.json"), auth.to_string()).unwrap();
}

pub fn standard_auth() -> Value {
    json!({
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": jwt(json!({
                "email": "user@example.com",
                "https://api.openai.com/auth": { "chatgpt_plan_type": "plus" }
            })),
            "access_token": fresh_access_token(),
            "refresh_token": "refresh-must-not-be-read",
            "account_id": "account-fixture"
        },
        "last_refresh": "2025-12-31T12:00:00Z"
    })
}

pub fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(path).unwrap()
}

pub struct RecordedRequest {
    pub url: String,
    pub bearer: Option<String>,
    pub headers: Vec<(String, String)>,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Returns a canned response and records every request it receives.
pub struct MockTransport {
    response: Result<Response, TransportError>,
    pub requests: Mutex<Vec<RecordedRequest>>,
}

impl MockTransport {
    pub fn new(response: Result<Response, TransportError>) -> Self {
        Self {
            response,
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn ok(body: Vec<u8>) -> Self {
        Self::status(200, body)
    }

    pub fn status(status: u16, body: Vec<u8>) -> Self {
        Self::new(Ok(Response {
            status,
            body,
            retry_after: None,
        }))
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().len()
    }
}

impl Transport for MockTransport {
    fn get(&self, request: &Request) -> Result<Response, TransportError> {
        self.requests.lock().push(RecordedRequest {
            url: request.url.clone(),
            bearer: request.bearer.as_ref().map(|s| s.expose().to_owned()),
            headers: request
                .headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        });
        self.response.clone()
    }
}
