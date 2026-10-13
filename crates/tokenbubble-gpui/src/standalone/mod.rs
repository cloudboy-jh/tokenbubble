//! Standalone host: one window showing the panel, with its own collection schedule.
//! The Windows tray icon replaces the plain window once its integration spike lands.

use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement, Render, Styled,
    Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, div, px, size,
};
use jiff::Timestamp;
use tokenbubble_core::model::UsageSnapshot;
use tokenbubble_core::providers::CollectError;
use tokenbubble_core::providers::codex::{self, CodexSource};
use tokenbubble_core::transport::HttpTransport;

use crate::panel::{PanelEvent, UsagePanel};
use crate::theme::PanelTheme;

/// Background collection interval.
const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Redraw cadence for relative times; never triggers a request by itself.
const TICK: Duration = Duration::from_secs(30);

pub fn run() {
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(340.0), px(380.0)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("TokenBubble".into()),
                    ..Default::default()
                }),
                focus: true,
                window_min_size: Some(size(px(280.0), px(240.0))),
                app_id: Some("tokenbubble".to_owned()),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| Host::new(PanelTheme::dark(), cx)),
        );
        if let Err(error) = opened {
            eprintln!("Could not open TokenBubble: {error}");
            cx.quit();
            return;
        }
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}

struct Host {
    panel: Entity<UsagePanel>,
    /// Coalesces refresh requests: at most one collection runs at a time.
    in_flight: bool,
    last_attempt: Option<Instant>,
    _events: Subscription,
    _ticker: Task<()>,
}

impl Host {
    fn new(theme: PanelTheme, cx: &mut Context<Self>) -> Self {
        let panel = cx.new(|_| UsagePanel::new(theme));
        let events = cx.subscribe(&panel, |host, _, event, cx| match event {
            PanelEvent::RefreshRequested => host.refresh(cx),
        });
        let ticker = cx.spawn(async move |host, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if host.update(cx, |host, cx| host.tick(cx)).is_err() {
                    break;
                }
            }
        });
        let mut host = Self {
            panel,
            in_flight: false,
            last_attempt: None,
            _events: events,
            _ticker: ticker,
        };
        host.refresh(cx);
        host
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let due = self
            .last_attempt
            .is_none_or(|attempt| attempt.elapsed() >= POLL_INTERVAL);
        if due {
            self.refresh(cx);
        } else {
            self.panel.update(cx, |_, cx| cx.notify());
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.in_flight {
            return;
        }
        self.in_flight = true;
        self.last_attempt = Some(Instant::now());
        self.panel.update(cx, |panel, cx| panel.begin_refresh(cx));
        cx.spawn(async move |host, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { collect_codex() })
                .await;
            let _ = host.update(cx, |host, cx| {
                host.in_flight = false;
                host.panel.update(cx, |panel, cx| panel.apply(result, cx));
            });
        })
        .detach();
    }
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.panel.clone())
    }
}

/// Blocking; runs on the background executor.
fn collect_codex() -> Result<UsageSnapshot, CollectError> {
    let source = CodexSource::from_env()?;
    codex::collect(&source, &HttpTransport::default(), Timestamp::now())
}
