use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use jiff::Timestamp;
use jiff::civil::date;
use jiff::tz::{self, TimeZone};
use serde_json::{Value, json};
use tokenbubble_core::activity::ActivityScanner;
use tokenbubble_core::model::{Activity, NamedTokens, ProviderId, TokenCounts};

const NOW: &str = "2026-01-10T15:00:00Z";

fn now() -> Timestamp {
    NOW.parse().unwrap()
}

fn tokens(input: u64, cache_read: u64, cache_write: u64, output: u64) -> TokenCounts {
    TokenCounts {
        input,
        cache_read,
        cache_write,
        output,
    }
}

fn names(named: &[NamedTokens]) -> Vec<(&str, TokenCounts)> {
    named
        .iter()
        .map(|entry| (entry.name.as_str(), entry.tokens))
        .collect()
}

fn total(activity: &Activity) -> TokenCounts {
    let mut sum = TokenCounts::default();
    for day in &activity.days {
        sum.add(&day.tokens);
    }
    sum
}

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, text).unwrap();
}

fn append(path: &Path, text: &str) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

fn codex_usage(input: u64, cached: u64, cache_write: u64, output: u64) -> Value {
    json!({
        "input_tokens": input,
        "cached_input_tokens": cached,
        "cache_write_input_tokens": cache_write,
        "output_tokens": output,
        "reasoning_output_tokens": 0,
        "total_tokens": input + output,
    })
}

fn codex_token_count(timestamp: &str, total: Value, last: Value) -> Value {
    json!({
        "timestamp": timestamp,
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": { "total_token_usage": total, "last_token_usage": last },
            "rate_limits": null,
        },
    })
}

fn codex_turn(timestamp: &str, model: &str, cwd: &str) -> Value {
    json!({
        "timestamp": timestamp,
        "type": "turn_context",
        "payload": { "model": model, "cwd": cwd, "approval_policy": "never" },
    })
}

fn claude_assistant(timestamp: &str, id: &str, request: &str, model: &str, cwd: &str) -> Value {
    json!({
        "parentUuid": null,
        "isSidechain": false,
        "requestId": request,
        "type": "assistant",
        "timestamp": timestamp,
        "cwd": cwd,
        "message": {
            "model": model,
            "id": id,
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "text", "text": "\"type\":\"assistant\" in content" }],
            "usage": {
                "input_tokens": 2,
                "cache_creation_input_tokens": 300,
                "cache_read_input_tokens": 4000,
                "output_tokens": 50,
            },
        },
    })
}

#[test]
fn missing_log_directory_is_none() {
    let home = tempfile::tempdir().unwrap();
    let mut scanner = ActivityScanner::new();
    for provider in ProviderId::ALL {
        assert_eq!(
            scanner.scan(provider, home.path(), now(), &TimeZone::UTC),
            None
        );
    }
    fs::create_dir(home.path().join("projects")).unwrap();
    let empty = scanner
        .scan(ProviderId::Claude, home.path(), now(), &TimeZone::UTC)
        .unwrap();
    assert_eq!(empty.days.len(), 7);
    assert_eq!(empty.days[6].date, date(2026, 1, 10));
    assert_eq!(empty.days[0].date, date(2026, 1, 4));
    assert!(empty.models.is_empty() && empty.projects.is_empty());
}

#[test]
fn codex_counts_total_deltas_once_across_repeats_and_model_switches() {
    let home = tempfile::tempdir().unwrap();
    let lines = [
        json!({ "timestamp": "2026-01-10T10:00:00Z", "type": "session_meta",
                "payload": { "cwd": "/home/u/alpha", "base_instructions": { "text": "x" } } }),
        codex_turn("2026-01-10T10:00:01Z", "gpt-a", "/home/u/alpha"),
        codex_token_count(
            "2026-01-10T10:00:02Z",
            codex_usage(100, 40, 0, 10),
            codex_usage(100, 40, 0, 10),
        ),
        // Rate-limit-only update: same totals.
        codex_token_count(
            "2026-01-10T10:00:03Z",
            codex_usage(100, 40, 0, 10),
            codex_usage(100, 40, 0, 10),
        ),
        json!({ "timestamp": "2026-01-10T10:00:04Z", "type": "event_msg",
                "payload": { "type": "token_count", "info": null, "rate_limits": null } }),
        codex_turn("2026-01-10T14:30:00Z", "gpt-b", "C:\\work\\beta\\"),
        codex_token_count(
            "2026-01-10T14:30:01Z",
            codex_usage(250, 100, 5, 30),
            codex_usage(150, 60, 5, 20),
        ),
    ];
    let rollout = home
        .path()
        .join("sessions/2026/01/10/rollout-2026-01-10T10-00-00-a.jsonl");
    write_lines(&rollout, &lines);
    // A forked session copies its parent's events verbatim; they are not new use.
    write_lines(
        &home.path().join("archived_sessions/rollout-fork.jsonl"),
        &lines[..3],
    );
    fs::write(rollout.with_extension("txt"), "not a log").unwrap();

    let activity = ActivityScanner::new()
        .scan(ProviderId::Codex, home.path(), now(), &TimeZone::UTC)
        .unwrap();

    let gpt_a = tokens(60, 40, 0, 10);
    let gpt_b = tokens(90, 60, 5, 20);
    assert_eq!(
        names(&activity.models),
        [("gpt-b", gpt_b), ("gpt-a", gpt_a)]
    );
    assert_eq!(
        names(&activity.projects),
        [("beta", gpt_b), ("alpha", gpt_a)]
    );
    assert_eq!(activity.days[6].tokens, tokens(150, 100, 5, 30));
    assert_eq!(total(&activity), activity.days[6].tokens);
    assert_eq!(activity.last_hour, gpt_b);
}

#[test]
fn claude_counts_each_response_once_across_chunks_and_files() {
    let home = tempfile::tempdir().unwrap();
    let projects = home.path().join("projects");
    let first = claude_assistant(
        "2026-01-10T09:00:00Z",
        "msg_1",
        "req_1",
        "claude-opus",
        "C:\\Users\\me\\compi",
    );
    write_lines(
        &projects.join("C--Users-me-compi/s1.jsonl"),
        &[
            json!({ "type": "user", "timestamp": "2026-01-10T08:59:00Z",
                    "message": { "role": "user", "content": "\"usage\": please" } }),
            first.clone(),
            first.clone(),
            claude_assistant(
                "2026-01-10T09:01:00Z",
                "msg_2",
                "req_2",
                "claude-sonnet",
                "/home/me/web",
            ),
        ],
    );
    // A resumed session copies the earlier history into a new file.
    write_lines(
        &projects.join("C--Users-me-compi/s2.jsonl"),
        &[
            first,
            claude_assistant(
                "2026-01-10T09:02:00Z",
                "msg_3",
                "req_3",
                "<synthetic>",
                "/home/me/web",
            ),
        ],
    );
    write_lines(
        &projects.join("C--Users-me-compi/s1/subagents/agent-1.jsonl"),
        &[claude_assistant(
            "2026-01-10T09:03:00Z",
            "msg_4",
            "req_4",
            "claude-opus",
            "C:\\Users\\me\\compi",
        )],
    );
    fs::write(
        projects.join("broken.jsonl"),
        "{\"type\":\"assistant\",\"usage\":\n",
    )
    .unwrap();

    let activity = ActivityScanner::new()
        .scan(ProviderId::Claude, home.path(), now(), &TimeZone::UTC)
        .unwrap();

    let one = tokens(2, 4000, 300, 50);
    let two = tokens(4, 8000, 600, 100);
    assert_eq!(
        names(&activity.models),
        [("claude-opus", two), ("claude-sonnet", one)]
    );
    assert_eq!(names(&activity.projects), [("compi", two), ("web", one)]);
    assert_eq!(total(&activity), tokens(6, 12_000, 900, 150));
    assert_eq!(activity.last_hour, TokenCounts::default());
}

#[test]
fn days_follow_the_local_calendar() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("sessions/2026/01/10/rollout-tz.jsonl");
    let usage = |n| codex_usage(n, 0, 0, 0);
    write_lines(
        &file,
        &[
            codex_turn("2026-01-03T05:00:00Z", "gpt-a", "/w/p"),
            // 2026-01-03 01:00 local: before the seven days.
            codex_token_count("2026-01-03T06:00:00Z", usage(1), usage(1)),
            // 2026-01-09 23:30 local, though already the 10th in UTC.
            codex_token_count("2026-01-10T04:30:00Z", usage(11), usage(10)),
            // 2026-01-10 00:30 local.
            codex_token_count("2026-01-10T05:30:00Z", usage(111), usage(100)),
            // Exactly an hour before the scan: outside the last hour.
            codex_token_count("2026-01-10T14:00:00Z", usage(1111), usage(1000)),
            codex_token_count("2026-01-10T15:00:00Z", usage(11111), usage(10000)),
            // After the scan time.
            codex_token_count("2026-01-10T15:00:01Z", usage(111_111), usage(100_000)),
        ],
    );
    let new_york_winter = TimeZone::fixed(tz::offset(-5));

    let activity = ActivityScanner::new()
        .scan(ProviderId::Codex, home.path(), now(), &new_york_winter)
        .unwrap();

    let dates: Vec<_> = activity.days.iter().map(|day| day.date).collect();
    assert_eq!(dates.first(), Some(&date(2026, 1, 4)));
    assert_eq!(dates.last(), Some(&date(2026, 1, 10)));
    assert_eq!(activity.days[5].tokens.input, 10);
    assert_eq!(activity.days[6].tokens.input, 100 + 1000 + 10000 + 100_000);
    assert!(activity.days[..5].iter().all(|day| day.tokens.input == 0));
    assert_eq!(activity.models[0].tokens.input, 111_110);
    assert_eq!(activity.last_hour.input, 10000);
}

#[test]
fn rescans_count_appended_lines_once() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("sessions/2026/01/10/rollout-grow.jsonl");
    write_lines(
        &file,
        &[
            codex_turn("2026-01-10T14:00:00Z", "gpt-a", "/w/p"),
            codex_token_count(
                "2026-01-10T14:10:00Z",
                codex_usage(100, 0, 0, 1),
                codex_usage(100, 0, 0, 1),
            ),
        ],
    );
    let mut scanner = ActivityScanner::new();
    let scan = |scanner: &mut ActivityScanner| {
        scanner
            .scan(ProviderId::Codex, home.path(), now(), &TimeZone::UTC)
            .unwrap()
    };
    assert_eq!(scan(&mut scanner).last_hour, tokens(100, 0, 0, 1));
    assert_eq!(scan(&mut scanner).last_hour, tokens(100, 0, 0, 1));

    // The writer is midway through the next line: not counted until it is complete.
    let next = codex_token_count(
        "2026-01-10T14:20:00Z",
        codex_usage(300, 50, 0, 3),
        codex_usage(200, 50, 0, 2),
    )
    .to_string();
    let (head, tail) = next.split_at(next.len() / 2);
    append(&file, head);
    assert_eq!(scan(&mut scanner).last_hour, tokens(100, 0, 0, 1));

    append(&file, &format!("{tail}\n"));
    let grown = scan(&mut scanner);
    assert_eq!(grown.last_hour, tokens(250, 50, 0, 3));
    assert_eq!(names(&grown.models), [("gpt-a", tokens(250, 50, 0, 3))]);

    // A replaced, shorter file is parsed again from the start.
    write_lines(
        &file,
        &[codex_token_count(
            "2026-01-10T14:30:00Z",
            codex_usage(7, 0, 0, 0),
            codex_usage(7, 0, 0, 0),
        )],
    );
    let replaced = scan(&mut scanner);
    assert_eq!(names(&replaced.models), [("Unknown", tokens(7, 0, 0, 0))]);
    assert_eq!(names(&replaced.projects), [("Unknown", tokens(7, 0, 0, 0))]);

    fs::remove_file(&file).unwrap();
    assert_eq!(total(&scan(&mut scanner)), TokenCounts::default());
}

#[test]
fn scanner_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<ActivityScanner>();
}
