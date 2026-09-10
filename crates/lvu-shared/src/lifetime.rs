//! Pure viewer-lifetime policy for the worker: when to stay, when to drain,
//! when to stop. No I/O here; the worker feeds socket connect/disconnect
//! observations in, and reads a single boolean out. Deterministic under an
//! injected clock, so the whole policy is unit-tested without processes.

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use crate::MAX_VIEWERS;

/// Whether a hello was admitted. The worker enforces the viewer cap here,
/// not by queueing: a seventeenth distinct window gets an explicit refusal
/// it can surface, while a re-hello from an attached window always succeeds
/// (it holds no additional slot).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewerAdmission {
    Admitted,
    RefusedFull,
}

/// Tracks attached viewers and decides worker shutdown. Shutdown fires only
/// when no viewers remain past the grace deadline, or immediately on an
/// explicit shutdown request. Any hello cancels a pending drain, so a quick
/// relaunch never flaps a running worker.
#[derive(Clone, Debug)]
pub struct ViewerSet {
    viewers: BTreeSet<u32>,
    grace: Duration,
    drain_until: Option<Instant>,
    shutdown_requested: bool,
}

impl ViewerSet {
    pub fn new(now: Instant, grace: Duration) -> Self {
        Self {
            viewers: BTreeSet::new(),
            grace,
            drain_until: Some(now + grace),
            shutdown_requested: false,
        }
    }

    /// A window attached (or re-attached). Always cancels a pending drain.
    /// Admits up to `MAX_VIEWERS` distinct windows; beyond that the hello
    /// is refused explicitly rather than queued or silently dropped.
    pub fn hello(&mut self, pid: u32) -> ViewerAdmission {
        if self.viewers.contains(&pid) {
            return ViewerAdmission::Admitted;
        }
        if self.viewers.len() >= MAX_VIEWERS {
            return ViewerAdmission::RefusedFull;
        }
        self.viewers.insert(pid);
        self.drain_until = None;
        ViewerAdmission::Admitted
    }

    /// A window detached cleanly or its socket died. Starts the drain clock
    /// when the last viewer leaves.
    pub fn goodbye(&mut self, now: Instant, pid: u32) {
        self.viewers.remove(&pid);
        if self.viewers.is_empty() && self.drain_until.is_none() {
            self.drain_until = Some(now + self.grace);
        }
    }

    /// An operator/shutdown request: stop at the next decision point
    /// regardless of viewers.
    pub fn request_shutdown(&mut self) {
        self.shutdown_requested = true;
    }

    pub fn viewer_count(&self) -> usize {
        self.viewers.len()
    }

    pub fn draining(&self) -> bool {
        self.drain_until.is_some()
    }

    /// True when the worker should shut down cleanly now: explicit request,
    /// or no viewers past the grace deadline.
    pub fn should_shutdown(&self, now: Instant) -> bool {
        if self.shutdown_requested {
            return true;
        }
        match self.drain_until {
            Some(deadline) => self.viewers.is_empty() && now >= deadline,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_grace_fires_without_viewers() {
        let start = Instant::now();
        let set = ViewerSet::new(start, Duration::from_secs(10));
        assert!(!set.should_shutdown(start));
        assert!(set.should_shutdown(start + Duration::from_secs(10)));
    }

    #[test]
    fn first_hello_cancels_boot_drain_forever_until_empty() {
        let start = Instant::now();
        let mut set = ViewerSet::new(start, Duration::from_secs(10));
        assert_eq!(set.hello(100), ViewerAdmission::Admitted);
        assert!(!set.draining());
        assert!(!set.should_shutdown(start + Duration::from_secs(3600)));
        set.goodbye(start + Duration::from_secs(2), 100);
        assert!(set.draining());
        assert!(!set.should_shutdown(start + Duration::from_secs(11)));
        assert!(set.should_shutdown(start + Duration::from_secs(12)));
    }

    #[test]
    fn reattach_during_drain_revives() {
        let start = Instant::now();
        let mut set = ViewerSet::new(start, Duration::from_secs(10));
        assert_eq!(set.hello(100), ViewerAdmission::Admitted);
        set.goodbye(start, 100);
        assert_eq!(set.hello(200), ViewerAdmission::Admitted);
        assert!(!set.draining());
        assert_eq!(set.viewer_count(), 1);
        assert!(!set.should_shutdown(start + Duration::from_secs(3600)));
    }

    #[test]
    fn explicit_shutdown_wins_over_viewers() {
        let start = Instant::now();
        let mut set = ViewerSet::new(start, Duration::from_secs(10));
        assert_eq!(set.hello(100), ViewerAdmission::Admitted);
        set.request_shutdown();
        assert!(set.should_shutdown(start));
    }

    #[test]
    fn sixteenth_viewer_admitted_seventeenth_refused() {
        let start = Instant::now();
        let mut set = ViewerSet::new(start, Duration::from_secs(10));
        for pid in 1..=MAX_VIEWERS as u32 {
            assert_eq!(set.hello(pid), ViewerAdmission::Admitted);
        }
        assert_eq!(set.viewer_count(), MAX_VIEWERS);
        assert_eq!(
            set.hello(MAX_VIEWERS as u32 + 1),
            ViewerAdmission::RefusedFull
        );
        assert_eq!(set.viewer_count(), MAX_VIEWERS);
        // A re-hello from an attached window holds no new slot.
        assert_eq!(set.hello(1), ViewerAdmission::Admitted);
        // A departure frees exactly one slot for the refused window.
        set.goodbye(start, 1);
        assert_eq!(set.hello(MAX_VIEWERS as u32 + 1), ViewerAdmission::Admitted);
    }

    #[test]
    fn unknown_goodbye_is_harmless() {
        let start = Instant::now();
        let mut set = ViewerSet::new(start, Duration::from_secs(10));
        set.hello(100);
        set.goodbye(start, 999);
        assert_eq!(set.viewer_count(), 1);
        assert!(!set.draining());
    }
}
