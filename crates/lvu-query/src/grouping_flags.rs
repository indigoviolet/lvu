//! Native grouping flags over one accepted enrichment column.
//!
//! The query engine computes; the view only presents. Both helpers below read
//! the already-evaluated typed batch frame with Polars kernels, so the
//! grouping layer never re-evaluates predicates or re-parses display text:
//!
//! * [`non_null_flags`] runs the native `is_not_null()` kernel. A produced
//!   `NULL` is a valid evaluated false, never unknown: only a missing column
//!   (or unreadable identities) fails, and the caller must treat that batch
//!   as unevaluated rather than as a silent false.
//! * [`exact_key_flags`] encodes the typed value exactly (never a truncated
//!   display string) so consecutive-equal runs cannot merge distinct long
//!   values. Values beyond [`MAX_EXACT_KEY_BYTES`] refuse with
//!   [`KeyFlag::Oversize`] instead of truncating into a false identity; the
//!   caller leaves those records unfolded with a diagnostic.
//!
//! Display truncation in [`super::engine::scalar_projection`] cannot change
//! either helper: neither reads truncated strings, and `None` from a
//! projection is not consulted here at all.

use polars::prelude::*;

use crate::{SEQUENCE_COLUMN, SOURCE_ID_COLUMN, StableRecordId};

/// Exact key bytes retained per value for run equality.
pub const MAX_EXACT_KEY_BYTES: usize = 512;

/// One record's grouping input for a run key column.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyFlag {
    /// Exactly encoded typed value; equality is byte equality, proven to
    /// match native Polars `==` (including signed zero canonicalization)
    /// by the oracle test below.
    Value(Vec<u8>),
    /// A produced null. Null never equals, not even another null.
    Null,
    /// A produced NaN. Never equals, not even another NaN, exactly as native
    /// `==` reports it: each NaN stands alone.
    Nan,
    /// A produced value beyond [`MAX_EXACT_KEY_BYTES`]. The caller must not
    /// group on it: leave the record unfolded with a diagnostic.
    Oversize,
}

fn frame_ids(frame: &DataFrame) -> Result<Vec<StableRecordId>, String> {
    let (Ok(sources), Ok(sequences)) = (
        frame.column(SOURCE_ID_COLUMN),
        frame.column(SEQUENCE_COLUMN),
    ) else {
        return Err("grouping flags need stable identity columns".to_owned());
    };
    let mut ids = Vec::with_capacity(frame.height());
    for index in 0..frame.height() {
        let source_id = sources
            .get(index)
            .map_err(|error| error.to_string())?
            .get_str()
            .ok_or_else(|| "source identity is not a string".to_owned())?
            .to_owned();
        let sequence = sequences
            .get(index)
            .map_err(|error| error.to_string())?
            .try_extract::<u64>()
            .map_err(|error| error.to_string())?;
        ids.push(StableRecordId {
            source_id,
            sequence,
        });
    }
    Ok(ids)
}

/// Native `is_not_null()` flags aligned by stable identity.
///
/// `true` opens an event under the filter-grouping rule, `false` continues
/// (or stands alone before any start). A produced null is a valid `false`.
/// A missing column is `Err`: the batch is unevaluated, never silently
/// false.
pub fn non_null_flags(
    frame: &DataFrame,
    name: &str,
) -> Result<Vec<(StableRecordId, bool)>, String> {
    let column = frame
        .column(name)
        .map_err(|_| format!("grouping column {name:?} is unavailable in this batch"))?;
    // The native kernel over the typed column: display truncation, string
    // rendering and `Option<String>` projections play no part, so a `None`
    // in a display projection can neither create nor clear a start here.
    let booleans = column.is_not_null();
    if booleans.len() != frame.height() {
        return Err("grouping mask length mismatches the batch".to_owned());
    }
    let ids = frame_ids(frame)?;
    let mut flags = Vec::with_capacity(frame.height());
    for (index, id) in ids.into_iter().enumerate() {
        // `is_not_null` is typed non-nullable, but that structural claim is
        // not proven here: a missing slot fails the batch closed (unevaluated)
        // rather than inventing a false start.
        let flag = booleans
            .get(index)
            .ok_or_else(|| "grouping mask has no value for a batch row".to_owned())?;
        flags.push((id, flag));
    }
    Ok(flags)
}

/// Why exact run keys are unavailable for a batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyError {
    /// The column is missing from this batch, or identities are unreadable.
    /// Batch-scoped: the caller treats the batch as unevaluated, never as a
    /// silent false, and other batches may still evaluate.
    Unavailable(String),
    /// The column's dtype has no exact typed encoding. Deterministic for the
    /// chain, so the caller rejects the candidate actionably instead of
    /// leaving every batch pending forever.
    Unsupported(String),
}

/// Exact typed key flags aligned by stable identity.
///
/// Typed encoding (one tag byte plus payload) keeps `1`, `"1"` and `true`
/// distinct without ever reading a truncated display string. A produced
/// null is [`KeyFlag::Null`]; see [`KeyError`] for the two failure modes.
pub fn exact_key_flags(
    frame: &DataFrame,
    name: &str,
) -> Result<Vec<(StableRecordId, KeyFlag)>, KeyError> {
    let column = frame
        .column(name)
        .map_err(|_| KeyError::Unavailable(format!("grouping column {name:?} is unavailable")))?;
    let supported = matches!(
        column.dtype(),
        DataType::Boolean
            | DataType::String
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float32
            | DataType::Float64
            | DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::Null
    );
    if !supported {
        return Err(KeyError::Unsupported(format!(
            "grouping keys need an exact scalar column; {name:?} is {:?}, which has no exact identity",
            column.dtype()
        )));
    }
    let ids = frame_ids(frame).map_err(KeyError::Unavailable)?;
    if ids.len() != frame.height() {
        return Err(KeyError::Unavailable(
            "grouping key length mismatches the batch".to_owned(),
        ));
    }
    let mut flags = Vec::with_capacity(frame.height());
    for (index, id) in ids.into_iter().enumerate() {
        let value = column
            .get(index)
            .map_err(|error| KeyError::Unavailable(error.to_string()))?;
        flags.push((id, encode_key_value(value)?));
    }
    Ok(flags)
}

/// Whether a variable payload already exceeds the exact-identity bound with
/// its tag counted, checked before any proportional allocation so a giant
/// value refuses without materialising.
fn exceeds_exact_key_bound(value: &AnyValue<'_>) -> bool {
    match value {
        AnyValue::String(text) => text.len().saturating_add(1) > MAX_EXACT_KEY_BYTES,
        AnyValue::StringOwned(text) => text.as_str().len().saturating_add(1) > MAX_EXACT_KEY_BYTES,
        _ => false,
    }
}

fn encode_key_value(value: AnyValue<'_>) -> Result<KeyFlag, KeyError> {
    // Tag bytes keep types distinct: the same payload under two tags never
    // compares equal, so no rendering coincidence can merge runs. The tag
    // counts against [`MAX_EXACT_KEY_BYTES`], and variable payloads are
    // measured before any proportional allocation, so a giant value refuses
    // without materialising.
    if exceeds_exact_key_bound(&value) {
        return Ok(KeyFlag::Oversize);
    }
    let mut encoded = Vec::new();
    match value {
        AnyValue::Null => return Ok(KeyFlag::Null),
        AnyValue::Boolean(flag) => {
            encoded.push(0x01);
            encoded.push(u8::from(flag));
        }
        AnyValue::String(text) => {
            encoded.push(0x02);
            encoded.extend_from_slice(text.as_bytes());
        }
        AnyValue::StringOwned(text) => {
            encoded.push(0x02);
            encoded.extend_from_slice(text.as_str().as_bytes());
        }
        // Floats canonicalize signed zero to +0.0 and isolate NaN, matching
        // native Polars `==` exactly (proven by the oracle test): -0.0 joins
        // +0.0 runs, while every NaN stands alone.
        AnyValue::Float32(value) if value.is_nan() => return Ok(KeyFlag::Nan),
        AnyValue::Float64(value) if value.is_nan() => return Ok(KeyFlag::Nan),
        AnyValue::Float32(value) => {
            encoded.push(0x0B);
            encoded.extend_from_slice(&canonical_f32(value).to_be_bytes());
        }
        AnyValue::Float64(value) => {
            encoded.push(0x0C);
            encoded.extend_from_slice(&canonical_f64(value).to_be_bytes());
        }
        AnyValue::UInt8(value) => {
            encoded.push(0x03);
            encoded.push(value);
        }
        AnyValue::UInt16(value) => {
            encoded.push(0x04);
            encoded.extend_from_slice(&value.to_be_bytes());
        }
        AnyValue::UInt32(value) => {
            encoded.push(0x05);
            encoded.extend_from_slice(&value.to_be_bytes());
        }
        AnyValue::UInt64(value) => {
            encoded.push(0x06);
            encoded.extend_from_slice(&value.to_be_bytes());
        }
        AnyValue::Int8(value) => {
            encoded.push(0x07);
            encoded.extend_from_slice(&value.to_be_bytes());
        }
        AnyValue::Int16(value) => {
            encoded.push(0x08);
            encoded.extend_from_slice(&value.to_be_bytes());
        }
        AnyValue::Int32(value) => {
            encoded.push(0x09);
            encoded.extend_from_slice(&value.to_be_bytes());
        }
        AnyValue::Int64(value) => {
            encoded.push(0x0A);
            encoded.extend_from_slice(&value.to_be_bytes());
        }
        unsupported => {
            return Err(KeyError::Unsupported(format!(
                "unsupported grouping key value of type {:?}",
                unsupported.dtype()
            )));
        }
    }
    debug_assert!(encoded.len() <= MAX_EXACT_KEY_BYTES);
    if encoded.len() > MAX_EXACT_KEY_BYTES {
        return Ok(KeyFlag::Oversize);
    }
    Ok(KeyFlag::Value(encoded))
}

/// Positive zero bits: -0.0 must join +0.0 runs exactly as native `==`.
fn canonical_f32(value: f32) -> u32 {
    if value == 0.0 { 0 } else { value.to_bits() }
}

/// Positive zero bits: -0.0 must join +0.0 runs exactly as native `==`.
fn canonical_f64(value: f64) -> u64 {
    if value == 0.0 { 0 } else { value.to_bits() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_frame(height: usize) -> DataFrame {
        let sources: Column = Series::new(SOURCE_ID_COLUMN.into(), vec!["source"; height]).into();
        let sequences: Column = Series::new(
            SEQUENCE_COLUMN.into(),
            (0..height as u64).collect::<Vec<_>>(),
        )
        .into();
        DataFrame::new(height, vec![sources, sequences]).expect("identity frame")
    }

    fn frame_with(name: &str, values: Series) -> DataFrame {
        let height = values.len();
        let mut frame = identity_frame(height);
        let mut column: Column = values.into();
        column.rename(name.into());
        frame.with_column(column).expect("add column");
        frame
    }

    #[test]
    fn non_null_treats_every_type_honestly() {
        let frame = frame_with(
            "flag",
            Series::new("flag".into(), &[Some(true), Some(false), None]),
        );
        let flags = non_null_flags(&frame, "flag").expect("mask");
        // `false` is a produced value (a start under filter grouping);
        // only the produced null is an evaluated false.
        assert_eq!(
            flags.iter().map(|(_, flag)| *flag).collect::<Vec<_>>(),
            vec![true, true, false]
        );
    }

    #[test]
    fn non_null_missing_column_is_unevaluated_not_false() {
        let frame = identity_frame(2);
        assert!(non_null_flags(&frame, "absent").is_err());
    }

    #[test]
    fn missing_key_column_is_unavailable_while_datetime_is_unsupported() {
        let frame = identity_frame(2);
        assert_eq!(
            exact_key_flags(&frame, "absent"),
            Err(KeyError::Unavailable(
                "grouping column \"absent\" is unavailable".to_owned()
            ))
        );
        // A temporal column has no exact typed identity. Through the real
        // pipeline it fails earlier (display projection), so a direct frame
        // pins the mapping: unavailable stays batch-scoped Unknown while
        // unsupported rejects the candidate.
        let datetimes = frame_with(
            "ts",
            Series::new("ts".into(), &[Some(1_700_000_000_000_000_000i64), None])
                .cast(&DataType::Datetime(TimeUnit::Microseconds, None))
                .expect("cast to datetime"),
        );
        assert!(
            matches!(
                exact_key_flags(&datetimes, "ts"),
                Err(KeyError::Unsupported(_))
            ),
            "datetime must reject, not stay unknown"
        );
    }

    #[test]
    fn keys_keep_types_distinct_and_nulls_unequal() {
        let frame = frame_with("key", Series::new("key".into(), &["1", "1", "2"]));
        let flags = exact_key_flags(&frame, "key").expect("keys");
        assert!(matches!(flags[0].1, KeyFlag::Value(_)));
        assert_eq!(flags[0].1, flags[1].1);
        assert_ne!(flags[0].1, flags[2].1);

        let ints = frame_with("n", Series::new("n".into(), &[1i64, 1]));
        let int_flags = exact_key_flags(&ints, "n").expect("int keys");
        // The tag byte keeps `1` apart from `"1"`.
        assert_ne!(int_flags[0].1, flags[0].1);

        let nulls = frame_with("k", Series::new("k".into(), &[None::<String>, None]));
        let null_flags = exact_key_flags(&nulls, "k").expect("null keys");
        assert!(matches!(null_flags[0].1, KeyFlag::Null));
    }

    #[test]
    fn keys_refuse_oversize_instead_of_merging() {
        let long = "x".repeat(MAX_EXACT_KEY_BYTES);
        let frame = frame_with("k", Series::new("k".into(), &[long.as_str(), "x"]));
        let flags = exact_key_flags(&frame, "k").expect("keys");
        assert!(matches!(flags[0].1, KeyFlag::Oversize));
        assert!(matches!(flags[1].1, KeyFlag::Value(_)));
        assert_ne!(flags[0].1, flags[1].1);
    }

    /// The encoding must partition exactly as native Polars `==` does: no
    /// invented typed-byte semantics. The oracle is one native comparison of
    /// each row against its predecessor; splits prove batch purity.
    #[test]
    fn float_keys_match_native_equality_including_signed_zero_and_nan() {
        let values = vec![
            Some(0.0f64),
            Some(-0.0),
            Some(f64::NAN),
            Some(1.0),
            Some(f64::NAN),
            Some(1.0),
            None,
            Some(f32::NAN as f64),
        ];
        let frame = frame_with("v", Series::new("v".into(), values.clone()));
        let flags = exact_key_flags(&frame, "v").expect("keys");
        // Native oracle: each row equals its predecessor?
        let column = frame.column("v").expect("column");
        let series = column.as_materialized_series();
        let shifted = series.shift(1);
        let equal = series.equal(&shifted).expect("native equality must run");
        assert_eq!(equal.len(), values.len());
        let mut oracle_breaks = vec![true];
        for index in 1..values.len() {
            // Kleene: only literal true continues a run; null and false both
            // break it.
            oracle_breaks.push(equal.get(index) != Some(true));
        }
        let mut encoded_breaks = vec![true];
        for window in flags.windows(2) {
            let continues = match (&window[0].1, &window[1].1) {
                (KeyFlag::Value(previous), KeyFlag::Value(next)) => previous == next,
                _ => false,
            };
            encoded_breaks.push(!continues);
        }
        assert_eq!(encoded_breaks, oracle_breaks);
        // Signed zero joins; every NaN and the null stand alone.
        assert!(!encoded_breaks[1]);
        assert!(encoded_breaks[2] && encoded_breaks[4] && encoded_breaks[6] && encoded_breaks[7]);
        // Batch purity: split frames concatenate to the whole-frame verdicts.
        let head = frame.head(Some(4));
        let tail = frame.slice(4, values.len() - 4);
        let mut split = exact_key_flags(&head, "v").expect("head keys");
        split.extend(exact_key_flags(&tail, "v").expect("tail keys"));
        assert_eq!(split, flags);
    }

    #[test]
    fn oversize_refuses_without_materialising_the_value() {
        // Five megabytes past the bound: the pre-check refuses before any
        // proportional allocation, so this returns instead of growing.
        let huge = "x".repeat(5 * 1024 * 1024);
        let frame = frame_with("k", Series::new("k".into(), &[huge.as_str()]));
        let flags = exact_key_flags(&frame, "k").expect("keys");
        assert_eq!(flags.len(), 1);
        assert!(matches!(flags[0].1, KeyFlag::Oversize));
    }

    #[test]
    fn long_strings_are_compared_exactly_not_truncated() {
        // Two values sharing a long prefix must not merge: the mask path
        // never truncates, unlike display or fold-key projections.
        let shared = "y".repeat(300);
        let first = format!("{shared}-one");
        let second = format!("{shared}-two");
        let frame = frame_with(
            "k",
            Series::new("k".into(), &[first.as_str(), second.as_str()]),
        );
        let flags = exact_key_flags(&frame, "k").expect("keys");
        assert_ne!(flags[0].1, flags[1].1);
    }
}
