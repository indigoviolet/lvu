//! Bounded restart/reconnect arithmetic shared by command restart policies and
//! HTTP reconnection.
//!
//! Nothing here performs I/O. Backoff is deterministic given its jitter input so
//! bounds can be proven by test rather than observed by sleeping.

use crate::RestartPolicy;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// Hard ceiling applied to every configured delay, independent of policy.
pub const MAXIMUM_BACKOFF: Duration = Duration::from_secs(300);
/// Hard ceiling on remembered attempts inside one window.
pub const MAXIMUM_TRACKED_ATTEMPTS: usize = 1024;

/// Bounded exponential backoff with downward jitter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Backoff {
    pub initial: Duration,
    pub maximum: Duration,
    /// Proportion of a delay that may be randomised away, 0..=100.
    pub jitter_percent: u8,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(500),
            maximum: Duration::from_secs(30),
            jitter_percent: 25,
        }
    }
}

impl Backoff {
    /// Undelayed exponential value for `attempt` (1 is the first retry),
    /// clamped by the configured maximum and the global ceiling.
    pub fn base_delay(&self, attempt: u32) -> Duration {
        let ceiling = self.maximum.min(MAXIMUM_BACKOFF);
        if attempt <= 1 {
            return self.initial.min(ceiling);
        }
        let shift = (attempt - 1).min(32);
        let scaled = self
            .initial
            .as_millis()
            .saturating_mul(1_u128 << shift)
            .min(u64::MAX as u128) as u64;
        Duration::from_millis(scaled).min(ceiling)
    }

    /// Applies downward jitter. `fraction` is a caller-supplied 0..=10_000
    /// value, so tests can pin both bounds exactly.
    pub fn delay_with_jitter(&self, attempt: u32, fraction: u32) -> Duration {
        let base = self.base_delay(attempt);
        let percent = u128::from(self.jitter_percent.min(100));
        let fraction = u128::from(fraction.min(10_000));
        let millis = base.as_millis();
        let removed = millis * percent * fraction / (100 * 10_000);
        Duration::from_millis((millis - removed).min(u64::MAX as u128) as u64)
    }

    /// Jitter drawn from a process-local sequence; no dependency on `rand` and
    /// no global state that could serialise unrelated sources.
    pub fn delay(&self, attempt: u32, jitter: &mut Jitter) -> Duration {
        self.delay_with_jitter(attempt, jitter.next_fraction())
    }
}

/// Small deterministic-when-seeded generator used only for backoff jitter.
#[derive(Clone, Copy, Debug)]
pub struct Jitter(u64);

impl Jitter {
    pub fn from_seed(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn from_entropy() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Self::from_seed(nanos ^ (std::process::id() as u64).rotate_left(32))
    }

    /// Next value in 0..=10_000.
    pub fn next_fraction(&mut self) -> u32 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z % 10_001) as u32
    }
}

impl Default for Jitter {
    fn default() -> Self {
        Self::from_entropy()
    }
}

/// A sliding window that caps how many attempts may be made in a period.
#[derive(Clone, Debug)]
pub struct AttemptWindow {
    maximum: u32,
    window: Duration,
    attempts: VecDeque<Instant>,
}

impl AttemptWindow {
    pub fn new(maximum: u32, window: Duration) -> Self {
        Self {
            maximum,
            window,
            attempts: VecDeque::new(),
        }
    }

    fn expire(&mut self, now: Instant) {
        while let Some(oldest) = self.attempts.front() {
            if now.duration_since(*oldest) >= self.window {
                self.attempts.pop_front();
            } else {
                break;
            }
        }
    }

    /// Attempts recorded inside the window ending at `now`.
    pub fn used(&mut self, now: Instant) -> u32 {
        self.expire(now);
        self.attempts.len() as u32
    }

    /// Whether another attempt is still permitted.
    pub fn permits(&mut self, now: Instant) -> bool {
        self.used(now) < self.maximum
    }

    pub fn maximum(&self) -> u32 {
        self.maximum
    }

    pub fn window(&self) -> Duration {
        self.window
    }

    /// Records an attempt. Storage is bounded even if `maximum` is large.
    pub fn record(&mut self, now: Instant) {
        self.expire(now);
        if self.attempts.len() >= MAXIMUM_TRACKED_ATTEMPTS {
            self.attempts.pop_front();
        }
        self.attempts.push_back(now);
    }
}

/// Runtime bounds applied to command restarts. The *policy* (never/on-failure/
/// always) is persisted with the source definition; these bounds are runtime
/// configuration so a definition can never encode an unbounded restart loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestartBounds {
    pub backoff: Backoff,
    pub maximum_restarts: u32,
    pub window: Duration,
}

impl Default for RestartBounds {
    fn default() -> Self {
        Self {
            backoff: Backoff::default(),
            maximum_restarts: 5,
            window: Duration::from_secs(60),
        }
    }
}

/// Why a finished run was or was not restarted. Every value becomes visible
/// source history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestartDecision {
    /// The policy does not restart this outcome.
    PolicyDeclines,
    /// The restart budget for the window is spent.
    BudgetExhausted { used: u32, maximum: u32 },
    /// Restart after waiting `delay`.
    Restart { attempt: u32, delay: Duration },
}

/// Decides whether an exited run is restarted. `success` is `None` when the
/// run ended without an observable exit status (spawn or wait failure), which
/// counts as a failure.
pub fn decide_restart(
    policy: RestartPolicy,
    success: Option<bool>,
    bounds: &RestartBounds,
    window: &mut AttemptWindow,
    jitter: &mut Jitter,
    now: Instant,
) -> RestartDecision {
    let restarts = match policy {
        RestartPolicy::Never => false,
        RestartPolicy::OnFailure => !success.unwrap_or(false),
        RestartPolicy::Always => true,
    };
    if !restarts {
        return RestartDecision::PolicyDeclines;
    }
    if !window.permits(now) {
        return RestartDecision::BudgetExhausted {
            used: window.used(now),
            maximum: window.maximum(),
        };
    }
    window.record(now);
    let attempt = window.used(now);
    RestartDecision::Restart {
        attempt,
        delay: bounds.backoff.delay(attempt, jitter),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_stops_at_the_configured_ceiling() {
        let backoff = Backoff {
            initial: Duration::from_millis(100),
            maximum: Duration::from_millis(700),
            jitter_percent: 0,
        };
        let delays: Vec<_> = (1..=6).map(|n| backoff.base_delay(n)).collect();
        assert_eq!(
            delays,
            vec![
                Duration::from_millis(100),
                Duration::from_millis(200),
                Duration::from_millis(400),
                Duration::from_millis(700),
                Duration::from_millis(700),
                Duration::from_millis(700),
            ]
        );
    }

    #[test]
    fn jitter_only_removes_a_bounded_share_of_the_delay() {
        let backoff = Backoff {
            initial: Duration::from_millis(1000),
            maximum: Duration::from_secs(10),
            jitter_percent: 25,
        };
        assert_eq!(
            backoff.delay_with_jitter(1, 0),
            Duration::from_millis(1000),
            "no jitter draw must keep the full delay"
        );
        assert_eq!(
            backoff.delay_with_jitter(1, 10_000),
            Duration::from_millis(750),
            "a maximal draw removes exactly the configured share"
        );
        let mut jitter = Jitter::from_seed(7);
        for _ in 0..1000 {
            let delay = backoff.delay(3, &mut jitter);
            assert!(
                delay >= Duration::from_millis(3000) && delay <= Duration::from_millis(4000),
                "delay {delay:?} escaped its bounds"
            );
        }
    }

    #[test]
    fn an_absurd_configuration_still_respects_the_global_ceiling() {
        let backoff = Backoff {
            initial: Duration::from_secs(3600),
            maximum: Duration::from_secs(86_400),
            jitter_percent: 0,
        };
        assert_eq!(backoff.base_delay(30), MAXIMUM_BACKOFF);
    }

    #[test]
    fn the_window_refuses_beyond_its_budget_and_recovers_after_it_slides() {
        let start = Instant::now();
        let mut window = AttemptWindow::new(2, Duration::from_secs(10));
        assert!(window.permits(start));
        window.record(start);
        window.record(start);
        assert!(!window.permits(start));
        assert!(window.permits(start + Duration::from_secs(11)));
    }

    #[test]
    fn policies_decide_by_outcome_and_never_exceed_the_budget() {
        let bounds = RestartBounds {
            maximum_restarts: 2,
            ..RestartBounds::default()
        };
        let mut jitter = Jitter::from_seed(1);
        let now = Instant::now();

        let mut window = AttemptWindow::new(bounds.maximum_restarts, bounds.window);
        assert_eq!(
            decide_restart(
                RestartPolicy::Never,
                Some(false),
                &bounds,
                &mut window,
                &mut jitter,
                now
            ),
            RestartDecision::PolicyDeclines
        );

        let mut window = AttemptWindow::new(bounds.maximum_restarts, bounds.window);
        assert_eq!(
            decide_restart(
                RestartPolicy::OnFailure,
                Some(true),
                &bounds,
                &mut window,
                &mut jitter,
                now
            ),
            RestartDecision::PolicyDeclines,
            "on-failure must not restart a clean exit"
        );
        assert!(
            matches!(
                decide_restart(
                    RestartPolicy::OnFailure,
                    None,
                    &bounds,
                    &mut window,
                    &mut jitter,
                    now
                ),
                RestartDecision::Restart { .. }
            ),
            "an unobservable outcome counts as a failure"
        );

        let mut window = AttemptWindow::new(bounds.maximum_restarts, bounds.window);
        for expected in 1..=2 {
            match decide_restart(
                RestartPolicy::Always,
                Some(true),
                &bounds,
                &mut window,
                &mut jitter,
                now,
            ) {
                RestartDecision::Restart { attempt, delay } => {
                    assert_eq!(attempt, expected);
                    assert!(delay <= bounds.backoff.maximum);
                }
                other => panic!("unexpected decision {other:?}"),
            }
        }
        assert_eq!(
            decide_restart(
                RestartPolicy::Always,
                Some(true),
                &bounds,
                &mut window,
                &mut jitter,
                now
            ),
            RestartDecision::BudgetExhausted {
                used: 2,
                maximum: 2
            }
        );
    }
}
