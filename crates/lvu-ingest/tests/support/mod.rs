//! A real local HTTP/1.1 server for acquisition tests.
//!
//! Shared shape with the core acquisition fixture; not every crate exercises
//! every reply form.
#![allow(dead_code)]
//!
//! Capture is exercised against actual sockets, actual chunked framing and
//! actual mid-stream disconnects rather than a substituted transport, because
//! the behaviour under test *is* the transport handling.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// One request as the server saw it.
#[derive(Clone, Debug, Default)]
pub struct SeenRequest {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
}

impl SeenRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// What the server does with one connection.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status_line: String,
    pub headers: Vec<String>,
    /// Body pieces written as chunked-encoding chunks, with a pause before each.
    pub chunks: Vec<(Vec<u8>, Duration)>,
    /// Drop the socket without the terminating zero chunk.
    pub abrupt: bool,
    /// Accept the connection and send nothing at all.
    pub silent: bool,
}

impl Default for Reply {
    fn default() -> Self {
        Self {
            status_line: "HTTP/1.1 200 OK".to_owned(),
            headers: vec!["Content-Type: text/plain".to_owned()],
            chunks: Vec::new(),
            abrupt: false,
            silent: false,
        }
    }
}

impl Reply {
    pub fn ok(body: &[u8]) -> Self {
        Self {
            chunks: vec![(body.to_vec(), Duration::ZERO)],
            ..Self::default()
        }
    }

    pub fn status(code: u16, reason: &str) -> Self {
        Self {
            status_line: format!("HTTP/1.1 {code} {reason}"),
            chunks: vec![(b"denied".to_vec(), Duration::ZERO)],
            ..Self::default()
        }
    }

    pub fn silent() -> Self {
        Self {
            silent: true,
            ..Self::default()
        }
    }

    pub fn abrupt(body: &[u8]) -> Self {
        Self {
            chunks: vec![(body.to_vec(), Duration::ZERO)],
            abrupt: true,
            ..Self::default()
        }
    }

    pub fn header(mut self, header: &str) -> Self {
        self.headers.push(header.to_owned());
        self
    }

    pub fn chunk(mut self, body: &[u8], pause: Duration) -> Self {
        self.chunks.push((body.to_vec(), pause));
        self
    }
}

type Handler = Arc<dyn Fn(usize, &SeenRequest) -> Reply + Send + Sync>;

pub struct TestServer {
    address: SocketAddr,
    seen: Arc<Mutex<Vec<SeenRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TestServer {
    /// Starts a server whose reply depends on the connection ordinal and the
    /// request it received.
    pub async fn start(
        handler: impl Fn(usize, &SeenRequest) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler: Handler = Arc::new(handler);
        let task = tokio::spawn({
            let seen = seen.clone();
            async move {
                let mut index = 0;
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    let handler = handler.clone();
                    let seen = seen.clone();
                    let ordinal = index;
                    index += 1;
                    tokio::spawn(async move {
                        let _ = serve(stream, ordinal, handler, seen).await;
                    });
                }
            }
        });
        Self {
            address,
            seen,
            task,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    pub fn requests(&self) -> Vec<SeenRequest> {
        self.seen.lock().unwrap().clone()
    }
}

async fn serve(
    mut stream: TcpStream,
    ordinal: usize,
    handler: Handler,
    seen: Arc<Mutex<Vec<SeenRequest>>>,
) -> std::io::Result<()> {
    let request = read_request(&mut stream).await?;
    seen.lock().unwrap().push(request.clone());
    let reply = handler(ordinal, &request);
    if reply.silent {
        // Hold the connection open without writing a single byte.
        tokio::time::sleep(Duration::from_secs(3600)).await;
        return Ok(());
    }
    let mut head = String::new();
    head.push_str(&reply.status_line);
    head.push_str("\r\n");
    for header in &reply.headers {
        head.push_str(header);
        head.push_str("\r\n");
    }
    head.push_str("Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.flush().await?;
    for (body, pause) in &reply.chunks {
        if !pause.is_zero() {
            tokio::time::sleep(*pause).await;
        }
        if body.is_empty() {
            continue;
        }
        stream
            .write_all(format!("{:x}\r\n", body.len()).as_bytes())
            .await?;
        stream.write_all(body).await?;
        stream.write_all(b"\r\n").await?;
        stream.flush().await?;
    }
    if reply.abrupt {
        // No terminating chunk: the client observes a mid-stream disconnect.
        drop(stream);
        return Ok(());
    }
    stream.write_all(b"0\r\n\r\n").await?;
    stream.flush().await?;
    stream.shutdown().await
}

async fn read_request(stream: &mut TcpStream) -> std::io::Result<SeenRequest> {
    let mut buffer = Vec::new();
    let mut byte = [0_u8; 1];
    while !buffer.ends_with(b"\r\n\r\n") {
        if buffer.len() > 16 * 1024 {
            return Err(std::io::Error::other("request head too large"));
        }
        if stream.read(&mut byte).await? == 0 {
            break;
        }
        buffer.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&buffer).into_owned();
    let mut lines = text.split("\r\n");
    let mut request = SeenRequest::default();
    if let Some(start) = lines.next() {
        let mut parts = start.split_whitespace();
        request.method = parts.next().unwrap_or_default().to_owned();
        request.target = parts.next().unwrap_or_default().to_owned();
    }
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            request
                .headers
                .push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    Ok(request)
}

// ---------------------------------------------------------------------------
// Cost accounting for the throughput test.
// ---------------------------------------------------------------------------

use std::time::Duration as StdDuration;

/// Bytes of source per CPU-second the capture path must sustain.
///
/// A floor, not a target: it is set below what the machine actually achieves so
/// that ordinary variance cannot fail it, while a change that adds a syscall or
/// an allocation per record — the regressions this path has actually had — puts
/// the ratio through it. Raised from 12 to 28 as the hand-over moved to one
/// event per read and framing stopped walking a byte at a time: the same
/// capture measured 44.6 to 49.0 across consecutive runs, so this sits well
/// under the spread rather than inside it.
pub const THROUGHPUT_FLOOR_BYTES_PER_CPU_SECOND: f64 = 28.0 * 1_048_576.0;

/// Records the pipeline must move per hand-over.
///
/// The hand-over cost is paid per event — two bounded channels and a semaphore
/// permit — so what matters is how many records an event carries. One per
/// record made the hand-over cost more than framing and journalling together.
/// A ratio counts decisions rather than time, so unlike a CPU-second figure it
/// does not move with what else the machine is doing: a return to one record
/// per event puts it at 1. The shipped 256 KB read carries about 2,300
/// hundred-byte records, so this floor also catches the read shrinking back to
/// something that makes the hand-over frequent again.
pub const RECORDS_PER_HANDOVER_FLOOR: f64 = 64.0;

/// Durable commits per MB of source the capture path may cost.
///
/// The number the group commit exists to hold down, and the one a busy machine
/// cannot move: it counts decisions, not time. Counting commits per *batch*
/// rather than per record put this at 24.7, and on a volume where fsync costs
/// tens of milliseconds that was the whole cost of capture.
pub const COMMITS_PER_MB_CEILING: f64 = 6.0;

/// Journal bytes per record beyond the record's own bytes.
pub const JOURNAL_OVERHEAD_CEILING_BYTES: f64 = 80.0;

/// Process CPU time, split into user and system so the phase table can say
/// whether the cost is work or syscalls.
#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub user: StdDuration,
    pub system: StdDuration,
}

impl Usage {
    pub fn now() -> Self {
        read_usage().unwrap_or_default()
    }

    pub fn since(self, before: Self) -> Self {
        Self {
            user: self.user.saturating_sub(before.user),
            system: self.system.saturating_sub(before.system),
        }
    }

    pub fn total(self) -> StdDuration {
        self.user + self.system
    }
}

/// `getrusage(RUSAGE_SELF)` sums every thread of this process, which is what a
/// capture spread over a reader task, a blocking writer thread and the runtime
/// needs. Microsecond resolution, unlike `/proc/self/stat`'s 10ms ticks, so a
/// run of a few CPU-seconds is measured rather than rounded.
#[cfg(unix)]
fn read_usage() -> Option<Usage> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage writes a whole `rusage` through this pointer and
    // reports failure through its return value; nothing else aliases it.
    let ok = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0;
    if !ok {
        return None;
    }
    // SAFETY: getrusage returned success, so the value is initialised.
    let usage = unsafe { usage.assume_init() };
    Some(Usage {
        user: timeval(usage.ru_utime),
        system: timeval(usage.ru_stime),
    })
}

#[cfg(unix)]
fn timeval(value: libc::timeval) -> StdDuration {
    StdDuration::new(
        value.tv_sec.max(0) as u64,
        (value.tv_usec.max(0) as u32).saturating_mul(1_000),
    )
}

#[cfg(not(unix))]
fn read_usage() -> Option<Usage> {
    None
}

/// What one measured capture cost.
#[derive(Clone, Copy, Debug)]
pub struct Cost {
    pub source_bytes: u64,
    pub records: u64,
    pub journal_bytes: u64,
    pub syncs: u64,
    pub handovers: u64,
    pub elapsed: StdDuration,
    pub usage: Usage,
}

impl Cost {
    pub fn bytes_per_cpu_second(&self) -> f64 {
        let cpu = self.usage.total().as_secs_f64();
        if cpu <= 0.0 {
            return f64::INFINITY;
        }
        self.source_bytes as f64 / cpu
    }

    pub fn bytes_per_wall_second(&self) -> f64 {
        let wall = self.elapsed.as_secs_f64();
        if wall <= 0.0 {
            return f64::INFINITY;
        }
        self.source_bytes as f64 / wall
    }

    /// Records per hand-over across the acquisition channel.
    pub fn records_per_handover(&self) -> f64 {
        if self.handovers == 0 {
            return 0.0;
        }
        self.records as f64 / self.handovers as f64
    }

    /// Durable commits per MB of source. The number the fsync policy exists to
    /// control, and the one a regression in it moves first.
    pub fn syncs_per_mb(&self) -> f64 {
        let megabytes = self.source_bytes as f64 / 1_048_576.0;
        if megabytes <= 0.0 {
            return 0.0;
        }
        self.syncs as f64 / megabytes
    }

    pub fn journal_overhead_per_record(&self) -> f64 {
        if self.records == 0 {
            return 0.0;
        }
        (self.journal_bytes.saturating_sub(self.source_bytes)) as f64 / self.records as f64
    }

    pub fn report(&self) -> String {
        format!(
            "capture: {:.1} MB source, {} records ({:.0} bytes/record)\n\
             wall    {:>8.2}s  {:>8.2} MB/s\n\
             cpu     {:>8.2}s  {:>8.2} MB/CPU-s  (user {:.2}s, system {:.2}s)\n\
             journal {:>8.1} MB  {:.2}x source, {:.0} bytes/record overhead\n\
             fsync   {:>8}    {:.1} per MB, one per {:.0} records\n\
             handover{:>8}    {:.0} records each",
            self.source_bytes as f64 / 1_048_576.0,
            self.records,
            self.source_bytes as f64 / self.records.max(1) as f64,
            self.elapsed.as_secs_f64(),
            self.bytes_per_wall_second() / 1_048_576.0,
            self.usage.total().as_secs_f64(),
            self.bytes_per_cpu_second() / 1_048_576.0,
            self.usage.user.as_secs_f64(),
            self.usage.system.as_secs_f64(),
            self.journal_bytes as f64 / 1_048_576.0,
            self.journal_bytes as f64 / self.source_bytes.max(1) as f64,
            self.journal_overhead_per_record(),
            self.syncs,
            self.syncs_per_mb(),
            self.records as f64 / self.syncs.max(1) as f64,
            self.handovers,
            self.records_per_handover(),
        )
    }
}

/// Reads the whole journal back through the runtime's own paging API and
/// concatenates each record's bytes and delimiter, which for a file source is
/// exactly the source's bytes if capture preserved them.
pub async fn replay(handle: &lvu_ingest::SourceHandle, records: u64) -> Vec<u8> {
    let mut replayed = Vec::new();
    let mut offset = 0_u64;
    let mut seen = 0_u64;
    while seen < records {
        let page = handle
            .read_page(offset, 4096, 4 * 1024 * 1024)
            .await
            .expect("read page");
        if page.records.is_empty() {
            break;
        }
        for record in &page.records {
            replayed.extend_from_slice(&record.bytes);
            replayed.extend_from_slice(&record.delimiter);
        }
        seen += page.records.len() as u64;
        offset = page.next_offset;
    }
    replayed
}
