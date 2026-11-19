//! The rate limits Codex CLI last wrote to its session logs.
//!
//! Every `token_count` event Codex CLI logs carries the account's rate limits as the service
//! last reported them. They stand in for the usage endpoint when the stored sign-in cannot be
//! used. Logs are only read.

use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::Path;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::{Map, Value};

use super::usage::{self, ParsedUsage};
use crate::activity::{CodexLine, Lines, LogFile, codex_log_roots, collect_jsonl};

/// Only the end of a large log is read unless it holds no rate limits.
const TAIL_BYTES: u64 = 16 * 1024 * 1024;
/// `token_count` lines are small; anything longer is some other event.
const MAX_LINE_BYTES: usize = 1024 * 1024;
/// The account-wide limits; model-scoped limits carry other ids.
const ACCOUNT_LIMIT_ID: &str = "codex";

pub(super) struct LoggedLimits {
    /// The log line's own time.
    pub logged_at: Timestamp,
    pub usage: ParsedUsage,
}

#[derive(Deserialize)]
struct TokenCountPayload {
    #[serde(rename = "type")]
    kind: String,
    rate_limits: Option<Map<String, Value>>,
}

/// The newest account-wide rate limits in any Codex session log, searching files from most
/// recently modified. Windows that have reset since they were logged read as reset at `now`.
pub(super) fn latest(codex_home: &Path, now: Timestamp) -> Option<LoggedLimits> {
    let mut files = Vec::new();
    for root in codex_log_roots(codex_home) {
        collect_jsonl(&root, &mut files);
    }
    files.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| b.path.cmp(&a.path))
    });
    files.iter().find_map(|file| latest_in_file(file, now))
}

fn latest_in_file(file: &LogFile, now: Timestamp) -> Option<LoggedLimits> {
    let tail_start = file.len.saturating_sub(TAIL_BYTES);
    latest_from(&file.path, tail_start, now)
        .or_else(|| (tail_start > 0).then(|| latest_from(&file.path, 0, now))?)
}

/// The last usable entry at or after `start`.
fn latest_from(path: &Path, start: u64, now: Timestamp) -> Option<LoggedLimits> {
    let mut handle = File::open(path).ok()?;
    handle.seek(SeekFrom::Start(start)).ok()?;
    let mut lines = Lines::new(BufReader::new(handle), MAX_LINE_BYTES);
    if start > 0 {
        // Starts mid-line.
        lines.next_line()?;
    }
    let mut latest = None;
    while let Some(line) = lines.next_line() {
        if let Some(limits) = parse_line(line, now) {
            latest = Some(limits);
        }
    }
    latest
}

fn parse_line(line: &str, now: Timestamp) -> Option<LoggedLimits> {
    if !line.contains(r#""type":"token_count""#) || !line.contains(r#""rate_limits":{"#) {
        return None;
    }
    let parsed: CodexLine<TokenCountPayload> = serde_json::from_str(line).ok()?;
    if parsed.kind != "event_msg" || parsed.payload.kind != "token_count" {
        return None;
    }
    let rate_limits = parsed.payload.rate_limits?;
    let account_wide = match rate_limits.get("limit_id") {
        None | Some(Value::Null) => true,
        Some(id) => id.as_str() == Some(ACCOUNT_LIMIT_ID),
    };
    if !account_wide {
        return None;
    }
    let logged_at: Timestamp = parsed.timestamp.parse().ok()?;
    Some(LoggedLimits {
        logged_at,
        usage: usage::parse_logged(&rate_limits, logged_at, now)?,
    })
}
