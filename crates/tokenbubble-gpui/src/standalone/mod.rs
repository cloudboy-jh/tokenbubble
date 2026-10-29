//! Standalone notification-area host on Windows and menu-bar host on macOS.
//! Both native icons open the shared GPUI panel and settings.

#[cfg(target_os = "macos")]
mod macos;
mod popup;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
use macos as native_tray;
#[cfg(windows)]
use windows as native_tray;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Bounds, Context, DisplayId, Entity, QuitMode, Subscription, Task,
    TitlebarOptions, WindowBounds, WindowHandle, WindowId, WindowKind, WindowOptions, point, px,
    size,
};
use jiff::Timestamp;
use tokenbubble_core::config::Settings;
use tokenbubble_core::model::{ProviderId, UsageSnapshot};
use tokenbubble_core::providers::CollectError;
use tokenbubble_core::providers::claude::{self, ClaudeSource};
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
    #[cfg(target_os = "macos")]
    let reopen_host = std::rc::Rc::new(std::cell::RefCell::new(None::<Entity<Host>>));
    #[cfg(target_os = "macos")]
    {
        let reopen_host = reopen_host.clone();
        application.on_reopen(move |cx| {
            if let Some(host) = reopen_host.borrow().as_ref() {
                host.update(cx, |host, cx| host.open_popup(None, cx));
            }
        });
    }
    application.run(move |cx: &mut App| {
        let host = cx.new(|cx| Host::new(settings, settings_path, problem, cx));
        #[cfg(target_os = "macos")]
        reopen_host.replace(Some(host.clone()));
        let closed = host.clone();
        // Windows can close while the host is mid-update (it removes its own flyout), so the
        // bookkeeping runs after the current update instead of re-entering it.
        cx.on_window_closed(move |cx, id| {
            let closed = closed.clone();
            cx.defer(move |cx| closed.update(cx, |host, cx| host.window_closed(id, cx)));
        })
        .detach();
        #[cfg(any(windows, target_os = "macos"))]
        if start_tray(host.clone(), cx) {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            // A menu-bar host without a status item would become an invisible process.
            cx.quit();
            return;
        }
        #[cfg(not(target_os = "macos"))]
        {
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
        }
    });
}

/// Returns false when the tray icon could not be created.
#[cfg(any(windows, target_os = "macos"))]
fn start_tray(host: Entity<Host>, cx: &mut App) -> bool {
    let (sender, events) = async_channel::unbounded();
    let tray = match native_tray::Tray::create(sender) {
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
    shown: Option<(String, u32)>,
}

#[cfg(target_os = "macos")]
struct TrayState {
    tray: macos::Tray,
    shown: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CollectionRequest {
    provider: ProviderId,
    generation: u64,
}

struct ProviderSchedule {
    schedule: Schedule,
    in_flight: Option<u64>,
    last_success: Option<Instant>,
}

/// One request per provider; selection generations prevent A → B → A stale completions.
/// Schedules stay with their provider so switching cannot bypass a rate-limit cooldown.
struct Collections {
    selected: ProviderId,
    generation: u64,
    providers: [ProviderSchedule; 2],
    pending_selection: bool,
}

impl Collections {
    fn new(selected: ProviderId, interval: Option<Duration>) -> Self {
        Self {
            selected,
            generation: 0,
            providers: ProviderId::ALL.map(|_| ProviderSchedule {
                schedule: Schedule::new(interval),
                in_flight: None,
                last_success: None,
            }),
            pending_selection: true,
        }
    }

    fn index(provider: ProviderId) -> usize {
        match provider {
            ProviderId::Codex => 0,
            ProviderId::Claude => 1,
        }
    }

    fn current(&self) -> &ProviderSchedule {
        &self.providers[Self::index(self.selected)]
    }

    fn select(&mut self, provider: ProviderId) {
        if self.selected != provider {
            self.selected = provider;
            self.generation = self.generation.wrapping_add(1);
            self.pending_selection = true;
        }
    }

    fn set_interval(&mut self, interval: Option<Duration>) {
        for provider in &mut self.providers {
            provider.schedule.set_interval(interval);
        }
    }

    fn is_due(&self, now: Instant) -> bool {
        self.current().schedule.is_due(now)
            || (self.pending_selection && self.current().schedule.may_start(now))
    }

    fn start(&mut self, now: Instant) -> Option<CollectionRequest> {
        let current = &mut self.providers[Self::index(self.selected)];
        if current.in_flight.is_some() || !current.schedule.may_start(now) {
            return None;
        }
        current.in_flight = Some(self.generation);
        current.schedule.started(now);
        self.pending_selection = false;
        Some(CollectionRequest {
            provider: self.selected,
            generation: self.generation,
        })
    }

    /// Always record cooldowns, but only publish a result for the current selection.
    fn finish(
        &mut self,
        request: CollectionRequest,
        result: &Result<UsageSnapshot, CollectError>,
        now: Instant,
    ) -> bool {
        let provider = &mut self.providers[Self::index(request.provider)];
        if provider.in_flight != Some(request.generation) {
            return false;
        }
        provider.in_flight = None;
        match result {
            Ok(_) => {
                provider.schedule.succeeded();
                provider.last_success = Some(now);
            }
            Err(error) => provider.schedule.failed(now, error),
        }
        let current = request.provider == self.selected && request.generation == self.generation;
        if request.provider == self.selected && !current {
            self.pending_selection = true;
        }
        current
    }
}

struct Host {
    settings: Settings,
    settings_path: Option<PathBuf>,
    settings_problem: Option<String>,
    panel: Entity<UsagePanel>,
    collections: Collections,
    last_redraw: Instant,
    popup: Option<WindowHandle<PopupView>>,
    popup_closed_at: Option<Instant>,
    settings_window: Option<(WindowHandle<SettingsView>, Entity<SettingsView>)>,
    /// No tray: the panel is an ordinary window.
    windowed: bool,
    #[cfg(any(windows, target_os = "macos"))]
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
        let panel = cx.new(|_| UsagePanel::new(settings.provider, PanelTheme::dark(), options));
        let subscriptions = vec![
            cx.subscribe(&panel, |host, _, event, cx| match event {
                PanelEvent::RefreshRequested => host.refresh(cx),
                PanelEvent::SettingsRequested => host.open_settings(Section::General, cx),
                PanelEvent::ProviderSelected(provider) => {
                    let mut settings = host.settings.clone();
                    settings.provider = *provider;
                    host.apply_settings(settings, cx);
                }
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
            collections: Collections::new(settings.provider, settings.refresh.duration()),
            settings,
            settings_path,
            settings_problem,
            panel,
            last_redraw: Instant::now(),
            popup: None,
            popup_closed_at: None,
            settings_window: None,
            windowed: false,
            #[cfg(any(windows, target_os = "macos"))]
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
        if self.collections.is_due(now) {
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
        let Some(request) = self.collections.start(now) else {
            return;
        };
        self.panel.update(cx, |panel, cx| panel.begin_refresh(cx));
        cx.spawn(async move |host, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { collect(request.provider) })
                .await;
            let _ = host.update(cx, |host, cx| host.finish_refresh(request, result, cx));
        })
        .detach();
    }

    fn finish_refresh(
        &mut self,
        request: CollectionRequest,
        result: Result<UsageSnapshot, CollectError>,
        cx: &mut Context<Self>,
    ) {
        if self.collections.finish(request, &result, Instant::now()) {
            self.panel.update(cx, |panel, cx| panel.apply(result, cx));
        } else if request.provider == self.settings.provider {
            // A → B → A coalesces with the old A request, then fetches a fresh reading.
            self.refresh(cx);
            self.show_cooldown(cx);
        }
    }

    fn open_popup(&mut self, anchor: Option<(i32, i32)>, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        cx.activate(true);
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
        #[cfg(target_os = "macos")]
        let placed = {
            let _ = anchor;
            self.tray
                .as_ref()
                .and_then(|state| state.tray.popup_origin(POPUP_WIDTH, height))
        };
        #[cfg(not(any(windows, target_os = "macos")))]
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
            .collections
            .current()
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
        #[cfg(target_os = "macos")]
        cx.activate(true);
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
                    codex_credentials: CodexSource::from_env()
                        .ok()
                        .map(|source| source.codex_home.join("auth.json").display().to_string()),
                    claude_credentials: ClaudeSource::from_env()
                        .ok()
                        .map(|source| source.credential_path().display().to_string()),
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
        let switched = settings.provider != self.settings.provider;
        self.collections.set_interval(settings.refresh.duration());
        self.collections.select(settings.provider);
        let options = DisplayOptions::from(&settings);
        self.panel
            .update(cx, |panel, cx| panel.set_options(options, cx));
        if switched {
            self.panel
                .update(cx, |panel, cx| panel.set_provider(settings.provider, cx));
        }
        let problem = match &self.settings_path {
            Some(path) => settings
                .save(path)
                .err()
                .map(|error| format!("Couldn't save settings: {error}")),
            None => self.settings_problem.clone(),
        };
        self.settings = settings;
        if let Some((_, view)) = &self.settings_window {
            view.update(cx, |view, cx| view.set_settings(self.settings.clone(), cx));
        }
        if switched {
            self.refresh(cx);
            self.show_cooldown(cx);
        }
        self.report_settings_problem(problem, cx);
        self.sync_tray(cx);
    }

    fn show_cooldown(&mut self, cx: &mut Context<Self>) {
        if let Some(remaining) = self
            .collections
            .current()
            .schedule
            .cooldown_remaining(Instant::now())
        {
            self.panel.update(cx, |panel, cx| {
                panel.apply(
                    Err(CollectError::RateLimited {
                        retry_after: Some(remaining),
                    }),
                    cx,
                );
            });
        }
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

    #[cfg(target_os = "macos")]
    fn tray_event(&mut self, event: macos::TrayEvent, cx: &mut Context<Self>) {
        match event {
            macos::TrayEvent::Toggle => self.toggle_popup(None, cx),
            macos::TrayEvent::Dismiss => self.close_popup(cx),
            macos::TrayEvent::Refresh => self.refresh(cx),
            macos::TrayEvent::Settings => self.open_settings(Section::General, cx),
            macos::TrayEvent::About => self.open_settings(Section::About, cx),
            macos::TrayEvent::Quit => {
                self.tray = None;
                cx.quit();
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn sync_tray(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &mut self.tray else {
            return;
        };
        let tooltip = crate::presentation::tray_tooltip(
            self.panel.read(cx).state(),
            Timestamp::now(),
            DisplayOptions::from(&self.settings),
        );
        if state.shown.as_ref() != Some(&tooltip) {
            state.tray.set_tooltip(&tooltip);
            state.shown = Some(tooltip);
        }
    }

    /// Update the tooltip when readings change; the bubbles icon stays recognizable.
    #[cfg(windows)]
    fn sync_tray(&mut self, cx: &mut Context<Self>) {
        use crate::presentation::tray_tooltip;
        let Some(state) = &mut self.tray else {
            return;
        };
        let options = DisplayOptions::from(&self.settings);
        let panel_state = self.panel.read(cx).state();
        let tooltip = tray_tooltip(panel_state, Timestamp::now(), options);
        let icon_size = windows::Tray::icon_size();
        let next = (tooltip, icon_size);
        if state.shown.as_ref() == Some(&next) {
            return;
        }
        match state.tray.set(icon_size, &next.0) {
            Ok(()) => state.shown = Some(next),
            Err(error) => eprintln!("TokenBubble could not update its tray icon: {error}"),
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn sync_tray(&mut self, _: &mut Context<Self>) {}
}

fn start_at_login() -> Option<bool> {
    #[cfg(windows)]
    return Some(windows::start_at_login());
    #[cfg(not(windows))]
    None
}

/// Blocking; runs on the background executor.
fn collect(provider: ProviderId) -> Result<UsageSnapshot, CollectError> {
    let transport = HttpTransport::default();
    let now = Timestamp::now();
    match provider {
        ProviderId::Codex => codex::collect(&CodexSource::from_env()?, &transport, now),
        ProviderId::Claude => claude::collect(&ClaudeSource::from_env()?, &transport, now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure() -> Result<UsageSnapshot, CollectError> {
        Err(CollectError::NoUsageData)
    }

    #[test]
    fn concurrent_refresh_requests_are_coalesced_per_provider() {
        let now = Instant::now();
        let mut collections = Collections::new(ProviderId::Codex, None);
        let codex = collections.start(now).unwrap();
        assert!(collections.start(now).is_none());
        collections.select(ProviderId::Claude);
        let claude = collections.start(now).unwrap();
        assert!(collections.start(now).is_none());
        assert!(!collections.finish(codex, &failure(), now));
        assert!(collections.finish(claude, &failure(), now));
    }

    #[test]
    fn rapid_round_trip_ignores_old_completion_and_requests_a_fresh_reading() {
        let now = Instant::now();
        let mut collections = Collections::new(ProviderId::Codex, None);
        let old_codex = collections.start(now).unwrap();
        collections.select(ProviderId::Claude);
        let claude = collections.start(now).unwrap();
        collections.select(ProviderId::Codex);
        assert!(
            collections.start(now).is_none(),
            "old Codex request is still running"
        );
        assert!(!collections.finish(old_codex, &failure(), now));
        assert!(
            collections.is_due(now),
            "selection refresh also runs in manual mode"
        );
        let new_codex = collections.start(now).unwrap();
        assert_ne!(old_codex, new_codex);
        assert!(!collections.finish(claude, &failure(), now));
        assert!(collections.finish(new_codex, &failure(), now));
        assert!(!collections.is_due(now));
    }

    #[test]
    fn stale_rate_limit_survives_switching_and_does_not_block_the_other_provider() {
        let now = Instant::now();
        let wait = Duration::from_secs(120);
        let rate_limit = Err(CollectError::RateLimited {
            retry_after: Some(wait),
        });
        let mut collections = Collections::new(ProviderId::Codex, None);
        let codex = collections.start(now).unwrap();
        collections.select(ProviderId::Claude);
        assert!(!collections.finish(codex, &rate_limit, now));
        assert!(
            collections.start(now).is_some(),
            "Claude has its own cooldown"
        );
        collections.select(ProviderId::Codex);
        assert!(collections.start(now).is_none());
        assert!(!collections.is_due(now));
        assert!(collections.is_due(now + wait));
        assert!(collections.start(now + wait).is_some());
    }

    #[test]
    fn current_rate_limit_and_interval_changes_preserve_cooldown() {
        let now = Instant::now();
        let wait = Duration::from_secs(120);
        let rate_limit = Err(CollectError::RateLimited {
            retry_after: Some(wait),
        });
        let mut collections = Collections::new(ProviderId::Claude, None);
        let claude = collections.start(now).unwrap();
        assert!(collections.finish(claude, &rate_limit, now));
        collections.select(ProviderId::Codex);
        collections.set_interval(Some(Duration::from_secs(60)));
        collections.select(ProviderId::Claude);
        assert!(collections.start(now + Duration::from_secs(119)).is_none());
        assert!(collections.start(now + wait).is_some());
    }
}
