//! Test-only rendezvous for the capture publication write attempt.
//!
//! Available only with the `test-support` Cargo feature (never in default
//! builds): with nothing registered [`pre_publish`] returns immediately, so
//! production behavior is unchanged. Registration is scoped per source with
//! fresh identities per test. It fires BEFORE the publication write lock is
//! acquired, so an observing test holding read guards sees the genuine
//! blocked-pending attempt — unlike a file-append barrier, which cannot tell
//! a blocked publish from unscheduled capture.
//!
//! Synchronization is channel-based, never barrier-based, and every channel
//! is bounded (`sync_channel(1)`) with non-blocking `try_send`, so neither
//! side can ever block on a send: the writer's wait ends when the test
//! releases, when the test goes away (sender drop disconnects the receive),
//! or at a bounded timeout, so a panicking or late test wedges neither the
//! writer nor the suite. The registry mutex is never held across a wait.
//! [`ProbeArm`] disarms on drop (RAII), token-matched so a stale arm can
//! never remove a newer registration; no arm history is kept anywhere.
//! [`pre_publish`] must only be called from synchronous writer-thread
//! context: it parks the calling thread.

use std::collections::HashMap;
use std::sync::{
    Mutex, OnceLock, RwLock, TryLockError,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
use std::time::Duration;

use lvu_core::SourceId;

/// Backstop on the writer's release wait: test-drop disconnect unblocks even
/// earlier, and the release normally arrives promptly after the test settles.
/// A firing timeout lets the writer proceed rather than wedge the suite —
/// the test then fails loudly on its verdict instead of hanging. Mirrors the
/// hang-guard family used across the test suites.
pub const PUBLISH_PROBE_WAIT: Duration = Duration::from_secs(60);

/// What the publication lock looked like at the hooked attempt: `WouldBlock`
/// proves a test held read guards (genuinely contended); `Acquired` means no
/// contention (the guard is dropped immediately); `Poisoned` surfaces a
/// poisoned lock instead of hanging on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrePublishObservation {
    WouldBlock,
    Acquired,
    Poisoned,
}

/// Exact pending publication at a hooked attempt: the lock observation plus
/// the pending generation, high-watermark sequence and record count read
/// from `WriterState.current` at the hook call — before the write lock, so
/// this is what the publish is about to make visible, not what is visible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishAttempt {
    pub observation: PrePublishObservation,
    pub generation: u64,
    pub high_watermark: Option<u64>,
    pub records: u64,
}

/// Selection for [`arm_filtered`]: only an attempt whose pending triple
/// matches exactly is reported and parked. Non-matching attempts pass
/// through WITHOUT consuming the arm, so a periodic same-fence publish can
/// never steal the rendezvous meant for the awaited records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedPublish {
    pub generation: u64,
    pub high_watermark: Option<u64>,
    pub records: u64,
}

impl ExpectedPublish {
    fn matches(&self, current: &crate::SourceProgress) -> bool {
        self.generation == current.generation
            && self.high_watermark
                == current
                    .high_watermark
                    .as_ref()
                    .map(|record| record.sequence)
            && self.records == current.records
    }
}

/// Duplicate-arm refusal: any existing registration for the source — live
/// or already consumed — refuses a second arm until the previous arm is
/// dropped (RAII cleanup). Re-arming a consumed one-shot therefore requires
/// dropping the old handle first; the token check below keeps even that
/// handoff exact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArmError {
    AlreadyArmed,
}

impl std::fmt::Display for ArmError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyArmed => write!(formatter, "publish probe already armed for source"),
        }
    }
}

impl std::error::Error for ArmError {}

struct Probe {
    token: u64,
    expected: Option<ExpectedPublish>,
    entered_tx: Option<SyncSender<PublishAttempt>>,
    release_rx: Option<Receiver<()>>,
}

fn registry() -> &'static Mutex<HashMap<SourceId, Probe>> {
    static REGISTRY: OnceLock<Mutex<HashMap<SourceId, Probe>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Process-wide arm token allocator. Tokens are unique for the process
/// lifetime in practice (2^64 space, never reset); they exist only so a
/// stale arm's cleanup can never alias a newer registration — no history is
/// retained.
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn alloc_token() -> u64 {
    NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
}

/// Armed rendezvous handle: entered notification, one-shot release, and RAII
/// disarm on drop so registrations cannot leak across tests.
#[derive(Debug)]
pub struct ProbeArm {
    source: SourceId,
    token: u64,
    entered_rx: Receiver<PublishAttempt>,
    release_tx: Option<SyncSender<()>>,
}

impl ProbeArm {
    /// Wait (bounded) for the writer's next selected publication attempt for
    /// this source, returning the full pending triple.
    pub fn await_attempt(&self, timeout: Duration) -> Result<PublishAttempt, String> {
        self.entered_rx
            .recv_timeout(timeout)
            .map_err(|error| format!("publish attempt not observed: {error:?}"))
    }

    /// Wait (bounded) for the next selected attempt's lock observation only.
    pub fn await_observation(&self, timeout: Duration) -> Result<PrePublishObservation, String> {
        self.await_attempt(timeout)
            .map(|attempt| attempt.observation)
    }

    /// Release the parked writer. Send failure (writer timed out or already
    /// proceeded) is ignored — the release is advisory, the verdict the test
    /// draws from the observation is authoritative. Non-blocking by
    /// construction (`try_send` on a capacity-1 channel holding at most this
    /// one release).
    pub fn release(&mut self) {
        if let Some(tx) = self.release_tx.take() {
            let _ = tx.try_send(());
        }
    }
}

impl Drop for ProbeArm {
    fn drop(&mut self) {
        disarm_if_token(&self.source, self.token);
    }
}

/// Arm the rendezvous for one source: the next publication attempt reports
/// its lock observation and parks until released. Any existing registration
/// for the source — live or already consumed — refuses the second arm until
/// the previous arm is dropped, so at most one arm ever names a source and a
/// stale handle can never shadow a live one.
pub fn arm(source: SourceId) -> Result<ProbeArm, ArmError> {
    arm_inner(source, None)
}

/// Arm the rendezvous for one source, selected to one exact pending
/// publication: only an attempt whose pending triple matches `expected`
/// notifies and parks. Anything else passes through WITHOUT consuming the
/// arm, so a periodic same-fence publish can never steal the rendezvous
/// meant for awaited records. Duplicate and drop rules match [`arm`].
pub fn arm_filtered(source: SourceId, expected: ExpectedPublish) -> Result<ProbeArm, ArmError> {
    arm_inner(source, Some(expected))
}

fn arm_inner(source: SourceId, expected: Option<ExpectedPublish>) -> Result<ProbeArm, ArmError> {
    let mut registry = registry().lock().expect("probe registry poisoned");
    if registry.contains_key(&source) {
        return Err(ArmError::AlreadyArmed);
    }
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let token = alloc_token();
    registry.insert(
        source,
        Probe {
            token,
            expected,
            entered_tx: Some(entered_tx),
            release_rx: Some(release_rx),
        },
    );
    Ok(ProbeArm {
        source,
        token,
        entered_rx,
        release_tx: Some(release_tx),
    })
}

/// Remove the registration only if it still names this arm's token: a stale
/// arm can never remove a newer registration.
fn disarm_if_token(source: &SourceId, token: u64) {
    let mut registry = registry().lock().expect("probe registry poisoned");
    let matched = matches!(registry.get(source), Some(probe) if probe.token == token);
    if matched {
        registry.remove(source);
    }
}

/// Observe-and-park hook for the publication write attempt. Reads the
/// pending triple from `current` (already the about-to-publish values),
/// skips unregistered sources and — for filtered arms — non-matching
/// attempts WITHOUT consuming the arm, then takes the one-shot ends under
/// the registry lock, observes the passed publication lock with a
/// non-blocking `try_write` (dropping an acquired guard immediately),
/// notifies the test with the full attempt, then parks for release with no
/// locks held. Selection and consumption are separate steps so a
/// non-matching publish can never steal a filtered rendezvous.
pub(crate) fn pre_publish(
    source: &SourceId,
    publication: &RwLock<()>,
    current: &crate::SourceProgress,
) {
    let selected = {
        let registry = registry().lock().expect("probe registry poisoned");
        match registry.get(source) {
            None => return,
            Some(probe) => match probe.expected {
                None => true,
                Some(want) => want.matches(current),
            },
        }
    };
    if !selected {
        return;
    }
    let taken = {
        let mut registry = registry().lock().expect("probe registry poisoned");
        match registry.get_mut(source) {
            None => return,
            Some(probe) => (probe.entered_tx.take(), probe.release_rx.take()),
        }
    };
    let (Some(entered_tx), Some(release_rx)) = taken else {
        return;
    };
    let observation = match publication.try_write() {
        Ok(guard) => {
            drop(guard);
            PrePublishObservation::Acquired
        }
        Err(TryLockError::WouldBlock) => PrePublishObservation::WouldBlock,
        Err(TryLockError::Poisoned(_)) => PrePublishObservation::Poisoned,
    };
    let _ = entered_tx.try_send(PublishAttempt {
        observation,
        generation: current.generation,
        high_watermark: current
            .high_watermark
            .as_ref()
            .map(|record| record.sequence),
        records: current.records,
    });
    let _ = release_rx.recv_timeout(PUBLISH_PROBE_WAIT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RuntimeState, SourceProgress};
    use lvu_core::RecordId;
    use std::sync::Arc;
    use std::thread::JoinHandle;

    fn test_source(n: u128) -> SourceId {
        SourceId(uuid::Uuid::from_u128(n))
    }

    fn progress(
        source: SourceId,
        generation: u64,
        high_watermark: Option<u64>,
        records: u64,
    ) -> SourceProgress {
        SourceProgress {
            source_id: source,
            generation,
            state: RuntimeState::Running,
            records,
            high_watermark: high_watermark.map(|sequence| RecordId {
                source_id: source,
                sequence,
            }),
            journal_bytes: 0,
            synced_records: 0,
            syncs: 0,
            handovers: 0,
            writer_cpu_nanos: 0,
            reader_cpu_nanos: 0,
            boundaries: 0,
            exit_code: None,
            discarded_bytes: 0,
            discarded_bytes_known: true,
            last_error: None,
        }
    }

    /// Run `pre_publish` on a helper thread (it may park until released) and
    /// hand back a bounded done channel: tests observe, release, then join
    /// through the channel — never a bare `join` on a possibly parked
    /// thread, never a Barrier (a panicking party would wedge the other side
    /// past any timeout).
    fn parked_call(
        source: SourceId,
        lock: Arc<RwLock<()>>,
        current: SourceProgress,
    ) -> (JoinHandle<()>, mpsc::Receiver<()>) {
        let (done_tx, done_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            pre_publish(&source, &lock, &current);
            let _ = done_tx.send(());
        });
        (handle, done_rx)
    }

    fn join_bounded(handle: JoinHandle<()>, done: mpsc::Receiver<()>) {
        done.recv_timeout(Duration::from_secs(60))
            .expect("helper thread");
        handle.join().expect("helper thread join");
    }

    #[test]
    fn unregistered_attempts_pass_through() {
        let lock = RwLock::new(());
        let current = progress(test_source(1), 1, None, 0);
        pre_publish(&test_source(1), &lock, &current);
        pre_publish(&test_source(1), &lock, &current);
    }

    /// Any existing registration refuses a second arm — live or already
    /// consumed — until the previous arm is dropped; a stale token never
    /// removes the live entry.
    #[test]
    fn duplicate_arm_refused_until_drop() {
        let source = test_source(2);
        let lock = Arc::new(RwLock::new(()));
        let mut first = arm(source).expect("first arm");
        assert!(matches!(arm(source), Err(ArmError::AlreadyArmed)));
        // Fire and consume through a helper thread; the consumed entry still
        // refuses until the old arm is dropped.
        let (writer, done) = parked_call(source, lock.clone(), progress(source, 1, None, 0));
        assert_eq!(
            first.await_observation(Duration::from_secs(10)),
            Ok(PrePublishObservation::Acquired)
        );
        first.release();
        join_bounded(writer, done);
        assert!(matches!(arm(source), Err(ArmError::AlreadyArmed)));
        disarm_if_token(&source, u64::MAX);
        assert!(matches!(arm(source), Err(ArmError::AlreadyArmed)));
        drop(first);
        let mut second = arm(source).expect("re-arm after drop");
        let (writer, done) = parked_call(source, lock.clone(), progress(source, 1, None, 0));
        assert_eq!(
            second.await_observation(Duration::from_secs(10)),
            Ok(PrePublishObservation::Acquired)
        );
        second.release();
        join_bounded(writer, done);
    }

    #[test]
    fn one_shot_consumes_then_rearm_after_drop() {
        let source = test_source(3);
        let lock = Arc::new(RwLock::new(()));
        let mut arm1 = arm(source).expect("arm");
        let (writer, done) = parked_call(source, lock.clone(), progress(source, 1, None, 0));
        assert_eq!(
            arm1.await_observation(Duration::from_secs(10)),
            Ok(PrePublishObservation::Acquired)
        );
        // Consumed: a further attempt proceeds unimpeded without notifying,
        // while the first arm still blocks re-arming until dropped.
        pre_publish(&source, &lock, &progress(source, 1, None, 0));
        arm1.release();
        join_bounded(writer, done);
        assert!(matches!(arm(source), Err(ArmError::AlreadyArmed)));
        drop(arm1);
        let mut arm2 = arm(source).expect("re-arm after drop");
        let (writer, done) = parked_call(source, lock.clone(), progress(source, 1, None, 0));
        assert_eq!(
            arm2.await_observation(Duration::from_secs(10)),
            Ok(PrePublishObservation::Acquired)
        );
        arm2.release();
        join_bounded(writer, done);
    }

    /// WouldBlock observation with the release arriving from a helper thread:
    /// the parked writer proceeds once released, and a late duplicate
    /// release is a silent no-op.
    #[test]
    fn blocked_attempt_parks_until_released() {
        let source = test_source(4);
        let lock = Arc::new(RwLock::new(()));
        let _held = lock.read().expect("hold read guard");
        let mut arm = arm(source).expect("arm");
        let (writer, done) = parked_call(source, lock.clone(), progress(source, 1, None, 0));
        assert_eq!(
            arm.await_observation(Duration::from_secs(30)),
            Ok(PrePublishObservation::WouldBlock)
        );
        arm.release();
        arm.release();
        join_bounded(writer, done);
    }

    #[test]
    fn poisoned_lock_observed_not_hung_on() {
        let source = test_source(5);
        let lock = Arc::new(RwLock::new(()));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = lock.write().expect("write guard");
            panic!("poison the lock");
        }));
        let mut arm = arm(source).expect("arm");
        let (writer, done) = parked_call(source, lock.clone(), progress(source, 1, None, 0));
        assert_eq!(
            arm.await_observation(Duration::from_secs(10)),
            Ok(PrePublishObservation::Poisoned)
        );
        arm.release();
        join_bounded(writer, done);
    }

    #[test]
    fn dropped_arm_unblocks_parked_writer() {
        let source = test_source(6);
        let lock = Arc::new(RwLock::new(()));
        let arm = arm(source).expect("arm");
        let (writer, done) = parked_call(source, lock.clone(), progress(source, 1, None, 0));
        assert_eq!(
            arm.await_observation(Duration::from_secs(30)),
            Ok(PrePublishObservation::Acquired)
        );
        drop(arm);
        join_bounded(writer, done);
    }

    /// Filtered selection: a non-matching attempt passes through WITHOUT
    /// consuming the arm (no notification, same thread safe), while the
    /// matching attempt notifies with the full pending triple.
    #[test]
    fn filtered_selection_skips_nonmatching() {
        let source = test_source(7);
        let lock = Arc::new(RwLock::new(()));
        let want = ExpectedPublish {
            generation: 1,
            high_watermark: Some(9),
            records: 10,
        };
        let mut arm = arm_filtered(source, want).expect("arm");
        pre_publish(&source, &lock, &progress(source, 1, Some(9), 7));
        assert!(arm.await_observation(Duration::from_millis(100)).is_err());
        let (writer, done) = parked_call(source, lock.clone(), progress(source, 1, Some(9), 10));
        assert_eq!(
            arm.await_attempt(Duration::from_secs(10)),
            Ok(PublishAttempt {
                observation: PrePublishObservation::Acquired,
                generation: 1,
                high_watermark: Some(9),
                records: 10,
            })
        );
        arm.release();
        join_bounded(writer, done);
    }
}
