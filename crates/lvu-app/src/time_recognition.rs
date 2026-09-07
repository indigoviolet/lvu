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
    TimeOutcome, ZoneAssumption,
};
use lvu_query::time_field::EpochUnit as ColumnEpochUnit;
use lvu_view::time_basis::validate_epoch_column;

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
        alternatives: Vec::new(),
    }
}

/// The explicit `choose a field` path: every enrichment-produced column, read
/// as an epoch in each unit and validated by the query layer against the real
/// sampled values. A unit that resolves nothing is not offered.
fn column_candidates(
    rows: &[lvu::DisplayRow],
    diagnostics: &mut Vec<String>,
) -> Vec<TimeFieldCandidate> {
    let mut columns: BTreeMap<String, Vec<Option<i64>>> = BTreeMap::new();
    let mut textual: Vec<String> = Vec::new();
    for row in rows {
        for (key, value) in &row.details {
            let Some(name) = key.strip_prefix(DERIVED_PREFIX) else {
                continue;
            };
            let parsed = value.trim().parse::<i64>().ok();
            if parsed.is_none() && !value.trim().is_empty() && !textual.iter().any(|s| s == name) {
                textual.push(name.to_owned());
            }
            columns.entry(name.to_owned()).or_default().push(parsed);
        }
    }
    for name in textual {
        // Naming the gap is more use than silently omitting the column.
        diagnostics.push(format!(
            "column {name:?} holds text; a text column basis needs an explicit format, which a field token cannot yet carry"
        ));
        columns.remove(&name);
    }
    let mut candidates = Vec::new();
    for (name, values) in columns {
        if values.iter().all(Option::is_none) {
            continue;
        }
        let mut readings: Vec<TimeFieldCandidate> = Vec::new();
        for (live, column) in [
            (EpochUnit::Seconds, ColumnEpochUnit::Seconds),
            (EpochUnit::Milliseconds, ColumnEpochUnit::Milliseconds),
            (EpochUnit::Microseconds, ColumnEpochUnit::Microseconds),
            (EpochUnit::Nanoseconds, ColumnEpochUnit::Nanoseconds),
        ] {
            let selection = TimeFieldSelection::column(name.clone())
                .with_interpretation(TimeInterpretation::Epoch(live));
            let reading = TimeFieldCandidate {
                token: selection.to_token(),
                label: format!("column: {name}"),
                reading: live.label().to_owned(),
                ..Default::default()
            };
            match validate_epoch_column(&name, column, &values) {
                Ok((coverage, parsed, assumptions)) if parsed > 0 => {
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
        // An epoch column is ambiguous by construction: the same integers are a
        // plausible instant in more than one unit. The best-covered reading
        // leads and the rest are the override, so the choice stays the user's.
        readings.sort_by(|left, right| {
            right
                .coverage_percent
                .cmp(&left.coverage_percent)
                .then_with(|| left.reading.cmp(&right.reading))
        });
        let mut chosen = match readings.first() {
            Some(first) => first.clone(),
            None => continue,
        };
        chosen.alternatives = readings.into_iter().skip(1).collect();
        if !chosen.alternatives.is_empty() {
            chosen.assumptions.push(format!(
                "epoch values do not state their unit; read as {}",
                chosen.reading
            ));
        }
        candidates.push(chosen);
    }
    candidates
}
