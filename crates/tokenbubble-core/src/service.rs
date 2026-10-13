//! When to collect: the user's interval, bounded backoff after transient failures, and
//! rate-limit cooldowns. Clock-agnostic so hosts and tests drive it with their own instants.

use std::time::{Duration, Instant};

use crate::providers::CollectError;

const FIRST_BACKOFF: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);
const DEFAULT_COOLDOWN: Duration = Duration::from_secs(5 * 60);
const MIN_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_COOLDOWN: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug, Default)]
pub struct Schedule {
    /// `None` collects only on request.
    interval: Option<Duration>,
    last_attempt: Option<Instant>,
    /// Earlier-than-interval retry after a transient failure.
    retry_at: Option<Instant>,
    /// No collection of any kind before this instant (rate limited).
    cooldown_until: Option<Instant>,
    transient_failures: u32,
}

impl Schedule {
    pub fn new(interval: Option<Duration>) -> Self {
        Self {
            interval,
            ..Self::default()
        }
    }

    pub fn set_interval(&mut self, interval: Option<Duration>) {
        self.interval = interval;
    }

    /// Whether an automatic collection should start now.
    pub fn is_due(&self, now: Instant) -> bool {
        if !self.may_start(now) {
            return false;
        }
        let Some(interval) = self.interval else {
            return false;
        };
        if let Some(retry_at) = self.retry_at {
            return now >= retry_at;
        }
        self.last_attempt
            .is_none_or(|last| now.saturating_duration_since(last) >= interval)
    }

    /// Whether any collection, including a manual one, may start now.
    pub fn may_start(&self, now: Instant) -> bool {
        self.cooldown_until.is_none_or(|until| now >= until)
    }

    pub fn cooldown_remaining(&self, now: Instant) -> Option<Duration> {
        self.cooldown_until
            .map(|until| until.saturating_duration_since(now))
            .filter(|remaining| !remaining.is_zero())
    }

    pub fn started(&mut self, now: Instant) {
        self.last_attempt = Some(now);
    }

    pub fn succeeded(&mut self) {
        self.retry_at = None;
        self.cooldown_until = None;
        self.transient_failures = 0;
    }

    pub fn failed(&mut self, now: Instant, error: &CollectError) {
        match error {
            CollectError::RateLimited { retry_after } => {
                let wait = retry_after
                    .unwrap_or(DEFAULT_COOLDOWN)
                    .clamp(MIN_COOLDOWN, MAX_COOLDOWN);
                self.cooldown_until = Some(now + wait);
                self.retry_at = self.interval.map(|_| now + wait);
            }
            error if error.is_transient() => {
                self.transient_failures = self.transient_failures.saturating_add(1);
                let exponent = (self.transient_failures - 1).min(10);
                let mut backoff = (FIRST_BACKOFF * 2u32.pow(exponent)).min(MAX_BACKOFF);
                if let Some(interval) = self.interval {
                    backoff = backoff.min(interval);
                }
                self.retry_at = self.interval.map(|_| now + backoff);
            }
            // Sign-in and configuration problems need the user; poll at the normal pace.
            _ => {
                self.retry_at = None;
                self.transient_failures = 0;
            }
        }
    }
}
