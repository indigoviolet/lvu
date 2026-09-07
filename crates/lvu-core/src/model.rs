use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);
        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}
uuid_id!(SourceId);
uuid_id!(ViewId);
uuid_id!(RecipeId);
uuid_id!(InvestigationId);
uuid_id!(SegmentId);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct RecordId {
    pub source_id: SourceId,
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Stdout,
    Stderr,
    File,
    Http,
    Stdin,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    Starting,
    Running,
    Paused,
    Backpressured,
    Exited,
    Disconnected,
    StorageBlocked,
    Error,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawRecord {
    pub record_id: RecordId,
    pub captured_at_unix_nanos: i64,
    pub stream: StreamKind,
    pub bytes: Vec<u8>,
    pub delimiter: Vec<u8>,
    pub acquisition_id: Uuid,
    pub chunk: crate::acquisition::ChunkPosition,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceDefinition {
    pub schema_version: u32,
    pub id: SourceId,
    pub name: String,
    #[serde(flatten)]
    pub acquisition: Acquisition,
    #[serde(default)]
    pub identity_hints: BTreeMap<String, String>,
    pub retention: Option<RetentionPolicy>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Acquisition {
    Stdin,
    File {
        path: PathBuf,
        follow: bool,
    },
    Command {
        command: CommandDefinition,
    },
    Http {
        url: String,
        framing: HttpFraming,
        reconnect: ReconnectPolicy,
        /// Request headers sent on every connection. Values commonly carry
        /// credentials, so they are never rendered into status or history.
        #[serde(default)]
        headers: Vec<HttpHeader>,
        #[serde(default)]
        limits: HttpLimits,
    },
}

/// One request header. `Debug` deliberately redacts the value so a definition
/// can be logged without leaking a bearer token or basic-auth credential.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

impl HttpHeader {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

impl std::fmt::Debug for HttpHeader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpHeader")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Per-source HTTP capture bounds. Every field is a hard cap; nothing in the
/// capture path is unbounded.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpLimits {
    /// Largest single newline record or SSE event body retained before the
    /// frame is emitted as bounded fragments.
    pub maximum_frame_bytes: usize,
    /// Largest amount of undelivered body bytes held in the framer at once.
    pub maximum_pending_bytes: usize,
    #[serde(with = "duration_millis")]
    pub connect_timeout: Duration,
    /// Maximum silence between body bytes before the connection is treated as
    /// dead. A server that never sends anything cannot stall capture forever.
    #[serde(with = "duration_millis")]
    pub read_timeout: Duration,
}

impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            maximum_frame_bytes: 64 * 1024,
            maximum_pending_bytes: 4 * 1024 * 1024,
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandProgram {
    Shell {
        text: String,
    },
    Exec {
        executable: PathBuf,
        args: Vec<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandDefinition {
    pub program: CommandProgram,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub restart: RestartPolicy,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    #[default]
    Never,
    OnFailure,
    Always,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpFraming {
    Newline,
    Sse,
}

/// Bounded reconnect behaviour for an HTTP source. Reconnection is explicit:
/// every attempt, failure and resulting capture gap is published as source
/// history, never retried silently.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReconnectPolicy {
    pub enabled: bool,
    /// First backoff delay; later attempts double it up to `maximum_delay`.
    #[serde(with = "duration_millis")]
    pub delay: Duration,
    #[serde(default = "default_maximum_delay", with = "duration_millis")]
    pub maximum_delay: Duration,
    /// Proportion of each delay that is randomised downward, 0..=100.
    #[serde(default = "default_jitter_percent")]
    pub jitter_percent: u8,
    /// Attempts permitted inside `attempt_window` before capture gives up.
    #[serde(default = "default_maximum_attempts")]
    pub maximum_attempts: u32,
    #[serde(default = "default_attempt_window", with = "duration_millis")]
    pub attempt_window: Duration,
    /// Ask the protocol to resume (SSE `Last-Event-ID`, HTTP `Range`) when the
    /// server allows it. A reconnect that cannot resume records a gap.
    #[serde(default = "default_resume")]
    pub resume: bool,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            delay: Duration::from_millis(500),
            maximum_delay: default_maximum_delay(),
            jitter_percent: default_jitter_percent(),
            maximum_attempts: default_maximum_attempts(),
            attempt_window: default_attempt_window(),
            resume: true,
        }
    }
}

fn default_maximum_delay() -> Duration {
    Duration::from_secs(30)
}
fn default_jitter_percent() -> u8 {
    25
}
fn default_maximum_attempts() -> u32 {
    10
}
fn default_attempt_window() -> Duration {
    Duration::from_secs(300)
}
fn default_resume() -> bool {
    true
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    pub maximum_bytes: Option<u64>,
    pub maximum_age_seconds: Option<u64>,
}

mod duration_millis {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;
    pub fn serialize<S: Serializer>(v: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(v.as_millis().try_into().unwrap_or(u64::MAX))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_millis(u64::deserialize(d)?))
    }
}
