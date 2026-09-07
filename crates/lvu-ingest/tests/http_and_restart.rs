//! HTTP sources and command restart policies through the durable runtime.

mod support;

use lvu_core::{
    Acquisition, CommandDefinition, CommandProgram, HttpFraming, HttpHeader, HttpLimits,
    ReconnectPolicy, RestartPolicy, SourceDefinition, SourceEvent, SourceId, StreamKind,
};
use lvu_ingest::{
    RuntimeConfig, RuntimeError, RuntimeState, SourceHandle, SourceManager, read_history,
};
use std::{collections::BTreeMap, path::Path, time::Duration};
use support::{Reply, TestServer};
use tempfile::tempdir;

fn config() -> RuntimeConfig {
    let mut config = RuntimeConfig::default();
    config.acquisition.channel_capacity = 8;
    config.acquisition.restart.backoff.initial = Duration::from_millis(10);
    config.acquisition.restart.backoff.maximum = Duration::from_millis(20);
    config.acquisition.restart.backoff.jitter_percent = 0;
    config.acquisition.restart.maximum_restarts = 1;
    config.graceful_stop_deadline = Duration::from_secs(3);
    config
}

fn http_source(id: SourceId, url: String, framing: HttpFraming, attempts: u32) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "fixture endpoint".into(),
        acquisition: Acquisition::Http {
            url,
            framing,
            reconnect: ReconnectPolicy {
                enabled: attempts > 0,
                delay: Duration::from_millis(10),
                maximum_delay: Duration::from_millis(20),
                jitter_percent: 0,
                maximum_attempts: attempts,
                attempt_window: Duration::from_secs(60),
                resume: true,
            },
            headers: vec![HttpHeader::new("authorization", "Bearer fixture-secret")],
            limits: HttpLimits {
                connect_timeout: Duration::from_millis(500),
                read_timeout: Duration::from_millis(500),
                ..HttpLimits::default()
            },
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

fn command_source(id: SourceId, script: &str, restart: RestartPolicy) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "fixture command".into(),
        acquisition: Acquisition::Command {
            command: CommandDefinition {
                program: CommandProgram::Exec {
                    executable: "sh".into(),
                    args: vec!["-c".into(), script.to_owned()],
                },
                cwd: None,
                environment: BTreeMap::new(),
                restart,
            },
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

async fn wait_for(handle: &SourceHandle, predicate: impl Fn(&lvu_ingest::SourceProgress) -> bool) {
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if predicate(&progress.borrow()) {
                return;
            }
            progress.changed().await.expect("progress channel closed");
        }
    })
    .await
    .expect("runtime state timed out");
}

async fn journal_bytes(handle: &SourceHandle) -> Vec<u8> {
    let mut offset = 0;
    let mut bytes = Vec::new();
    loop {
        let page = handle.read_page(offset, 64, 64 * 1024).await.unwrap();
        for record in &page.records {
            assert_eq!(record.stream, StreamKind::Http);
            bytes.extend_from_slice(&record.bytes);
            bytes.extend_from_slice(&record.delimiter);
        }
        if page.end_of_journal {
            return bytes;
        }
        offset = page.next_offset;
    }
}

#[tokio::test]
async fn an_http_source_is_journaled_losslessly_and_publishes_durable_history() {
    let server = TestServer::start(|ordinal, _| match ordinal {
        0 => Reply::abrupt(b"alpha\nbeta\n"),
        _ => Reply::ok(b"gamma\n"),
    })
    .await;
    let root = tempdir().unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path(), config()).unwrap();
    let handle = manager
        .start(http_source(
            id,
            server.url("/tail"),
            HttpFraming::Newline,
            1,
        ))
        .await
        .unwrap();
    wait_for(&handle, |progress| progress.state.is_terminal()).await;

    assert_eq!(journal_bytes(&handle).await, b"alpha\nbeta\ngamma\n");
    assert_eq!(
        handle.progress().boundaries,
        2,
        "each connection is a separate capture extent in the journal"
    );

    let history = handle.history();
    assert!(history.published >= 4, "{history:?}");
    assert!(
        history
            .entries
            .iter()
            .any(|record| matches!(record.event, SourceEvent::CaptureGap { .. })),
        "the reconnect boundary must be visible in source history"
    );
    assert_eq!(history.dropped, 0);
    assert!(history.complete());

    let durable = read_history(
        &root.path().join(id.0.to_string()).join("history.jsonl"),
        1024,
    )
    .expect("history file must be readable");
    assert_eq!(durable.len(), history.published as usize);
    let rendered = format!("{durable:?}");
    assert!(
        !rendered.contains("fixture-secret"),
        "durable history leaked a credential: {rendered}"
    );
}

#[tokio::test]
async fn an_http_source_that_is_rejected_reports_the_status_without_capturing_a_body() {
    let server = TestServer::start(|_, _| Reply::status(403, "Forbidden")).await;
    let root = tempdir().unwrap();
    let manager = SourceManager::new(root.path(), config()).unwrap();
    let handle = manager
        .start(http_source(
            SourceId::new(),
            server.url("/tail"),
            HttpFraming::Newline,
            0,
        ))
        .await
        .unwrap();
    wait_for(&handle, |progress| progress.state.is_terminal()).await;

    assert_eq!(handle.progress().state, RuntimeState::Error);
    assert!(journal_bytes(&handle).await.is_empty());
    assert!(
        handle
            .progress()
            .last_error
            .is_some_and(|message| message.contains("403")),
        "the rejection must reach source status"
    );
    assert!(
        handle
            .history()
            .entries
            .iter()
            .any(|record| matches!(record.event, SourceEvent::HttpRejected { status: 403, .. }))
    );
}

#[tokio::test]
async fn restoring_a_remembered_command_never_launches_it() {
    let root = tempdir().unwrap();
    let evidence = root.path().join("launched");
    let script = format!("echo ran > {}; echo output", evidence.display());
    let manager = SourceManager::new(root.path().join("capture"), config()).unwrap();
    let id = SourceId::new();
    let definition = command_source(id, &script, RestartPolicy::Always);

    let refused = manager.restore(definition.clone()).await.err();
    assert!(
        matches!(refused, Some(RuntimeError::RestoreWouldLaunchCommand)),
        "restore must refuse a remembered command: {refused:?}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !evidence.exists(),
        "restoration executed a remembered command"
    );
    assert!(
        !root.path().join("capture").join(id.0.to_string()).exists(),
        "a refused restore must not create capture state"
    );

    // The same definition started explicitly does run, and its restart policy
    // applies only to that session-started capture.
    let handle = manager.start(definition).await.unwrap();
    wait_for(&handle, |progress| progress.state.is_terminal()).await;
    assert!(
        evidence.exists(),
        "an explicit start must launch the command"
    );
    assert!(
        handle
            .history()
            .entries
            .iter()
            .any(|record| matches!(record.event, SourceEvent::RestartScheduled { .. })),
        "the restart policy applies to a source started in this session"
    );
}

#[tokio::test]
async fn restoring_a_remembered_endpoint_never_contacts_it() {
    let server = TestServer::start(|_, _| Reply::ok(b"should not happen\n")).await;
    let root = tempdir().unwrap();
    let manager = SourceManager::new(root.path(), config()).unwrap();
    let id = SourceId::new();

    let refused = manager
        .restore(http_source(
            id,
            server.url("/tail"),
            HttpFraming::Newline,
            1,
        ))
        .await
        .err();
    assert!(
        matches!(refused, Some(RuntimeError::RestoreWouldConnect)),
        "restore must refuse a remembered endpoint: {refused:?}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        server.requests().is_empty(),
        "restoration contacted a remembered endpoint"
    );
    assert!(!root.path().join(id.0.to_string()).exists());
}

#[tokio::test]
async fn restoring_a_file_source_is_still_permitted() {
    let root = tempdir().unwrap();
    let path = root.path().join("restored.log");
    std::fs::write(&path, b"kept\n").unwrap();
    let manager = SourceManager::new(root.path().join("capture"), config()).unwrap();
    let handle = manager
        .restore(SourceDefinition {
            schema_version: 1,
            id: SourceId::new(),
            name: "restored file".into(),
            acquisition: Acquisition::File {
                path: path.clone(),
                follow: true,
            },
            identity_hints: BTreeMap::new(),
            retention: None,
        })
        .await
        .expect("restoring a file has no outward side effect");
    wait_for(&handle, |progress| progress.records > 0).await;
    handle.stop().await.unwrap();
}

#[tokio::test]
async fn a_restarted_command_records_every_run_boundary() {
    let root = tempdir().unwrap();
    let manager = SourceManager::new(root.path(), config()).unwrap();
    let handle = manager
        .start(command_source(
            SourceId::new(),
            "echo run; exit 2",
            RestartPolicy::OnFailure,
        ))
        .await
        .unwrap();
    wait_for(&handle, |progress| progress.state.is_terminal()).await;

    let progress = handle.progress();
    assert_eq!(
        progress.boundaries, 2,
        "one original run plus the single permitted restart"
    );
    assert_eq!(progress.exit_code, Some(2));
    let history = handle.history();
    let starts = history
        .entries
        .iter()
        .filter(|record| matches!(record.event, SourceEvent::CommandStarted { .. }))
        .count();
    assert_eq!(starts, 2);
    assert!(
        history.entries.iter().any(|record| matches!(
            record.event,
            SourceEvent::RestartsExhausted { attempts: 1, .. }
        )),
        "{history:?}"
    );
}

#[tokio::test]
async fn a_never_restart_command_keeps_todays_behaviour() {
    let root = tempdir().unwrap();
    let manager = SourceManager::new(root.path(), config()).unwrap();
    let handle = manager
        .start(command_source(
            SourceId::new(),
            "echo once; exit 5",
            RestartPolicy::Never,
        ))
        .await
        .unwrap();
    wait_for(&handle, |progress| progress.state.is_terminal()).await;

    let progress = handle.progress();
    assert_eq!(progress.boundaries, 1);
    assert_eq!(progress.records, 1);
    assert_eq!(progress.exit_code, Some(5));
    assert!(
        handle
            .history()
            .entries
            .iter()
            .any(|record| matches!(record.event, SourceEvent::RestartDeclined { .. }))
    );
}

#[tokio::test]
async fn a_stopped_http_source_settles_without_reconnecting() {
    let server = TestServer::start(|_, _| {
        let mut reply = Reply::default();
        for _ in 0..10_000 {
            reply = reply.chunk(b"line\n", Duration::from_millis(1));
        }
        reply
    })
    .await;
    let root = tempdir().unwrap();
    let manager = SourceManager::new(root.path(), config()).unwrap();
    let handle = manager
        .start(http_source(
            SourceId::new(),
            server.url("/tail"),
            HttpFraming::Newline,
            5,
        ))
        .await
        .unwrap();
    wait_for(&handle, |progress| progress.records > 1).await;
    let report = handle.stop().await.unwrap();
    assert!(report.complete);
    assert_eq!(handle.progress().state, RuntimeState::Stopped);
    let connections = server.requests().len();
    assert_eq!(connections, 1, "an explicit stop must not reconnect");
}

/// The durable history file is bounded and its own path stays inside the
/// source directory.
#[tokio::test]
async fn history_is_written_beside_the_journal_and_survives_the_run() {
    let server = TestServer::start(|_, _| Reply::ok(b"one\n")).await;
    let root = tempdir().unwrap();
    let id = SourceId::new();
    let manager = SourceManager::new(root.path(), config()).unwrap();
    let handle = manager
        .start(http_source(id, server.url("/tail"), HttpFraming::Sse, 0))
        .await
        .unwrap();
    wait_for(&handle, |progress| progress.state.is_terminal()).await;
    manager.shutdown().await;

    let path: &Path = &root.path().join(id.0.to_string()).join("history.jsonl");
    let entries = read_history(path, 1024).unwrap();
    assert!(!entries.is_empty());
    assert!(
        entries
            .iter()
            .any(|record| matches!(record.event, SourceEvent::HttpConnecting { .. })),
        "{entries:?}"
    );
}
