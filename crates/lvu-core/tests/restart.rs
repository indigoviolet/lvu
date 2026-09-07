//! Explicit command restart policies.

use lvu_core::{
    Capture, CaptureEvent, CommandDefinition, CommandProgram, RestartBounds, RestartPolicy,
    SourceEvent,
    acquisition::{CaptureLimits, capture_command_supervised},
};
use std::{collections::BTreeMap, path::Path, time::Duration};
use tempfile::tempdir;
use uuid::Uuid;

fn limits(maximum_restarts: u32) -> CaptureLimits {
    CaptureLimits {
        restart: RestartBounds {
            backoff: lvu_core::Backoff {
                initial: Duration::from_millis(10),
                maximum: Duration::from_millis(40),
                jitter_percent: 0,
            },
            maximum_restarts,
            window: Duration::from_secs(60),
        },
        ..CaptureLimits::default()
    }
}

fn command(directory: &Path, script: &str, restart: RestartPolicy) -> CommandDefinition {
    CommandDefinition {
        program: CommandProgram::Exec {
            executable: "sh".into(),
            args: vec!["-c".into(), script.to_owned()],
        },
        cwd: Some(directory.to_owned()),
        environment: BTreeMap::new(),
        restart,
    }
}

#[derive(Default)]
struct Collected {
    events: Vec<CaptureEvent>,
    history: Vec<SourceEvent>,
}

impl Collected {
    fn lines(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|event| match event {
                CaptureEvent::Record(record) => Some(String::from_utf8_lossy(&record.bytes).into()),
                _ => None,
            })
            .collect()
    }

    fn acquisitions(&self) -> Vec<Uuid> {
        let mut seen: Vec<Uuid> = Vec::new();
        for event in &self.events {
            if let CaptureEvent::Boundary { acquisition_id, .. } = event
                && !seen.contains(acquisition_id)
            {
                seen.push(*acquisition_id);
            }
        }
        seen
    }

    fn starts(&self) -> usize {
        self.history
            .iter()
            .filter(|event| matches!(event, SourceEvent::CommandStarted { .. }))
            .count()
    }

    fn restart_delays(&self) -> Vec<u64> {
        self.history
            .iter()
            .filter_map(|event| match event {
                SourceEvent::RestartScheduled { delay_millis, .. } => Some(*delay_millis),
                _ => None,
            })
            .collect()
    }
}

async fn drain(capture: Capture, limit: Duration) -> Collected {
    let Capture {
        handle,
        mut events,
        mut history,
        ..
    } = capture;
    let mut collected = Collected::default();
    let finished = tokio::time::timeout(limit, async {
        while let Some(event) = events.recv().await {
            collected.events.push(event);
        }
    })
    .await;
    assert!(finished.is_ok(), "capture did not finish within {limit:?}");
    while let Ok(record) = history.try_recv() {
        collected.history.push(record.event);
    }
    drop(handle);
    collected
}

#[tokio::test]
async fn never_is_the_default_and_a_failing_command_is_not_restarted() {
    assert_eq!(RestartPolicy::default(), RestartPolicy::Never);
    let directory = tempdir().unwrap();
    let capture = capture_command_supervised(
        command(directory.path(), "echo once; exit 3", RestartPolicy::Never),
        limits(5),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    assert_eq!(collected.lines(), vec!["once".to_owned()]);
    assert_eq!(collected.starts(), 1);
    assert!(
        collected.history.iter().any(|event| matches!(
            event,
            SourceEvent::RestartDeclined {
                policy: RestartPolicy::Never,
                ..
            }
        )),
        "declining to restart is a published decision, not silence"
    );
}

#[tokio::test]
async fn on_failure_restarts_a_failing_command_and_stops_at_the_budget() {
    let directory = tempdir().unwrap();
    let capture = capture_command_supervised(
        command(
            directory.path(),
            "echo attempt; exit 7",
            RestartPolicy::OnFailure,
        ),
        limits(2),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    assert_eq!(
        collected.lines().len(),
        3,
        "one original run plus exactly the permitted restarts"
    );
    assert_eq!(collected.starts(), 3);
    assert_eq!(
        collected.acquisitions().len(),
        3,
        "each run is its own extent"
    );
    assert_eq!(collected.restart_delays(), vec![10, 20]);
    assert!(
        collected
            .history
            .iter()
            .any(|event| matches!(event, SourceEvent::RestartsExhausted { attempts: 2, .. })),
        "budget exhaustion must be published: {:?}",
        collected.history
    );
}

#[tokio::test]
async fn on_failure_does_not_restart_a_clean_exit() {
    let directory = tempdir().unwrap();
    let capture = capture_command_supervised(
        command(directory.path(), "echo done", RestartPolicy::OnFailure),
        limits(5),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    assert_eq!(collected.starts(), 1);
    assert!(collected.history.iter().any(|event| matches!(
        event,
        SourceEvent::RestartDeclined {
            policy: RestartPolicy::OnFailure,
            success: Some(true),
        }
    )));
}

#[tokio::test]
async fn always_restarts_a_clean_exit_within_bounded_backoff() {
    let directory = tempdir().unwrap();
    let capture = capture_command_supervised(
        command(directory.path(), "echo tick", RestartPolicy::Always),
        limits(3),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(10)).await;

    assert_eq!(collected.lines().len(), 4);
    assert_eq!(
        collected.restart_delays(),
        vec![10, 20, 40],
        "backoff doubles and then holds at the configured maximum"
    );
    assert!(collected.history.iter().any(|event| matches!(
        event,
        SourceEvent::CommandExited {
            success: true,
            code: Some(0),
            ..
        }
    )));
}

#[tokio::test]
async fn an_explicit_stop_is_never_overridden_by_a_restart_policy() {
    let directory = tempdir().unwrap();
    let capture = capture_command_supervised(
        command(
            directory.path(),
            "echo started; while :; do sleep 1; done",
            RestartPolicy::Always,
        ),
        limits(5),
    )
    .unwrap();
    let Capture {
        mut handle,
        mut events,
        mut history,
        ..
    } = capture;
    loop {
        match tokio::time::timeout(Duration::from_secs(5), events.recv()).await {
            Ok(Some(CaptureEvent::Record(_))) => break,
            Ok(Some(_)) => {}
            _ => panic!("command produced no output"),
        }
    }
    handle.stop();
    let completion = tokio::time::timeout(Duration::from_secs(5), handle.wait())
        .await
        .expect("stop did not settle")
        .expect("capture task panicked");
    assert!(!completion.aborted);
    while tokio::time::timeout(Duration::from_millis(200), events.recv())
        .await
        .is_ok_and(|event| event.is_some())
    {}
    let mut starts = 0;
    while let Ok(record) = history.try_recv() {
        if matches!(record.event, SourceEvent::CommandStarted { .. }) {
            starts += 1;
        }
        assert!(
            !matches!(record.event, SourceEvent::RestartScheduled { .. }),
            "a stop must not schedule a restart"
        );
    }
    assert_eq!(starts, 1);
}

#[tokio::test]
async fn cancellation_is_never_overridden_by_a_restart_policy() {
    let directory = tempdir().unwrap();
    let capture = capture_command_supervised(
        command(
            directory.path(),
            "echo started; while :; do sleep 1; done",
            RestartPolicy::Always,
        ),
        limits(5),
    )
    .unwrap();
    let Capture {
        mut handle,
        mut events,
        mut history,
        ..
    } = capture;
    loop {
        match tokio::time::timeout(Duration::from_secs(5), events.recv()).await {
            Ok(Some(CaptureEvent::Record(_))) => break,
            Ok(Some(_)) => {}
            _ => panic!("command produced no output"),
        }
    }
    handle.cancel();
    let completion = tokio::time::timeout(Duration::from_secs(5), handle.wait())
        .await
        .expect("cancellation did not settle")
        .expect("capture task panicked");
    assert!(completion.aborted);
    while let Ok(record) = history.try_recv() {
        assert!(!matches!(
            record.event,
            SourceEvent::RestartScheduled { .. }
        ));
    }
}
