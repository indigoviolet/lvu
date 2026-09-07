//! A real local HTTP/1.1 server for acquisition tests.
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
