use gpui::{Rgba, rgb};
use tokenbubble_core::model::ProviderId;

use crate::presentation::Tone;

/// The few colors the panel needs. Compi passes one derived from its own theme; the
/// standalone app uses [`PanelTheme::dark`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelTheme {
    pub background: Rgba,
    pub surface: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub accent: Rgba,
    pub warning: Rgba,
    pub critical: Rgba,
    /// Unfilled part of a quota bar.
    pub track: Rgba,
}

impl PanelTheme {
    pub fn dark() -> Self {
        Self {
            background: rgb(0x17171a),
            surface: rgb(0x222226),
            border: rgb(0x303036),
            text: rgb(0xf2f2f3),
            muted: rgb(0xa0a0aa),
            accent: rgb(0x3ccfa0),
            warning: rgb(0xf2b33d),
            critical: rgb(0xf06058),
            track: rgb(0x34343b),
        }
    }

    pub fn for_provider(mut self, provider: ProviderId) -> Self {
        if provider == ProviderId::Claude {
            self.accent = rgb(0xd97757);
        }
        self
    }

    pub fn tone(&self, tone: Tone) -> Rgba {
        match tone {
            Tone::Normal => self.accent,
            Tone::Muted => self.muted,
            Tone::Warning => self.warning,
            Tone::Critical => self.critical,
        }
    }
}
