//! User settings shared by every host. Never holds credentials.

use std::io;
use std::path::Path;
use std::time::Duration;

use crate::model::ProviderId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefreshInterval {
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "1m")]
    OneMinute,
    #[serde(rename = "2m")]
    TwoMinutes,
    #[default]
    #[serde(rename = "5m")]
    FiveMinutes,
    #[serde(rename = "15m")]
    FifteenMinutes,
    #[serde(rename = "30m")]
    ThirtyMinutes,
}

impl RefreshInterval {
    pub const ALL: [Self; 6] = [
        Self::Manual,
        Self::OneMinute,
        Self::TwoMinutes,
        Self::FiveMinutes,
        Self::FifteenMinutes,
        Self::ThirtyMinutes,
    ];

    /// `None` for manual refresh only.
    pub fn duration(self) -> Option<Duration> {
        let minutes = match self {
            Self::Manual => return None,
            Self::OneMinute => 1,
            Self::TwoMinutes => 2,
            Self::FiveMinutes => 5,
            Self::FifteenMinutes => 15,
            Self::ThirtyMinutes => 30,
        };
        Some(Duration::from_secs(minutes * 60))
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => "Manual",
            Self::OneMinute => "1 min",
            Self::TwoMinutes => "2 min",
            Self::FiveMinutes => "5 min",
            Self::FifteenMinutes => "15 min",
            Self::ThirtyMinutes => "30 min",
        }
    }
}

/// Whether bars and percentages show consumption or what is left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarFill {
    #[default]
    Used,
    Remaining,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetDisplay {
    /// `Resets in 5d 15h · Sun Oct 18 12:44 PM`
    #[default]
    Both,
    /// `Resets in 5d 15h`
    Countdown,
    /// `Resets Sun Oct 18 12:44 PM`
    Clock,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub provider: ProviderId,
    pub refresh: RefreshInterval,
    /// Collect when the panel opens if the reading is older than a minute.
    pub refresh_on_open: bool,
    pub bar_fill: BarFill,
    pub reset_display: ResetDisplay,
    pub show_credits: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: ProviderId::default(),
            refresh: RefreshInterval::default(),
            refresh_on_open: true,
            bar_fill: BarFill::default(),
            reset_display: ResetDisplay::default(),
            show_credits: true,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Read(io::Error),
    /// The file exists but is not valid settings; message is from the TOML parser.
    Parse(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(error) => write!(f, "could not read settings: {error}"),
            Self::Parse(message) => write!(f, "settings file is invalid: {message}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Settings {
    /// Missing files yield defaults. Unknown keys are ignored; missing keys take defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(ConfigError::Read(error)),
        };
        toml::from_str(&text).map_err(|error| ConfigError::Parse(error.message().to_owned()))
    }

    /// Write via a sibling temporary file and rename, so readers never see a partial file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let staging = path.with_extension("toml.partial");
        std::fs::write(&staging, text)?;
        std::fs::rename(&staging, path)
    }
}
