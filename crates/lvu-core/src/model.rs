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
    },
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReconnectPolicy {
    pub enabled: bool,
    #[serde(with = "duration_millis")]
    pub delay: Duration,
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
