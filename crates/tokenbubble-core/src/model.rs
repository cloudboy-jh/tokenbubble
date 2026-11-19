//! Provider-neutral usage readings. Nothing here holds credential material.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    #[default]
    Codex,
    Claude,
}

impl ProviderId {
    pub const ALL: [Self; 2] = [Self::Codex, Self::Claude];

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
        }
    }
}

/// One successful reading of a subscription's quota state.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UsageSnapshot {
    pub provider: ProviderId,
    /// When the numbers were true: the request time for live readings, the log entry time
    /// for [`SnapshotOrigin::LocalLog`].
    pub measured_at: Timestamp,
    pub origin: SnapshotOrigin,
    pub account: Account,
    /// Account-wide windows ordered session, weekly, other; then model-scoped windows in
    /// response order.
    pub windows: Vec<QuotaWindow>,
    /// Prepaid credit balance. Never folded into quota windows.
    pub credits: Option<Credits>,
    /// Workspace/individual monetary spend limit. Never folded into quota windows.
    pub spend_limit: Option<SpendLimit>,
    /// Optional response parts that were present but could not be decoded.
    pub issues: Vec<String>,
}

/// Where a snapshot's numbers came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotOrigin {
    /// The provider's usage endpoint.
    #[default]
    Live,
    /// The last limits the provider's CLI wrote to its local session log, used when the
    /// endpoint cannot be asked.
    LocalLog,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Account {
    pub id: Option<String>,
    pub email: Option<String>,
    pub plan: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QuotaWindow {
    /// Stable identifier derived from scope and duration, not from response position.
    pub id: String,
    /// Model or feature the window applies to; `None` for the account-wide windows.
    pub scope: Option<String>,
    pub kind: WindowKind,
    pub duration_secs: Option<u64>,
    /// Raw provider value; may exceed 100. Clamp only when drawing.
    pub used_percent: f64,
    pub resets_at: Option<Timestamp>,
}

/// Classified solely from the reported window duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    Session,
    Weekly,
    Other,
}

impl WindowKind {
    pub const SESSION_SECS: u64 = 5 * 60 * 60;
    pub const WEEKLY_SECS: u64 = 7 * 24 * 60 * 60;

    pub fn from_duration(duration_secs: Option<u64>) -> Self {
        match duration_secs {
            Some(Self::SESSION_SECS) => Self::Session,
            Some(Self::WEEKLY_SECS) => Self::Weekly,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Credits {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SpendLimit {
    /// ISO currency code when reported; amounts are in major units, not cents.
    pub currency: Option<String>,
    pub limit: Option<f64>,
    pub used: Option<f64>,
    pub remaining_percent: Option<f64>,
    pub resets_at: Option<Timestamp>,
}

/// Token use read from the provider CLI's local session logs. Independent of quota windows:
/// providers do not publish how tokens map to limit percentages.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Activity {
    /// Seven local calendar days ending today, oldest first; days without use are zero.
    pub days: Vec<DayActivity>,
    /// Totals over those seven days, largest [`TokenCounts::billable`] first.
    pub models: Vec<NamedTokens>,
    /// Working-directory name, same order and period as `models`.
    pub projects: Vec<NamedTokens>,
    /// The 60 minutes before the scan.
    pub last_hour: TokenCounts,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DayActivity {
    pub date: jiff::civil::Date,
    pub tokens: TokenCounts,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NamedTokens {
    pub name: String,
    pub tokens: TokenCounts,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TokenCounts {
    /// Input not served from cache.
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Includes reasoning/thinking tokens.
    pub output: u64,
}

impl TokenCounts {
    /// Fresh input, cache writes, and output. Cache reads are excluded because they are
    /// cheap re-reads of earlier context and would otherwise dwarf everything else.
    pub fn billable(&self) -> u64 {
        self.input + self.cache_write + self.output
    }

    pub fn add(&mut self, other: &Self) {
        self.input += other.input;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.output += other.output;
    }
}
