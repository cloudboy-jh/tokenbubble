//! Tolerant decoding of the `wham/usage` response into normalized windows.
//!
//! Adapted from CodexBar `CodexOAuthUsageFetcher.swift`, `CodexAdditionalRateLimitMapper.swift`, and
//! `CodexSpendControlNumber.swift` (MIT; see THIRD_PARTY_NOTICES.md). Unlike CodexBar, window
//! roles come only from their durations, never from their primary/secondary position.

use std::collections::HashSet;

use jiff::Timestamp;
use serde_json::{Map, Value};

use crate::model::{Credits, QuotaWindow, SpendLimit, WindowKind};
use crate::providers::CollectError;

#[derive(Debug)]
pub(super) struct ParsedUsage {
    pub plan: Option<String>,
    pub windows: Vec<QuotaWindow>,
    pub credits: Option<Credits>,
    pub spend_limit: Option<SpendLimit>,
    pub issues: Vec<String>,
}

struct RawWindow {
    used_percent: f64,
    reset_at: i64,
    duration_secs: i64,
}

pub(super) fn parse(body: &[u8]) -> Result<ParsedUsage, CollectError> {
    let root: Map<String, Value> =
        serde_json::from_slice(body).map_err(|_| CollectError::InvalidResponse)?;
    let mut issues = Vec::new();
    let mut ids = HashSet::new();

    let rate_limit = object_field(&root, "rate_limit", "rate_limit", &mut issues);
    let mut account_windows = Vec::new();
    if let Some(rate_limit) = rate_limit {
        for slot in ["primary_window", "secondary_window"] {
            let path = format!("rate_limit.{slot}");
            if let Some(raw) = window_slot(rate_limit, slot, &path, &mut issues) {
                account_windows.push(raw);
            }
        }
    }
    if account_windows.is_empty() {
        return Err(CollectError::NoUsageData);
    }
    // Stable sort keeps response order among windows of the same kind.
    let mut windows: Vec<QuotaWindow> = account_windows
        .into_iter()
        .map(|raw| normalize_window(raw, None, "", &mut ids))
        .collect();
    windows.sort_by_key(|window| window.kind);

    match root.get("additional_rate_limits") {
        None | Some(Value::Null) => {}
        Some(Value::Array(entries)) => {
            for (index, entry) in entries.iter().enumerate() {
                additional_windows(entry, index, &mut windows, &mut ids, &mut issues);
            }
        }
        Some(_) => issues.push("additional_rate_limits".to_owned()),
    }

    let credits = object_field(&root, "credits", "credits", &mut issues).map(|credits| Credits {
        has_credits: credits
            .get("has_credits")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        unlimited: credits
            .get("unlimited")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        balance: credits.get("balance").and_then(lenient_number),
    });

    let plan = root
        .get("plan_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|plan| !plan.is_empty())
        .map(str::to_owned);

    Ok(ParsedUsage {
        plan,
        windows,
        credits,
        spend_limit: resolve_spend_limit(&root, rate_limit),
        issues,
    })
}

fn additional_windows(
    entry: &Value,
    index: usize,
    windows: &mut Vec<QuotaWindow>,
    ids: &mut HashSet<String>,
    issues: &mut Vec<String>,
) {
    let path = format!("additional_rate_limits[{index}]");
    let Some(entry) = entry.as_object() else {
        issues.push(path);
        return;
    };
    let limit_name = trimmed_str(entry.get("limit_name"));
    let metered_feature = trimmed_str(entry.get("metered_feature"));
    let Some(rate_limit) = object_field(entry, "rate_limit", &format!("{path}.rate_limit"), issues)
    else {
        return;
    };
    let primary = window_slot(
        rate_limit,
        "primary_window",
        &format!("{path}.rate_limit.primary_window"),
        issues,
    );
    let secondary = window_slot(
        rate_limit,
        "secondary_window",
        &format!("{path}.rate_limit.secondary_window"),
        issues,
    );

    let is_spark = [limit_name, metered_feature]
        .into_iter()
        .flatten()
        .any(|name| name.to_lowercase().contains("spark"));
    let (id_base, raws) = if is_spark {
        ("codex-spark".to_owned(), [primary, secondary])
    } else {
        // Model-specific limits report utilization in the primary window; the secondary is used
        // only when no primary is present.
        let Some(source) = metered_feature.or(limit_name) else {
            return;
        };
        let slug = slug(source);
        if slug.is_empty() {
            return;
        }
        (format!("codex-{slug}"), [primary.or(secondary), None])
    };
    let scope = limit_name
        .or(metered_feature)
        .unwrap_or("Codex extra limit")
        .to_owned();
    for raw in raws.into_iter().flatten() {
        windows.push(normalize_window(raw, Some(scope.clone()), &id_base, ids));
    }
}

fn normalize_window(
    raw: RawWindow,
    scope: Option<String>,
    id_base: &str,
    ids: &mut HashSet<String>,
) -> QuotaWindow {
    let duration_secs = u64::try_from(raw.duration_secs)
        .ok()
        .filter(|secs| *secs > 0);
    let kind = WindowKind::from_duration(duration_secs);
    let kind_slug = match (kind, duration_secs) {
        (WindowKind::Session, _) => "session".to_owned(),
        (WindowKind::Weekly, _) => "weekly".to_owned(),
        (WindowKind::Other, Some(secs)) => format!("window-{secs}s"),
        (WindowKind::Other, None) => "window".to_owned(),
    };
    let base = if id_base.is_empty() {
        kind_slug
    } else {
        format!("{id_base}-{kind_slug}")
    };
    let mut id = base.clone();
    let mut ordinal = 2;
    while !ids.insert(id.clone()) {
        id = format!("{base}-{ordinal}");
        ordinal += 1;
    }
    QuotaWindow {
        id,
        scope,
        kind,
        duration_secs,
        used_percent: raw.used_percent,
        resets_at: (raw.reset_at > 0)
            .then(|| Timestamp::from_second(raw.reset_at).ok())
            .flatten(),
    }
}

/// `None` when absent or null; records an issue when present but not an object.
fn object_field<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    path: &str,
    issues: &mut Vec<String>,
) -> Option<&'a Map<String, Value>> {
    match map.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::Object(object)) => Some(object),
        Some(_) => {
            issues.push(path.to_owned());
            None
        }
    }
}

/// `None` when absent, null, or malformed; malformed windows are recorded as issues.
fn window_slot(
    map: &Map<String, Value>,
    key: &str,
    path: &str,
    issues: &mut Vec<String>,
) -> Option<RawWindow> {
    let value = match map.get(key) {
        None | Some(Value::Null) => return None,
        Some(value) => value,
    };
    let raw = value.as_object().and_then(|window| {
        Some(RawWindow {
            used_percent: window
                .get("used_percent")?
                .as_f64()
                .filter(|v| v.is_finite())?,
            reset_at: strict_integer(window.get("reset_at")?)?,
            duration_secs: strict_integer(window.get("limit_window_seconds")?)?,
        })
    });
    if raw.is_none() {
        issues.push(path.to_owned());
    }
    raw
}

/// Spend-limit precedence: response root, then `rate_limit`, then `spend_control`.
fn resolve_spend_limit(
    root: &Map<String, Value>,
    rate_limit: Option<&Map<String, Value>>,
) -> Option<SpendLimit> {
    let spend_control = ["spend_control", "spendControl"]
        .into_iter()
        .find_map(|key| root.get(key).and_then(Value::as_object));
    [Some(root), rate_limit, spend_control]
        .into_iter()
        .flatten()
        .find_map(individual_limit)
        .map(|limit| SpendLimit {
            limit: limit.get("limit").and_then(lenient_number),
            used: limit.get("used").and_then(lenient_number),
            remaining_percent: ["remainingPercent", "remaining_percent"]
                .into_iter()
                .find_map(|key| limit.get(key).and_then(lenient_number)),
            resets_at: ["resetsAt", "resets_at", "reset_at"]
                .into_iter()
                .find_map(|key| limit.get(key).and_then(lenient_integer))
                .and_then(|seconds| Timestamp::from_second(seconds).ok()),
        })
        .filter(|limit| {
            limit.limit.is_some()
                || limit.used.is_some()
                || limit.remaining_percent.is_some()
                || limit.resets_at.is_some()
        })
}

fn individual_limit(map: &Map<String, Value>) -> Option<&Map<String, Value>> {
    ["individual_limit", "individualLimit"]
        .into_iter()
        .find_map(|key| map.get(key).and_then(Value::as_object))
}

fn trimmed_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Integer JSON numbers, or floats that are exactly integral.
fn strict_integer(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        let float = value.as_f64()?;
        (float.fract() == 0.0 && float >= i64::MIN as f64 && float < i64::MAX as f64)
            .then_some(float as i64)
    })
}

/// Number, float truncated toward zero, or integer string.
fn lenient_integer(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64().or_else(|| {
            let float = number.as_f64()?.trunc();
            (float >= i64::MIN as f64 && float < i64::MAX as f64).then_some(float as i64)
        }),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// Number or numeric string.
fn lenient_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|value| value.is_finite())
}

fn slug(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            slug.push(ch);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_owned()
}
