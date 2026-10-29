//! The window hosting the shared panel: a notification-area/menu-bar flyout,
//! or an ordinary window when no native host is available.

use gpui::{
    Context, Entity, FocusHandle, IntoElement, KeyDownEvent, ParentElement, Render, Styled,
    Subscription, Window, div, prelude::*,
};

use crate::panel::UsagePanel;

pub struct PopupView {
    panel: Entity<UsagePanel>,
    focus: FocusHandle,
    _activation: Option<Subscription>,
}

impl PopupView {
    /// `dismiss_on_blur` closes the window when it loses activation, like a flyout.
    pub fn new(
        panel: Entity<UsagePanel>,
        dismiss_on_blur: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        #[cfg(target_os = "macos")]
        window.set_window_title("TokenBubble Usage");
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let activation = dismiss_on_blur.then(|| {
            cx.observe_window_activation(window, |_, window, _| {
                if !window.is_window_active() {
                    window.remove_window();
                }
            })
        });
        Self {
            panel,
            focus,
            _activation: activation,
        }
    }
}

impl Render for PopupView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, window, _| {
                if event.keystroke.key == "escape" {
                    window.remove_window();
                }
            }))
            .child(self.panel.clone())
    }
}
