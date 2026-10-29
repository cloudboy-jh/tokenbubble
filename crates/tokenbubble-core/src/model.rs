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
    pub measured_at: Timestamp,
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
