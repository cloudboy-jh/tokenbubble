use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokenbubble_core::providers::{AuthReason, CollectError};
use tokenbubble_core::service::Schedule;
use tokenbubble_core::transport::TransportError;

const MIN: Duration = Duration::from_secs(60);

fn timeout() -> CollectError {
    CollectError::Transport(TransportError::Timeout)
}

#[test]
fn polls_at_the_interval_and_never_in_manual_mode() {
    let t0 = Instant::now();
    let mut schedule = Schedule::new(Some(5 * MIN));
    assert!(schedule.is_due(t0), "first collection is due immediately");

    schedule.started(t0);
    schedule.succeeded();
    assert!(!schedule.is_due(t0 + 4 * MIN));
    assert!(schedule.is_due(t0 + 5 * MIN));

    let manual = Schedule::new(None);
    assert!(!manual.is_due(t0));
    assert!(manual.may_start(t0), "manual refresh is always allowed");
}

#[test]
fn transient_failures_back_off_exponentially_but_never_past_the_interval() {
    let t0 = Instant::now();
    let mut schedule = Schedule::new(Some(5 * MIN));
    let mut now = t0;
    let mut delays = Vec::new();
    for _ in 0..5 {
        schedule.started(now);
        schedule.failed(now, &timeout());
        let retry = (1..=600)
            .map(|secs| now + Duration::from_secs(secs))
            .find(|at| schedule.is_due(*at))
            .unwrap();
        delays.push(retry - now);
        now = retry;
    }
    assert_eq!(
        delays,
        [30, 60, 120, 240, 300].map(Duration::from_secs),
        "30s doubling, capped at the 5 min interval"
    );

    schedule.started(now);
    schedule.succeeded();
    schedule.started(now);
    schedule.failed(now, &timeout());
    assert!(
        schedule.is_due(now + Duration::from_secs(30)),
        "success resets backoff"
    );
}

#[test]
fn rate_limit_blocks_manual_and_automatic_collection_until_cooldown() {
    let t0 = Instant::now();
    let mut schedule = Schedule::new(Some(MIN));
    schedule.started(t0);
    schedule.failed(
        t0,
        &CollectError::RateLimited {
            retry_after: Some(Duration::from_secs(120)),
        },
    );

    assert!(!schedule.may_start(t0 + MIN));
    assert!(
        !schedule.is_due(t0 + MIN),
        "cooldown outranks a shorter interval"
    );
    assert_eq!(schedule.cooldown_remaining(t0 + MIN), Some(MIN));
    assert!(schedule.may_start(t0 + 2 * MIN));
    assert!(schedule.is_due(t0 + 2 * MIN));

    // Missing or absurd Retry-After values are bounded.
    let mut unbounded = Schedule::new(None);
    unbounded.failed(
        t0,
        &CollectError::RateLimited {
            retry_after: Some(Duration::from_secs(86_400)),
        },
    );
    assert_eq!(unbounded.cooldown_remaining(t0), Some(60 * MIN));
    unbounded.failed(t0, &CollectError::RateLimited { retry_after: None });
    assert_eq!(unbounded.cooldown_remaining(t0), Some(5 * MIN));
}

#[test]
fn sign_in_failures_wait_for_the_normal_interval() {
    let t0 = Instant::now();
    let mut schedule = Schedule::new(Some(5 * MIN));
    for error in [
        CollectError::AuthRequired {
            reason: AuthReason::Expired,
        },
        CollectError::CredentialsMissing {
            path: PathBuf::from("auth.json"),
        },
    ] {
        schedule.started(t0);
        schedule.failed(t0, &error);
        assert!(!schedule.is_due(t0 + 4 * MIN), "{error:?}");
        assert!(schedule.is_due(t0 + 5 * MIN), "{error:?}");
    }
}
