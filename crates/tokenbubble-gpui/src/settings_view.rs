//! Settings UI shared by every host. It renders plain data and emits events; the host
//! persists settings and applies side effects, so Compi can embed the same view.

use gpui::{
    ClipboardItem, Context, Entity, EventEmitter, FontWeight, IntoElement, ParentElement, Render,
    SharedString, Stateful, Styled, Subscription, Window, div, prelude::*, px, relative,
};
use jiff::Timestamp;
use tokenbubble_core::config::{BarFill, RefreshInterval, ResetDisplay, Settings};

use crate::panel::UsagePanel;
use crate::presentation::plan_name;
use crate::theme::PanelTheme;

pub const REPOSITORY_URL: &str = "https://github.com/cloudboy-jh/tokenbubble";
const CODEXBAR_URL: &str = "https://github.com/steipete/CodexBar";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsEvent {
    Changed(Settings),
    RefreshRequested,
    StartAtLoginChanged(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    General,
    Display,
    Codex,
    About,
}

impl Section {
    const ALL: [Self; 4] = [Self::General, Self::Display, Self::Codex, Self::About];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Display => "Display",
            Self::Codex => "Codex",
            Self::About => "About",
        }
    }
}

pub struct SettingsView {
    settings: Settings,
    panel: Entity<UsagePanel>,
    theme: PanelTheme,
    section: Section,
    /// `None` where the host cannot start at login.
    start_at_login: Option<bool>,
    /// Where settings are stored, and why the last load or save failed.
    settings_location: Option<String>,
    settings_problem: Option<String>,
    /// Shown as the Codex credential source.
    codex_home: Option<String>,
    copied: bool,
    _panel: Subscription,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

pub struct SettingsInit {
    pub settings: Settings,
    pub panel: Entity<UsagePanel>,
    pub theme: PanelTheme,
    pub start_at_login: Option<bool>,
    pub settings_location: Option<String>,
    pub settings_problem: Option<String>,
    pub codex_home: Option<String>,
}

impl SettingsView {
    pub fn new(init: SettingsInit, cx: &mut Context<Self>) -> Self {
        let observed = cx.observe(&init.panel, |_, _, cx| cx.notify());
        Self {
            settings: init.settings,
            panel: init.panel,
            theme: init.theme,
            section: Section::General,
            start_at_login: init.start_at_login,
            settings_location: init.settings_location,
            settings_problem: init.settings_problem,
            codex_home: init.codex_home,
            copied: false,
            _panel: observed,
        }
    }

    pub fn show_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        cx.notify();
    }

    pub fn set_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        self.settings = settings;
        cx.notify();
    }

    pub fn set_settings_problem(&mut self, problem: Option<String>, cx: &mut Context<Self>) {
        self.settings_problem = problem;
        cx.notify();
    }

    pub fn set_start_at_login(&mut self, enabled: Option<bool>, cx: &mut Context<Self>) {
        self.start_at_login = enabled;
        cx.notify();
    }

    fn change(&mut self, update: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let mut next = self.settings.clone();
        update(&mut next);
        if next != self.settings {
            self.settings = next.clone();
            cx.emit(SettingsEvent::Changed(next));
            cx.notify();
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let content = match self.section {
            Section::General => self.general(cx).into_any_element(),
            Section::Display => self.display(cx).into_any_element(),
            Section::Codex => self.codex(cx).into_any_element(),
            Section::About => self.about(cx).into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .bg(theme.background)
            .text_color(theme.text)
            .text_size(px(13.0))
            .child(
                div()
                    .flex_none()
                    .w(px(160.0))
                    .h_full()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .p(px(12.0))
                    .bg(theme.surface)
                    .border_r_1()
                    .border_color(theme.border)
                    .children(Section::ALL.into_iter().map(|section| {
                        let selected = section == self.section;
                        let hover = theme.track;
                        div()
                            .id(SharedString::from(format!("section-{}", section.label())))
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .when(selected, |item| {
                                item.bg(theme.track).font_weight(FontWeight::MEDIUM)
                            })
                            .when(!selected, |item| {
                                item.text_color(theme.muted)
                                    .hover(move |style| style.bg(hover))
                            })
                            .child(section.label())
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.show_section(section, cx);
                            }))
                    })),
            )
            .child(
                div()
                    .id("settings-content")
                    .flex_1()
                    // Without this the column grows to its longest line instead of wrapping.
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .p(px(24.0))
                    .flex()
                    .flex_col()
                    .gap(px(20.0))
                    .child(
                        div()
                            .text_size(px(18.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.section.label()),
                    )
                    .child(content),
            )
    }
}

impl SettingsView {
    fn general(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let interval = segmented(
            "refresh-interval",
            &RefreshInterval::ALL.map(|interval| (interval, interval.label())),
            self.settings.refresh,
            &theme,
            cx,
            |view, interval, cx| view.change(|settings| settings.refresh = interval, cx),
        );
        let on_open = toggle("refresh-on-open", self.settings.refresh_on_open, &theme).on_click(
            cx.listener(|view, _, _, cx| {
                view.change(
                    |settings| settings.refresh_on_open = !settings.refresh_on_open,
                    cx,
                )
            }),
        );
        div()
            .flex()
            .flex_col()
            .gap(px(18.0))
            .child(row(
                "Refresh automatically",
                "How often TokenBubble asks for new usage. Manual only refreshes on request.",
                interval,
                &theme,
            ))
            .child(row(
                "Refresh when the panel opens",
                "Fetch new usage if the reading is more than a minute old.",
                on_open,
                &theme,
            ))
            .children(self.start_at_login.map(|enabled| {
                row(
                    "Start at sign-in",
                    "Launch TokenBubble in the notification area when you sign in.",
                    toggle("start-at-login", enabled, &theme).on_click(cx.listener(
                        move |_, _, _, cx| cx.emit(SettingsEvent::StartAtLoginChanged(!enabled)),
                    )),
                    &theme,
                )
            }))
            .children(self.settings_location.clone().map(|location| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .text_size(px(12.0))
                    .text_color(theme.muted)
                    .child(format!("Settings file: {location}"))
                    .children(
                        self.settings_problem
                            .clone()
                            .map(|problem| div().text_color(theme.warning).child(problem)),
                    )
            }))
    }

    fn display(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let fill = segmented(
            "bar-fill",
            &[(BarFill::Used, "Used"), (BarFill::Remaining, "Remaining")],
            self.settings.bar_fill,
            &theme,
            cx,
            |view, fill, cx| view.change(|settings| settings.bar_fill = fill, cx),
        );
        let reset = segmented(
            "reset-display",
            &[
                (ResetDisplay::Both, "Both"),
                (ResetDisplay::Countdown, "Countdown"),
                (ResetDisplay::Clock, "Clock time"),
            ],
            self.settings.reset_display,
            &theme,
            cx,
            |view, display, cx| view.change(|settings| settings.reset_display = display, cx),
        );
        let credits = toggle("show-credits", self.settings.show_credits, &theme).on_click(
            cx.listener(|view, _, _, cx| {
                view.change(
                    |settings| settings.show_credits = !settings.show_credits,
                    cx,
                )
            }),
        );
        div()
            .flex()
            .flex_col()
            .gap(px(18.0))
            .child(row(
                "Bars show",
                "Fill bars and the tray meter with what you've used or what's left.",
                fill,
                &theme,
            ))
            .child(row(
                "Reset times",
                "A countdown, the local clock time, or both.",
                reset,
                &theme,
            ))
            .child(row(
                "Show credits and spend limits",
                "Monetary balances reported alongside quota.",
                credits,
                &theme,
            ))
    }

    fn codex(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let state = self.panel.read(cx).state().clone();
        let snapshot = state.snapshot.as_ref();
        let status = if state.refreshing {
            "Refreshing…".to_owned()
        } else if let Some(error) = &state.error {
            if snapshot.is_some() {
                format!("Last refresh failed: {error}")
            } else {
                capitalize(&error.to_string())
            }
        } else if snapshot.is_some() {
            "OK".to_owned()
        } else {
            "Not fetched yet".to_owned()
        };
        let updated = snapshot.map(|snapshot| {
            let minutes = Timestamp::now()
                .duration_since(snapshot.measured_at)
                .as_secs()
                .max(0)
                / 60;
            match minutes {
                0 => "Just now".to_owned(),
                1 => "1 minute ago".to_owned(),
                n if n < 60 => format!("{n} minutes ago"),
                n => format!("{} hours ago", n / 60),
            }
        });
        let info = [
            (
                "Source",
                Some(match &self.codex_home {
                    Some(home) => format!("This PC · {home}"),
                    None => "This PC".to_owned(),
                }),
            ),
            (
                "Account",
                snapshot.and_then(|snapshot| snapshot.account.email.clone()),
            ),
            (
                "Plan",
                snapshot.and_then(|snapshot| snapshot.account.plan.as_deref().map(plan_name)),
            ),
            ("Updated", updated),
            ("Status", Some(status)),
        ];
        let error_detail = state
            .error
            .as_ref()
            .map(|error| format!("{} ({})", error, error.kind()));
        let refresh = if state.refreshing {
            button("codex-refresh", "Refreshing…", &theme).text_color(theme.muted)
        } else {
            button("codex-refresh", "Refresh now", &theme)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::RefreshRequested)))
        };
        let copy = error_detail.map(|detail| {
            button(
                "codex-copy-error",
                if self.copied { "Copied" } else { "Copy error" },
                &theme,
            )
            .on_click(cx.listener(move |view, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(detail.clone()));
                view.copied = true;
                cx.notify();
            }))
        });
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .text_color(theme.muted)
                    .line_height(relative(1.4))
                    .child(
                        "Reads the sign-in Codex CLI stores on this PC. TokenBubble never signs \
                         in, renews tokens, or changes Codex files.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border)
                    .children(info.into_iter().enumerate().map(|(index, (label, value))| {
                        div()
                            .flex()
                            .gap(px(12.0))
                            .px(px(12.0))
                            .py(px(8.0))
                            .when(index > 0, |row| row.border_t_1().border_color(theme.border))
                            .child(
                                div()
                                    .w(px(80.0))
                                    .flex_none()
                                    .text_color(theme.muted)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .line_height(relative(1.4))
                                    .child(value.unwrap_or_else(|| "—".to_owned())),
                            )
                    })),
            )
            .child(div().flex().gap(px(8.0)).child(refresh).children(copy))
    }

    fn about(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(16.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("TokenBubble"),
                    )
                    .child(
                        div()
                            .text_color(theme.muted)
                            .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                    ),
            )
            .child(
                div()
                    .line_height(relative(1.4))
                    .child("A subscription usage meter for AI coding plans."),
            )
            .child(
                div()
                    .text_color(theme.muted)
                    .line_height(relative(1.4))
                    .child(
                        "Codex usage handling is adapted from CodexBar by Peter Steinberger \
                         (MIT license).",
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        button("about-repository", "TokenBubble on GitHub", &theme)
                            .on_click(cx.listener(|_, _, _, cx| cx.open_url(REPOSITORY_URL))),
                    )
                    .child(
                        button("about-codexbar", "CodexBar", &theme)
                            .on_click(cx.listener(|_, _, _, cx| cx.open_url(CODEXBAR_URL))),
                    ),
            )
    }
}

fn row(
    title: &'static str,
    description: &'static str,
    control: impl IntoElement,
    theme: &PanelTheme,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.muted)
                        .child(description),
                ),
        )
        .child(div().flex().child(control))
}

fn button(id: &'static str, label: &'static str, theme: &PanelTheme) -> Stateful<gpui::Div> {
    let hover = theme.surface;
    div()
        .id(id)
        .px(px(12.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(label)
}

/// A switch drawn as a pill with a knob.
fn toggle(id: &'static str, on: bool, theme: &PanelTheme) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(36.0))
        .h(px(20.0))
        .p(px(2.0))
        .rounded_full()
        .cursor_pointer()
        .flex()
        .when(on, |track| track.bg(theme.accent).justify_end())
        .when(!on, |track| track.bg(theme.track).justify_start())
        .child(div().size(px(16.0)).rounded_full().bg(theme.text))
}

/// Mutually exclusive options; clicking one calls `select`.
fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &[(T, &'static str)],
    current: T,
    theme: &PanelTheme,
    cx: &mut Context<SettingsView>,
    select: fn(&mut SettingsView, T, &mut Context<SettingsView>),
) -> impl IntoElement + use<T> {
    div()
        .flex()
        .flex_wrap()
        .p(px(2.0))
        .gap(px(2.0))
        .rounded(px(8.0))
        .bg(theme.surface)
        .border_1()
        .border_color(theme.border)
        .children(options.iter().enumerate().map(|(index, (value, label))| {
            let value = *value;
            let selected = value == current;
            let hover = theme.track;
            div()
                .id(SharedString::from(format!("{id}-{index}")))
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .when(selected, |option| {
                    option.bg(theme.track).font_weight(FontWeight::MEDIUM)
                })
                .when(!selected, |option| {
                    option
                        .text_color(theme.muted)
                        .hover(move |style| style.bg(hover))
                })
                .child(*label)
                .on_click(cx.listener(move |view, _, _, cx| select(view, value, cx)))
        }))
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
