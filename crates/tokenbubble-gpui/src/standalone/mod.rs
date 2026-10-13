//! Standalone host. On Windows TokenBubble lives in the notification area: the tray icon
//! shows a two-bar meter and opens the shared panel as a flyout. Elsewhere, until the
//! macOS menu-bar host exists, the panel opens in a plain window.

mod popup;
#[cfg(windows)]
mod windows;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Bounds, Context, DisplayId, Entity, QuitMode, Subscription, Task,
    TitlebarOptions, WindowBounds, WindowHandle, WindowId, WindowKind, WindowOptions, point, px,
    size,
};
use jiff::Timestamp;
use tokenbubble_core::config::Settings;
use tokenbubble_core::model::UsageSnapshot;
use tokenbubble_core::providers::CollectError;
use tokenbubble_core::providers::codex::{self, CodexSource};
use tokenbubble_core::service::Schedule;
use tokenbubble_core::transport::HttpTransport;

use crate::panel::{PanelEvent, UsagePanel};
use crate::presentation::DisplayOptions;
use crate::settings_view::{Section, SettingsEvent, SettingsInit, SettingsView};
use crate::theme::PanelTheme;
use popup::PopupView;

/// How often the host checks its schedule.
const TICK: Duration = Duration::from_secs(5);
/// Relative times ("2m ago", "resets in 3h") are redrawn at this pace while running.
const REDRAW: Duration = Duration::from_secs(30);
/// A reading younger than this is not refetched when the panel opens.
const FRESH_ON_OPEN: Duration = Duration::from_secs(60);
/// A tray click that arrives this soon after the popup closed is the click that closed it.
const REOPEN_GUARD: Duration = Duration::from_millis(300);
const POPUP_WIDTH: f32 = 340.0;

pub fn run() {
    #[cfg(windows)]
    let Some(_instance) = windows::claim_instance() else {
        return;
    };
    let settings_path = settings_path();
    let (settings, problem) = match settings_path.as_deref().map(Settings::load) {
        Some(Ok(settings)) => (settings, None),
        Some(Err(error)) => (
            Settings::default(),
            Some(format!("{error}; using defaults")),
        ),
        None => (
            Settings::default(),
            Some("No settings directory; changes won't be saved".to_owned()),
        ),
    };

    // The tray app keeps running with no windows; quitting is always explicit.
    let application = gpui_platform::application().with_quit_mode(QuitMode::Explicit);
    application.run(move |cx: &mut App| {
        let host = cx.new(|cx| Host::new(settings, settings_path, problem, cx));
        let closed = host.clone();
        // Windows can close while the host is mid-update (it removes its own flyout), so the
        // bookkeeping runs after the current update instead of re-entering it.
        cx.on_window_closed(move |cx, id| {
            let closed = closed.clone();
            cx.defer(move |cx| closed.update(cx, |host, cx| host.window_closed(id, cx)));
        })
        .detach();
        #[cfg(windows)]
        if start_tray(host.clone(), cx) {
            return;
        }
        // No tray: a plain window, and quitting when it closes.
        host.update(cx, |host, cx| {
            host.windowed = true;
            host.open_popup(None, cx);
        });
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}

/// Returns false when the tray icon could not be created.
#[cfg(windows)]
fn start_tray(host: Entity<Host>, cx: &mut App) -> bool {
    let (sender, events) = async_channel::unbounded();
    let tray = match windows::Tray::create(sender) {
        Ok(tray) => tray,
        Err(error) => {
            eprintln!("TokenBubble could not create its tray icon: {error}");
            return false;
        }
    };
    host.update(cx, |host, cx| {
        host.tray = Some(TrayState { tray, shown: None });
        host.sync_tray(cx);
    });
    // Owns a strong handle: the host lives as long as the tray does.
    cx.spawn(async move |cx| {
        while let Ok(event) = events.recv().await {
            host.update(cx, |host, cx| host.tray_event(event, cx));
        }
    })
    .detach();
    true
}

fn settings_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::home_dir().map(|home| home.join("Library").join("Application Support"));
    base.map(|base| base.join("TokenBubble").join("settings.toml"))
}

#[cfg(windows)]
struct TrayState {
    tray: windows::Tray,
    /// What the icon currently shows, to skip redundant updates.
    shown: Option<(crate::presentation::TrayMeter, String, bool, u32)>,
}

struct Host {
    settings: Settings,
    settings_path: Option<PathBuf>,
    settings_problem: Option<String>,
    panel: Entity<UsagePanel>,
    schedule: Schedule,
    in_flight: bool,
    last_success: Option<Instant>,
    last_redraw: Instant,
    popup: Option<WindowHandle<PopupView>>,
    popup_closed_at: Option<Instant>,
    settings_window: Option<(WindowHandle<SettingsView>, Entity<SettingsView>)>,
    /// No tray: the panel is an ordinary window.
    windowed: bool,
    #[cfg(windows)]
    tray: Option<TrayState>,
    _subscriptions: Vec<Subscription>,
    _ticker: Task<()>,
}

impl Host {
    fn new(
        settings: Settings,
        settings_path: Option<PathBuf>,
        settings_problem: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let options = DisplayOptions::from(&settings);
        let panel = cx.new(|_| UsagePanel::new(PanelTheme::dark(), options));
        let subscriptions = vec![
            cx.subscribe(&panel, |host, _, event, cx| match event {
                PanelEvent::RefreshRequested => host.refresh(cx),
                PanelEvent::SettingsRequested => host.open_settings(Section::General, cx),
            }),
            cx.observe(&panel, |host, _, cx| host.sync_tray(cx)),
        ];
        let ticker = cx.spawn(async move |host, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if host.update(cx, |host, cx| host.tick(cx)).is_err() {
                    break;
                }
            }
        });
        let mut host = Self {
            schedule: Schedule::new(settings.refresh.duration()),
            settings,
            settings_path,
            settings_problem,
            panel,
            in_flight: false,
            last_success: None,
            last_redraw: Instant::now(),
            popup: None,
            popup_closed_at: None,
            settings_window: None,
            windowed: false,
            #[cfg(windows)]
            tray: None,
            _subscriptions: subscriptions,
            _ticker: ticker,
        };
        // The first reading is wanted even in manual mode.
        host.refresh(cx);
        host
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self.schedule.is_due(now) {
            self.refresh(cx);
        }
        if now.duration_since(self.last_redraw) >= REDRAW {
            self.last_redraw = now;
            self.panel.update(cx, |_, cx| cx.notify());
        }
    }

    /// Start one collection unless one is running or the provider asked us to wait.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self.in_flight || !self.schedule.may_start(now) {
            return;
        }
        self.in_flight = true;
        self.schedule.started(now);
        self.panel.update(cx, |panel, cx| panel.begin_refresh(cx));
        cx.spawn(async move |host, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { collect_codex() })
                .await;
            let _ = host.update(cx, |host, cx| host.finish_refresh(result, cx));
        })
        .detach();
    }

    fn finish_refresh(
        &mut self,
        result: Result<UsageSnapshot, CollectError>,
        cx: &mut Context<Self>,
    ) {
        self.in_flight = false;
        match &result {
            Ok(_) => {
                self.schedule.succeeded();
                self.last_success = Some(Instant::now());
            }
            Err(error) => self.schedule.failed(Instant::now(), error),
        }
        self.panel.update(cx, |panel, cx| panel.apply(result, cx));
    }

    fn open_popup(&mut self, anchor: Option<(i32, i32)>, cx: &mut Context<Self>) {
        if let Some(popup) = self.popup {
            let _ = popup.update(cx, |_, window, _| window.activate_window());
            return;
        }
        let panel = self.panel.read(cx);
        let height = if panel.state().snapshot.is_none() && panel.state().error.is_none() {
            300.0
        } else {
            panel.preferred_height().clamp(180.0, 640.0)
        };
        let popup_size = size(px(POPUP_WIDTH), px(height));
        let flyout = !self.windowed;
        #[cfg(windows)]
        let placed = flyout
            .then(|| windows::popup_origin(anchor, POPUP_WIDTH, height))
            .flatten();
        #[cfg(not(windows))]
        let placed: Option<(u64, f32, f32)> = {
            let _ = anchor;
            None
        };
        let (display_id, bounds) = match placed {
            Some((display, x, y)) => (
                Some(DisplayId::new(display)),
                Bounds {
                    origin: point(px(x), px(y)),
                    size: popup_size,
                },
            ),
            None => (None, Bounds::centered(None, popup_size, cx)),
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: (!flyout).then(|| TitlebarOptions {
                title: Some("TokenBubble".into()),
                ..Default::default()
            }),
            kind: if flyout {
                WindowKind::PopUp
            } else {
                WindowKind::Normal
            },
            focus: true,
            is_movable: !flyout,
            is_resizable: !flyout,
            is_minimizable: !flyout,
            display_id,
            app_id: Some("tokenbubble".to_owned()),
            ..Default::default()
        };
        let panel = self.panel.clone();
        match cx.open_window(options, move |window, cx| {
            cx.new(|cx| PopupView::new(panel, flyout, window, cx))
        }) {
            Ok(popup) => {
                #[cfg(windows)]
                let _ = popup.update(cx, |_, window, _| windows::bring_to_front(window));
                self.popup = Some(popup);
            }
            Err(error) => eprintln!("TokenBubble could not open its panel: {error}"),
        }
        let stale = self
            .last_success
            .is_none_or(|success| success.elapsed() >= FRESH_ON_OPEN);
        if self.settings.refresh_on_open && stale {
            self.refresh(cx);
        }
    }

    fn close_popup(&mut self, cx: &mut Context<Self>) {
        if let Some(popup) = self.popup.take() {
            let _ = popup.update(cx, |_, window, _| window.remove_window());
            self.popup_closed_at = Some(Instant::now());
        }
    }

    fn toggle_popup(&mut self, anchor: Option<(i32, i32)>, cx: &mut Context<Self>) {
        if self.popup.is_some() {
            self.close_popup(cx);
        } else if self
            .popup_closed_at
            .is_some_and(|closed| closed.elapsed() < REOPEN_GUARD)
        {
            // Clicking the icon deactivated the flyout first; this click already closed it.
        } else {
            self.open_popup(anchor, cx);
        }
    }

    fn open_settings(&mut self, section: Section, cx: &mut Context<Self>) {
        if let Some((window, view)) = &self.settings_window {
            view.update(cx, |view, cx| view.show_section(section, cx));
            let _ = window.update(cx, |_, window, _| window.activate_window());
            return;
        }
        self.close_popup(cx);
        let view = cx.new(|cx| {
            let mut view = SettingsView::new(
                SettingsInit {
                    settings: self.settings.clone(),
                    panel: self.panel.clone(),
                    theme: PanelTheme::dark(),
                    start_at_login: start_at_login(),
                    settings_location: self
                        .settings_path
                        .as_ref()
                        .map(|path| path.display().to_string()),
                    settings_problem: self.settings_problem.clone(),
                    codex_home: CodexSource::from_env()
                        .ok()
                        .map(|source| source.codex_home.display().to_string()),
                },
                cx,
            );
            view.show_section(section, cx);
            view
        });
        self._subscriptions
            .push(cx.subscribe(&view, |host, _, event, cx| host.settings_event(event, cx)));
        let bounds = Bounds::centered(None, size(px(640.0), px(460.0)), cx);
        let root = view.clone();
        match cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("TokenBubble Settings".into()),
                    ..Default::default()
                }),
                focus: true,
                window_min_size: Some(size(px(520.0), px(360.0))),
                app_id: Some("tokenbubble".to_owned()),
                ..Default::default()
            },
            move |_, _| root,
        ) {
            Ok(window) => self.settings_window = Some((window, view)),
            Err(error) => eprintln!("TokenBubble could not open settings: {error}"),
        }
    }

    fn settings_event(&mut self, event: &SettingsEvent, cx: &mut Context<Self>) {
        match event {
            SettingsEvent::Changed(settings) => self.apply_settings(settings.clone(), cx),
            SettingsEvent::RefreshRequested => self.refresh(cx),
            SettingsEvent::StartAtLoginChanged(enabled) => {
                #[cfg(windows)]
                if let Err(error) = windows::set_start_at_login(*enabled) {
                    self.report_settings_problem(
                        Some(format!("Couldn't change sign-in launch: {error}")),
                        cx,
                    );
                }
                #[cfg(not(windows))]
                let _ = enabled;
                if let Some((_, view)) = &self.settings_window {
                    view.update(cx, |view, cx| view.set_start_at_login(start_at_login(), cx));
                }
            }
        }
    }

    fn apply_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        self.schedule.set_interval(settings.refresh.duration());
        let options = DisplayOptions::from(&settings);
        self.panel
            .update(cx, |panel, cx| panel.set_options(options, cx));
        let problem = match &self.settings_path {
            Some(path) => settings
                .save(path)
                .err()
                .map(|error| format!("Couldn't save settings: {error}")),
            None => self.settings_problem.clone(),
        };
        self.settings = settings;
        self.report_settings_problem(problem, cx);
        self.sync_tray(cx);
    }

    fn report_settings_problem(&mut self, problem: Option<String>, cx: &mut Context<Self>) {
        self.settings_problem = problem.clone();
        if let Some((_, view)) = &self.settings_window {
            view.update(cx, |view, cx| view.set_settings_problem(problem, cx));
        }
    }

    fn window_closed(&mut self, id: WindowId, _: &mut Context<Self>) {
        if self.popup.is_some_and(|popup| popup.window_id() == id) {
            self.popup = None;
            self.popup_closed_at = Some(Instant::now());
        }
        if self
            .settings_window
            .as_ref()
            .is_some_and(|(window, _)| window.window_id() == id)
        {
            self.settings_window = None;
        }
    }

    #[cfg(windows)]
    fn tray_event(&mut self, event: windows::TrayEvent, cx: &mut Context<Self>) {
        use windows::TrayEvent;
        match event {
            TrayEvent::Toggle { anchor } => self.toggle_popup(Some(anchor), cx),
            TrayEvent::Refresh => self.refresh(cx),
            TrayEvent::Settings => self.open_settings(Section::General, cx),
            TrayEvent::About => self.open_settings(Section::About, cx),
            TrayEvent::Reveal => {
                let anchor = self.tray.as_ref().and_then(|state| state.tray.anchor());
                self.open_popup(anchor, cx);
            }
            TrayEvent::TaskbarCreated => {
                if let Some(state) = &self.tray
                    && let Err(error) = state.tray.restore()
                {
                    eprintln!("TokenBubble could not restore its tray icon: {error}");
                }
            }
            TrayEvent::SettingsChanged => {
                if let Some(state) = &mut self.tray {
                    state.shown = None;
                }
                self.sync_tray(cx);
            }
            TrayEvent::Quit => {
                // Removes the icon before the process exits.
                self.tray = None;
                cx.quit();
            }
        }
    }

    /// Redraw the tray icon and tooltip when what they show changed.
    #[cfg(windows)]
    fn sync_tray(&mut self, cx: &mut Context<Self>) {
        use crate::presentation::{tray_meter, tray_tooltip};
        let Some(state) = &mut self.tray else {
            return;
        };
        let options = DisplayOptions::from(&self.settings);
        let panel_state = self.panel.read(cx).state();
        let meter = tray_meter(panel_state, options);
        let tooltip = tray_tooltip(panel_state, Timestamp::now(), options);
        let light = windows::taskbar_is_light();
        let icon_size = windows::Tray::icon_size();
        let next = (meter, tooltip, light, icon_size);
        if state.shown.as_ref() == Some(&next) {
            return;
        }
        let pixels = crate::meter_icon::render(icon_size, &next.0, light);
        match state.tray.set(&pixels, icon_size, &next.1) {
            Ok(()) => state.shown = Some(next),
            Err(error) => eprintln!("TokenBubble could not update its tray icon: {error}"),
        }
    }

    #[cfg(not(windows))]
    fn sync_tray(&mut self, _: &mut Context<Self>) {}
}

fn start_at_login() -> Option<bool> {
    #[cfg(windows)]
    return Some(windows::start_at_login());
    #[cfg(not(windows))]
    None
}

/// Blocking; runs on the background executor.
fn collect_codex() -> Result<UsageSnapshot, CollectError> {
    let source = CodexSource::from_env()?;
    codex::collect(&source, &HttpTransport::default(), Timestamp::now())
}
