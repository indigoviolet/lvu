use lvu_command_enrich::*;
use lvu_core::{
    ChunkPosition, CommandDefinition, CommandProgram, RawRecord, RecordId, RestartPolicy, SourceId,
    StreamKind,
};
use serde_json::{Map, json};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;
fn event(source: SourceId, sequence: u64) -> EnrichmentEvent {
    EnrichmentEvent {
        record: RawRecord {
            record_id: RecordId {
                source_id: source,
                sequence,
            },
            captured_at_unix_nanos: 0,
            stream: StreamKind::File,
            bytes: vec![sequence as u8, 255],
            delimiter: vec![],
            acquisition_id: Uuid::nil(),
            chunk: ChunkPosition::Complete,
        },
        raw: format!("row {sequence}"),
        fields: Map::from_iter([("input".into(), json!({"n":sequence,"nested":[null,true]}))]),
    }
}
fn fixture(temp: &TempDir) -> std::path::PathBuf {
    let p = temp.path().join("helper.py");
    fs::write(&p,r#"import json,sys,time
mode=sys.argv[1]; batch=None; rows=[]
for line in sys.stdin:
 r=json.loads(line); typ=r['type']
 if typ=='batch_begin': batch=r; rows=[]
 elif typ=='event': rows.append(r)
 elif typ=='batch_end':
  if mode=='hang': time.sleep(60)
  if mode=='eof': raise SystemExit
  if mode=='malformed': print('{bad',flush=True); continue
  if mode=='unknown':
   x=dict(rows[0]['event_id']);x['sequence']+=99;print(json.dumps({'type':'event','session':batch['session'],'revision':batch['revision'],'event_id':x,'fields':{}}),flush=True);continue
  for x in reversed(rows):
   fields={'value':x['event_id']['sequence'],'nested':x['fields']['input'],'null':None}
   if mode=='protected' and x['event_id']['sequence']==2: fields={'_lvu_raw':'bad'}
   print(json.dumps({'type':'event','session':batch['session']-(1 if mode=='stale' else 0),'revision':batch['revision'],'event_id':x['event_id'],'fields':fields}),flush=True)
   if mode=='duplicate': time.sleep(.05);print(json.dumps({'type':'event','session':batch['session'],'revision':batch['revision'],'event_id':x['event_id'],'fields':{'again':1}}),flush=True)
  if mode!='missing': print(json.dumps({'type':'batch_complete','session':batch['session'],'revision':batch['revision']}),flush=True)
"#).unwrap();
    p
}
fn runner(path: &Path, mode: &str, timeout: Duration) -> CommandEnricher {
    CommandEnricher::new(
        CommandDefinition {
            program: CommandProgram::Exec {
                executable: "python".into(),
                args: vec![path.display().to_string(), mode.into()],
            },
            cwd: None,
            environment: BTreeMap::new(),
            restart: RestartPolicy::Never,
        },
        4,
        Limits {
            timeout,
            ..Limits::default()
        },
    )
    .unwrap()
}
fn run(
    r: &mut CommandEnricher,
    events: Vec<EnrichmentEvent>,
    ledger: &mut AttemptLedger,
) -> BatchOutcome {
    r.run_batch(4, events, &AtomicBool::new(false), ledger)
        .unwrap()
}

#[test]
fn framed_reordered_typed_outputs_preserve_order_and_bytes() {
    let t = TempDir::new().unwrap();
    let p = fixture(&t);
    let s = SourceId::new();
    let input = vec![event(s, 1), event(s, 2)];
    let bytes = input
        .iter()
        .map(|e| e.record.bytes.clone())
        .collect::<Vec<_>>();
    let mut r = runner(&p, "valid", Duration::from_secs(1));
    let mut a = AttemptLedger::new(8).unwrap();
    let out = run(&mut r, input, &mut a);
    assert!(out.events.iter().all(|e| e.state == OutcomeState::Ready));
    assert_eq!(out.events[0].derived["value"], json!(1));
    assert_eq!(
        out.events[0].derived["nested"],
        json!({"n":1,"nested":[null,true]})
    );
    assert_eq!(
        out.events
            .iter()
            .map(|e| e.event.record.bytes.clone())
            .collect::<Vec<_>>(),
        bytes
    );
}
#[test]
fn delayed_duplicate_is_rejected_before_completion_and_marks_pending() {
    let t = TempDir::new().unwrap();
    let p = fixture(&t);
    let s = SourceId::new();
    let mut r = runner(&p, "duplicate", Duration::from_secs(1));
    let mut a = AttemptLedger::new(8).unwrap();
    let out = run(&mut r, vec![event(s, 1), event(s, 2)], &mut a);
    assert!(out.events.iter().all(|e| e.state == OutcomeState::Error));
    assert!(out.events.iter().all(|e| !e.diagnostics.is_empty()));
    assert!(
        out.events
            .iter()
            .any(|e| e.diagnostics.iter().any(|d| d.code == "duplicate_id"))
    );
}
#[test]
fn missing_completion_unknown_malformed_and_eof_are_visible() {
    for (mode, code) in [
        ("missing", "missing_completion"),
        ("unknown", "protocol_error"),
        ("malformed", "malformed_json"),
        ("eof", "process_exit"),
        ("stale", "protocol_error"),
    ] {
        let t = TempDir::new().unwrap();
        let p = fixture(&t);
        let s = SourceId::new();
        let mut r = runner(&p, mode, Duration::from_millis(120));
        let mut a = AttemptLedger::new(4).unwrap();
        let out = run(&mut r, vec![event(s, 1)], &mut a);
        if mode == "missing" {
            assert_eq!(out.events[0].state, OutcomeState::Ready);
            assert!(
                out.diagnostics.iter().any(|d| d.code == code),
                "{mode}: {:?}",
                out.diagnostics
            );
        } else {
            assert_eq!(out.events[0].state, OutcomeState::Error);
            assert!(
                out.events[0].diagnostics.iter().any(|d| d.code == code),
                "{mode}: {:?}",
                out.events[0].diagnostics
            );
        }
    }
}
#[test]
fn protected_failure_does_not_erase_independent_success() {
    let t = TempDir::new().unwrap();
    let p = fixture(&t);
    let s = SourceId::new();
    let mut r = runner(&p, "protected", Duration::from_secs(1));
    let mut a = AttemptLedger::new(4).unwrap();
    let out = run(&mut r, vec![event(s, 1), event(s, 2)], &mut a);
    assert_eq!(out.events[0].state, OutcomeState::Ready);
    assert_eq!(out.events[1].state, OutcomeState::Error);
    assert!(
        out.events[1]
            .diagnostics
            .iter()
            .any(|d| d.code == "protected_field")
    );
}
#[test]
fn caller_ledger_is_bounded_and_survives_timeout_reset() {
    let t = TempDir::new().unwrap();
    let p = fixture(&t);
    let s = SourceId::new();
    let mut a = AttemptLedger::new(2).unwrap();
    let mut hung = runner(&p, "hang", Duration::from_millis(80));
    let first = run(&mut hung, vec![event(s, 1)], &mut a);
    assert!(
        first.events[0]
            .diagnostics
            .iter()
            .any(|d| d.code == "timeout")
    );
    let mut good = runner(&p, "valid", Duration::from_secs(1));
    let repeated = run(&mut good, vec![event(s, 1)], &mut a);
    assert!(
        repeated.events[0]
            .diagnostics
            .iter()
            .any(|d| d.code == "already_attempted")
    );
    let _ = run(&mut good, vec![event(s, 2)], &mut a);
    let refused = run(&mut good, vec![event(s, 3)], &mut a);
    assert_eq!(a.len(), 2);
    assert!(
        refused.events[0]
            .diagnostics
            .iter()
            .any(|d| d.code == "attempt_capacity")
    );
}
#[test]
fn serialization_is_hard_bounded_and_restart_policy_rejected() {
    let t = TempDir::new().unwrap();
    let p = fixture(&t);
    let s = SourceId::new();
    let mut r = CommandEnricher::new(
        CommandDefinition {
            program: CommandProgram::Exec {
                executable: "python".into(),
                args: vec![p.display().to_string(), "valid".into()],
            },
            cwd: None,
            environment: BTreeMap::new(),
            restart: RestartPolicy::Never,
        },
        4,
        Limits {
            max_input_bytes: 128,
            ..Limits::default()
        },
    )
    .unwrap();
    let mut e = event(s, 1);
    e.raw = "x".repeat(1000000);
    let mut a = AttemptLedger::new(2).unwrap();
    let out = run(&mut r, vec![e], &mut a);
    assert!(
        out.events[0]
            .diagnostics
            .iter()
            .any(|d| d.code == "input_too_large")
    );
    let bad = CommandEnricher::new(
        CommandDefinition {
            program: CommandProgram::Exec {
                executable: "x".into(),
                args: vec![],
            },
            cwd: None,
            environment: BTreeMap::new(),
            restart: RestartPolicy::Always,
        },
        4,
        Limits::default(),
    );
    assert!(matches!(bad, Err(RunnerError::UnsupportedRestartPolicy)));
}

#[test]
fn framed_lifecycle_bounds_blocked_io_stderr_descendants_and_cancel() {
    let t = TempDir::new().unwrap();
    let pid_file = t.path().join("descendant.pid");
    let helper = t.path().join("blocked.py");
    fs::write(
        &helper,
        r#"import subprocess,sys,time
p=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'])
open(sys.argv[1],'w').write(str(p.pid))
sys.stderr.write('e'*200000);sys.stderr.flush()
time.sleep(60)
"#,
    )
    .unwrap();
    let definition = CommandDefinition {
        program: CommandProgram::Exec {
            executable: "python".into(),
            args: vec![helper.display().to_string(), pid_file.display().to_string()],
        },
        cwd: None,
        environment: BTreeMap::new(),
        restart: RestartPolicy::Never,
    };
    let mut blocked = CommandEnricher::new(
        definition,
        4,
        Limits {
            max_input_bytes: 1024 * 1024,
            max_stderr_bytes: 2048,
            timeout: Duration::from_millis(120),
            ..Limits::default()
        },
    )
    .unwrap();
    let source = SourceId::new();
    let mut large = event(source, 1);
    large.raw = "x".repeat(512_000);
    let mut attempts = AttemptLedger::new(8).unwrap();
    let started = Instant::now();
    let outcome = run(&mut blocked, vec![large], &mut attempts);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(
        outcome.events[0]
            .diagnostics
            .iter()
            .any(|d| d.code == "timeout")
    );
    assert!(blocked.stderr_snapshot().len() <= 2048);
    let descendant: u32 = fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    let status = fs::read_to_string(format!("/proc/{descendant}/status")).unwrap_or_default();
    assert!(status.is_empty() || status.contains("State:\tZ"));

    let framed = fixture(&t);
    let mut cancellable = runner(&framed, "hang", Duration::from_secs(2));
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&cancelled);
    let setter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(30));
        signal.store(true, Ordering::Release);
    });
    let mut attempts = AttemptLedger::new(8).unwrap();
    let started = Instant::now();
    let outcome = cancellable
        .run_batch(4, vec![event(source, 2)], cancelled.as_ref(), &mut attempts)
        .unwrap();
    setter.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(
        outcome.events[0]
            .diagnostics
            .iter()
            .any(|d| d.code == "cancelled")
    );
}
