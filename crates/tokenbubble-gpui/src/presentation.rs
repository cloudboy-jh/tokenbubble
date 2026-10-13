//! Renderer-independent panel state and the text it displays.

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use tokenbubble_core::model::{
    Credits, ProviderId, QuotaWindow, SpendLimit, UsageSnapshot, WindowKind,
};
use tokenbubble_core::providers::{AuthReason, CollectError};

/// What the panel knows: the last good reading and the outcome of the latest attempt.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelState {
    pub snapshot: Option<UsageSnapshot>,
    /// Set when the most recent collection failed.
    pub error: Option<CollectError>,
    pub refreshing: bool,
}

impl PanelState {
    pub fn begin_refresh(&mut self) {
        self.refreshing = true;
    }

    /// Record a collection outcome. Transient failures keep the previous reading (shown as
    /// stale); sign-in and configuration failures drop it, because it may no longer
    /// describe the account the credentials now belong to.
    pub fn apply(&mut self, result: Result<UsageSnapshot, CollectError>) {
        self.refreshing = false;
        match result {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.error = None;
            }
            Err(error) => {
                if !is_transient(&error) {
                    self.snapshot = None;
                }
                self.error = Some(error);
            }
        }
    }
}

fn is_transient(error: &CollectError) -> bool {
    matches!(
        error,
        CollectError::RateLimited { .. }
            | CollectError::Server { .. }
            | CollectError::Transport(_)
            | CollectError::InvalidResponse
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Muted,
    Warning,
    Critical,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PanelView {
    pub title: &'static str,
    pub plan: Option<String>,
    pub account: Option<String>,
    pub body: Body,
    pub money: Vec<MoneyRow>,
    pub status: Option<(String, Tone)>,
    pub refreshing: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    Loading,
    Message(String),
    Bars(Vec<BarView>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarView {
    pub id: String,
    pub label: String,
    pub value: String,
    /// Visual fill in `0.0..=1.0`; the raw percentage stays in `value`.
    pub fill: f32,
    pub tone: Tone,
    pub reset: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoneyRow {
    pub label: &'static str,
    pub value: String,
}

const WARNING_PERCENT: f64 = 75.0;

pub fn present(state: &PanelState, now: Timestamp, tz: &TimeZone) -> PanelView {
    let provider = state
        .snapshot
        .as_ref()
        .map_or(ProviderId::Codex, |snapshot| snapshot.provider);
    let mut view = PanelView {
        title: provider_name(provider),
        plan: None,
        account: None,
        body: Body::Loading,
        money: Vec::new(),
        status: None,
        refreshing: state.refreshing,
    };

    let Some(snapshot) = &state.snapshot else {
        match &state.error {
            Some(error) => {
                view.body = Body::Message(error_message(provider, error));
                if state.refreshing {
                    view.status = Some(("Refreshing…".to_owned(), Tone::Muted));
                }
            }
            None => view.body = Body::Loading,
        }
        return view;
    };

    view.plan = snapshot.account.plan.as_deref().map(plan_name);
    view.account = snapshot.account.email.clone();
    view.body = Body::Bars(
        snapshot
            .windows
            .iter()
            .map(|window| bar(window, now, tz))
            .collect(),
    );
    view.money = money_rows(snapshot.credits.as_ref(), snapshot.spend_limit.as_ref());
    let age = ago(now.duration_since(snapshot.measured_at));
    view.status = Some(if state.refreshing {
        ("Refreshing…".to_owned(), Tone::Muted)
    } else if let Some(error) = &state.error {
        (
            format!(
                "Couldn't refresh ({}) · showing data from {age}",
                short_error(error)
            ),
            Tone::Warning,
        )
    } else {
        (format!("Updated {age}"), Tone::Muted)
    });
    view
}

fn provider_name(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Codex => "Codex",
    }
}

/// `pro` → `Pro`, `free_workspace` → `Free workspace`.
pub fn plan_name(raw: &str) -> String {
    let spaced = raw.trim().replace(['_', '-'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn bar(window: &QuotaWindow, now: Timestamp, tz: &TimeZone) -> BarView {
    let kind = match (window.kind, window.duration_secs) {
        (WindowKind::Session, _) => "Session".to_owned(),
        (WindowKind::Weekly, _) => "Weekly".to_owned(),
        (WindowKind::Other, Some(secs)) => format!("{} window", duration_label(secs)),
        (WindowKind::Other, None) => "Usage window".to_owned(),
    };
    let label = match &window.scope {
        Some(scope) => format!("{scope} · {kind}"),
        None => kind,
    };
    let used = window.used_percent;
    let tone = if used >= 100.0 {
        Tone::Critical
    } else if used >= WARNING_PERCENT {
        Tone::Warning
    } else {
        Tone::Normal
    };
    let value = if used > 100.0 {
        format!("{} used · over limit", percent(used))
    } else {
        format!("{} used", percent(used))
    };
    BarView {
        id: window.id.clone(),
        label,
        value,
        fill: (used / 100.0).clamp(0.0, 1.0) as f32,
        tone,
        reset: window.resets_at.map(|at| reset_text(at, now, tz)),
    }
}

fn percent(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}%")
    } else {
        format!("{value:.1}%")
    }
}

fn duration_label(secs: u64) -> String {
    match secs {
        s if s % 86_400 == 0 => format!("{}-day", s / 86_400),
        s if s % 3_600 == 0 => format!("{}-hour", s / 3_600),
        s => format!("{}-minute", s.div_ceil(60)),
    }
}

/// `Resets in 5d 15h · Sat 12:44 PM`, in the viewer's time zone.
pub fn reset_text(at: Timestamp, now: Timestamp, tz: &TimeZone) -> String {
    let remaining = at.duration_since(now);
    if remaining <= SignedDuration::ZERO {
        return "Reset due".to_owned();
    }
    let local = at.to_zoned(tz.clone());
    let today = now.to_zoned(tz.clone()).date();
    let clock = local.strftime("%-I:%M %p").to_string();
    let when = if local.date() == today {
        format!("today {clock}")
    } else if Some(local.date()) == today.tomorrow().ok() {
        format!("tomorrow {clock}")
    } else {
        format!("{} {clock}", local.strftime("%a %b %-d"))
    };
    format!("Resets in {} · {when}", span(remaining))
}

fn span(duration: SignedDuration) -> String {
    let minutes = duration.as_secs().div_euclid(60).max(1);
    let (days, hours, mins) = (minutes / 1_440, minutes / 60 % 24, minutes % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

fn ago(elapsed: SignedDuration) -> String {
    let secs = elapsed.as_secs().max(0);
    match secs {
        0..60 => "just now".to_owned(),
        60..3_600 => format!("{}m ago", secs / 60),
        3_600..86_400 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

fn money_rows(credits: Option<&Credits>, spend: Option<&SpendLimit>) -> Vec<MoneyRow> {
    let mut rows = Vec::new();
    if let Some(credits) = credits {
        let value = if credits.unlimited {
            Some("Unlimited".to_owned())
        } else {
            credits.balance.map(amount)
        };
        if let Some(value) = value {
            rows.push(MoneyRow {
                label: "Credits",
                value,
            });
        }
    }
    if let Some(spend) = spend {
        let value = match (spend.used, spend.limit, spend.remaining_percent) {
            (Some(used), Some(limit), _) => Some(format!("{} of {}", amount(used), amount(limit))),
            (None, Some(limit), _) => Some(format!("Limit {}", amount(limit))),
            (Some(used), None, _) => Some(format!("{} used", amount(used))),
            (None, None, Some(remaining)) => Some(format!("{} left", percent(remaining))),
            (None, None, None) => None,
        };
        if let Some(value) = value {
            rows.push(MoneyRow {
                label: "Spend limit",
                value,
            });
        }
    }
    rows
}

/// Unit-less: the response does not say which currency or credit unit it uses.
fn amount(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

fn error_message(provider: ProviderId, error: &CollectError) -> String {
    let (name, cli) = match provider {
        ProviderId::Codex => ("Codex", "codex"),
    };
    match error {
        CollectError::AuthRequired {
            reason: AuthReason::Expired,
        } => {
            format!("{name} sign-in expired. Run “{cli}” in a terminal to renew it, then refresh.")
        }
        CollectError::AuthRequired {
            reason: AuthReason::Rejected,
        } => format!("{name} rejected the stored sign-in. Run “{cli} login”, then refresh."),
        CollectError::CredentialsMissing { .. } => {
            format!("{name} isn't signed in on this computer. Run “{cli} login”, then refresh.")
        }
        other => capitalized(&other.to_string()),
    }
}

fn short_error(error: &CollectError) -> String {
    match error {
        CollectError::RateLimited { .. } => "rate limited".to_owned(),
        CollectError::Server { status } => format!("HTTP {status}"),
        CollectError::Transport(_) => "network".to_owned(),
        other => other.kind().replace('_', " "),
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
