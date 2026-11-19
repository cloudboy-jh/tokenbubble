//! Token use read from the provider CLIs' local session logs.
//!
//! Logs are append-only JSONL files the CLIs write while they run. A scan walks the provider's
//! log directory, re-reads only files whose length or modification time changed since the
//! previous scan (resuming after the last complete line when a file only grew), and aggregates
//! the cached entries. Unreadable files and malformed lines are skipped.

use std::borrow::Cow;
use std::collections::hash_map::Entry as MapEntry;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, ToSpan};
use serde::Deserialize;

use crate::model::{Activity, DayActivity, NamedTokens, ProviderId, TokenCounts};

/// Calendar days reported in [`Activity::days`], ending today.
const DAYS: i64 = 7;
/// Files untouched for longer cannot hold entries from the reported days in any time zone.
const MAX_FILE_AGE: SignedDuration = SignedDuration::from_hours(8 * 24);
const LAST_HOUR: SignedDuration = SignedDuration::from_hours(1);
/// Longer lines are skipped whole rather than buffered.
const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;
const READ_BUFFER_BYTES: usize = 256 * 1024;
const UNKNOWN: &str = "Unknown";
/// Claude Code's placeholder for locally generated assistant messages.
const SYNTHETIC_MODEL: &str = "<synthetic>";

/// Incremental scanner over one or more providers' session logs. Keep one instance alive and
/// call [`scan`](Self::scan) periodically; unchanged files are never re-read.
#[derive(Debug, Default)]
pub struct ActivityScanner {
    /// Per provider, every recent log file seen by the last scan, in path order.
    files: HashMap<ProviderId, BTreeMap<PathBuf, CachedFile>>,
}

impl ActivityScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Token activity for the seven local days ending today in `tz`. `home` is the provider's
    /// CLI home (`CODEX_HOME` or Claude's config directory). `None` when the provider's log
    /// directory does not exist.
    pub fn scan(
        &mut self,
        provider: ProviderId,
        home: &Path,
        now: Timestamp,
        tz: &TimeZone,
    ) -> Option<Activity> {
        let roots = log_roots(provider, home);
        if !roots.iter().any(|root| root.is_dir()) {
            self.files.remove(&provider);
            return None;
        }
        let oldest = now
            .checked_sub(MAX_FILE_AGE)
            .map_or(SystemTime::UNIX_EPOCH, SystemTime::from);
        let mut found = Vec::new();
        for root in &roots {
            collect_jsonl(root, &mut found);
        }

        let cache = self.files.entry(provider).or_default();
        let mut previous = std::mem::take(cache);
        for file in found.into_iter().filter(|file| file.modified >= oldest) {
            let cached = previous.remove(&file.path);
            if let Some(cached) = refresh(provider, &file, cached, now) {
                cache.insert(file.path, cached);
            }
        }
        Some(aggregate(cache.values(), now, tz))
    }
}

/// A log file found on disk, with the metadata that decides whether it must be re-read.
#[derive(Debug)]
pub(crate) struct LogFile {
    pub path: PathBuf,
    pub len: u64,
    pub modified: SystemTime,
}

/// Directories holding Codex session logs: active sessions, then archived ones.
pub(crate) fn codex_log_roots(codex_home: &Path) -> [PathBuf; 2] {
    [
        codex_home.join("sessions"),
        codex_home.join("archived_sessions"),
    ]
}

fn log_roots(provider: ProviderId, home: &Path) -> Vec<PathBuf> {
    match provider {
        ProviderId::Codex => codex_log_roots(home).into(),
        ProviderId::Claude => vec![home.join("projects")],
    }
}

/// Appends every `*.jsonl` file below `dir`. Symlinks are not followed, so cycles are impossible.
pub(crate) fn collect_jsonl(dir: &Path, out: &mut Vec<LogFile>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_jsonl(&path, out);
        } else if file_type.is_file()
            && path.extension().is_some_and(|ext| ext == "jsonl")
            && let Ok(metadata) = entry.metadata()
        {
            out.push(LogFile {
                path,
                len: metadata.len(),
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
    }
}

/// Complete lines of a log, without their newline. Lines longer than the limit are skipped
/// whole. Iteration stops before a final line without a newline: its writer may still be
/// appending to it.
pub(crate) struct Lines<R> {
    reader: R,
    buf: Vec<u8>,
    max: usize,
    /// Offset just past the last line returned or skipped, relative to the starting position.
    pub consumed: u64,
}

impl<R: BufRead> Lines<R> {
    pub(crate) fn new(reader: R, max: usize) -> Self {
        Self {
            reader,
            buf: Vec::new(),
            max,
            consumed: 0,
        }
    }

    /// The next complete line as UTF-8; lines that are not UTF-8 come back empty.
    pub(crate) fn next_line(&mut self) -> Option<&str> {
        loop {
            self.buf.clear();
            let limit = self.max as u64 + 1;
            let read = (&mut self.reader)
                .take(limit)
                .read_until(b'\n', &mut self.buf)
                .ok()?;
            if read == 0 {
                return None;
            }
            if self.buf.last() == Some(&b'\n') {
                self.consumed += read as u64;
                let line = &self.buf[..read - 1];
                return Some(std::str::from_utf8(line).unwrap_or(""));
            }
            if (read as u64) < limit {
                return None;
            }
            self.consumed += read as u64 + skip_line(&mut self.reader)?;
        }
    }
}

/// Consumes through the next newline; `None` when the input ends first.
fn skip_line(reader: &mut impl BufRead) -> Option<u64> {
    let mut skipped = 0;
    loop {
        let available = reader.fill_buf().ok()?;
        if available.is_empty() {
            return None;
        }
        match available.iter().position(|&byte| byte == b'\n') {
            Some(index) => {
                reader.consume(index + 1);
                return Some(skipped + index as u64 + 1);
            }
            None => {
                let len = available.len();
                reader.consume(len);
                skipped += len as u64;
            }
        }
    }
}

/// Parsed entries of one file plus what is needed to continue parsing where it stopped.
#[derive(Debug)]
struct CachedFile {
    len: u64,
    modified: SystemTime,
    /// Offset just past the last complete line parsed.
    parsed_to: u64,
    state: ParseState,
    entries: Vec<Entry>,
}

/// Context carried between lines of one file.
#[derive(Debug, Default)]
struct ParseState {
    /// Codex: model of the current turn. Claude: last model seen, reused to share the string.
    model: Option<Arc<str>>,
    /// Codex: project of the current turn. Claude: last project seen, reused likewise.
    project: Option<Arc<str>>,
    /// Codex: the session's cumulative usage at the previous `token_count` event.
    total: Option<RawCodexUsage>,
}

#[derive(Debug)]
struct Entry {
    at: Timestamp,
    model: Arc<str>,
    project: Arc<str>,
    tokens: TokenCounts,
    /// Identity of the logged usage; entries with equal keys are copies of one API response.
    key: Option<u64>,
}

/// Brings a file's cache up to date, or `None` when the file cannot be read.
fn refresh(
    provider: ProviderId,
    file: &LogFile,
    cached: Option<CachedFile>,
    now: Timestamp,
) -> Option<CachedFile> {
    let mut cached = match cached {
        Some(cached) if cached.len == file.len && cached.modified == file.modified => {
            return Some(cached);
        }
        // Logs only grow; a shorter file was replaced and is parsed from the start.
        Some(cached) if file.len >= cached.len => cached,
        _ => CachedFile {
            len: 0,
            modified: SystemTime::UNIX_EPOCH,
            parsed_to: 0,
            state: ParseState::default(),
            entries: Vec::new(),
        },
    };
    let mut handle = File::open(&file.path).ok()?;
    handle.seek(SeekFrom::Start(cached.parsed_to)).ok()?;
    let mut lines = Lines::new(
        BufReader::with_capacity(READ_BUFFER_BYTES, handle),
        MAX_LINE_BYTES,
    );
    while let Some(line) = lines.next_line() {
        match provider {
            ProviderId::Codex => parse_codex_line(line, &mut cached.state, &mut cached.entries),
            ProviderId::Claude => {
                parse_claude_line(line, &mut cached.state, &mut cached.entries);
            }
        }
    }
    cached.parsed_to += lines.consumed;
    cached.len = file.len;
    cached.modified = file.modified;
    if let Ok(oldest) = now.checked_sub(MAX_FILE_AGE) {
        cached.entries.retain(|entry| entry.at >= oldest);
    }
    Some(cached)
}

/// A Codex rollout line: `{"timestamp":…,"type":…,"payload":{…}}`.
#[derive(Deserialize)]
pub(crate) struct CodexLine<'a, P> {
    #[serde(borrow)]
    pub timestamp: Cow<'a, str>,
    #[serde(rename = "type", borrow)]
    pub kind: Cow<'a, str>,
    pub payload: P,
}

#[derive(Deserialize)]
struct TokenCountPayload<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    info: Option<TokenCountInfo>,
}

#[derive(Deserialize)]
struct TokenCountInfo {
    total_token_usage: Option<RawCodexUsage>,
    last_token_usage: Option<RawCodexUsage>,
}

/// Codex token usage. `input_tokens` includes `cached_input_tokens`; `output_tokens` includes
/// reasoning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize)]
struct RawCodexUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    cached_input_tokens: Option<u64>,
    #[serde(default)]
    cache_write_input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

impl RawCodexUsage {
    fn fields(self) -> [u64; 4] {
        [
            self.input_tokens,
            self.cached_input_tokens,
            self.cache_write_input_tokens,
            self.output_tokens,
        ]
        .map(|value| value.unwrap_or(0))
    }

    /// Field-wise difference; `None` when any counter went backwards.
    fn since(self, earlier: Self) -> Option<[u64; 4]> {
        let (now, then) = (self.fields(), earlier.fields());
        let mut delta = [0; 4];
        for index in 0..4 {
            delta[index] = now[index].checked_sub(then[index])?;
        }
        Some(delta)
    }
}

#[derive(Deserialize)]
struct TurnContextPayload<'a> {
    #[serde(default, borrow)]
    model: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    cwd: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct SessionMetaPayload<'a> {
    #[serde(default, borrow)]
    cwd: Option<Cow<'a, str>>,
}

fn parse_codex_line(line: &str, state: &mut ParseState, entries: &mut Vec<Entry>) {
    if line.contains(r#""type":"token_count""#) {
        let Ok(parsed) = serde_json::from_str::<CodexLine<TokenCountPayload>>(line) else {
            return;
        };
        if parsed.kind != "event_msg" || parsed.payload.kind != "token_count" {
            return;
        }
        let Some(info) = parsed.payload.info else {
            return;
        };
        let Ok(at) = parsed.timestamp.parse::<Timestamp>() else {
            return;
        };
        let (usage, identity) = match (info.total_token_usage, info.last_token_usage) {
            (Some(total), last) => {
                let delta = match state.total {
                    Some(previous) => total.since(previous),
                    None => Some(total.fields()),
                };
                state.total = Some(total);
                // A counter that went backwards means the session's totals restarted.
                let usage = delta.unwrap_or_else(|| last.unwrap_or_default().fields());
                (usage, total)
            }
            (None, Some(last)) => (last.fields(), last),
            (None, None) => return,
        };
        if usage == [0; 4] {
            return;
        }
        let [input, cached, cache_write, output] = usage;
        // Forked sessions copy their parent's events verbatim, timestamps included.
        let mut hasher = DefaultHasher::new();
        (parsed.timestamp.as_ref(), identity).hash(&mut hasher);
        entries.push(Entry {
            at,
            model: state.model.clone().unwrap_or_else(|| UNKNOWN.into()),
            project: state.project.clone().unwrap_or_else(|| UNKNOWN.into()),
            tokens: TokenCounts {
                input: input.saturating_sub(cached),
                cache_read: cached,
                cache_write,
                output,
            },
            key: Some(hasher.finish()),
        });
    } else if line.contains(r#""type":"turn_context""#) {
        let Ok(parsed) = serde_json::from_str::<CodexLine<TurnContextPayload>>(line) else {
            return;
        };
        if parsed.kind != "turn_context" {
            return;
        }
        if let Some(model) = parsed
            .payload
            .model
            .filter(|model| !model.trim().is_empty())
        {
            intern(&mut state.model, model.trim());
        }
        if let Some(cwd) = parsed.payload.cwd {
            intern(&mut state.project, project_name(&cwd));
        }
    } else if line.contains(r#""type":"session_meta""#) {
        let Ok(parsed) = serde_json::from_str::<CodexLine<SessionMetaPayload>>(line) else {
            return;
        };
        if parsed.kind == "session_meta"
            && let Some(cwd) = parsed.payload.cwd
        {
            intern(&mut state.project, project_name(&cwd));
        }
    }
}

#[derive(Deserialize)]
struct ClaudeLine<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    timestamp: Cow<'a, str>,
    #[serde(default, borrow)]
    cwd: Option<Cow<'a, str>>,
    #[serde(rename = "requestId", default, borrow)]
    request_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    message: ClaudeMessage<'a>,
}

#[derive(Deserialize)]
struct ClaudeMessage<'a> {
    #[serde(default, borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    model: Option<Cow<'a, str>>,
    usage: ClaudeUsage,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

fn parse_claude_line(line: &str, state: &mut ParseState, entries: &mut Vec<Entry>) {
    if !line.contains(r#""type":"assistant""#) || !line.contains(r#""usage":"#) {
        return;
    }
    let Ok(parsed) = serde_json::from_str::<ClaudeLine>(line) else {
        return;
    };
    if parsed.kind != "assistant" {
        return;
    }
    let model = parsed
        .message
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(UNKNOWN);
    if model == SYNTHETIC_MODEL {
        return;
    }
    let Ok(at) = parsed.timestamp.parse::<Timestamp>() else {
        return;
    };
    let usage = parsed.message.usage;
    let tokens = TokenCounts {
        input: usage.input_tokens.unwrap_or(0),
        cache_read: usage.cache_read_input_tokens.unwrap_or(0),
        cache_write: usage.cache_creation_input_tokens.unwrap_or(0),
        output: usage.output_tokens.unwrap_or(0),
    };
    // Streaming chunks and resumed sessions repeat a response; its ids identify it.
    let key = match (&parsed.message.id, &parsed.request_id) {
        (None, None) => None,
        (id, request) => {
            let mut hasher = DefaultHasher::new();
            (id.as_deref(), request.as_deref()).hash(&mut hasher);
            Some(hasher.finish())
        }
    };
    let project = parsed.cwd.as_deref().map_or(UNKNOWN, project_name);
    entries.push(Entry {
        at,
        model: intern(&mut state.model, model),
        project: intern(&mut state.project, project),
        tokens,
        key,
    });
}

/// Returns `slot` when it already holds `value`, otherwise stores and returns a new string.
fn intern(slot: &mut Option<Arc<str>>, value: &str) -> Arc<str> {
    match slot {
        Some(current) if **current == *value => current.clone(),
        _ => slot.insert(value.into()).clone(),
    }
}

/// Last component of a POSIX or Windows path; `Unknown` when there is none.
fn project_name(cwd: &str) -> &str {
    cwd.trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(UNKNOWN)
}

fn aggregate<'a>(
    files: impl Iterator<Item = &'a CachedFile>,
    now: Timestamp,
    tz: &TimeZone,
) -> Activity {
    let today = tz.to_datetime(now).date();
    let mut days: Vec<DayActivity> = (0..DAYS)
        .rev()
        .map(|back| DayActivity {
            date: today.saturating_sub(back.days()),
            tokens: TokenCounts::default(),
        })
        .collect();
    let first_day = days[0].date;
    let hour_ago = now.checked_sub(LAST_HOUR).unwrap_or(Timestamp::MIN);

    let mut counted: Vec<&Entry> = Vec::new();
    let mut by_key: HashMap<u64, usize> = HashMap::new();
    for entry in files.flat_map(|file| &file.entries) {
        let Some(key) = entry.key else {
            counted.push(entry);
            continue;
        };
        match by_key.entry(key) {
            MapEntry::Vacant(slot) => {
                slot.insert(counted.len());
                counted.push(entry);
            }
            // Copies normally agree; if one was cut short, keep the most complete.
            MapEntry::Occupied(slot) => {
                let kept = &mut counted[*slot.get()];
                if entry.tokens.billable() > kept.tokens.billable() {
                    *kept = entry;
                }
            }
        }
    }

    let mut models: HashMap<Arc<str>, TokenCounts> = HashMap::new();
    let mut projects: HashMap<Arc<str>, TokenCounts> = HashMap::new();
    let mut last_hour = TokenCounts::default();
    for entry in counted {
        if entry.at > hour_ago && entry.at <= now {
            last_hour.add(&entry.tokens);
        }
        let date: Date = tz.to_datetime(entry.at).date();
        if date < first_day || date > today {
            continue;
        }
        if let Some(day) = days.iter_mut().find(|day| day.date == date) {
            day.tokens.add(&entry.tokens);
        }
        models
            .entry(entry.model.clone())
            .or_default()
            .add(&entry.tokens);
        projects
            .entry(entry.project.clone())
            .or_default()
            .add(&entry.tokens);
    }

    Activity {
        days,
        models: ranked(models),
        projects: ranked(projects),
        last_hour,
    }
}

/// Largest billable first; ties by name so the order is stable between scans.
fn ranked(totals: HashMap<Arc<str>, TokenCounts>) -> Vec<NamedTokens> {
    let mut named: Vec<NamedTokens> = totals
        .into_iter()
        .map(|(name, tokens)| NamedTokens {
            name: name.to_string(),
            tokens,
        })
        .collect();
    named.sort_by(|a, b| {
        b.tokens
            .billable()
            .cmp(&a.tokens.billable())
            .then_with(|| a.name.cmp(&b.name))
    });
    named
}
