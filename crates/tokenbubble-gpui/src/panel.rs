//! The usage panel view. It renders [`PanelState`] and asks its host to refresh; hosts own
//! collection so Compi and the standalone app can schedule it their own way.

use gpui::{
    Context, EventEmitter, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, prelude::*, px, relative,
};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use tokenbubble_core::model::UsageSnapshot;
use tokenbubble_core::providers::CollectError;

use crate::presentation::{BarView, Body, PanelState, PanelView, Tone, present};
use crate::theme::PanelTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelEvent {
    RefreshRequested,
}

pub struct UsagePanel {
    state: PanelState,
    theme: PanelTheme,
}

impl EventEmitter<PanelEvent> for UsagePanel {}

impl UsagePanel {
    pub fn new(theme: PanelTheme) -> Self {
        Self {
            state: PanelState::default(),
            theme,
        }
    }

    pub fn state(&self) -> &PanelState {
        &self.state
    }

    pub fn begin_refresh(&mut self, cx: &mut Context<Self>) {
        self.state.begin_refresh();
        cx.notify();
    }

    pub fn apply(&mut self, result: Result<UsageSnapshot, CollectError>, cx: &mut Context<Self>) {
        self.state.apply(result);
        cx.notify();
    }

    pub fn set_theme(&mut self, theme: PanelTheme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }
}

impl Render for UsagePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = present(&self.state, Timestamp::now(), &TimeZone::system());
        let theme = self.theme;
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
            .child(header(&view, &theme))
            .child(body(&view, &theme))
            .children((!view.money.is_empty()).then(|| money(&view, &theme)))
            .child(footer(&view, &theme, cx))
    }
}

fn header(view: &PanelView, theme: &PanelTheme) -> impl IntoElement {
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
                        .text_size(px(16.0))
                        .font_weight(FontWeight::SEMIBOLD)
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

fn body(view: &PanelView, theme: &PanelTheme) -> impl IntoElement {
    let container = div().flex().flex_col().gap(px(14.0));
    match &view.body {
        Body::Loading => container.child(
            div()
                .text_color(theme.muted)
                .child(format!("Loading {} usage…", view.title)),
        ),
        Body::Message(message) => container.child(
            div()
                .p(px(12.0))
                .rounded(px(8.0))
                .bg(theme.surface)
                .border_1()
                .border_color(theme.border)
                .line_height(relative(1.4))
                .child(SharedString::from(message.clone())),
        ),
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

fn footer(view: &PanelView, theme: &PanelTheme, cx: &mut Context<UsagePanel>) -> impl IntoElement {
    let button = div()
        .id("tokenbubble-refresh")
        .flex_none()
        .px(px(10.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.border)
        .text_size(px(12.0));
    let button = if view.refreshing {
        button.text_color(theme.muted).child("Refreshing…")
    } else {
        let hover = theme.surface;
        button
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .child("Refresh")
            .on_click(cx.listener(|_, _, _, cx| cx.emit(PanelEvent::RefreshRequested)))
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
        .child(
            div().min_w_0().text_size(px(12.0)).children(
                view.status
                    .clone()
                    .map(|(text, tone)| div().text_color(theme.tone(tone)).child(text)),
            ),
        )
        .child(button)
}
