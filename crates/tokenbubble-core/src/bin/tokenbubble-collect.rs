//! One-shot collector: prints a normalized snapshot or error as JSON. Never prints credentials.
//!
//! Usage: `tokenbubble-collect codex`

use std::process::ExitCode;

use jiff::Timestamp;
use serde_json::json;
use tokenbubble_core::providers::codex::{self, CodexSource};
use tokenbubble_core::transport::HttpTransport;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 1 || args[0] != "codex" {
        eprintln!("usage: tokenbubble-collect codex");
        return ExitCode::from(2);
    }

    let result = CodexSource::from_env()
        .and_then(|source| codex::collect(&source, &HttpTransport::default(), Timestamp::now()));
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
