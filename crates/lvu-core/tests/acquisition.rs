use lvu_core::{
    CaptureEvent, ChunkPosition, CommandDefinition, CommandProgram, RestartPolicy, StreamKind,
    acquisition::{BoundaryReason, CaptureLimits, capture_command, capture_file},
};
use std::{collections::BTreeMap, fs, io::Write, time::Duration};
use tempfile::tempdir;

fn limits() -> CaptureLimits {
    CaptureLimits {
        channel_capacity: 4,
        read_chunk_bytes: 3,
        maximum_record_bytes: 4,
        poll_interval: Duration::from_millis(10),
        partial_flush_interval: Duration::from_millis(20),
    }
}
async fn receive_until_closed(
    mut rx: tokio::sync::mpsc::Receiver<CaptureEvent>,
) -> Vec<CaptureEvent> {
    let mut v = vec![];
    while let Ok(Some(e)) = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await {
        v.push(e)
    }
    v
}

async fn next_event(rx: &mut tokio::sync::mpsc::Receiver<CaptureEvent>) -> CaptureEvent {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("capture event timed out")
        .expect("capture ended early")
}

async fn expect_boundary(
    rx: &mut tokio::sync::mpsc::Receiver<CaptureEvent>,
    expected: BoundaryReason,
) {
    loop {
        if let CaptureEvent::Boundary { reason, .. } = next_event(rx).await {
            assert_eq!(reason, expected);
            return;
        }
    }
}

async fn expect_file_record(rx: &mut tokio::sync::mpsc::Receiver<CaptureEvent>, expected: &[u8]) {
    loop {
        if let CaptureEvent::Record(record) = next_event(rx).await {
            assert_eq!(record.stream, StreamKind::File);
            assert_eq!(record.bytes, expected);
            assert_eq!(record.delimiter, b"\n");
            return;
        }
    }
}

#[tokio::test]
async fn file_capture_preserves_partial_invalid_and_long_lines() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("fixture.log");
    fs::write(&path, b"ab\xff\r\n123456789\nlast").unwrap();
    let (handle, rx) = capture_file(path, false, limits()).unwrap();
    let events = receive_until_closed(rx).await;
    handle.wait().await.unwrap();
    let records: Vec<_> = events
        .into_iter()
        .filter_map(|e| {
            if let CaptureEvent::Record(r) = e {
                Some(r)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        records
            .iter()
            .flat_map(|r| r.bytes.iter().chain(r.delimiter.iter()))
            .copied()
            .collect::<Vec<_>>(),
        b"ab\xff\r\n123456789\nlast"
    );
    assert_eq!(
        records.iter().map(|r| r.chunk).collect::<Vec<_>>(),
        [
            ChunkPosition::Complete,
            ChunkPosition::Start,
            ChunkPosition::Continue,
            ChunkPosition::End,
            ChunkPosition::Complete
        ]
    );
}

#[tokio::test]
async fn follow_observes_append_rotation_and_truncation_boundaries() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("follow.log");
    fs::write(&path, b"old\n").unwrap();
    let mut generous = limits();
    generous.channel_capacity = 64;
    let (mut handle, mut rx) = capture_file(path.clone(), true, generous).unwrap();
    expect_boundary(&mut rx, BoundaryReason::Started).await;
    expect_file_record(&mut rx, b"old").await;
    let mut rotated_writer = fs::OpenOptions::new().append(true).open(&path).unwrap();
    fs::write(dir.path().join("new.tmp"), b"new\n").unwrap();
    fs::rename(dir.path().join("new.tmp"), &path).unwrap();
    rotated_writer.write_all(b"late\n").unwrap();
    rotated_writer.flush().unwrap();
    expect_file_record(&mut rx, b"late").await;
    expect_boundary(&mut rx, BoundaryReason::Rotated).await;
    expect_file_record(&mut rx, b"new").await;
    fs::write(&path, b"x\n").unwrap();
    expect_boundary(&mut rx, BoundaryReason::Truncated).await;
    expect_file_record(&mut rx, b"x").await;
    handle.cancel();
    handle.wait().await.unwrap();
}

#[tokio::test]
async fn command_captures_both_streams_exit_and_cancellation_reaps() {
    let def = CommandDefinition {
        program: CommandProgram::Exec {
            executable: "sh".into(),
            args: vec![
                "-c".into(),
                "printf 'out\\n'; printf 'err\\n' >&2; exit 7".into(),
            ],
        },
        cwd: None,
        environment: BTreeMap::new(),
        restart: RestartPolicy::Never,
    };
    let (handle, rx) = capture_command(def, limits()).unwrap();
    let events = receive_until_closed(rx).await;
    handle.wait().await.unwrap();
    assert!(events.iter().any(
        |e| matches!(e,CaptureEvent::Record(r) if r.stream==StreamKind::Stdout&&r.bytes==b"out")
    ));
    assert!(events.iter().any(
        |e| matches!(e,CaptureEvent::Record(r) if r.stream==StreamKind::Stderr&&r.bytes==b"err")
    ));
    assert!(
        events
            .iter()
            .any(|e| matches!(e,CaptureEvent::CommandExit{status,..} if status.code()==Some(7)))
    );
    let def = CommandDefinition {
        program: CommandProgram::Exec {
            executable: "sh".into(),
            args: vec!["-c".into(), "sleep 30".into()],
        },
        cwd: None,
        environment: BTreeMap::new(),
        restart: RestartPolicy::Never,
    };
    let (mut handle, mut rx) = capture_command(def, limits()).unwrap();
    handle.cancel();
    tokio::time::timeout(Duration::from_secs(2), handle.wait())
        .await
        .unwrap()
        .unwrap();
    while rx.recv().await.is_some() {}
}

#[tokio::test]
async fn cancellation_does_not_wait_for_a_full_file_queue() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("full.log");
    fs::write(&path, b"one\ntwo\nthree\n").unwrap();
    let constrained = CaptureLimits {
        channel_capacity: 1,
        ..CaptureLimits::default()
    };
    let (mut handle, receiver) = capture_file(path, true, constrained).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.cancel();
    tokio::time::timeout(Duration::from_secs(1), handle.wait())
        .await
        .unwrap()
        .unwrap();
    drop(receiver);
}

#[tokio::test]
async fn cancellation_does_not_wait_for_a_full_command_queue() {
    let definition = CommandDefinition {
        program: CommandProgram::Exec {
            executable: "sh".into(),
            args: vec!["-c".into(), "while :; do echo output; done".into()],
        },
        cwd: None,
        environment: BTreeMap::new(),
        restart: RestartPolicy::Never,
    };
    let constrained = CaptureLimits {
        channel_capacity: 1,
        ..CaptureLimits::default()
    };
    let (mut handle, receiver) = capture_command(definition, constrained).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.cancel();
    tokio::time::timeout(Duration::from_secs(1), handle.wait())
        .await
        .unwrap()
        .unwrap();
    drop(receiver);
}

#[tokio::test]
async fn cancellation_after_pipe_eof_reaps_the_process_tree() {
    let dir = tempdir().unwrap();
    let script = "echo $$ > parent.pid; sleep 30 & echo $! > child.pid; exec 1>&- 2>&-; wait";
    let definition = CommandDefinition {
        program: CommandProgram::Exec {
            executable: "sh".into(),
            args: vec!["-c".into(), script.into()],
        },
        cwd: Some(dir.path().to_owned()),
        environment: BTreeMap::new(),
        restart: RestartPolicy::Never,
    };
    let (mut handle, receiver) = capture_command(definition, CaptureLimits::default()).unwrap();
    let pid_files = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let parent = read_pid(&dir.path().join("parent.pid"));
            let child = read_pid(&dir.path().join("child.pid"));
            if let (Some(parent), Some(child)) = (parent, child) {
                break (parent, child);
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    handle.cancel();
    tokio::time::timeout(Duration::from_secs(1), handle.wait())
        .await
        .unwrap()
        .unwrap();
    drop(receiver);
    let (parent, child) = pid_files.expect("command did not publish complete parseable PID files");
    wait_for_process_exit(parent).await;
    wait_for_process_exit(child).await;
}

#[tokio::test]
async fn non_never_restart_is_explicitly_unsupported() {
    let definition = CommandDefinition {
        program: CommandProgram::Exec {
            executable: "true".into(),
            args: vec![],
        },
        cwd: None,
        environment: BTreeMap::new(),
        restart: RestartPolicy::Always,
    };
    assert!(capture_command(definition, CaptureLimits::default()).is_err());
}

#[tokio::test]
async fn dropping_receivers_stops_owned_work() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("idle.log");
    fs::write(&path, b"").unwrap();
    let (file_handle, file_receiver) = capture_file(path, true, limits()).unwrap();
    drop(file_receiver);
    tokio::time::timeout(Duration::from_secs(1), file_handle.wait())
        .await
        .unwrap()
        .unwrap();

    let definition = CommandDefinition {
        program: CommandProgram::Exec {
            executable: "sh".into(),
            args: vec!["-c".into(), "sleep 30".into()],
        },
        cwd: None,
        environment: BTreeMap::new(),
        restart: RestartPolicy::Never,
    };
    let (command_handle, command_receiver) = capture_command(definition, limits()).unwrap();
    drop(command_receiver);
    tokio::time::timeout(Duration::from_secs(1), command_handle.wait())
        .await
        .unwrap()
        .unwrap();
}

fn read_pid(path: &std::path::Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

async fn wait_for_process_exit(process_id: u32) {
    let path = std::path::PathBuf::from(format!("/proc/{process_id}"));
    tokio::time::timeout(Duration::from_secs(2), async {
        while path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
