use std::path::PathBuf;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use tokenbubble_core::model::{
    Account, Credits, ProviderId, QuotaWindow, SpendLimit, UsageSnapshot, WindowKind,
};
use tokenbubble_core::providers::{AuthReason, CollectError};
use tokenbubble_core::transport::TransportError;
use tokenbubble_gpui::presentation::{
    Body, MoneyRow, PanelState, Tone, plan_name, present, reset_text,
};

/// 2026-10-13T04:00:00Z, a Tuesday.
fn now() -> Timestamp {
    "2026-10-13T04:00:00Z".parse().unwrap()
}

fn utc() -> TimeZone {
    TimeZone::UTC
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
    let mut state = PanelState::default();
    state.apply(Ok(snapshot));
    state
}

fn bars(state: &PanelState) -> Vec<tokenbubble_gpui::presentation::BarView> {
    match present(state, now(), &utc()).body {
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
        window("weekly", WindowKind::Weekly, Some(604_800), 137.5),
        window("window-3600s", WindowKind::Other, Some(3_600), 80.0),
        window("window", WindowKind::Other, None, 10.0),
        spark,
    ]));
    let bars = bars(&state);

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
fn reset_text_uses_viewer_time_zone() {
    let reset: Timestamp = "2026-10-18T19:44:40Z".parse().unwrap();
    assert_eq!(
        reset_text(reset, now(), &utc()),
        "Resets in 5d 15h · Sun Oct 18 7:44 PM"
    );
    let pacific = TimeZone::get("America/Los_Angeles").unwrap();
    assert_eq!(
        reset_text(reset, now(), &pacific),
        "Resets in 5d 15h · Sun Oct 18 12:44 PM"
    );

    // "Now" is Oct 12 21:00 in Los Angeles, so "today" differs from UTC's calendar day.
    let soon: Timestamp = "2026-10-13T05:05:00Z".parse().unwrap();
    assert_eq!(
        reset_text(soon, now(), &utc()),
        "Resets in 1h 5m · today 5:05 AM"
    );
    assert_eq!(
        reset_text(soon, now(), &pacific),
        "Resets in 1h 5m · today 10:05 PM"
    );
    let tomorrow: Timestamp = "2026-10-14T01:00:00Z".parse().unwrap();
    assert_eq!(
        reset_text(tomorrow, now(), &utc()),
        "Resets in 21h 0m · tomorrow 1:00 AM"
    );
    assert_eq!(reset_text(now(), now(), &utc()), "Reset due");
}

#[test]
fn header_and_status_describe_a_fresh_reading() {
    let view = present(
        &ready(snapshot(vec![window(
            "weekly",
            WindowKind::Weekly,
            Some(604_800),
            1.0,
        )])),
        now(),
        &utc(),
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
    let view = present(&state, now(), &utc());
    assert_eq!(view.body, Body::Loading);
    assert!(view.refreshing);
}

#[test]
fn transient_failure_keeps_reading_as_stale() {
    let mut state = ready(snapshot(vec![window(
        "weekly",
        WindowKind::Weekly,
        Some(604_800),
        1.0,
    )]));
    state.begin_refresh();
    state.apply(Err(CollectError::Transport(TransportError::Timeout)));

    let view = present(&state, now(), &utc());
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
        let mut state = ready(snapshot(vec![window(
            "weekly",
            WindowKind::Weekly,
            Some(604_800),
            1.0,
        )]));
        state.apply(Err(error.clone()));
        let view = present(&state, now(), &utc());
        assert_eq!(view.account, None, "{error:?}");
        assert!(matches!(view.body, Body::Message(_)), "{error:?}");
    }

    let mut state = PanelState::default();
    state.apply(Err(CollectError::AuthRequired {
        reason: AuthReason::Expired,
    }));
    assert_eq!(
        present(&state, now(), &utc()).body,
        Body::Message(
            "Codex sign-in expired. Run “codex” in a terminal to renew it, then refresh.".into()
        )
    );
}

#[test]
fn money_rows_show_only_supplied_values_and_no_currency() {
    let mut reading = snapshot(vec![window(
        "weekly",
        WindowKind::Weekly,
        Some(604_800),
        1.0,
    )]);
    reading.credits = Some(Credits {
        has_credits: false,
        unlimited: false,
        balance: Some(0.0),
    });
    reading.spend_limit = Some(SpendLimit {
        limit: Some(200.0),
        used: Some(42.25),
        remaining_percent: None,
        resets_at: None,
    });
    assert_eq!(
        present(&ready(reading.clone()), now(), &utc()).money,
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

    reading.credits = Some(Credits {
        has_credits: true,
        unlimited: false,
        balance: None,
    });
    reading.spend_limit = None;
    assert!(present(&ready(reading), now(), &utc()).money.is_empty());
}
