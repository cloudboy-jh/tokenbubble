use std::path::PathBuf;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use tokenbubble_core::config::{BarFill, ResetDisplay};
use tokenbubble_core::model::{
    Account, Credits, ProviderId, QuotaWindow, SpendLimit, UsageSnapshot, WindowKind,
};
use tokenbubble_core::providers::{AuthReason, CollectError};
use tokenbubble_core::transport::TransportError;
use tokenbubble_gpui::presentation::{
    BarView, Body, DisplayOptions, MoneyRow, PanelState, Tone, plan_name, present, reset_text,
    tray_tooltip,
};

/// 2026-10-13T04:00:00Z, a Tuesday.
fn now() -> Timestamp {
    "2026-10-13T04:00:00Z".parse().unwrap()
}

fn utc() -> TimeZone {
    TimeZone::UTC
}

fn defaults() -> DisplayOptions {
    DisplayOptions::default()
}

fn window(id: &str, kind: WindowKind, secs: Option<u64>, used: f64) -> QuotaWindow {
    QuotaWindow {
        id: id.to_owned(),
        scope: None,
        kind,
        duration_secs: secs,
        used_percent: used,
        resets_at: None,
    }
}

fn weekly(used: f64) -> QuotaWindow {
    window("weekly", WindowKind::Weekly, Some(604_800), used)
}

fn snapshot(windows: Vec<QuotaWindow>) -> UsageSnapshot {
    UsageSnapshot {
        provider: ProviderId::Codex,
        measured_at: "2026-10-13T03:58:00Z".parse().unwrap(),
        account: Account {
            id: Some("acct".into()),
            email: Some("user@example.com".into()),
            plan: Some("pro".into()),
        },
        windows,
        credits: None,
        spend_limit: None,
        issues: Vec::new(),
    }
}

fn ready(snapshot: UsageSnapshot) -> PanelState {
    let mut state = PanelState {
        provider: snapshot.provider,
        ..PanelState::default()
    };
    state.apply(Ok(snapshot));
    state
}

fn bars(state: &PanelState, options: DisplayOptions) -> Vec<BarView> {
    match present(state, now(), &utc(), options).body {
        Body::Bars(bars) => bars,
        other => panic!("expected bars, got {other:?}"),
    }
}

#[test]
fn labels_and_values_keep_raw_percentages_but_clamp_the_fill() {
    let mut spark = window(
        "codex-spark-session",
        WindowKind::Session,
        Some(18_000),
        2.5,
    );
    spark.scope = Some("GPT-5.3-Codex-Spark".into());
    let state = ready(snapshot(vec![
        window("session", WindowKind::Session, Some(18_000), 0.0),
        weekly(137.5),
        window("window-3600s", WindowKind::Other, Some(3_600), 80.0),
        window("window", WindowKind::Other, None, 10.0),
        spark,
    ]));
    let bars = bars(&state, defaults());

    let summary: Vec<_> = bars
        .iter()
        .map(|bar| (bar.label.as_str(), bar.value.as_str(), bar.tone))
        .collect();
    assert_eq!(
        summary,
        [
            ("Session", "0% used", Tone::Normal),
            ("Weekly", "137.5% used · over limit", Tone::Critical),
            ("1-hour window", "80% used", Tone::Warning),
            ("Usage window", "10% used", Tone::Normal),
            ("GPT-5.3-Codex-Spark · Session", "2.5% used", Tone::Normal),
        ]
    );
    assert_eq!(bars[0].fill, 0.0, "zero is a real reading");
    assert_eq!(bars[1].fill, 1.0);
}

#[test]
fn remaining_mode_inverts_values_and_fill_but_not_warning_tones() {
    let state = ready(snapshot(vec![weekly(80.0), weekly(137.5)]));
    let options = DisplayOptions {
        bar_fill: BarFill::Remaining,
        ..defaults()
    };
    let bars = bars(&state, options);
    assert_eq!(bars[0].value, "20% left");
    assert!((bars[0].fill - 0.2).abs() < 1e-6);
    assert_eq!(bars[0].tone, Tone::Warning, "tone follows consumption");
    assert_eq!(bars[1].value, "0% left · over limit");
    assert_eq!(bars[1].fill, 0.0);
}

#[test]
fn reset_text_uses_viewer_time_zone_and_chosen_format() {
    let reset: Timestamp = "2026-10-18T19:44:40Z".parse().unwrap();
    let both = ResetDisplay::Both;
    assert_eq!(
        reset_text(reset, now(), &utc(), both),
        "Resets in 5d 15h · Sun Oct 18 7:44 PM"
    );
    let pacific = TimeZone::get("America/Los_Angeles").unwrap();
    assert_eq!(
        reset_text(reset, now(), &pacific, both),
        "Resets in 5d 15h · Sun Oct 18 12:44 PM"
    );
    assert_eq!(
        reset_text(reset, now(), &utc(), ResetDisplay::Countdown),
        "Resets in 5d 15h"
    );
    assert_eq!(
        reset_text(reset, now(), &utc(), ResetDisplay::Clock),
        "Resets Sun Oct 18 7:44 PM"
    );

    // "Now" is Oct 12 21:00 in Los Angeles, so "today" differs from UTC's calendar day.
    let soon: Timestamp = "2026-10-13T05:05:00Z".parse().unwrap();
    assert_eq!(
        reset_text(soon, now(), &utc(), both),
        "Resets in 1h 5m · today 5:05 AM"
    );
    assert_eq!(
        reset_text(soon, now(), &pacific, both),
        "Resets in 1h 5m · today 10:05 PM"
    );
    let tomorrow: Timestamp = "2026-10-14T01:00:00Z".parse().unwrap();
    assert_eq!(
        reset_text(tomorrow, now(), &utc(), both),
        "Resets in 21h 0m · tomorrow 1:00 AM"
    );
    assert_eq!(reset_text(now(), now(), &utc(), both), "Reset due");
}

#[test]
fn header_and_status_describe_a_fresh_reading() {
    let view = present(
        &ready(snapshot(vec![weekly(1.0)])),
        now(),
        &utc(),
        defaults(),
    );
    assert_eq!(view.title, "Codex");
    assert_eq!(view.plan.as_deref(), Some("Pro"));
    assert_eq!(view.account.as_deref(), Some("user@example.com"));
    assert_eq!(view.status, Some(("Updated 2m ago".into(), Tone::Muted)));
    assert_eq!(plan_name("free_workspace"), "Free workspace");
}

#[test]
fn nothing_collected_yet_is_loading_not_an_empty_meter() {
    let mut state = PanelState::default();
    state.begin_refresh();
    let view = present(&state, now(), &utc(), defaults());
    assert_eq!(view.body, Body::Loading);
    assert!(view.refreshing);
}

#[test]
fn transient_failure_keeps_reading_as_stale() {
    let mut state = ready(snapshot(vec![weekly(1.0)]));
    state.begin_refresh();
    state.apply(Err(CollectError::Transport(TransportError::Timeout)));

    let view = present(&state, now(), &utc(), defaults());
    assert!(matches!(&view.body, Body::Bars(bars) if bars.len() == 1));
    assert_eq!(
        view.status,
        Some((
            "Couldn't refresh (network) · showing data from 2m ago".into(),
            Tone::Warning
        ))
    );
}

#[test]
fn sign_in_failures_clear_the_previous_account_reading() {
    for error in [
        CollectError::AuthRequired {
            reason: AuthReason::Expired,
        },
        CollectError::CredentialsMissing {
            path: PathBuf::from("auth.json"),
        },
    ] {
        let mut state = ready(snapshot(vec![weekly(1.0)]));
        state.apply(Err(error.clone()));
        let view = present(&state, now(), &utc(), defaults());
        assert_eq!(view.account, None, "{error:?}");
        assert!(matches!(view.body, Body::Message { .. }), "{error:?}");
    }
}

#[test]
fn money_rows_show_only_supplied_values_and_respect_the_toggle() {
    let mut reading = snapshot(vec![weekly(1.0)]);
    reading.credits = Some(Credits {
        has_credits: false,
        unlimited: false,
        balance: Some(0.0),
    });
    reading.spend_limit = Some(SpendLimit {
        currency: None,
        limit: Some(200.0),
        used: Some(42.25),
        remaining_percent: None,
        resets_at: None,
    });
    let state = ready(reading.clone());
    assert_eq!(
        present(&state, now(), &utc(), defaults()).money,
        [
            MoneyRow {
                label: "Credits",
                value: "0".into()
            },
            MoneyRow {
                label: "Spend limit",
                value: "42.25 of 200".into()
            },
        ]
    );
    let hidden = DisplayOptions {
        show_credits: false,
        ..defaults()
    };
    assert!(present(&state, now(), &utc(), hidden).money.is_empty());

    reading.credits = Some(Credits {
        has_credits: true,
        unlimited: false,
        balance: None,
    });
    reading.spend_limit = None;
    assert!(
        present(&ready(reading), now(), &utc(), defaults())
            .money
            .is_empty()
    );
}

#[test]
fn tray_tooltip_fits_the_shell_utf16_limit() {
    let mut long = snapshot(vec![weekly(1.0), weekly(2.0)]);
    long.account.plan = Some("x".repeat(200));
    let tooltip = tray_tooltip(&ready(long), now(), defaults());
    assert!(tooltip.encode_utf16().count() <= 127);
}

#[test]
fn selected_provider_controls_loading_errors_and_tooltip_without_a_snapshot() {
    for provider in ProviderId::ALL {
        let mut state = PanelState {
            provider,
            ..PanelState::default()
        };
        state.begin_refresh();
        let loading = present(&state, now(), &utc(), defaults());
        assert_eq!(loading.title, provider.label());
        assert_eq!(loading.body, Body::Loading);
        assert!(tray_tooltip(&state, now(), defaults()).contains(provider.label()));

        state.apply(Err(CollectError::CredentialsMissing {
            path: PathBuf::from("credentials.json"),
        }));
        let view = present(&state, now(), &utc(), defaults());
        assert_eq!(view.title, provider.label());
        let login = match provider {
            ProviderId::Codex => "codex login",
            ProviderId::Claude => "claude auth login",
        };
        assert!(matches!(view.body, Body::Message { text, .. }
            if text.contains(provider.label()) && text.contains(login)));
        assert!(tray_tooltip(&state, now(), defaults()).contains(login));
    }
}

#[test]
fn switching_provider_clears_previous_reading_error_and_refresh_state() {
    let mut state = ready(snapshot(vec![weekly(10.0)]));
    state.apply(Err(CollectError::Transport(TransportError::Timeout)));
    state.begin_refresh();
    state.select_provider(ProviderId::Claude);
    assert_eq!(state.provider, ProviderId::Claude);
    assert!(state.snapshot.is_none());
    assert!(state.error.is_none());
    assert!(!state.refreshing);
    assert_eq!(present(&state, now(), &utc(), defaults()).title, "Claude");

    state.begin_refresh();
    state.apply(Ok(snapshot(vec![weekly(95.0)])));
    assert!(
        state.snapshot.is_none(),
        "a mismatched snapshot cannot leak into the panel"
    );
    assert!(
        state.refreshing,
        "an ignored result cannot finish the selected refresh"
    );

    let mut claude = snapshot(vec![weekly(20.0)]);
    claude.provider = ProviderId::Claude;
    state.apply(Ok(claude));
    assert_eq!(present(&state, now(), &utc(), defaults()).title, "Claude");
    state.select_provider(ProviderId::Codex);
    assert!(state.snapshot.is_none());
    assert_eq!(
        present(&state, now(), &utc(), defaults()).body,
        Body::Loading
    );
}

#[test]
fn selecting_the_current_provider_keeps_its_reading() {
    let mut state = ready(snapshot(vec![weekly(10.0)]));
    let previous = state.clone();
    state.select_provider(ProviderId::Codex);
    assert_eq!(state, previous);
}

#[test]
fn spend_currency_is_explicit_and_credits_remain_unitless() {
    for (currency, expected) in [
        ("USD", "$12.50 of $100.00"),
        ("EUR", "EUR 12.50 of EUR 100.00"),
    ] {
        let mut reading = snapshot(vec![weekly(1.0)]);
        reading.provider = ProviderId::Claude;
        reading.credits = Some(Credits {
            has_credits: true,
            unlimited: false,
            balance: Some(12.5),
        });
        reading.spend_limit = Some(SpendLimit {
            currency: Some(currency.into()),
            limit: Some(100.0),
            used: Some(12.5),
            remaining_percent: None,
            resets_at: None,
        });
        let money = present(&ready(reading), now(), &utc(), defaults()).money;
        assert_eq!(money[0].value, "12.50");
        assert_eq!(money[1].value, expected);
    }
}
