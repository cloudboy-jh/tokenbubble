//! Renderer-independent panel state, panel text, and the tray tooltip.

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use tokenbubble_core::config::{BarFill, ResetDisplay, Settings};
use tokenbubble_core::model::{
    Activity, Credits, NamedTokens, ProviderId, QuotaWindow, SnapshotOrigin, SpendLimit,
    TokenCounts, UsageSnapshot, WindowKind,
};
use tokenbubble_core::providers::{AuthReason, CollectError};

/// What the panel knows: the last good reading and the outcome of the latest attempt.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelState {
    /// Selected source, including before its first successful reading.
    pub provider: ProviderId,
    pub snapshot: Option<UsageSnapshot>,
    /// Token use from the selected provider's local logs; `None` when it keeps none.
    pub activity: Option<Activity>,
    /// Set when the most recent collection failed.
    pub error: Option<CollectError>,
    pub refreshing: bool,
}

impl PanelState {
    pub fn select_provider(&mut self, provider: ProviderId) {
        if self.provider != provider {
            *self = Self {
                provider,
                ..Self::default()
            };
        }
    }

    pub fn begin_refresh(&mut self) {
        self.refreshing = true;
    }

    /// Ignored when it belongs to a provider that is no longer selected.
    pub fn set_activity(&mut self, provider: ProviderId, activity: Option<Activity>) {
        if provider == self.provider {
            self.activity = activity;
        }
    }

    /// Record a collection outcome. Transient failures keep the previous reading (shown as
    /// stale); sign-in and configuration failures drop it, because it may no longer
    /// describe the account the credentials now belong to.
    pub fn apply(&mut self, result: Result<UsageSnapshot, CollectError>) {
        if result
            .as_ref()
            .is_ok_and(|snapshot| snapshot.provider != self.provider)
        {
            return;
        }
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
    pub activity: Option<ActivityView>,
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
    /// Where the window is heading at its current rate.
    pub pace: Option<(String, Tone)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoneyRow {
    pub label: &'static str,
    pub value: String,
}

/// Seven days of local token use.
#[derive(Clone, Debug, PartialEq)]
pub struct ActivityView {
    /// `1.2M today · 8.4M this week`.
    pub summary: String,
    /// Cache reads left out of the totals, and the last hour's use.
    pub note: Option<String>,
    /// Oldest first; empty when nothing was used all week.
    pub days: Vec<DayBar>,
    pub models: Vec<ShareRow>,
    pub projects: Vec<ShareRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DayBar {
    /// `Mon`.
    pub label: String,
    pub value: String,
    /// Height relative to the busiest day, `0.0..=1.0`.
    pub fill: f32,
    pub today: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareRow {
    pub name: String,
    /// `6.1M · 72%`.
    pub value: String,
}

const WARNING_PERCENT: f64 = 75.0;
/// Projections need this much of the window behind them to mean anything.
const MIN_PACE_ELAPSED_SECS: i64 = 60 * 60;
/// Rows per breakdown list.
const TOP_ROWS: usize = 3;

pub fn present(
    state: &PanelState,
    now: Timestamp,
    tz: &TimeZone,
    options: DisplayOptions,
) -> PanelView {
    let provider = state.provider;
    let mut view = PanelView {
        title: provider.label(),
        plan: None,
        account: None,
        body: Body::Loading,
        money: Vec::new(),
        activity: state.activity.as_ref().map(activity_view),
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
            .map(|window| bar(window, snapshot.measured_at, now, tz, options))
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
    } else if snapshot.origin == SnapshotOrigin::LocalLog {
        (
            format!("From {}'s local log · {age}", provider.label()),
            Tone::Muted,
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
    /// Title/account header plus the provider buttons and their section gap.
    const HEADER: f32 = 94.0;
    /// Border, 12 px padding, 30 px buttons.
    const FOOTER: f32 = 44.0;
    const LINE: f32 = 21.0;
    const SMALL_LINE: f32 = 19.5;
    /// Day values, columns, and labels.
    const CHART: f32 = 16.0 + 4.0 + 40.0 + 4.0 + SMALL_LINE;
    let body = match &view.body {
        Body::Loading => LINE,
        // Card padding and border, the wrapped message, and the copy button row.
        Body::Message { text, .. } => 26.0 + LINE * (text.len() / 44 + 1) as f32 + 10.0 + 30.0,
        Body::Bars(bars) => {
            bars.iter()
                .map(|bar| {
                    let extra = [bar.reset.is_some(), bar.pace.is_some()]
                        .into_iter()
                        .filter(|shown| *shown)
                        .count() as f32;
                    LINE + 6.0 + 8.0 + extra * (6.0 + SMALL_LINE)
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
    let activity = view.activity.as_ref().map_or(0.0, |activity| {
        let rows = activity.models.len() + activity.projects.len();
        let lists = [&activity.models, &activity.projects]
            .into_iter()
            .filter(|list| !list.is_empty())
            .count();
        SECTION_GAP
            + 13.0
            + SMALL_LINE
            + LINE
            + activity.note.as_ref().map_or(0.0, |_| SMALL_LINE)
            + if activity.days.is_empty() {
                0.0
            } else {
                10.0 + CHART
            }
            + lists as f32 * (10.0 + SMALL_LINE)
            + rows as f32 * (LINE + 2.0)
    });
    PADDING + HEADER + SECTION_GAP + body + money + activity + SECTION_GAP + FOOTER
}

/// At most 127 UTF-16 units, the Windows notification-area tooltip limit.
pub fn tray_tooltip(state: &PanelState, now: Timestamp, options: DisplayOptions) -> String {
    let provider = state.provider;
    let name = provider.label();
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
                None => {
                    let age = ago(now.duration_since(snapshot.measured_at));
                    match snapshot.origin {
                        SnapshotOrigin::Live => format!("Updated {age}"),
                        SnapshotOrigin::LocalLog => format!("From local log {age}"),
                    }
                }
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

/// `measured_at` is when the window's percentage was true; projections start from it.
fn bar(
    window: &QuotaWindow,
    measured_at: Timestamp,
    now: Timestamp,
    tz: &TimeZone,
    options: DisplayOptions,
) -> BarView {
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
        pace: pace(window, measured_at, now, tz),
    }
}

/// Straight-line projection of this window's use to its reset. Silent until an hour of the
/// window has passed, and once it is already at or over the limit.
fn pace(
    window: &QuotaWindow,
    measured_at: Timestamp,
    now: Timestamp,
    tz: &TimeZone,
) -> Option<(String, Tone)> {
    let used = window.used_percent;
    let duration = i64::try_from(window.duration_secs?).ok()?;
    let reset = window.resets_at?;
    let remaining = reset.duration_since(measured_at).as_secs();
    let elapsed = duration - remaining;
    if remaining <= 0 || elapsed < MIN_PACE_ELAPSED_SECS || used <= 0.0 || used >= 100.0 {
        return None;
    }
    let per_sec = used / elapsed as f64;
    let projected = used + per_sec * remaining as f64;
    if projected < 100.0 {
        return Some((
            format!("On pace for about {:.0}% by reset", projected),
            Tone::Muted,
        ));
    }
    let full = measured_at + SignedDuration::from_secs(((100.0 - used) / per_sec) as i64);
    let text = if full <= now {
        "At this pace the limit is already reached".to_owned()
    } else {
        format!(
            "At this pace you'll hit the limit {}",
            local_when(full, now, tz)
        )
    };
    Some((text, Tone::Warning))
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
    let when = local_when(at, now, tz);
    match display {
        ResetDisplay::Both => format!("Resets in {} · {when}", span(remaining)),
        ResetDisplay::Countdown => format!("Resets in {}", span(remaining)),
        ResetDisplay::Clock => format!("Resets {when}"),
    }
}

/// `today 5:05 AM`, `tomorrow 1:00 AM`, or `Sun Oct 18 7:44 PM`, in the viewer's time zone.
fn local_when(at: Timestamp, now: Timestamp, tz: &TimeZone) -> String {
    let local = at.to_zoned(tz.clone());
    let today = now.to_zoned(tz.clone()).date();
    let clock = local.strftime("%-I:%M %p").to_string();
    if local.date() == today {
        format!("today {clock}")
    } else if Some(local.date()) == today.tomorrow().ok() {
        format!("tomorrow {clock}")
    } else {
        format!("{} {clock}", local.strftime("%a %b %-d"))
    }
}

fn activity_view(activity: &Activity) -> ActivityView {
    let today = activity
        .days
        .last()
        .map(|day| day.tokens)
        .unwrap_or_default();
    let mut week = TokenCounts::default();
    for day in &activity.days {
        week.add(&day.tokens);
    }
    let summary = if week.billable() == 0 {
        "No use in the last 7 days".to_owned()
    } else {
        format!(
            "{} today · {} this week",
            tokens(today.billable()),
            tokens(week.billable())
        )
    };
    let note = [
        (activity.last_hour.billable() > 0)
            .then(|| format!("{} in the last hour", tokens(activity.last_hour.billable()))),
        (week.cache_read > 0)
            .then(|| format!("{} cache reads not counted", tokens(week.cache_read))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let busiest = activity
        .days
        .iter()
        .map(|day| day.tokens.billable())
        .max()
        .unwrap_or(0);
    let last = activity.days.len().saturating_sub(1);
    let days = if busiest == 0 {
        Vec::new()
    } else {
        activity
            .days
            .iter()
            .enumerate()
            .map(|(index, day)| DayBar {
                label: day.date.strftime("%a").to_string(),
                value: tokens(day.tokens.billable()),
                fill: (day.tokens.billable() as f64 / busiest as f64) as f32,
                today: index == last,
            })
            .collect()
    };
    let shares = |list: &[NamedTokens]| {
        list.iter()
            .filter(|entry| entry.tokens.billable() > 0)
            .take(TOP_ROWS)
            .map(|entry| ShareRow {
                name: entry.name.clone(),
                value: format!(
                    "{} · {:.0}%",
                    tokens(entry.tokens.billable()),
                    entry.tokens.billable() as f64 * 100.0 / week.billable() as f64
                ),
            })
            .collect()
    };
    ActivityView {
        summary,
        note: (!note.is_empty()).then(|| note.join(" · ")),
        days,
        models: shares(&activity.models),
        projects: shares(&activity.projects),
    }
}

/// `950`, `12.3K`, `4.5M`, `1.2B`.
pub fn tokens(count: u64) -> String {
    let value = count as f64;
    let (scaled, unit) = match count {
        0..1_000 => return count.to_string(),
        1_000..1_000_000 => (value / 1e3, "K"),
        1_000_000..1_000_000_000 => (value / 1e6, "M"),
        _ => (value / 1e9, "B"),
    };
    if scaled < 10.0 {
        format!("{scaled:.1}{unit}")
    } else {
        format!("{scaled:.0}{unit}")
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
        let spend_amount = |value| match spend.currency.as_deref() {
            None => amount(value),
            Some("USD") => format!("${value:.2}"),
            Some(currency) => format!("{currency} {value:.2}"),
        };
        let value = match (spend.used, spend.limit, spend.remaining_percent) {
            (Some(used), Some(limit), _) => {
                Some(format!("{} of {}", spend_amount(used), spend_amount(limit)))
            }
            (None, Some(limit), _) => Some(format!("Limit {}", spend_amount(limit))),
            (Some(used), None, _) => Some(format!("{} used", spend_amount(used))),
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

fn renewal_command(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Codex => "codex",
        ProviderId::Claude => "claude auth login",
    }
}

fn login_command(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Codex => "codex login",
        ProviderId::Claude => "claude auth login",
    }
}

fn error_message(provider: ProviderId, error: &CollectError) -> String {
    let (name, cli) = (provider.label(), renewal_command(provider));
    let login = login_command(provider);
    match error {
        CollectError::AuthRequired {
            reason: AuthReason::Expired,
        } => {
            format!("{name} sign-in expired. Run “{cli}” in a terminal to renew it, then refresh.")
        }
        CollectError::AuthRequired {
            reason: AuthReason::Rejected,
        } => format!("{name} rejected the stored sign-in. Run “{login}”, then refresh."),
        CollectError::CredentialsUnwritable { path } => format!(
            "TokenBubble renewed the {name} sign-in but couldn't save it to {}. Check that file's \
             permissions, then run “{login}”.",
            path.display()
        ),
        CollectError::CredentialsMissing { .. } => {
            format!("{name} isn't signed in on this computer. Run “{login}”, then refresh.")
        }
        CollectError::NoUsageData => {
            format!("{name} didn't report any usage limits for this account.")
        }
        other => capitalized(&other.to_string()),
    }
}

/// One tooltip line.
fn short_message(provider: ProviderId, error: &CollectError) -> String {
    let cli = renewal_command(provider);
    let login = login_command(provider);
    match error {
        CollectError::AuthRequired {
            reason: AuthReason::Expired,
        } => format!("Sign-in expired · run “{cli}”"),
        CollectError::AuthRequired { .. } | CollectError::CredentialsMissing { .. } => {
            format!("Not signed in · run “{login}”")
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
