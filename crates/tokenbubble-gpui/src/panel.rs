//! The usage panel view. It renders [`PanelState`] and asks its host to refresh or open
//! settings; hosts own collection and windows, so Compi and the standalone app can each
//! schedule and place it their own way.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, ClipboardItem, Context, EventEmitter, FontWeight, IntoElement,
    ParentElement, Render, SharedString, Stateful, Styled, Window, div, prelude::*,
    pulsating_between, px, relative,
};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use tokenbubble_core::model::{Activity, ProviderId, UsageSnapshot};
use tokenbubble_core::providers::CollectError;

use crate::presentation::{
    ActivityView, BarView, Body, DisplayOptions, PanelState, PanelView, ShareRow, Tone,
    preferred_height, present,
};
use crate::provider_icon::provider_icon;
use crate::theme::PanelTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelEvent {
    RefreshRequested,
    SettingsRequested,
    ProviderSelected(ProviderId),
}

pub struct UsagePanel {
    state: PanelState,
    theme: PanelTheme,
    options: DisplayOptions,
    /// The error detail was just copied; cleared by the next state change.
    copied: bool,
}

impl EventEmitter<PanelEvent> for UsagePanel {}

impl UsagePanel {
    pub fn new(provider: ProviderId, theme: PanelTheme, options: DisplayOptions) -> Self {
        Self {
            state: PanelState {
                provider,
                ..PanelState::default()
            },
            theme,
            options,
            copied: false,
        }
    }

    pub fn state(&self) -> &PanelState {
        &self.state
    }

    pub fn options(&self) -> DisplayOptions {
        self.options
    }

    pub fn set_provider(&mut self, provider: ProviderId, cx: &mut Context<Self>) {
        self.state.select_provider(provider);
        self.copied = false;
        cx.notify();
    }

    pub fn begin_refresh(&mut self, cx: &mut Context<Self>) {
        self.state.begin_refresh();
        cx.notify();
    }

    pub fn apply(&mut self, result: Result<UsageSnapshot, CollectError>, cx: &mut Context<Self>) {
        self.state.apply(result);
        self.copied = false;
        cx.notify();
    }

    pub fn set_activity(
        &mut self,
        provider: ProviderId,
        activity: Option<Activity>,
        cx: &mut Context<Self>,
    ) {
        self.state.set_activity(provider, activity);
        cx.notify();
    }

    pub fn set_theme(&mut self, theme: PanelTheme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    pub fn set_options(&mut self, options: DisplayOptions, cx: &mut Context<Self>) {
        self.options = options;
        cx.notify();
    }

    /// Logical height that fits the current content without scrolling.
    pub fn preferred_height(&self) -> f32 {
        preferred_height(&self.view())
    }

    fn view(&self) -> PanelView {
        present(
            &self.state,
            Timestamp::now(),
            &TimeZone::system(),
            self.options,
        )
    }
}

impl Render for UsagePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.view();
        let theme = self.theme.for_provider(self.state.provider);
        div()
            .id("tokenbubble-panel")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .p(px(16.0))
            .bg(theme.background)
            .text_color(theme.text)
            .text_size(px(13.0))
            .child(header(&view, self.state.provider, &theme))
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .children(ProviderId::ALL.map(|provider| {
                        let selected = self.state.provider == provider;
                        let id = match provider {
                            ProviderId::Codex => "tokenbubble-provider-codex",
                            ProviderId::Claude => "tokenbubble-provider-claude",
                        };
                        action(
                            id,
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(provider_icon(provider, px(16.0)))
                                .child(provider.label()),
                            &theme,
                        )
                        .when(selected, |button| {
                            button.border_color(theme.accent).text_color(theme.accent)
                        })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(PanelEvent::ProviderSelected(provider));
                        }))
                    })),
            )
            .child(body(&view, &theme, self.copied, cx))
            .children((!view.money.is_empty()).then(|| money(&view, &theme)))
            .children(
                view.activity
                    .as_ref()
                    .map(|activity| activity_section(activity, &theme)),
            )
            .child(footer(&view, &theme, cx))
    }
}

fn header(view: &PanelView, provider: ProviderId, theme: &PanelTheme) -> impl IntoElement {
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap(px(12.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(px(16.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(provider == ProviderId::Claude, |title| {
                            title.text_color(theme.accent)
                        })
                        .child(provider_icon(provider, px(24.0)))
                        .child(view.title),
                )
                .children(view.account.clone().map(|account| {
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.muted)
                        .truncate()
                        .child(account)
                })),
        )
        .children(view.plan.clone().map(|plan| {
            div()
                .flex_none()
                .px(px(8.0))
                .py(px(2.0))
                .rounded_full()
                .border_1()
                .border_color(theme.accent)
                .text_color(theme.accent)
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .child(plan)
        }))
}

fn body(
    view: &PanelView,
    theme: &PanelTheme,
    copied: bool,
    cx: &mut Context<UsagePanel>,
) -> impl IntoElement {
    let container = div().flex().flex_col().gap(px(14.0));
    match &view.body {
        Body::Loading => container.child(
            div()
                .text_color(theme.muted)
                .child(format!("Loading {} usage…", view.title)),
        ),
        Body::Message { text, detail } => {
            let detail = detail.clone();
            container.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .p(px(12.0))
                    .rounded(px(8.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .line_height(relative(1.4))
                            .child(SharedString::from(text.clone())),
                    )
                    .child(
                        div().flex().child(
                            action(
                                "tokenbubble-copy-error",
                                if copied { "Copied" } else { "Copy error" },
                                theme,
                            )
                            .on_click(cx.listener(
                                move |panel, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        detail.clone(),
                                    ));
                                    panel.copied = true;
                                    cx.notify();
                                },
                            )),
                        ),
                    ),
            )
        }
        Body::Bars(bars) => container.children(bars.iter().map(|bar| quota_bar(bar, theme))),
    }
}

fn quota_bar(bar: &BarView, theme: &PanelTheme) -> impl IntoElement {
    let value_color = match bar.tone {
        Tone::Normal => theme.muted,
        tone => theme.tone(tone),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .flex()
                .justify_between()
                .items_baseline()
                .gap(px(8.0))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(bar.label.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.0))
                        .text_color(value_color)
                        .child(bar.value.clone()),
                ),
        )
        .child(
            div()
                .h(px(8.0))
                .w_full()
                .rounded_full()
                .bg(theme.track)
                .overflow_hidden()
                .child(
                    div()
                        .h_full()
                        .w(relative(bar.fill))
                        .rounded_full()
                        .bg(theme.tone(bar.tone)),
                ),
        )
        .children(bar.reset.clone().map(|reset| {
            div()
                .text_size(px(12.0))
                .text_color(theme.muted)
                .child(reset)
        }))
        .children(bar.pace.clone().map(|(pace, tone)| {
            div()
                .text_size(px(12.0))
                .text_color(theme.tone(tone))
                .child(pace)
        }))
}

fn money(view: &PanelView, theme: &PanelTheme) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .pt(px(12.0))
        .border_t_1()
        .border_color(theme.border)
        .children(view.money.iter().map(|row| {
            div()
                .flex()
                .justify_between()
                .child(div().text_color(theme.muted).child(row.label))
                .child(row.value.clone())
        }))
}

fn activity_section(activity: &ActivityView, theme: &PanelTheme) -> impl IntoElement {
    let busiest_height = 40.0;
    let chart = (!activity.days.is_empty()).then(|| {
        div()
            .flex()
            .gap(px(6.0))
            .pt(px(10.0))
            .children(activity.days.iter().map(|day| {
                let color = if day.today { theme.accent } else { theme.muted };
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(theme.muted)
                            .child(day.value.clone()),
                    )
                    .child(
                        div()
                            .h(px(busiest_height))
                            .w_full()
                            .flex()
                            .flex_col()
                            .justify_end()
                            .rounded(px(3.0))
                            .bg(theme.track)
                            .overflow_hidden()
                            .child(
                                div()
                                    .w_full()
                                    // A sliver keeps light days visible.
                                    .h(px((day.fill * busiest_height).max(if day.fill > 0.0 {
                                        2.0
                                    } else {
                                        0.0
                                    })))
                                    .bg(color),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(if day.today { theme.text } else { theme.muted })
                            .child(day.label.clone()),
                    )
            }))
    });
    div()
        .flex()
        .flex_col()
        .pt(px(12.0))
        .border_t_1()
        .border_color(theme.border)
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme.muted)
                .child("Local activity · last 7 days"),
        )
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .child(activity.summary.clone()),
        )
        .children(activity.note.clone().map(|note| {
            div()
                .text_size(px(12.0))
                .text_color(theme.muted)
                .child(note)
        }))
        .children(chart)
        .children(share_list("Models", &activity.models, theme))
        .children(share_list("Projects", &activity.projects, theme))
}

fn share_list(title: &'static str, rows: &[ShareRow], theme: &PanelTheme) -> Option<gpui::Div> {
    if rows.is_empty() {
        return None;
    }
    Some(
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .pt(px(10.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.muted)
                    .child(title),
            )
            .children(rows.iter().map(|row| {
                div()
                    .flex()
                    .justify_between()
                    .gap(px(8.0))
                    .child(div().min_w_0().truncate().child(row.name.clone()))
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.muted)
                            .child(row.value.clone()),
                    )
            })),
    )
}

/// A small bordered text button.
fn action(id: &'static str, label: impl IntoElement, theme: &PanelTheme) -> Stateful<gpui::Div> {
    let hover = theme.surface;
    div()
        .id(id)
        .flex_none()
        .px(px(10.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.border)
        .text_size(px(12.0))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(label)
}

fn footer(view: &PanelView, theme: &PanelTheme, cx: &mut Context<UsagePanel>) -> impl IntoElement {
    let status = view.status.clone().map(|(text, tone)| {
        let label = div().text_color(theme.tone(tone)).child(text);
        if view.refreshing {
            label
                .with_animation(
                    "tokenbubble-refreshing",
                    Animation::new(Duration::from_millis(1200))
                        .repeat()
                        .with_easing(pulsating_between(0.35, 1.0)),
                    |label, opacity| label.opacity(opacity),
                )
                .into_any_element()
        } else {
            label.into_any_element()
        }
    });
    let refresh = if view.refreshing {
        div()
            .flex_none()
            .px(px(10.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .text_size(px(12.0))
            .text_color(theme.muted)
            .child("Refresh")
            .into_any_element()
    } else {
        action("tokenbubble-refresh", "Refresh", theme)
            .on_click(cx.listener(|_, _, _, cx| cx.emit(PanelEvent::RefreshRequested)))
            .into_any_element()
    };
    div()
        .mt_auto()
        .pt(px(12.0))
        .border_t_1()
        .border_color(theme.border)
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .child(div().min_w_0().text_size(px(12.0)).children(status))
        .child(
            div().flex().flex_none().gap(px(6.0)).child(refresh).child(
                action("tokenbubble-settings", "Settings", theme)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PanelEvent::SettingsRequested))),
            ),
        )
}
