//! Renderer-independent panel state and everything it displays: panel text, the tray
//! tooltip, and the tray meter's bar fractions.

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use tokenbubble_core::config::{BarFill, ResetDisplay, Settings};
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
                if !error.is_transient() {
                    self.snapshot = None;
                }
                self.error = Some(error);
            }
        }
    }
}

/// The display-related subset of [`Settings`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayOptions {
    pub bar_fill: BarFill,
    pub reset_display: ResetDisplay,
    pub show_credits: bool,
}

impl From<&Settings> for DisplayOptions {
    fn from(settings: &Settings) -> Self {
        Self {
            bar_fill: settings.bar_fill,
            reset_display: settings.reset_display,
            show_credits: settings.show_credits,
        }
    }
}

impl Default for DisplayOptions {
    fn default() -> Self {
        Self::from(&Settings::default())
    }
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
    /// A failure with no reading to show. `detail` is the raw error for copying.
    Message {
        text: String,
        detail: String,
    },
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

pub fn present(
    state: &PanelState,
    now: Timestamp,
    tz: &TimeZone,
    options: DisplayOptions,
) -> PanelView {
    let provider = provider_of(state);
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
        if let Some(error) = &state.error {
            view.body = Body::Message {
                text: error_message(provider, error),
                detail: format!("{} ({})", error, error.kind()),
            };
        }
        if state.refreshing {
            view.status = Some(("Refreshing…".to_owned(), Tone::Muted));
        }
        return view;
    };

    view.plan = snapshot.account.plan.as_deref().map(plan_name);
    view.account = snapshot.account.email.clone();
    view.body = Body::Bars(
        snapshot
            .windows
            .iter()
            .map(|window| bar(window, now, tz, options))
            .collect(),
    );
    if options.show_credits {
        view.money = money_rows(snapshot.credits.as_ref(), snapshot.spend_limit.as_ref());
    }
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

/// Logical height that fits `view` without scrolling, mirroring the panel's layout metrics.
/// GPUI's default line height is about 1.6 × the font size.
pub fn preferred_height(view: &PanelView) -> f32 {
    const PADDING: f32 = 32.0;
    const SECTION_GAP: f32 = 16.0;
    /// 16 px title, 2 px gap, 12 px account line.
    const HEADER: f32 = 48.0;
    /// Border, 12 px padding, 30 px buttons.
    const FOOTER: f32 = 44.0;
    const LINE: f32 = 21.0;
    const SMALL_LINE: f32 = 19.5;
    let body = match &view.body {
        Body::Loading => LINE,
        // Card padding and border, the wrapped message, and the copy button row.
        Body::Message { text, .. } => 26.0 + LINE * (text.len() / 44 + 1) as f32 + 10.0 + 30.0,
        Body::Bars(bars) => {
            bars.iter()
                .map(|bar| {
                    let reset = if bar.reset.is_some() {
                        6.0 + SMALL_LINE
                    } else {
                        0.0
                    };
                    LINE + 6.0 + 8.0 + reset
                })
                .sum::<f32>()
                + 14.0 * bars.len().saturating_sub(1) as f32
        }
    };
    let money = if view.money.is_empty() {
        0.0
    } else {
        SECTION_GAP + 13.0 + LINE * view.money.len() as f32 + 6.0 * (view.money.len() - 1) as f32
    };
    PADDING + HEADER + SECTION_GAP + body + money + SECTION_GAP + FOOTER
}

/// Fractions for the tray meter: session on top, weekly below, each `0.0..=1.0` in the
/// user's fill direction. Empty when there is no reading.
#[derive(Clone, Debug, PartialEq)]
pub struct TrayMeter {
    pub bars: Vec<f32>,
    /// Drawn faded: the reading is stale or missing.
    pub dimmed: bool,
}

pub fn tray_meter(state: &PanelState, options: DisplayOptions) -> TrayMeter {
    let bars = state
        .snapshot
        .as_ref()
        .map(|snapshot| {
            meter_windows(snapshot)
                .map(|window| fill_fraction(window.used_percent, options.bar_fill))
                .collect()
        })
        .unwrap_or_default();
    TrayMeter {
        bars,
        dimmed: state.snapshot.is_none() || state.error.is_some(),
    }
}

/// At most 127 UTF-16 units, the Windows notification-area tooltip limit.
pub fn tray_tooltip(state: &PanelState, now: Timestamp, options: DisplayOptions) -> String {
    let provider = provider_of(state);
    let name = provider_name(provider);
    let mut lines = Vec::new();
    match &state.snapshot {
        None => {
            lines.push(format!("TokenBubble · {name}"));
            lines.push(match &state.error {
                Some(error) => short_message(provider, error),
                None => "Loading…".to_owned(),
            });
        }
        Some(snapshot) => {
            lines.push(match snapshot.account.plan.as_deref() {
                Some(plan) => format!("{name} · {}", plan_name(plan)),
                None => name.to_owned(),
            });
            for window in meter_windows(snapshot) {
                let mut line = format!(
                    "{} {}",
                    kind_label(window),
                    percent_value(window.used_percent, options.bar_fill)
                );
                if let Some(reset) = window.resets_at {
                    let remaining = reset.duration_since(now);
                    if remaining > SignedDuration::ZERO {
                        line.push_str(&format!(" · resets in {}", span(remaining)));
                    }
                }
                lines.push(line);
            }
            lines.push(match &state.error {
                Some(error) => format!("Couldn't refresh ({})", short_error(error)),
                None => format!("Updated {}", ago(now.duration_since(snapshot.measured_at))),
            });
        }
    }
    truncate_utf16(&lines.join("\n"), 127)
}

fn truncate_utf16(text: &str, max_units: usize) -> String {
    let mut units = 0;
    let mut out = String::new();
    for ch in text.chars() {
        units += ch.len_utf16();
        if units > max_units {
            break;
        }
        out.push(ch);
    }
    out
}

/// Account-wide session and weekly windows, else the first two account-wide windows.
fn meter_windows(snapshot: &UsageSnapshot) -> impl Iterator<Item = &QuotaWindow> {
    let account: Vec<&QuotaWindow> = snapshot
        .windows
        .iter()
        .filter(|window| window.scope.is_none())
        .collect();
    let preferred: Vec<&QuotaWindow> = account
        .iter()
        .copied()
        .filter(|window| matches!(window.kind, WindowKind::Session | WindowKind::Weekly))
        .collect();
    if preferred.is_empty() {
        account
    } else {
        preferred
    }
    .into_iter()
    .take(2)
}

fn provider_of(state: &PanelState) -> ProviderId {
    state
        .snapshot
        .as_ref()
        .map_or(ProviderId::Codex, |snapshot| snapshot.provider)
}

fn provider_name(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Codex => "Codex",
    }
}

/// `pro` → `Pro`, `free_workspace` → `Free workspace`.
pub fn plan_name(raw: &str) -> String {
    capitalized(&raw.trim().replace(['_', '-'], " "))
}

fn kind_label(window: &QuotaWindow) -> String {
    match (window.kind, window.duration_secs) {
        (WindowKind::Session, _) => "Session".to_owned(),
        (WindowKind::Weekly, _) => "Weekly".to_owned(),
        (WindowKind::Other, Some(secs)) => format!("{} window", duration_label(secs)),
        (WindowKind::Other, None) => "Usage window".to_owned(),
    }
}

fn bar(window: &QuotaWindow, now: Timestamp, tz: &TimeZone, options: DisplayOptions) -> BarView {
    let kind = kind_label(window);
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
    let mut value = percent_value(used, options.bar_fill);
    if used > 100.0 {
        value.push_str(" · over limit");
    }
    BarView {
        id: window.id.clone(),
        label,
        value,
        fill: fill_fraction(used, options.bar_fill),
        tone,
        reset: window
            .resets_at
            .map(|at| reset_text(at, now, tz, options.reset_display)),
    }
}

fn fill_fraction(used_percent: f64, fill: BarFill) -> f32 {
    let percent = match fill {
        BarFill::Used => used_percent,
        BarFill::Remaining => 100.0 - used_percent,
    };
    (percent / 100.0).clamp(0.0, 1.0) as f32
}

fn percent_value(used_percent: f64, fill: BarFill) -> String {
    match fill {
        BarFill::Used => format!("{} used", percent(used_percent)),
        BarFill::Remaining => format!("{} left", percent((100.0 - used_percent).max(0.0))),
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

/// `Resets in 5d 15h · Sun Oct 18 12:44 PM`, in the viewer's time zone.
pub fn reset_text(at: Timestamp, now: Timestamp, tz: &TimeZone, display: ResetDisplay) -> String {
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
    match display {
        ResetDisplay::Both => format!("Resets in {} · {when}", span(remaining)),
        ResetDisplay::Countdown => format!("Resets in {}", span(remaining)),
        ResetDisplay::Clock => format!("Resets {when}"),
    }
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

fn cli_name(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Codex => "codex",
    }
}

fn error_message(provider: ProviderId, error: &CollectError) -> String {
    let (name, cli) = (provider_name(provider), cli_name(provider));
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
        CollectError::NoUsageData => {
            format!("{name} didn't report any usage limits for this account.")
        }
        other => capitalized(&other.to_string()),
    }
}

/// One tooltip line.
fn short_message(provider: ProviderId, error: &CollectError) -> String {
    let cli = cli_name(provider);
    match error {
        CollectError::AuthRequired {
            reason: AuthReason::Expired,
        } => format!("Sign-in expired · run “{cli}”"),
        CollectError::AuthRequired { .. } | CollectError::CredentialsMissing { .. } => {
            format!("Not signed in · run “{cli} login”")
        }
        other => capitalized(&short_error(other)),
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
