//! Servicing the Time dialog's request for timestamp-field candidates.
//!
//! Recognition lives in `lvu-live` and column validation in `lvu-query`, and
//! `lvu` can depend on neither, so the dialog asks and this module answers. It
//! runs the engines and reports what they said; it never re-implements them and
//! never accepts an assumption on the user's behalf.

use std::collections::BTreeMap;

use lvu::app::{TimeFieldCandidate, TimeRecognition, TimeRecognitionRequest};
use lvu::{RowProvider, ViewportRequest};
use lvu_live::time::{
    CandidateSummary, EpochUnit, RecognitionOptions, TimeFieldSelection, TimeInterpretation,
    TimeOutcome, ZoneAssumption, infer_text_time_formats,
};
use lvu_query::time_field::{EpochUnit as ColumnEpochUnit, format_reads_zone};
use lvu_view::time_basis::{validate_epoch_column, validate_text_column};

/// Records sampled for one recognition pass. Bounded so opening the dialog
/// costs the same on a small capture and a large one.
const SAMPLE_ROWS: usize = 256;

/// Prefix the display layer puts on enrichment-produced detail keys.
const DERIVED_PREFIX: &str = "derived.";

/// Explicitly designable fields offered beyond the ranked candidates. Bounded
/// so a wide record cannot turn the dropdown into a schema dump.
const MAX_EXPLICIT_FIELDS: usize = 24;

/// Answers one request. Cheap enough to run inline: it reads a bounded page.
pub fn recognize(
    request: &TimeRecognitionRequest,
    rows: &dyn RowProvider,
    year: i32,
) -> TimeRecognition {
    let page = rows.page(
        &request.view_id,
        ViewportRequest {
            start: 0,
            len: SAMPLE_ROWS,
        },
    );
    let records: Vec<Vec<u8>> = page
        .rows
        .iter()
        .map(|row| row.text.as_bytes().to_vec())
        .collect();
    let mut recognition = TimeRecognition {
        sampled_records: records.len(),
        ..Default::default()
    };
    if records.is_empty() {
        recognition
            .diagnostics
            .push("no records are loaded yet, so no field could be sampled".into());
    }
    let borrowed: Vec<&[u8]> = records.iter().map(Vec::as_slice).collect();

    // An edited format is measured against the same sample the offer was made
    // from, so the two rates the dialog shows are comparable.
    if let Some(probe) = &request.text_format_probe {
        recognition.probe = Some(measure_text_format(&page.rows, probe));
    }

    // Three passes over the same sample, so every reading on offer carries a
    // coverage the engine actually measured rather than one inferred from the
    // strict pass. The strict pass is first and is what a candidate defaults to.
    let strict = lvu_live::time::recognize_sample(
        borrowed.iter().copied(),
        &RecognitionOptions {
            zone: ZoneAssumption::Reject,
            ..RecognitionOptions::default()
        },
    );
    let assumed_utc = lvu_live::time::recognize_sample(
        borrowed.iter().copied(),
        &RecognitionOptions {
            zone: ZoneAssumption::Utc,
            ..RecognitionOptions::default()
        },
    );
    let assumed_utc_year = lvu_live::time::recognize_sample(
        borrowed.iter().copied(),
        &RecognitionOptions {
            zone: ZoneAssumption::Utc,
            assumed_year: Some(year),
            ..RecognitionOptions::default()
        },
    );
    recognition.diagnostics.extend(strict.diagnostics.clone());

    for summary in &strict.candidates {
        let mut candidate = plain(summary);
        for relaxed in [&assumed_utc, &assumed_utc_year] {
            if let Some(alternative) = relaxed
                .candidates
                .iter()
                .find(|other| other.selection.field == summary.selection.field)
                && alternative.selection != summary.selection
                && !candidate
                    .alternatives
                    .iter()
                    .any(|seen| seen.token == alternative.selection.to_token())
            {
                candidate.alternatives.push(plain(alternative));
            }
        }
        recognition.candidates.push(candidate);
    }
    recognition
        .candidates
        .extend(column_candidates(&page.rows, &mut recognition.diagnostics));
    // Recognition ranks the fields that look like times. The explicit path adds
    // every other structured field, so a field it did not rank is still
    // designable — measured by reading it, never by guessing at it.
    recognition.candidates.extend(structured_candidates(
        &page.rows,
        &borrowed,
        &recognition.candidates,
    ));

    if let Some(token) = request.token.as_deref()
        && let Some(row) = request
            .anchored_row
            .as_ref()
            .and_then(|id| rows.row_by_id(&request.view_id, id))
        && let Ok(selection) = TimeFieldSelection::parse_token(token)
        && let TimeOutcome::Valid(reading) = selection.read(row.text.as_bytes())
    {
        recognition.anchored_selected_nanos = Some(reading.unix_nanos);
    }
    recognition
}

/// Restates one engine summary in the plain form the dialog reads.
fn plain(summary: &CandidateSummary) -> TimeFieldCandidate {
    TimeFieldCandidate {
        token: summary.selection.to_token(),
        label: summary.label.clone(),
        reading: summary
            .formats
            .first()
            .map(|format| format.label().to_owned())
            .unwrap_or_else(|| interpretation_label(summary.selection.interpretation)),
        coverage_percent: Some(percent(summary.coverage)),
        assumptions: summary.assumptions.clone(),
        blocked: summary.blocked.clone(),
        text_format: None,
        alternatives: Vec::new(),
    }
}

fn interpretation_label(interpretation: TimeInterpretation) -> String {
    match interpretation {
        TimeInterpretation::Auto => "detected per row".into(),
        TimeInterpretation::Text => "calendar date-time".into(),
        TimeInterpretation::Epoch(unit) => unit.label().to_owned(),
    }
}

fn percent(coverage: f64) -> u8 {
    (coverage * 100.0).round().clamp(0.0, 100.0) as u8
}

/// Every structured field the sample carries that recognition did not already
/// rank, each read over the sample so its coverage is measured rather than
/// assumed. A field no reading resolves is offered with the reason it failed.
fn structured_candidates(
    rows: &[lvu::DisplayRow],
    sample: &[&[u8]],
    ranked: &[TimeFieldCandidate],
) -> Vec<TimeFieldCandidate> {
    let mut names: Vec<String> = Vec::new();
    for row in rows {
        for (name, _) in &row.fields {
            if names.len() >= MAX_EXPLICIT_FIELDS {
                break;
            }
            if !names.iter().any(|seen| seen == name) {
                names.push(name.clone());
            }
        }
    }
    let mut candidates = Vec::new();
    for name in names {
        let strict = TimeFieldSelection::structured(name.clone());
        if ranked
            .iter()
            .any(|candidate| candidate.token == strict.to_token() || candidate.label == name)
        {
            continue;
        }
        let readings: Vec<TimeFieldSelection> = vec![
            strict,
            TimeFieldSelection::structured(name.clone()).with_zone(ZoneAssumption::Utc),
        ];
        let mut measured: Vec<TimeFieldCandidate> = readings
            .into_iter()
            .map(|selection| measure(&selection, &name, sample))
            .collect();
        // Nothing readable at all is not worth offering as a basis.
        if measured
            .iter()
            .all(|reading| reading.coverage_percent.unwrap_or_default() == 0)
        {
            continue;
        }
        measured.sort_by(|left, right| {
            right
                .coverage_percent
                .cmp(&left.coverage_percent)
                .then_with(|| left.assumptions.len().cmp(&right.assumptions.len()))
        });
        let mut chosen = measured.remove(0);
        chosen.alternatives = measured;
        candidates.push(chosen);
    }
    candidates
}

/// Reads one declared selection across the sample and reports what it resolved.
fn measure(selection: &TimeFieldSelection, label: &str, sample: &[&[u8]]) -> TimeFieldCandidate {
    let mut read = 0usize;
    let mut present = 0usize;
    let mut refusal = None;
    for record in sample {
        match selection.read(record) {
            TimeOutcome::Valid(_) => {
                read += 1;
                present += 1;
            }
            TimeOutcome::Invalid { diagnostic, .. } => {
                present += 1;
                refusal.get_or_insert(diagnostic);
            }
            TimeOutcome::Missing => {}
        }
    }
    let coverage = if present == 0 {
        0.0
    } else {
        read as f64 / present as f64
    };
    TimeFieldCandidate {
        token: selection.to_token(),
        label: label.to_owned(),
        reading: interpretation_label(selection.interpretation),
        coverage_percent: Some(percent(coverage)),
        assumptions: selection.assumptions(),
        blocked: (read == 0).then(|| {
            refusal.unwrap_or_else(|| "no sampled record carries a readable time here".into())
        }),
        text_format: None,
        alternatives: Vec::new(),
    }
}

/// The explicit `choose a field` path: every enrichment-produced column, read
/// as an epoch in each unit and as each text shape its values are in, and
/// validated by the query layer against the real sampled values. A reading
/// that resolves nothing is not offered.
fn column_candidates(
    rows: &[lvu::DisplayRow],
    diagnostics: &mut Vec<String>,
) -> Vec<TimeFieldCandidate> {
    let mut columns: BTreeMap<String, ColumnSample> = BTreeMap::new();
    for row in rows {
        for (key, value) in &row.details {
            let Some(name) = key.strip_prefix(DERIVED_PREFIX) else {
                continue;
            };
            let sample = columns.entry(name.to_owned()).or_default();
            sample.epoch.push(value.trim().parse::<i64>().ok());
            sample.text.push(value.trim().to_owned());
        }
    }
    let mut candidates = Vec::new();
    for (name, sample) in columns {
        let mut readings = epoch_readings(&name, &sample.epoch);
        readings.extend(text_readings(&name, &sample.text, diagnostics));
        // A column is ambiguous by construction: the same characters can be a
        // plausible instant under more than one reading. The best-covered one
        // leads and the rest are the override, so the choice stays the user's.
        readings.sort_by(|left, right| {
            right
                .coverage_percent
                .cmp(&left.coverage_percent)
                .then_with(|| left.reading.cmp(&right.reading))
        });
        let usable = readings.iter().filter(|r| r.blocked.is_none()).count();
        let mut chosen = match readings.first() {
            Some(first) => first.clone(),
            None => continue,
        };
        chosen.alternatives = readings.into_iter().skip(1).collect();
        if usable > 1 && chosen.blocked.is_none() && !chosen.reading.starts_with("text") {
            chosen.assumptions.push(format!(
                "epoch values do not state their unit; read as {}",
                chosen.reading
            ));
        }
        candidates.push(chosen);
    }
    candidates
}

/// One column's sampled values, in both the shapes a basis can read them in.
#[derive(Default)]
struct ColumnSample {
    epoch: Vec<Option<i64>>,
    text: Vec<String>,
}

/// The four epoch units, kept only where they resolve something.
fn epoch_readings(name: &str, values: &[Option<i64>]) -> Vec<TimeFieldCandidate> {
    if values.iter().all(Option::is_none) {
        return Vec::new();
    }
    let mut readings = Vec::new();
    for (live, column) in [
        (EpochUnit::Seconds, ColumnEpochUnit::Seconds),
        (EpochUnit::Milliseconds, ColumnEpochUnit::Milliseconds),
        (EpochUnit::Microseconds, ColumnEpochUnit::Microseconds),
        (EpochUnit::Nanoseconds, ColumnEpochUnit::Nanoseconds),
    ] {
        let selection =
            TimeFieldSelection::column(name).with_interpretation(TimeInterpretation::Epoch(live));
        let reading = TimeFieldCandidate {
            token: selection.to_token(),
            label: format!("column: {name}"),
            reading: live.label().to_owned(),
            ..Default::default()
        };
        match validate_epoch_column(name, column, values) {
            Ok((coverage, parsed, assumptions)) if parsed > 0 => {
                readings.push(TimeFieldCandidate {
                    coverage_percent: Some(percent(coverage)),
                    assumptions,
                    ..reading
                })
            }
            Ok(_) => {}
            Err(error) => readings.push(TimeFieldCandidate {
                blocked: Some(error),
                ..reading
            }),
        }
    }
    readings
}

/// The text shapes the column's values are in, each with the format inferred
/// for it and the share the query layer's own parser measured for that format.
///
/// This is what turns `TimeFieldError::FormatRequired` from a refusal into a
/// state with a suggestion: the format is never guessed silently, it is
/// proposed with its evidence and carried as an assumption the dialog makes
/// the user accept, and the dialog can edit it.
fn text_readings(
    name: &str,
    values: &[String],
    diagnostics: &mut Vec<String>,
) -> Vec<TimeFieldCandidate> {
    let borrowed: Vec<&str> = values.iter().map(String::as_str).collect();
    let mut readings = Vec::new();
    for shape in infer_text_time_formats(&borrowed) {
        if !shape.readable {
            // Recognised but not compilable. Naming it is more use than
            // omitting the column, which is what the dialog did before.
            diagnostics.push(format!(
                "column {name:?} looks like {} ({}% of the sample), which carries no year; \
                 a column basis needs a year in the value",
                shape.label,
                shape.match_percent()
            ));
            continue;
        }
        // A format that reads its own offset cannot also take an assumption;
        // one that does not can only be read under a declared zone, and that
        // declaration is the assumption the dialog makes the user confirm.
        let zone = if format_reads_zone(&shape.format) {
            ZoneAssumption::Reject
        } else {
            ZoneAssumption::Utc
        };
        let selection = TimeFieldSelection::column(name)
            .with_interpretation(TimeInterpretation::Text)
            .with_text_format(Some(shape.format.clone()))
            .with_zone(zone);
        let reading = TimeFieldCandidate {
            token: selection.to_token(),
            label: format!("column: {name}"),
            reading: format!("text · {}", shape.label),
            text_format: Some(shape.format.clone()),
            ..Default::default()
        };
        let owned: Vec<Option<&str>> = values
            .iter()
            .map(|value| (!value.is_empty()).then_some(value.as_str()))
            .collect();
        match validate_text_column(name, &shape.format, zone, &owned) {
            Ok((coverage, parsed, mut assumptions)) if parsed > 0 => {
                if let Some(sample) = &shape.sample {
                    assumptions.push(format!("for example {sample:?}"));
                }
                readings.push(TimeFieldCandidate {
                    coverage_percent: Some(percent(coverage)),
                    assumptions,
                    ..reading
                });
            }
            Ok(_) => {}
            Err(error) => readings.push(TimeFieldCandidate {
                blocked: Some(error),
                ..reading
            }),
        }
    }
    readings
}

/// Measures one edited format against the sampled column, as a reading.
///
/// Returned even when it reads nothing: "this format reads 0%" is the answer
/// the user asked for, and hiding it would leave the previous rate on screen
/// beside the new format.
fn measure_text_format(
    rows: &[lvu::DisplayRow],
    probe: &lvu::app::TextFormatProbe,
) -> TimeFieldCandidate {
    let key = format!("{DERIVED_PREFIX}{}", probe.column);
    let values: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            row.details
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.trim().to_owned())
        })
        .collect();
    let zone = if format_reads_zone(&probe.format) {
        ZoneAssumption::Reject
    } else {
        ZoneAssumption::Utc
    };
    let selection = TimeFieldSelection::column(probe.column.clone())
        .with_interpretation(TimeInterpretation::Text)
        .with_text_format(Some(probe.format.clone()))
        .with_zone(zone);
    let reading = TimeFieldCandidate {
        token: selection.to_token(),
        label: format!("column: {}", probe.column),
        reading: "text · your format".to_owned(),
        text_format: Some(probe.format.clone()),
        ..Default::default()
    };
    let owned: Vec<Option<&str>> = values
        .iter()
        .map(|value| (!value.is_empty()).then_some(value.as_str()))
        .collect();
    match validate_text_column(&probe.column, &probe.format, zone, &owned) {
        Ok((coverage, parsed, mut assumptions)) => {
            if parsed == 0 {
                return TimeFieldCandidate {
                    blocked: Some("this format reads none of the sampled values".into()),
                    coverage_percent: Some(0),
                    ..reading
                };
            }
            if let Some(sample) = values.iter().find(|value| !value.is_empty()) {
                assumptions.push(format!("for example {sample:?}"));
            }
            TimeFieldCandidate {
                coverage_percent: Some(percent(coverage)),
                assumptions,
                ..reading
            }
        }
        Err(error) => TimeFieldCandidate {
            blocked: Some(error),
            ..reading
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvu::provider::RowId;

    fn rows(column: &str, values: &[&str]) -> Vec<lvu::DisplayRow> {
        values
            .iter()
            .enumerate()
            .map(|(index, value)| lvu::DisplayRow {
                id: RowId {
                    source_id: "s".into(),
                    sequence: index as u64,
                },
                timestamp: String::new(),
                captured_at_unix_nanos: None,
                level: String::new(),
                text: String::new(),
                details: vec![(format!("{DERIVED_PREFIX}{column}"), (*value).to_owned())],
                fields: Vec::new(),
            })
            .collect()
    }

    /// The TODO row this replaces: a text column was dropped with a diagnostic
    /// saying a field token could not carry a format. It can now.
    #[test]
    fn a_text_column_is_offered_with_the_format_inferred_for_it() {
        let mut diagnostics = Vec::new();
        let rows = rows(
            "started_at",
            &["2026-09-07T12:00:01Z", "2026-09-07T12:00:02Z"],
        );
        let candidates = column_candidates(&rows, &mut diagnostics);
        let chosen = candidates
            .iter()
            .find(|candidate| candidate.label == "column: started_at")
            .expect("the text column is offered");
        assert_eq!(chosen.reading, "text · RFC 3339");
        assert_eq!(chosen.coverage_percent, Some(100));
        assert!(chosen.blocked.is_none());
        assert!(chosen.token.contains("|text|"), "{}", chosen.token);
        assert!(chosen.token.ends_with("|%+"), "{}", chosen.token);
        // The format is an assumption, so the dialog has to have it confirmed.
        assert!(
            chosen
                .assumptions
                .iter()
                .any(|note| note.contains("%+") || note.contains("format")),
            "{:?}",
            chosen.assumptions
        );
        assert!(
            diagnostics.is_empty(),
            "nothing is refused any more: {diagnostics:?}"
        );
    }

    /// Rows the format cannot read lower the rate it is offered with. They are
    /// nulls in the basis, not rows removed from the view.
    #[test]
    fn values_the_format_cannot_read_are_reported_not_hidden() {
        let mut diagnostics = Vec::new();
        let rows = rows(
            "started_at",
            &["2026-09-07T12:00:01Z", "not a time", "2026-09-07T12:00:03Z"],
        );
        let chosen = column_candidates(&rows, &mut diagnostics)
            .into_iter()
            .find(|candidate| candidate.reading.starts_with("text"))
            .expect("still offered below 100%");
        assert_eq!(chosen.coverage_percent, Some(67));
        assert!(chosen.blocked.is_none(), "a partial read is still a basis");
    }

    /// A zone-less shape is readable only under a declared zone, and that
    /// declaration is an assumption the user must accept.
    #[test]
    fn a_zone_less_text_column_carries_its_assumption() {
        let mut diagnostics = Vec::new();
        let rows = rows(
            "started_at",
            &["2026-09-07T12:00:01", "2026-09-07T12:00:02"],
        );
        let chosen = column_candidates(&rows, &mut diagnostics)
            .into_iter()
            .find(|candidate| candidate.reading.starts_with("text"))
            .unwrap();
        assert_eq!(chosen.reading, "text · ISO without a zone");
        assert!(
            chosen
                .assumptions
                .iter()
                .any(|note| note.contains("no timezone")),
            "{:?}",
            chosen.assumptions
        );
    }

    /// Syslog is recognised and explained rather than silently missing.
    #[test]
    fn a_syslog_column_is_explained_in_the_diagnostics() {
        let mut diagnostics = Vec::new();
        let rows = rows("when", &["Sep  7 12:00:01", "Sep  8 12:00:02"]);
        let candidates = column_candidates(&rows, &mut diagnostics);
        assert!(candidates.is_empty(), "{candidates:?}");
        assert!(
            diagnostics.iter().any(|note| note.contains("syslog")
                && note.contains("no year")
                && note.contains("when")),
            "{diagnostics:?}"
        );
    }

    /// A column of digits keeps going down the epoch path, which states the
    /// unit, rather than being read as seconds by a text format.
    #[test]
    fn a_numeric_column_is_still_read_as_an_epoch() {
        let mut diagnostics = Vec::new();
        let rows = rows("latency_start", &["1757246401", "1757246402"]);
        let chosen = column_candidates(&rows, &mut diagnostics)
            .into_iter()
            .next()
            .unwrap();
        assert!(
            !chosen.reading.starts_with("text"),
            "read as {}",
            chosen.reading
        );
        assert!(chosen.reading.contains("epoch"), "{}", chosen.reading);
    }
}
