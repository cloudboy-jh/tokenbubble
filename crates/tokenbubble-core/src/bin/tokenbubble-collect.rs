//! One-shot collector: prints a normalized snapshot or error as JSON. Never prints credentials.
//!
//! Usage: `tokenbubble-collect codex|claude`

use std::process::ExitCode;

use jiff::Timestamp;
use serde_json::json;
use tokenbubble_core::providers::claude::{self, ClaudeSource};
use tokenbubble_core::providers::codex::{self, CodexSource};
use tokenbubble_core::transport::HttpTransport;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 1 || !matches!(args[0].as_str(), "codex" | "claude") {
        eprintln!("usage: tokenbubble-collect codex|claude");
        return ExitCode::from(2);
    }

    let transport = HttpTransport::default();
    let now = Timestamp::now();
    let result = match args[0].as_str() {
        "claude" => {
            ClaudeSource::from_env().and_then(|source| claude::collect(&source, &transport, now))
        }
        _ => CodexSource::from_env().and_then(|source| codex::collect(&source, &transport, now)),
    };
    let (output, code) = match result {
        Ok(snapshot) => (json!({ "ok": snapshot }), ExitCode::SUCCESS),
        Err(error) => (
            json!({ "error": { "kind": error.kind(), "message": error.to_string() } }),
            ExitCode::FAILURE,
        ),
    };
    match serde_json::to_string_pretty(&output) {
        Ok(text) => println!("{text}"),
        Err(error) => {
            eprintln!("failed to encode output: {error}");
            return ExitCode::FAILURE;
        }
    }
    code
}
