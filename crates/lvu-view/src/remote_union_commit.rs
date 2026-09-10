//! Window-side transport and canonical candidate commitment for remote unions.
//!
//! The transport only carries commit control data. Journal bytes remain in the
//! shared capture, and Polars/materialization remain in the union worker. A
//! digest identifies one already-retained candidate; it grants no authority
//! and provides no authentication.

use lvu_shared::union_commit::{CommitDigest, CommitReceipt, CommitRequest};
use sha2::{Digest, Sha256};
use std::sync::{Arc, mpsc::Receiver};

/// Non-blocking submission seam implemented by the window's shared-capture
/// controller. The returned one-shot is waited only by the union worker.
pub trait RemoteUnionCommitTransport: Send + Sync {
    fn submit(
        &self,
        expected_worker_session: &str,
        request: CommitRequest,
    ) -> Result<Receiver<Result<CommitReceipt, String>>, String>;
}

#[derive(Clone)]
pub(crate) struct RemoteUnionCommitRegistration {
    pub(crate) window_id: String,
    pub(crate) transport: Arc<dyn RemoteUnionCommitTransport>,
}

/// Domain-separated, length-delimited commitment writer. Callers traverse
/// semantically ordered vectors as-is and sort only maps/sets before writing.
/// This hashes retained values; it never interprets or evaluates them.
pub(crate) struct CandidateDigest(Sha256);

impl CandidateDigest {
    const DOMAIN: &'static [u8] = b"lvu.remote-union.commit.v1\0";

    pub(crate) fn new() -> Self {
        let mut hash = Sha256::new();
        hash.update(Self::DOMAIN);
        Self(hash)
    }

    pub(crate) fn tag(&mut self, value: u8) {
        self.0.update([value]);
    }

    pub(crate) fn bool(&mut self, value: bool) {
        self.tag(u8::from(value));
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.0.update(value.to_be_bytes());
    }

    pub(crate) fn i64(&mut self, value: i64) {
        self.0.update(value.to_be_bytes());
    }

    pub(crate) fn bytes(&mut self, value: &[u8]) {
        self.u64(u64::try_from(value.len()).expect("bounded candidate length fits u64"));
        self.0.update(value);
    }

    pub(crate) fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    pub(crate) fn optional_u64(&mut self, value: Option<u64>) {
        match value {
            Some(value) => {
                self.tag(1);
                self.u64(value);
            }
            None => self.tag(0),
        }
    }

    pub(crate) fn optional_i64(&mut self, value: Option<i64>) {
        match value {
            Some(value) => {
                self.tag(1);
                self.i64(value);
            }
            None => self.tag(0),
        }
    }

    pub(crate) fn json(&mut self, value: &serde_json::Value) {
        match value {
            serde_json::Value::Null => self.tag(0),
            serde_json::Value::Bool(value) => {
                self.tag(1);
                self.bool(*value);
            }
            serde_json::Value::Number(value) => {
                self.tag(2);
                if let Some(value) = value.as_i64() {
                    self.tag(0);
                    self.i64(value);
                } else if let Some(value) = value.as_u64() {
                    self.tag(1);
                    self.u64(value);
                } else if let Some(value) = value.as_f64() {
                    self.tag(2);
                    self.u64(value.to_bits());
                } else {
                    // `serde_json::Number` guarantees one of these exact
                    // numeric projections. Never fall back to rendered JSON:
                    // display text is not commitment authority.
                    unreachable!("JSON number has no numeric projection");
                }
            }
            serde_json::Value::String(value) => {
                self.tag(3);
                self.string(value);
            }
            serde_json::Value::Array(values) => {
                self.tag(4);
                self.u64(u64::try_from(values.len()).expect("bounded candidate length fits u64"));
                for value in values {
                    self.json(value);
                }
            }
            serde_json::Value::Object(values) => {
                self.tag(5);
                self.u64(u64::try_from(values.len()).expect("bounded candidate length fits u64"));
                let mut entries = values.iter().collect::<Vec<_>>();
                entries.sort_by(|left, right| left.0.cmp(right.0));
                for (key, value) in entries {
                    self.string(key);
                    self.json(value);
                }
            }
        }
    }

    pub(crate) fn finish(self) -> CommitDigest {
        self.0.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commitment_distinguishes_missing_null_and_order_but_canonicalizes_maps() {
        let digest = |values: &[(&str, serde_json::Value)]| {
            let mut hash = CandidateDigest::new();
            hash.u64(values.len() as u64);
            for (name, value) in values {
                hash.string(name);
                hash.json(value);
            }
            hash.finish()
        };
        let missing = digest(&[]);
        let null = digest(&[("key", serde_json::Value::Null)]);
        assert_ne!(missing, null);
        assert_ne!(
            digest(&[("a", 1.into()), ("b", 2.into())]),
            digest(&[("b", 2.into()), ("a", 1.into())])
        );

        let left = serde_json::json!({"b": 2, "a": 1});
        let right = serde_json::json!({"a": 1, "b": 2});
        assert_eq!(digest(&[("map", left)]), digest(&[("map", right)]));
    }
}
