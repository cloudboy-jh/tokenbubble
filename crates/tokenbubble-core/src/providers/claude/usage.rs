//! Tolerant Claude OAuth usage mapping, adapted from CodexBar's ClaudeOAuthUsageFetcher,
//! ClaudeUsageFetcher, and ClaudeScopedWeeklyLimitMapper (MIT; see THIRD_PARTY_NOTICES.md).

use std::collections::HashSet;

use jiff::Timestamp;
use serde_json::{Map, Value};

use crate::model::{QuotaWindow, SpendLimit, WindowKind};
use crate::providers::CollectError;

pub(super) struct ParsedUsage {
    pub windows: Vec<QuotaWindow>,
    pub spend_limit: Option<SpendLimit>,
    pub issues: Vec<String>,
}

pub(super) fn parse(bytes: &[u8]) -> Result<ParsedUsage, CollectError> {
    let root: Map<String, Value> =
        serde_json::from_slice(bytes).map_err(|_| CollectError::InvalidResponse)?;
    let mut windows = Vec::new();
    let mut issues = Vec::new();
    for (key, scope, duration) in [
        ("five_hour", None, WindowKind::SESSION_SECS),
        ("seven_day", None, WindowKind::WEEKLY_SECS),
        (
            "seven_day_oauth_apps",
            Some("OAuth apps"),
            WindowKind::WEEKLY_SECS,
        ),
        ("seven_day_opus", Some("Opus"), WindowKind::WEEKLY_SECS),
        ("seven_day_sonnet", Some("Sonnet"), WindowKind::WEEKLY_SECS),
    ] {
        if let Some(value) = root.get(key).filter(|value| !value.is_null())
            && let Some(window) = window(value, key, scope, duration, &mut issues)
        {
            windows.push(window);
        }
    }
    // Claude has shipped several aliases for this one feature. Keep the first healthy
    // alias rather than duplicating the quota or letting a malformed alias hide it.
    for key in [
        "seven_day_routines",
        "seven_day_claude_routines",
        "claude_routines",
        "routines",
        "routine",
        "seven_day_cowork",
        "cowork",
    ] {
        if let Some(value) = root.get(key).filter(|value| !value.is_null())
            && let Some(mut window) = window(
                value,
                key,
                Some("Routines"),
                WindowKind::WEEKLY_SECS,
                &mut issues,
            )
        {
            window.id = "claude-routines".to_owned();
            windows.push(window);
            break;
        }
    }
    scoped_windows(root.get("limits"), &mut windows, &mut issues);
    let spend_limit = extra_usage(root.get("extra_usage"), &mut issues);
    if windows.is_empty() && spend_limit.is_none() {
        return Err(CollectError::NoUsageData);
    }
    Ok(ParsedUsage {
        windows,
        spend_limit,
        issues,
    })
}

fn window(
    value: &Value,
    key: &str,
    scope: Option<&str>,
    duration: u64,
    issues: &mut Vec<String>,
) -> Option<QuotaWindow> {
    let Some(object) = value.as_object() else {
        issues.push(key.to_owned());
        return None;
    };
    let Some(used_percent) = object.get("utilization").and_then(number) else {
        issues.push(format!("{key}.utilization"));
        return None;
    };
    Some(QuotaWindow {
        id: key.to_owned(),
        scope: scope.map(str::to_owned),
        kind: WindowKind::from_duration(Some(duration)),
        duration_secs: Some(duration),
        used_percent,
        resets_at: reset(object.get("resets_at"), &format!("{key}.resets_at"), issues),
    })
}

fn reset(value: Option<&Value>, path: &str, issues: &mut Vec<String>) -> Option<Timestamp> {
    match value {
        None | Some(Value::Null) => None,
        Some(value) => {
            let timestamp = value
                .as_str()
                .and_then(|text| text.parse::<Timestamp>().ok());
            if timestamp.is_none() {
                issues.push(path.to_owned());
            }
            timestamp
        }
    }
}

fn scoped_windows(value: Option<&Value>, windows: &mut Vec<QuotaWindow>, issues: &mut Vec<String>) {
    let entries = match value {
        None | Some(Value::Null) => return,
        Some(Value::Array(entries)) => entries,
        Some(_) => {
            issues.push("limits".to_owned());
            return;
        }
    };
    let mut ids = HashSet::new();
    for (index, entry) in entries.iter().enumerate() {
        let path = format!("limits[{index}]");
        let Some(entry) = entry.as_object() else {
            issues.push(path);
            continue;
        };
        if entry.get("group").and_then(Value::as_str) != Some("weekly")
            || entry.get("kind").and_then(Value::as_str) != Some("weekly_scoped")
        {
            continue;
        }
        let model = entry.get("scope").and_then(|scope| scope.get("model"));
        let name = model.and_then(|model| text(model.get("display_name")));
        let percent = entry.get("percent").and_then(number);
        let (Some(name), Some(used_percent)) = (name, percent) else {
            issues.push(path);
            continue;
        };
        let model_id = model.and_then(|model| text(model.get("id")));
        let name_slug = slug(name);
        let identity = slug(model_id.unwrap_or(name));
        if name_slug == "all-models"
            || identity == "all-models"
            || identity.ends_with("-all-models")
        {
            continue;
        }
        if identity.is_empty() {
            issues.push(path);
            continue;
        }
        let id = format!("claude-weekly-scoped-{identity}");
        if !ids.insert(id.clone()) {
            continue;
        }
        // is_active=false is not a filter: observed enforceable limits report false.
        windows.push(QuotaWindow {
            id,
            scope: Some(name.to_owned()),
            kind: WindowKind::Weekly,
            duration_secs: Some(WindowKind::WEEKLY_SECS),
            used_percent,
            resets_at: reset(entry.get("resets_at"), &format!("{path}.resets_at"), issues),
        });
    }
}

fn extra_usage(value: Option<&Value>, issues: &mut Vec<String>) -> Option<SpendLimit> {
    let object = match value {
        None | Some(Value::Null) => return None,
        Some(Value::Object(object)) => object,
        Some(_) => {
            issues.push("extra_usage".to_owned());
            return None;
        }
    };
    match object.get("is_enabled").and_then(Value::as_bool) {
        Some(false) => return None,
        Some(true) => {}
        None => {
            issues.push("extra_usage.is_enabled".to_owned());
            return None;
        }
    }
    let mut numeric = |key: &str| {
        let value = object.get(key);
        let parsed = value.and_then(number);
        if value.is_some_and(|value| !value.is_null()) && parsed.is_none() {
            issues.push(format!("extra_usage.{key}"));
        }
        parsed
    };
    // Primary reference explicitly documents cents for all OAuth accounts, including
    // Enterprise spend-only accounts. Spend remains separate from subscription windows.
    let limit = numeric("monthly_limit").map(|cents| cents / 100.0);
    let used = numeric("used_credits").map(|cents| cents / 100.0);
    let utilization = numeric("utilization");
    let currency = match object.get("currency") {
        None | Some(Value::Null) => Some("USD".to_owned()),
        Some(Value::String(code)) if code.trim().is_empty() => Some("USD".to_owned()),
        Some(Value::String(code)) => Some(code.trim().to_owned()),
        Some(_) => {
            issues.push("extra_usage.currency".to_owned());
            None
        }
    };
    if limit.is_none() && used.is_none() && utilization.is_none() {
        return None;
    }
    Some(SpendLimit {
        limit,
        used,
        currency,
        remaining_percent: utilization.map(|percent| 100.0 - percent),
        resets_at: None,
    })
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

fn text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn slug(value: &str) -> String {
    let mut result = String::new();
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            result.push(character);
        } else if !result.ends_with('-') {
            result.push('-');
        }
    }
    result.trim_matches('-').to_owned()
}
