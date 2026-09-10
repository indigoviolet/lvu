//! Bounded JSON-lines framing for the local control socket.
//!
//! One message is one line: UTF-8 JSON, `\n` terminated, at most
//! [`crate::MAX_CONTROL_MESSAGE_BYTES`] bytes on the wire. The decoder is a
//! small explicit state machine: it buffers partial reads, yields complete
//! values, and fails closed (errors, discards nothing silently — the caller
//! drops the connection) on oversize, invalid UTF-8, or malformed JSON.

use crate::MAX_CONTROL_MESSAGE_BYTES;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrameError {
    /// Encoded message (plus terminator) exceeds the bound.
    TooLarge { bytes: usize },
    /// Buffered input exceeded the bound without a terminator.
    Overflow,
    /// Complete line is not valid UTF-8 JSON.
    Malformed(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLarge { bytes } => write!(
                formatter,
                "control message is {bytes} bytes; limit is {MAX_CONTROL_MESSAGE_BYTES}"
            ),
            FrameError::Overflow => write!(
                formatter,
                "control input exceeded {MAX_CONTROL_MESSAGE_BYTES} bytes without a terminator"
            ),
            FrameError::Malformed(error) => write!(formatter, "malformed control message: {error}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Encode one JSON value as a bounded line, terminator included.
pub fn encode_frame(value: &serde_json::Value) -> Result<Vec<u8>, FrameError> {
    let mut bytes =
        serde_json::to_vec(value).map_err(|error| FrameError::Malformed(error.to_string()))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_CONTROL_MESSAGE_BYTES {
        return Err(FrameError::TooLarge { bytes: bytes.len() });
    }
    Ok(bytes)
}

/// Decode exactly one complete line (terminator already split off); the
/// caller must have split framing. Enforces the same wire cap as the
/// streaming decoder: a line whose bytes plus terminator exceed the bound
/// is rejected before JSON decoding, so no call path accepts oversize.
pub fn decode_frame(line: &[u8]) -> Result<serde_json::Value, FrameError> {
    if line.len() + 1 > MAX_CONTROL_MESSAGE_BYTES {
        return Err(FrameError::TooLarge {
            bytes: line.len() + 1,
        });
    }
    serde_json::from_slice(line).map_err(|error| FrameError::Malformed(error.to_string()))
}

/// Incremental decoder fed with arbitrary read chunks.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffered: Vec<u8>,
}

/// The bound is enforced *before* allocation and *before* JSON decoding, on
/// every path: a complete line longer than the cap (with terminator) is
/// rejected without parsing it; an unterminated remainder at or above the
/// cap can never complete and fails fast. Complete lines stream out of each
/// call, so a chunk holding many small frames is processed, never buffered
/// wholesale: only the trailing partial remainder is retained, always below
/// the cap. On the first protocol violation the connection is unusable: the
/// error is returned and state is left untouched for diagnosis.
impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> Result<Vec<serde_json::Value>, FrameError> {
        let mut values = Vec::new();
        let mut rest = bytes;
        // Finish a previously partial line first, within budget.
        if !self.buffered.is_empty() {
            match rest.iter().position(|byte| *byte == b'\n') {
                Some(position) => {
                    let wire = self.buffered.len() + position + 1;
                    if wire > MAX_CONTROL_MESSAGE_BYTES {
                        return Err(FrameError::TooLarge { bytes: wire });
                    }
                    self.buffered.extend_from_slice(&rest[..=position]);
                    rest = &rest[position + 1..];
                    let line = std::mem::take(&mut self.buffered);
                    values.push(decode_frame(&line[..line.len() - 1])?);
                }
                None => {
                    if self.buffered.len() + rest.len() >= MAX_CONTROL_MESSAGE_BYTES {
                        return Err(FrameError::Overflow);
                    }
                    self.buffered.extend_from_slice(rest);
                    return Ok(values);
                }
            }
        }
        // The buffer is empty: split complete lines out of the caller's
        // slice directly, never copying them into our own allocation.
        while let Some(position) = rest.iter().position(|byte| *byte == b'\n') {
            values.push(decode_frame(&rest[..position])?);
            rest = &rest[position + 1..];
        }
        // Only an unterminated tail is retained, and only while it can
        // still complete within the bound.
        if !rest.is_empty() {
            if rest.len() >= MAX_CONTROL_MESSAGE_BYTES {
                return Err(FrameError::Overflow);
            }
            self.buffered.extend_from_slice(rest);
        }
        Ok(values)
    }

    pub fn buffered_bytes(&self) -> usize {
        self.buffered.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip_preserves_object_values() {
        let value = json!({"method": "hello", "request_id": "7", "window_pid": 1234});
        let bytes = encode_frame(&value).unwrap();
        assert!(bytes.ends_with(b"\n"));
        assert_eq!(decode_frame(&bytes[..bytes.len() - 1]).unwrap(), value);
    }

    #[test]
    fn split_feeds_reassemble_in_order() {
        let first = encode_frame(&json!({"id": 1})).unwrap();
        let second = encode_frame(&json!({"id": 2})).unwrap();
        let mut decoder = FrameDecoder::new();
        let mut out = Vec::new();
        let stream: Vec<u8> = first.iter().chain(second.iter()).copied().collect();
        for chunk in stream.chunks(3) {
            out.extend(decoder.push_bytes(chunk).unwrap());
        }
        assert_eq!(out, vec![json!({"id": 1}), json!({"id": 2})]);
        assert_eq!(decoder.buffered_bytes(), 0);
    }

    #[test]
    fn oversize_encode_and_overflow_decode_fail_closed() {
        let big = serde_json::Value::String("x".repeat(MAX_CONTROL_MESSAGE_BYTES));
        assert!(matches!(
            encode_frame(&big),
            Err(FrameError::TooLarge { .. })
        ));
        let mut decoder = FrameDecoder::new();
        let chunk = vec![b'a'; MAX_CONTROL_MESSAGE_BYTES];
        assert_eq!(decoder.push_bytes(&chunk), Err(FrameError::Overflow));
    }

    #[test]
    fn exact_bound_message_is_accepted_bound_plus_one_is_not() {
        // A JSON string of exactly MAX-3 chars encodes to `"..."` plus `\n`:
        // total wire bytes exactly MAX_CONTROL_MESSAGE_BYTES.
        let exact = serde_json::Value::String("x".repeat(MAX_CONTROL_MESSAGE_BYTES - 3));
        let wire = encode_frame(&exact).unwrap();
        assert_eq!(wire.len(), MAX_CONTROL_MESSAGE_BYTES);
        assert_eq!(decode_frame(&wire[..wire.len() - 1]).unwrap(), exact);
        let mut decoder = FrameDecoder::new();
        assert_eq!(decoder.push_bytes(&wire).unwrap(), vec![exact]);
        // One byte more, terminated, is rejected before JSON decoding.
        let mut over = vec![b'"'];
        over.extend(std::iter::repeat_n(b'x', MAX_CONTROL_MESSAGE_BYTES - 2));
        over.extend_from_slice(b"\"\n");
        assert_eq!(over.len(), MAX_CONTROL_MESSAGE_BYTES + 1);
        assert!(matches!(
            decode_frame(&over[..over.len() - 1]),
            Err(FrameError::TooLarge { bytes }) if bytes == MAX_CONTROL_MESSAGE_BYTES + 1
        ));
        let mut decoder = FrameDecoder::new();
        assert!(matches!(
            decoder.push_bytes(&over),
            Err(FrameError::TooLarge { .. })
        ));
    }

    #[test]
    fn huge_single_chunk_fails_without_buffering_it() {
        let mut decoder = FrameDecoder::new();
        let chunk = vec![b'a'; MAX_CONTROL_MESSAGE_BYTES * 4];
        assert_eq!(decoder.push_bytes(&chunk), Err(FrameError::Overflow));
        assert_eq!(decoder.buffered_bytes(), 0);
    }

    #[test]
    fn many_small_frames_stream_out_of_one_call() {
        let mut stream = Vec::new();
        for id in 0..1000u32 {
            stream.extend(encode_frame(&json!({"id": id})).unwrap());
        }
        let mut decoder = FrameDecoder::new();
        let values = decoder.push_bytes(&stream).unwrap();
        assert_eq!(values.len(), 1000);
        assert_eq!(decoder.buffered_bytes(), 0);
    }

    #[test]
    fn oversize_line_split_across_feeds_is_rejected() {
        let mut decoder = FrameDecoder::new();
        let head = vec![b'"'; MAX_CONTROL_MESSAGE_BYTES - 10];
        assert_eq!(
            decoder.push_bytes(&head).unwrap(),
            Vec::<serde_json::Value>::new()
        );
        let mut tail = vec![b'x'; 20];
        tail.push(b'\n');
        assert!(matches!(
            decoder.push_bytes(&tail),
            Err(FrameError::TooLarge { .. })
        ));
    }

    #[test]
    fn malformed_line_reports_without_yielding() {
        let mut decoder = FrameDecoder::new();
        assert!(matches!(
            decoder.push_bytes(b"{oops}\n"),
            Err(FrameError::Malformed(_))
        ));
    }
}
