//! One-shot collector: prints a normalized snapshot or error, plus token activity from the
//! provider CLI's local logs, as JSON. Never prints credentials.
//!
//! Usage: `tokenbubble-collect codex|claude`

use std::path::PathBuf;
use std::process::ExitCode;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::json;
use tokenbubble_core::activity::ActivityScanner;
use tokenbubble_core::model::{ProviderId, UsageSnapshot};
use tokenbubble_core::providers::CollectError;
use tokenbubble_core::providers::claude::{self, ClaudeSource};
use tokenbubble_core::providers::codex::{self, CodexSource};
use tokenbubble_core::transport::HttpTransport;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let provider = match args.as_slice() {
        [name] if name == "codex" => ProviderId::Codex,
        [name] if name == "claude" => ProviderId::Claude,
        _ => {
            eprintln!("usage: tokenbubble-collect codex|claude");
            return ExitCode::from(2);
        }
    };

    let now = Timestamp::now();
    let (home, result) = collect(provider, now);
    let activity = home
        .and_then(|home| ActivityScanner::new().scan(provider, &home, now, &TimeZone::system()));
    let (mut output, code) = match result {
        Ok(snapshot) => (json!({ "ok": snapshot }), ExitCode::SUCCESS),
        Err(error) => (
            json!({ "error": { "kind": error.kind(), "message": error.to_string() } }),
            ExitCode::FAILURE,
        ),
    };
    output["activity"] = json!(activity);
    match serde_json::to_string_pretty(&output) {
        Ok(text) => println!("{text}"),
        Err(error) => {
            eprintln!("failed to encode output: {error}");
            return ExitCode::FAILURE;
        }
    }
    code
}

/// The provider's CLI home, when known, and one collection from it.
fn collect(
    provider: ProviderId,
    now: Timestamp,
) -> (Option<PathBuf>, Result<UsageSnapshot, CollectError>) {
    let transport = HttpTransport::default();
    match provider {
        ProviderId::Claude => match ClaudeSource::from_env() {
            Ok(source) => (
                Some(source.claude_home.clone()),
                claude::collect(&source, &transport, now),
            ),
            Err(error) => (None, Err(error)),
        },
        ProviderId::Codex => match CodexSource::from_env() {
            Ok(source) => (
                Some(source.codex_home.clone()),
                codex::collect(&source, &transport, now),
            ),
            Err(error) => (None, Err(error)),
        },
    }
}
