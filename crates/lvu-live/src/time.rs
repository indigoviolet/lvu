//! Deterministic event-time recognition and explicit time-field selection.
//!
//! Recognition is row-local: every reading is derived from one record's own
//! bytes, so partitioned batches and whole-frame runs produce identical values.
//! Sampling is only used to *rank* candidate fields; the selection it proposes
//! is then applied one record at a time.
//!
//! Three product invariants are encoded here rather than assumed:
//!
//! * a value without a timezone is never silently treated as UTC. It is refused
//!   until the caller declares [`ZoneAssumption`], and the applied assumption is
//!   reported back in [`TimeReading::assumption`].
//! * an epoch number whose unit cannot be resolved to exactly one plausible unit
//!   is refused with the competing units named, never guessed.
//! * `timestamp_utc` is an output contract. It is deliberately absent from the
//!   recognised input key list; enriched columns are designated explicitly.

/// Records larger than this are refused rather than scanned.
pub const MAX_RECOGNITION_RECORD_BYTES: usize = 1024 * 1024;

/// Only the leading bytes of an unstructured record are scanned for a prefix.
pub const MAX_RAW_PREFIX_BYTES: usize = 128;

/// 1980-01-01T00:00:00Z: the earliest instant accepted for epoch unit inference.
pub const PLAUSIBLE_EPOCH_START_SECONDS: i64 = 315_532_800;
/// 2200-01-01T00:00:00Z: the latest instant accepted for epoch unit inference.
/// Also keeps nanosecond epochs inside `i64`.
pub const PLAUSIBLE_EPOCH_END_SECONDS: i64 = 7_258_118_400;

const MAX_STRUCTURED_FIELDS: usize = 256;
/// Internal candidate key for the unstructured prefix. Not a field path, so a
/// source that really does have a field named `raw` cannot collide with it.
const RAW_CANDIDATE_KEY: &str = "\u{1}raw";
const MAX_JSON_DEPTH: usize = 3;

/// Keys recognised as event-time fields without an explicit declaration, in
/// precedence order. `timestamp_utc` is intentionally excluded: it names lvu's
/// enrichment *output*, and treating it as an input would make recognition
/// depend on a name the source never promised.
const PRIORITY_KEYS: [&str; 16] = [
    "timestamp",
    "@timestamp",
    "time",
    "ts",
    "event_time",
    "eventtime",
    "datetime",
    "date_time",
    "asctime",
    "log_time",
    "logtime",
    "occurred_at",
    "recorded_at",
    "created_at",
    "epoch",
    "date",
];

const MONTH_NAMES: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Encoded unit of an integer or numeric-string epoch value.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EpochUnit {
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
}

impl EpochUnit {
    pub const ALL: [EpochUnit; 4] = [
        EpochUnit::Seconds,
        EpochUnit::Milliseconds,
        EpochUnit::Microseconds,
        EpochUnit::Nanoseconds,
    ];

    /// Stable token used by persisted selections and by the query layer.
    pub fn token(self) -> &'static str {
        match self {
            EpochUnit::Seconds => "s",
            EpochUnit::Milliseconds => "ms",
            EpochUnit::Microseconds => "us",
            EpochUnit::Nanoseconds => "ns",
        }
    }

    pub fn parse_token(token: &str) -> Option<Self> {
        match token {
            "s" | "sec" | "secs" | "seconds" => Some(EpochUnit::Seconds),
            "ms" | "millis" | "milliseconds" => Some(EpochUnit::Milliseconds),
            "us" | "micros" | "microseconds" => Some(EpochUnit::Microseconds),
            "ns" | "nanos" | "nanoseconds" => Some(EpochUnit::Nanoseconds),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EpochUnit::Seconds => "epoch seconds",
            EpochUnit::Milliseconds => "epoch milliseconds",
            EpochUnit::Microseconds => "epoch microseconds",
            EpochUnit::Nanoseconds => "epoch nanoseconds",
        }
    }

    pub fn nanos_per_unit(self) -> i64 {
        match self {
            EpochUnit::Seconds => 1_000_000_000,
            EpochUnit::Milliseconds => 1_000_000,
            EpochUnit::Microseconds => 1_000,
            EpochUnit::Nanoseconds => 1,
        }
    }

    fn units_per_second(self) -> i64 {
        1_000_000_000 / self.nanos_per_unit()
    }

    fn plausible_whole(self, whole: i64) -> bool {
        let per_second = self.units_per_second();
        whole >= PLAUSIBLE_EPOCH_START_SECONDS.saturating_mul(per_second)
            && whole < PLAUSIBLE_EPOCH_END_SECONDS.saturating_mul(per_second)
    }
}

/// How values that carry no timezone are interpreted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ZoneAssumption {
    /// Refuse zone-less values. The default: a missing zone is a real ambiguity.
    #[default]
    Reject,
    /// Read zone-less values as UTC and report the assumption.
    Utc,
    /// Read zone-less values at a fixed offset east of UTC, in seconds.
    FixedOffsetSeconds(i32),
}

impl ZoneAssumption {
    fn offset_seconds(self) -> Option<i32> {
        match self {
            ZoneAssumption::Reject => None,
            ZoneAssumption::Utc => Some(0),
            ZoneAssumption::FixedOffsetSeconds(offset) => Some(offset),
        }
    }

    fn note(self) -> String {
        match self {
            ZoneAssumption::Reject => String::new(),
            ZoneAssumption::Utc => "value has no timezone; assumed UTC".into(),
            ZoneAssumption::FixedOffsetSeconds(offset) => {
                format!(
                    "value has no timezone; assumed {}",
                    format_offset_seconds(offset)
                )
            }
        }
    }

    pub fn token(self) -> String {
        match self {
            ZoneAssumption::Reject => "reject".into(),
            ZoneAssumption::Utc => "utc".into(),
            ZoneAssumption::FixedOffsetSeconds(offset) => format!("offset{offset}"),
        }
    }

    pub fn parse_token(token: &str) -> Option<Self> {
        match token {
            "reject" => Some(ZoneAssumption::Reject),
            "utc" => Some(ZoneAssumption::Utc),
            _ => token
                .strip_prefix("offset")
                .and_then(|value| value.parse::<i32>().ok())
                .filter(|offset| offset.abs() < 86_400)
                .map(ZoneAssumption::FixedOffsetSeconds),
        }
    }
}

fn format_offset_seconds(offset: i32) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let absolute = offset.unsigned_abs();
    format!(
        "UTC{sign}{:02}:{:02}",
        absolute / 3600,
        (absolute % 3600) / 60
    )
}

/// Declared reading of a designated field's values.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TimeInterpretation {
    /// Detect per row; refuse rather than guess an ambiguous epoch unit.
    #[default]
    Auto,
    /// Textual calendar date-time (RFC3339/ISO-8601 or a bare date-time).
    Text,
    /// Numeric epoch in the declared unit.
    Epoch(EpochUnit),
}

impl TimeInterpretation {
    pub fn token(self) -> String {
        match self {
            TimeInterpretation::Auto => "auto".into(),
            TimeInterpretation::Text => "text".into(),
            TimeInterpretation::Epoch(unit) => format!("epoch_{}", unit.token()),
        }
    }

    pub fn parse_token(token: &str) -> Option<Self> {
        match token {
            "auto" => Some(TimeInterpretation::Auto),
            "text" => Some(TimeInterpretation::Text),
            _ => token
                .strip_prefix("epoch_")
                .and_then(EpochUnit::parse_token)
                .map(TimeInterpretation::Epoch),
        }
    }
}

/// The concrete shape a value was read as.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeFormat {
    /// Calendar date-time carrying `Z` or an explicit numeric offset.
    OffsetDateTime,
    /// Calendar date-time with no timezone; only usable under an assumption.
    NaiveDateTime,
    /// Syslog-style `Mmm D HH:MM:SS`: no year and no timezone.
    SyslogDateTime,
    Epoch(EpochUnit),
}

impl TimeFormat {
    pub fn label(self) -> &'static str {
        match self {
            TimeFormat::OffsetDateTime => "date-time with explicit offset",
            TimeFormat::NaiveDateTime => "date-time without timezone",
            TimeFormat::SyslogDateTime => "syslog date-time without year or timezone",
            TimeFormat::Epoch(unit) => unit.label(),
        }
    }
}

/// Where an event time is read from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TimeFieldRef {
    /// A key path inside the record's own JSON or logfmt content.
    Structured(String),
    /// The leading date-time of an unstructured record.
    RawPrefix,
    /// A column produced by the accepted enrichment chain. lvu-live cannot read
    /// this from raw bytes; the query layer resolves it.
    Column(String),
}

impl TimeFieldRef {
    pub fn label(&self) -> String {
        match self {
            TimeFieldRef::Structured(path) => path.clone(),
            TimeFieldRef::RawPrefix => "raw".into(),
            TimeFieldRef::Column(name) => format!("column:{name}"),
        }
    }
}

/// A fully declared event-time basis: which field, how its values are read, and
/// which assumptions the caller accepted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeFieldSelection {
    pub field: TimeFieldRef,
    pub interpretation: TimeInterpretation,
    pub zone: ZoneAssumption,
    /// Year applied to formats that carry none (syslog). Never inferred.
    pub assumed_year: Option<i32>,
}

impl TimeFieldSelection {
    pub fn structured(path: impl Into<String>) -> Self {
        Self::new(TimeFieldRef::Structured(path.into()))
    }

    pub fn raw_prefix() -> Self {
        Self::new(TimeFieldRef::RawPrefix)
    }

    pub fn column(name: impl Into<String>) -> Self {
        Self::new(TimeFieldRef::Column(name.into()))
    }

    pub fn new(field: TimeFieldRef) -> Self {
        Self {
            field,
            interpretation: TimeInterpretation::Auto,
            zone: ZoneAssumption::Reject,
            assumed_year: None,
        }
    }

    pub fn with_interpretation(mut self, interpretation: TimeInterpretation) -> Self {
        self.interpretation = interpretation;
        self
    }

    pub fn with_zone(mut self, zone: ZoneAssumption) -> Self {
        self.zone = zone;
        self
    }

    pub fn with_assumed_year(mut self, year: Option<i32>) -> Self {
        self.assumed_year = year;
        self
    }

    /// Assumptions this selection applies, for display before it is accepted.
    pub fn assumptions(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.zone != ZoneAssumption::Reject {
            notes.push(self.zone.note());
        }
        if let Some(year) = self.assumed_year {
            notes.push(format!("values without a year are read as {year}"));
        }
        notes
    }

    /// Stable text encoding for persistence. Field paths may contain any byte
    /// except the reserved separator, which is rejected on parse.
    pub fn to_token(&self) -> String {
        let field = match &self.field {
            TimeFieldRef::Structured(path) => format!("structured:{path}"),
            TimeFieldRef::RawPrefix => "raw".into(),
            TimeFieldRef::Column(name) => format!("column:{name}"),
        };
        let year = self
            .assumed_year
            .map_or_else(|| "-".to_owned(), |year| year.to_string());
        format!(
            "{field}|{}|{}|{year}",
            self.interpretation.token(),
            self.zone.token()
        )
    }

    pub fn parse_token(token: &str) -> Result<Self, String> {
        let parts = token.split('|').collect::<Vec<_>>();
        let [field, interpretation, zone, year] = parts.as_slice() else {
            return Err("time field selection must have four '|' separated parts".into());
        };
        let field = if *field == "raw" {
            TimeFieldRef::RawPrefix
        } else if let Some(path) = field.strip_prefix("structured:") {
            TimeFieldRef::Structured(path.to_owned())
        } else if let Some(name) = field.strip_prefix("column:") {
            TimeFieldRef::Column(name.to_owned())
        } else {
            return Err(format!("unknown time field reference {field:?}"));
        };
        let interpretation = TimeInterpretation::parse_token(interpretation)
            .ok_or_else(|| format!("unknown time interpretation {interpretation:?}"))?;
        let zone = ZoneAssumption::parse_token(zone)
            .ok_or_else(|| format!("unknown timezone assumption {zone:?}"))?;
        let assumed_year = if *year == "-" {
            None
        } else {
            Some(
                year.parse::<i32>()
                    .map_err(|_| format!("invalid assumed year {year:?}"))?,
            )
        };
        Ok(Self {
            field,
            interpretation,
            zone,
            assumed_year,
        })
    }

    /// Reads one record. Row-local: no state outside `raw` participates.
    pub fn read(&self, raw: &[u8]) -> TimeOutcome {
        let label = self.field.label();
        if raw.len() > MAX_RECOGNITION_RECORD_BYTES {
            return TimeOutcome::Invalid {
                field: label,
                diagnostic: "record exceeds bounded event-time recognition limit".into(),
            };
        }
        let text = String::from_utf8_lossy(raw);
        match &self.field {
            TimeFieldRef::Column(name) => TimeOutcome::Invalid {
                field: label,
                diagnostic: format!(
                    "enriched column {name:?} is resolved by the query layer, not from raw bytes"
                ),
            },
            TimeFieldRef::RawPrefix => match raw_prefix_reading(&text, self) {
                Some(Ok(reading)) => TimeOutcome::Valid(reading),
                Some(Err(refusal)) => TimeOutcome::Invalid {
                    field: label,
                    diagnostic: refusal.message(),
                },
                None => TimeOutcome::Missing,
            },
            TimeFieldRef::Structured(path) => {
                let fields = match structured_fields(&text) {
                    Ok(fields) => fields,
                    Err(diagnostic) => {
                        return TimeOutcome::Invalid {
                            field: label,
                            diagnostic,
                        };
                    }
                };
                let Some(found) = fields.iter().find(|field| field.path == *path) else {
                    return TimeOutcome::Missing;
                };
                match interpret_value(found, self) {
                    Ok(reading) => TimeOutcome::Valid(reading),
                    Err(refusal) => TimeOutcome::Invalid {
                        field: label,
                        diagnostic: refusal.message(),
                    },
                }
            }
        }
    }
}

/// One record's event time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeReading {
    /// Field the value came from, as displayed to the user.
    pub field: String,
    pub unix_nanos: i64,
    pub format: TimeFormat,
    /// Non-empty when an assumption the source did not state was applied.
    pub assumption: Option<String>,
}

impl TimeReading {
    /// Single-line description for record details.
    pub fn note(&self) -> String {
        match &self.assumption {
            Some(assumption) => format!("{}; {assumption}", self.format.label()),
            None => match self.format {
                TimeFormat::OffsetDateTime => "explicit offset normalized to UTC".into(),
                other => other.label().to_owned(),
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TimeOutcome {
    Valid(TimeReading),
    Invalid { field: String, diagnostic: String },
    Missing,
}

/// Why a value could not be read as a time.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Refusal {
    NotATime(String),
    Invalid(String),
    AmbiguousEpoch(Vec<EpochUnit>),
    ImplausibleEpoch,
    NeedsZone,
    NeedsYear,
    NeedsUnit(Option<EpochUnit>),
}

impl Refusal {
    fn message(&self) -> String {
        match self {
            Refusal::NotATime(detail) | Refusal::Invalid(detail) => detail.clone(),
            Refusal::AmbiguousEpoch(units) => format!(
                "ambiguous epoch unit: plausible as {}; declare the unit explicitly",
                units
                    .iter()
                    .map(|unit| unit.token())
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
            Refusal::ImplausibleEpoch => format!(
                "numeric value is not a plausible epoch between {} and {} in seconds, \
                 milliseconds, microseconds or nanoseconds",
                PLAUSIBLE_EPOCH_START_SECONDS, PLAUSIBLE_EPOCH_END_SECONDS
            ),
            Refusal::NeedsZone => {
                "value has no timezone; declare a timezone assumption before using it".into()
            }
            Refusal::NeedsYear => {
                "value has no year; declare an assumed year before using it".into()
            }
            Refusal::NeedsUnit(hint) => match hint {
                Some(unit) => format!(
                    "numeric field is not a recognised time key; declare an epoch unit \
                     (values are plausible as {})",
                    unit.token()
                ),
                None => "numeric field is not a recognised time key; declare an epoch unit".into(),
            },
        }
    }
}

/// A value found in the record, with what its key implies about it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ExtractedField {
    path: String,
    value: String,
    /// The source encoded this as a JSON number rather than a string.
    numeric_type: bool,
    /// Rank in [`PRIORITY_KEYS`], or one past the end for suffix-hinted keys.
    priority: Option<usize>,
    /// Unit implied by a key suffix such as `_ms`. Never overrides the value.
    unit_hint: Option<EpochUnit>,
}

fn normalize_key(key: &str) -> String {
    key.trim().to_ascii_lowercase()
}

fn key_unit_hint(key: &str) -> Option<EpochUnit> {
    let (stem, unit) = [
        ("_ms", EpochUnit::Milliseconds),
        ("_millis", EpochUnit::Milliseconds),
        ("_us", EpochUnit::Microseconds),
        ("_micros", EpochUnit::Microseconds),
        ("_ns", EpochUnit::Nanoseconds),
        ("_nanos", EpochUnit::Nanoseconds),
        ("_s", EpochUnit::Seconds),
        ("_sec", EpochUnit::Seconds),
        ("_secs", EpochUnit::Seconds),
        ("_seconds", EpochUnit::Seconds),
    ]
    .into_iter()
    .find_map(|(suffix, unit)| key.strip_suffix(suffix).map(|stem| (stem, unit)))?;
    matches!(
        stem,
        "ts" | "time" | "timestamp" | "epoch" | "unix" | "unix_time" | "event_time"
    )
    .then_some(unit)
}

/// Priority of a key as an event-time candidate. `None` means the key says
/// nothing; such fields are still considered, but never read as bare epochs.
fn key_priority(key: &str) -> Option<usize> {
    if let Some(index) = PRIORITY_KEYS.iter().position(|known| *known == key) {
        return Some(index);
    }
    key_unit_hint(key).map(|_| PRIORITY_KEYS.len())
}

/// Extracts every candidate value from the *whole* record. This deliberately
/// does not reuse the display projection, which caps both bytes and field
/// count; recognition that inherited those caps would miss real timestamps.
fn structured_fields(text: &str) -> Result<Vec<ExtractedField>, String> {
    if let Ok(serde_json::Value::Object(object)) = serde_json::from_str::<serde_json::Value>(text) {
        let mut fields = Vec::new();
        collect_json(&serde_json::Value::Object(object), "", 0, &mut fields);
        return Ok(fields);
    }
    logfmt_pairs(text)
}

fn collect_json(
    value: &serde_json::Value,
    prefix: &str,
    depth: usize,
    out: &mut Vec<ExtractedField>,
) {
    let serde_json::Value::Object(object) = value else {
        return;
    };
    for (key, value) in object {
        if out.len() >= MAX_STRUCTURED_FIELDS {
            return;
        }
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            serde_json::Value::Object(_) if depth + 1 < MAX_JSON_DEPTH => {
                collect_json(value, &path, depth + 1, out);
            }
            serde_json::Value::String(text) => out.push(field(path, text.clone(), false)),
            serde_json::Value::Number(number) => out.push(field(path, number.to_string(), true)),
            _ => {}
        }
    }
}

fn field(path: String, value: String, numeric_type: bool) -> ExtractedField {
    let key = normalize_key(path.rsplit('.').next().unwrap_or(&path));
    ExtractedField {
        priority: key_priority(&key),
        unit_hint: key_unit_hint(&key),
        path,
        value,
        numeric_type,
    }
}

/// Full-record logfmt scan. Quoted values are consumed as opaque values so a
/// `message="... timestamp=..."` body can never masquerade as a key.
fn logfmt_pairs(text: &str) -> Result<Vec<ExtractedField>, String> {
    let bytes = text.as_bytes();
    let mut fields = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() && fields.len() < MAX_STRUCTURED_FIELDS {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        let key_start = cursor;
        while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() && bytes[cursor] != b'='
        {
            cursor += 1;
        }
        if cursor == bytes.len() || bytes[cursor] != b'=' {
            while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            continue;
        }
        let key = &text[key_start..cursor];
        cursor += 1;
        let value = if cursor < bytes.len() && bytes[cursor] == b'"' {
            cursor += 1;
            let mut value = String::new();
            let mut closed = false;
            while cursor < bytes.len() {
                match bytes[cursor] {
                    b'"' => {
                        cursor += 1;
                        closed = true;
                        break;
                    }
                    b'\\' => {
                        cursor += 1;
                        if cursor == bytes.len() {
                            break;
                        }
                        let escaped = text[cursor..]
                            .chars()
                            .next()
                            .expect("cursor is within text");
                        value.push(escaped);
                        cursor += escaped.len_utf8();
                    }
                    _ => {
                        let character = text[cursor..]
                            .chars()
                            .next()
                            .expect("cursor is within text");
                        value.push(character);
                        cursor += character.len_utf8();
                    }
                }
            }
            if !closed {
                return Err("malformed logfmt: unterminated quoted value".into());
            }
            value
        } else {
            let value_start = cursor;
            while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            text[value_start..cursor].to_owned()
        };
        if key.is_empty() {
            continue;
        }
        fields.push(field(key.to_owned(), value, false));
    }
    Ok(fields)
}

fn is_numeric_token(value: &str) -> bool {
    let body = value.strip_prefix('-').unwrap_or(value);
    let (whole, fraction) = body.split_once('.').unwrap_or((body, "0"));
    !whole.is_empty()
        && !fraction.is_empty()
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
}

/// Reads one extracted value under a declared selection.
fn interpret_value(
    found: &ExtractedField,
    selection: &TimeFieldSelection,
) -> Result<TimeReading, Refusal> {
    let value = found.value.trim();
    if value.is_empty() {
        return Err(Refusal::NotATime("value is empty".into()));
    }
    let numeric = found.numeric_type || is_numeric_token(value);
    let reading = |unix_nanos, format, assumption| TimeReading {
        field: found.path.clone(),
        unix_nanos,
        format,
        assumption,
    };
    match selection.interpretation {
        TimeInterpretation::Epoch(unit) => {
            if !numeric {
                return Err(Refusal::Invalid(format!(
                    "declared {} but the value is not numeric",
                    unit.label()
                )));
            }
            let unix_nanos = epoch_nanos(value, unit)?;
            Ok(reading(unix_nanos, TimeFormat::Epoch(unit), None))
        }
        TimeInterpretation::Text => {
            if numeric {
                return Err(Refusal::Invalid(
                    "declared a textual date-time but the value is numeric".into(),
                ));
            }
            let parsed = parse_text_time(value, selection)?;
            Ok(reading(parsed.unix_nanos, parsed.format, parsed.assumption))
        }
        TimeInterpretation::Auto if numeric => {
            // A bare number is only read as an epoch when the key says it is a
            // time. Otherwise `bytes=1788611445` would become an event time.
            if found.priority.is_none() {
                let hint = EpochUnit::ALL
                    .into_iter()
                    .find(|unit| plausible(value, *unit));
                return Err(Refusal::NeedsUnit(hint));
            }
            let unit = infer_epoch_unit(value)?;
            if let Some(hint) = found.unit_hint
                && hint != unit
            {
                return Err(Refusal::Invalid(format!(
                    "field name implies {} but the value is only plausible as {}",
                    hint.token(),
                    unit.token()
                )));
            }
            let unix_nanos = epoch_nanos(value, unit)?;
            Ok(reading(unix_nanos, TimeFormat::Epoch(unit), None))
        }
        TimeInterpretation::Auto => {
            let parsed = parse_text_time(value, selection)?;
            Ok(reading(parsed.unix_nanos, parsed.format, parsed.assumption))
        }
    }
}

fn plausible(value: &str, unit: EpochUnit) -> bool {
    if value.starts_with('-') {
        return false;
    }
    let whole = value.split(['.', ',']).next().unwrap_or(value);
    whole
        .parse::<i64>()
        .is_ok_and(|whole| unit.plausible_whole(whole))
}

/// Resolves the unit of a numeric epoch by plausible-range check. Exactly one
/// unit must match; zero or several are refused, never guessed.
fn infer_epoch_unit(value: &str) -> Result<EpochUnit, Refusal> {
    let matches = EpochUnit::ALL
        .into_iter()
        .filter(|unit| plausible(value, *unit))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [unit] => Ok(*unit),
        [] => Err(Refusal::ImplausibleEpoch),
        several => Err(Refusal::AmbiguousEpoch(several.to_vec())),
    }
}

fn epoch_nanos(value: &str, unit: EpochUnit) -> Result<i64, Refusal> {
    let negative = value.starts_with('-');
    let body = value.strip_prefix('-').unwrap_or(value);
    let (whole, fraction) = body.split_once(['.', ',']).unwrap_or((body, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Refusal::NotATime(format!(
            "value {value:?} is not a numeric epoch"
        )));
    }
    let whole = whole
        .parse::<i128>()
        .map_err(|_| Refusal::Invalid("epoch value overflows supported range".into()))?;
    let scale = i128::from(unit.nanos_per_unit());
    let mut nanos = whole * scale;
    if !fraction.is_empty() {
        let digits = fraction.len().min(18);
        let numerator = fraction[..digits]
            .parse::<i128>()
            .map_err(|_| Refusal::Invalid("epoch fraction overflows supported range".into()))?;
        nanos += numerator * scale / 10_i128.pow(digits as u32);
    }
    if negative {
        nanos = -nanos;
    }
    i64::try_from(nanos)
        .map_err(|_| Refusal::Invalid("epoch value overflows nanosecond range".into()))
}

struct ParsedText {
    unix_nanos: i64,
    format: TimeFormat,
    assumption: Option<String>,
}

/// Calendar fields recovered from text, before any assumption is applied.
struct ScannedTime {
    year: Option<i64>,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    nanos: i64,
    offset_seconds: Option<i64>,
    format: TimeFormat,
}

/// Parses a whole structured value as a calendar date-time.
fn parse_text_time(value: &str, selection: &TimeFieldSelection) -> Result<ParsedText, Refusal> {
    let (scanned, consumed) = scan_datetime(value)
        .or_else(|| scan_syslog(value))
        .ok_or_else(|| Refusal::NotATime(format!("value {value:?} is not a date-time")))?;
    if consumed != value.len() {
        return Err(Refusal::Invalid(format!(
            "value {value:?} has trailing content after the date-time"
        )));
    }
    finalize(scanned, selection)
}

fn finalize(scanned: ScannedTime, selection: &TimeFieldSelection) -> Result<ParsedText, Refusal> {
    let mut assumptions = Vec::new();
    let year = match scanned.year {
        Some(year) => year,
        None => {
            let year = selection.assumed_year.ok_or(Refusal::NeedsYear)?;
            assumptions.push(format!("value has no year; assumed {year}"));
            i64::from(year)
        }
    };
    let offset_seconds = match scanned.offset_seconds {
        Some(offset) => offset,
        None => {
            let offset = selection.zone.offset_seconds().ok_or(Refusal::NeedsZone)?;
            assumptions.push(selection.zone.note());
            i64::from(offset)
        }
    };
    if year < 1 || !(1..=12).contains(&scanned.month) {
        return Err(Refusal::Invalid("invalid calendar month".into()));
    }
    if scanned.day < 1 || scanned.day > days_in_month(year, scanned.month) {
        return Err(Refusal::Invalid("invalid calendar date".into()));
    }
    if scanned.hour > 23 || scanned.minute > 59 || scanned.second > 60 {
        return Err(Refusal::Invalid("invalid clock time".into()));
    }
    let seconds = civil_days(year, scanned.month, scanned.day)
        .checked_mul(86_400)
        .and_then(|base| {
            base.checked_add(scanned.hour * 3600 + scanned.minute * 60 + scanned.second)
        })
        .and_then(|base| base.checked_sub(offset_seconds))
        .ok_or_else(|| Refusal::Invalid("timestamp overflows supported range".into()))?;
    let unix_nanos = seconds
        .checked_mul(1_000_000_000)
        .and_then(|base| base.checked_add(scanned.nanos))
        .ok_or_else(|| Refusal::Invalid("timestamp overflows supported range".into()))?;
    Ok(ParsedText {
        unix_nanos,
        format: scanned.format,
        assumption: (!assumptions.is_empty()).then(|| assumptions.join("; ")),
    })
}

fn days_in_month(year: i64, month: i64) -> i64 {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ][(month - 1) as usize]
}

/// Days from the Unix epoch to a proleptic Gregorian civil date.
fn civil_days(year: i64, month: i64, day: i64) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let shifted = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year - 719_468
}

fn digits(bytes: &[u8], start: usize, len: usize) -> Option<i64> {
    let slice = bytes.get(start..start + len)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(slice).ok()?.parse::<i64>().ok()
}

/// Scans `YYYY-MM-DD(T| )HH:MM:SS[.fraction][Z|±HH:MM|±HHMM]` at the start of
/// `text`, returning the fields and the number of bytes consumed.
fn scan_datetime(text: &str) -> Option<(ScannedTime, usize)> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !matches!(bytes[10], b'T' | b't' | b' ') || bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let year = digits(bytes, 0, 4)?;
    let month = digits(bytes, 5, 2)?;
    let day = digits(bytes, 8, 2)?;
    let hour = digits(bytes, 11, 2)?;
    let minute = digits(bytes, 14, 2)?;
    let second = digits(bytes, 17, 2)?;
    let mut cursor = 19usize;
    let mut nanos = 0i64;
    if matches!(bytes.get(cursor), Some(b'.' | b',')) {
        let start = cursor + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == start {
            return None;
        }
        let taken = &text[start..end.min(start + 9)];
        nanos = format!("{taken:0<9}").parse::<i64>().ok()?;
        cursor = end;
    }
    let (offset_seconds, cursor) = match scan_offset(bytes, cursor) {
        Some((offset, next)) => (Some(offset), next),
        None => (None, cursor),
    };
    Some((
        ScannedTime {
            year: Some(year),
            month,
            day,
            hour,
            minute,
            second,
            nanos,
            offset_seconds,
            format: if offset_seconds.is_some() {
                TimeFormat::OffsetDateTime
            } else {
                TimeFormat::NaiveDateTime
            },
        },
        cursor,
    ))
}

fn scan_offset(bytes: &[u8], cursor: usize) -> Option<(i64, usize)> {
    match bytes.get(cursor)? {
        b'Z' | b'z' => Some((0, cursor + 1)),
        sign @ (b'+' | b'-') => {
            let sign = if *sign == b'+' { 1 } else { -1 };
            let hours = digits(bytes, cursor + 1, 2)?;
            let (minutes, end) = if bytes.get(cursor + 3) == Some(&b':') {
                (digits(bytes, cursor + 4, 2)?, cursor + 6)
            } else if let Some(minutes) = digits(bytes, cursor + 3, 2) {
                (minutes, cursor + 5)
            } else {
                (0, cursor + 3)
            };
            if hours > 23 || minutes > 59 {
                return None;
            }
            Some((sign * (hours * 3600 + minutes * 60), end))
        }
        _ => None,
    }
}

/// Scans syslog `Mmm D HH:MM:SS[.fraction]`. There is no year and no zone, so
/// the reading only completes when the caller has declared both.
fn scan_syslog(text: &str) -> Option<(ScannedTime, usize)> {
    let bytes = text.as_bytes();
    let name = text.get(0..3)?.to_ascii_lowercase();
    let month = MONTH_NAMES.iter().position(|known| *known == name)? as i64 + 1;
    let mut cursor = 3usize;
    let spaces = {
        let start = cursor;
        while matches!(bytes.get(cursor), Some(b' ')) {
            cursor += 1;
        }
        cursor - start
    };
    if !(1..=2).contains(&spaces) {
        return None;
    }
    let day = if let Some(day) = digits(bytes, cursor, 2) {
        cursor += 2;
        day
    } else {
        let day = digits(bytes, cursor, 1)?;
        cursor += 1;
        day
    };
    if bytes.get(cursor) != Some(&b' ') {
        return None;
    }
    cursor += 1;
    let hour = digits(bytes, cursor, 2)?;
    if bytes.get(cursor + 2) != Some(&b':') || bytes.get(cursor + 5) != Some(&b':') {
        return None;
    }
    let minute = digits(bytes, cursor + 3, 2)?;
    let second = digits(bytes, cursor + 6, 2)?;
    cursor += 8;
    let mut nanos = 0i64;
    if matches!(bytes.get(cursor), Some(b'.')) {
        let start = cursor + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end > start {
            let taken = &text[start..end.min(start + 9)];
            nanos = format!("{taken:0<9}").parse::<i64>().ok()?;
            cursor = end;
        }
    }
    Some((
        ScannedTime {
            year: None,
            month,
            day,
            hour,
            minute,
            second,
            nanos,
            offset_seconds: None,
            format: TimeFormat::SyslogDateTime,
        },
        cursor,
    ))
}

/// Byte offsets at which an unstructured record may begin its timestamp:
/// directly, after a syslog `<priority>`, after an RFC5424 `<priority>version`,
/// or inside a leading bracket.
fn prefix_starts(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut starts = Vec::new();
    let mut cursor = 0usize;
    while matches!(bytes.get(cursor), Some(b' ' | b'\t')) {
        cursor += 1;
    }
    starts.push(cursor);
    if bytes.get(cursor) == Some(&b'<')
        && let Some(end) = text[cursor..]
            .find('>')
            .map(|index| cursor + index + 1)
            .filter(|end| {
                text[cursor + 1..end - 1]
                    .bytes()
                    .all(|b| b.is_ascii_digit())
            })
    {
        starts.push(end);
        let mut version = end;
        while matches!(bytes.get(version), Some(byte) if byte.is_ascii_digit()) {
            version += 1;
        }
        if version > end && bytes.get(version) == Some(&b' ') {
            starts.push(version + 1);
        }
    }
    for start in starts.clone() {
        if matches!(bytes.get(start), Some(b'[' | b'(')) {
            starts.push(start + 1);
        }
    }
    starts.truncate(6);
    starts
}

/// Reads the leading date-time of an unstructured record. Only the record's
/// prefix is scanned, so a timestamp quoted inside a message body is never
/// mistaken for the record's own event time.
fn raw_prefix_reading(
    text: &str,
    selection: &TimeFieldSelection,
) -> Option<Result<TimeReading, Refusal>> {
    let mut end = MAX_RAW_PREFIX_BYTES.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let window = &text[..end];
    let starts = prefix_starts(window);
    let reading = |unix_nanos, format, assumption| TimeReading {
        field: "raw".into(),
        unix_nanos,
        format,
        assumption,
    };
    if let TimeInterpretation::Epoch(unit) = selection.interpretation {
        let token = window[*starts.first()?..]
            .split_ascii_whitespace()
            .next()
            .filter(|token| is_numeric_token(token))?;
        return Some(
            epoch_nanos(token, unit)
                .map(|unix_nanos| reading(unix_nanos, TimeFormat::Epoch(unit), None)),
        );
    }
    let scanned = starts
        .into_iter()
        .find_map(|start| {
            let rest = window.get(start..)?;
            scan_datetime(rest).or_else(|| scan_syslog(rest))
        })
        .map(|(scanned, _)| scanned)?;
    Some(
        finalize(scanned, selection)
            .map(|parsed| reading(parsed.unix_nanos, parsed.format, parsed.assumption)),
    )
}

/// Bounds and declared assumptions for automatic recognition.
#[derive(Clone, Debug, PartialEq)]
pub struct RecognitionOptions {
    /// Records inspected when ranking candidate fields.
    pub maximum_sampled_records: usize,
    /// Distinct field paths tracked while ranking.
    pub maximum_candidate_fields: usize,
    /// Fraction of sampled records a field must yield a time for, to be chosen.
    pub minimum_coverage: f64,
    pub zone: ZoneAssumption,
    pub assumed_year: Option<i32>,
}

impl Default for RecognitionOptions {
    fn default() -> Self {
        Self {
            maximum_sampled_records: 512,
            maximum_candidate_fields: 64,
            minimum_coverage: 0.5,
            zone: ZoneAssumption::Reject,
            assumed_year: None,
        }
    }
}

impl RecognitionOptions {
    fn selection(&self, field: TimeFieldRef) -> TimeFieldSelection {
        TimeFieldSelection {
            field,
            interpretation: TimeInterpretation::Auto,
            zone: self.zone,
            assumed_year: self.assumed_year,
        }
    }
}

/// Reads one record with no prior selection. Row-local, so a record read alone
/// and the same record read inside any batch produce the same outcome.
///
/// Precedence: a field whose *key* names a time wins outright, even when its
/// value is unusable, because silently reading some other field would hide a
/// broken source. Otherwise any structured value that parses as a date-time is
/// used, and finally the record's own leading prefix.
pub fn recognize_record(raw: &[u8], options: &RecognitionOptions) -> TimeOutcome {
    if raw.len() > MAX_RECOGNITION_RECORD_BYTES {
        return TimeOutcome::Invalid {
            field: "record".into(),
            diagnostic: "record exceeds bounded event-time recognition limit".into(),
        };
    }
    let text = String::from_utf8_lossy(raw);
    let raw_selection = options.selection(TimeFieldRef::RawPrefix);
    let fields = match structured_fields(&text) {
        Ok(fields) => fields,
        Err(diagnostic) => {
            return match raw_prefix_reading(&text, &raw_selection) {
                Some(Ok(reading)) => TimeOutcome::Valid(reading),
                _ => TimeOutcome::Invalid {
                    field: "logfmt".into(),
                    diagnostic,
                },
            };
        }
    };
    if let Some(found) = fields
        .iter()
        .filter(|found| found.priority.is_some())
        .min_by_key(|found| found.priority)
    {
        let selection = options.selection(TimeFieldRef::Structured(found.path.clone()));
        return match interpret_value(found, &selection) {
            Ok(reading) => TimeOutcome::Valid(reading),
            Err(refusal) => TimeOutcome::Invalid {
                field: found.path.clone(),
                diagnostic: refusal.message(),
            },
        };
    }
    let mut deferred: Option<(String, String)> = None;
    for found in &fields {
        let selection = options.selection(TimeFieldRef::Structured(found.path.clone()));
        match interpret_value(found, &selection) {
            Ok(reading) => return TimeOutcome::Valid(reading),
            Err(refusal @ (Refusal::NeedsZone | Refusal::NeedsYear)) => {
                deferred.get_or_insert((found.path.clone(), refusal.message()));
            }
            Err(_) => {}
        }
    }
    match raw_prefix_reading(&text, &raw_selection) {
        Some(Ok(reading)) => TimeOutcome::Valid(reading),
        Some(Err(refusal)) => TimeOutcome::Invalid {
            field: "raw".into(),
            diagnostic: refusal.message(),
        },
        None => match deferred {
            Some((field, diagnostic)) => TimeOutcome::Invalid { field, diagnostic },
            None => TimeOutcome::Missing,
        },
    }
}

/// Evidence gathered for one candidate field over the sample.
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateSummary {
    /// The selection this candidate proposes, with any discovered epoch unit
    /// pinned so later batches cannot re-infer a different one.
    pub selection: TimeFieldSelection,
    pub label: String,
    /// Records in which the field was present at all.
    pub observed: usize,
    /// Records in which it yielded a time.
    pub parsed: usize,
    /// Records refused because the epoch unit was ambiguous.
    pub ambiguous: usize,
    /// Records refused for any other reason.
    pub rejected: usize,
    /// `parsed` over the sampled record count.
    pub coverage: f64,
    /// `observed` over the sampled record count.
    pub presence: f64,
    pub formats: Vec<TimeFormat>,
    /// Set when the candidate cannot be used as it stands.
    pub blocked: Option<String>,
    /// Assumptions that were applied to reach `parsed`.
    pub assumptions: Vec<String>,
}

/// Ranked recognition evidence over a bounded sample.
#[derive(Clone, Debug, PartialEq)]
pub struct RecognitionReport {
    pub sampled_records: usize,
    /// Records skipped for exceeding the recognition byte bound.
    pub oversized_records: usize,
    /// Best usable candidate, if any cleared `minimum_coverage`.
    pub selected: Option<TimeFieldSelection>,
    /// All candidates, best first. Blocked candidates are retained so the
    /// reason a field was not chosen stays visible.
    pub candidates: Vec<CandidateSummary>,
    pub diagnostics: Vec<String>,
}

impl RecognitionReport {
    pub fn selected_summary(&self) -> Option<&CandidateSummary> {
        let selection = self.selected.as_ref()?;
        self.candidates
            .iter()
            .find(|candidate| candidate.selection == *selection)
    }
}

#[derive(Default)]
struct CandidateStats {
    priority: Option<usize>,
    observed: usize,
    parsed: usize,
    ambiguous: usize,
    rejected: usize,
    needs_zone: usize,
    needs_year: usize,
    needs_unit: usize,
    unit_hint: Option<EpochUnit>,
    formats: Vec<TimeFormat>,
    assumptions: Vec<String>,
    epoch_units: Vec<EpochUnit>,
}

/// Ranks candidate fields over a bounded sample of records.
///
/// Sampling only decides *which* field to use. The returned selection is then
/// applied one record at a time, so results never depend on how records were
/// partitioned into batches.
pub fn recognize_sample<'a, I>(records: I, options: &RecognitionOptions) -> RecognitionReport
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut order: Vec<String> = Vec::new();
    let mut stats: std::collections::HashMap<String, CandidateStats> =
        std::collections::HashMap::new();
    let mut sampled_records = 0usize;
    let mut oversized_records = 0usize;
    let mut malformed_records = 0usize;
    for raw in records.into_iter().take(options.maximum_sampled_records) {
        if raw.len() > MAX_RECOGNITION_RECORD_BYTES {
            oversized_records += 1;
            continue;
        }
        sampled_records += 1;
        let text = String::from_utf8_lossy(raw);
        let mut fields = match structured_fields(&text) {
            Ok(fields) => fields,
            Err(_) => {
                malformed_records += 1;
                Vec::new()
            }
        };
        let raw_selection = options.selection(TimeFieldRef::RawPrefix);
        if let Some(outcome) = raw_prefix_reading(&text, &raw_selection) {
            let entry = entry(
                &mut order,
                &mut stats,
                RAW_CANDIDATE_KEY,
                options.maximum_candidate_fields,
            );
            if let Some(entry) = entry {
                entry.observed += 1;
                record_outcome(entry, outcome);
            }
        }
        fields.truncate(MAX_STRUCTURED_FIELDS);
        for found in &fields {
            let Some(entry) = entry(
                &mut order,
                &mut stats,
                &found.path,
                options.maximum_candidate_fields,
            ) else {
                continue;
            };
            entry.priority = found.priority;
            entry.unit_hint = found.unit_hint;
            entry.observed += 1;
            let selection = options.selection(TimeFieldRef::Structured(found.path.clone()));
            record_outcome(entry, interpret_value(found, &selection));
        }
    }
    let mut candidates = order
        .iter()
        .filter_map(|path| summarize(path, stats.get(path)?, sampled_records, options))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .coverage
            .partial_cmp(&left.coverage)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.blocked.is_some().cmp(&right.blocked.is_some()))
            .then_with(|| rank(left).cmp(&rank(right)))
            .then_with(|| left.label.cmp(&right.label))
    });
    let selected = candidates
        .iter()
        .find(|candidate| {
            candidate.blocked.is_none()
                && candidate.parsed > 0
                && candidate.coverage + f64::EPSILON >= options.minimum_coverage
        })
        .map(|candidate| candidate.selection.clone());
    let mut diagnostics = Vec::new();
    if sampled_records == 0 {
        diagnostics.push("no records were available to sample".into());
    }
    if oversized_records > 0 {
        diagnostics.push(format!(
            "{oversized_records} records exceeded the {MAX_RECOGNITION_RECORD_BYTES} byte \
             recognition bound and were not sampled"
        ));
    }
    if malformed_records > 0 {
        diagnostics.push(format!(
            "{malformed_records} records were not parseable as JSON or logfmt; only their raw \
             prefix was considered"
        ));
    }
    if selected.is_none() && sampled_records > 0 {
        diagnostics.push(match candidates.first() {
            Some(best) => format!(
                "no field reached {:.0}% coverage; best was {} at {:.0}%{}",
                options.minimum_coverage * 100.0,
                best.label,
                best.coverage * 100.0,
                best.blocked
                    .as_ref()
                    .map(|reason| format!(" ({reason})"))
                    .unwrap_or_default()
            ),
            None => "no candidate timestamp field was found".into(),
        });
    }
    RecognitionReport {
        sampled_records,
        oversized_records,
        selected,
        candidates,
        diagnostics,
    }
}

fn rank(candidate: &CandidateSummary) -> (usize, usize) {
    let structured = usize::from(candidate.selection.field == TimeFieldRef::RawPrefix);
    (structured, candidate.label.len())
}

fn entry<'a>(
    order: &mut Vec<String>,
    stats: &'a mut std::collections::HashMap<String, CandidateStats>,
    path: &str,
    maximum: usize,
) -> Option<&'a mut CandidateStats> {
    if !stats.contains_key(path) {
        if order.len() >= maximum {
            return None;
        }
        order.push(path.to_owned());
        stats.insert(path.to_owned(), CandidateStats::default());
    }
    stats.get_mut(path)
}

fn record_outcome(entry: &mut CandidateStats, outcome: Result<TimeReading, Refusal>) {
    match outcome {
        Ok(reading) => {
            entry.parsed += 1;
            if !entry.formats.contains(&reading.format) {
                entry.formats.push(reading.format);
            }
            if let TimeFormat::Epoch(unit) = reading.format
                && !entry.epoch_units.contains(&unit)
            {
                entry.epoch_units.push(unit);
            }
            if let Some(assumption) = reading.assumption
                && !entry.assumptions.contains(&assumption)
            {
                entry.assumptions.push(assumption);
            }
        }
        Err(Refusal::AmbiguousEpoch(_)) => entry.ambiguous += 1,
        Err(Refusal::NeedsZone) => entry.needs_zone += 1,
        Err(Refusal::NeedsYear) => entry.needs_year += 1,
        Err(Refusal::NeedsUnit(hint)) => {
            entry.needs_unit += 1;
            entry.unit_hint = entry.unit_hint.or(hint);
        }
        Err(_) => entry.rejected += 1,
    }
}

fn summarize(
    path: &str,
    stats: &CandidateStats,
    sampled_records: usize,
    options: &RecognitionOptions,
) -> Option<CandidateSummary> {
    let usable = stats.parsed + stats.ambiguous + stats.needs_zone + stats.needs_year;
    if usable == 0 && stats.needs_unit == 0 {
        return None;
    }
    let (field, label) = if path == RAW_CANDIDATE_KEY {
        (TimeFieldRef::RawPrefix, "raw".to_owned())
    } else {
        (TimeFieldRef::Structured(path.to_owned()), path.to_owned())
    };
    let mut interpretation = TimeInterpretation::Auto;
    let mut blocked = None;
    if stats.epoch_units.len() > 1 {
        blocked = Some(format!(
            "values were read as {} in different records; declare one unit",
            stats
                .epoch_units
                .iter()
                .map(|unit| unit.token())
                .collect::<Vec<_>>()
                .join(" and ")
        ));
    } else if let Some(unit) = stats.epoch_units.first() {
        interpretation = TimeInterpretation::Epoch(*unit);
    } else if stats.parsed > 0 {
        interpretation = TimeInterpretation::Text;
    }
    if blocked.is_none() && stats.parsed == 0 {
        blocked = Some(if stats.ambiguous > 0 {
            "epoch unit is ambiguous; declare seconds, milliseconds, microseconds or nanoseconds"
                .into()
        } else if stats.needs_zone > 0 {
            "values carry no timezone; declare a timezone assumption".into()
        } else if stats.needs_year > 0 {
            "values carry no year; declare an assumed year".into()
        } else {
            match stats.unit_hint {
                Some(unit) => format!(
                    "field is not a recognised time key; declare {} to use it",
                    unit.label()
                ),
                None => {
                    "field is not a recognised time key; declare an epoch unit to use it".into()
                }
            }
        });
        if let Some(unit) = stats.unit_hint
            && stats.needs_unit > 0
        {
            interpretation = TimeInterpretation::Epoch(unit);
        }
    } else if blocked.is_none() && stats.ambiguous > 0 {
        blocked = Some(format!(
            "{} of {sampled_records} sampled records had an ambiguous epoch unit",
            stats.ambiguous
        ));
    }
    let ratio = |count: usize| {
        if sampled_records == 0 {
            0.0
        } else {
            count as f64 / sampled_records as f64
        }
    };
    Some(CandidateSummary {
        selection: TimeFieldSelection {
            field,
            interpretation,
            zone: options.zone,
            assumed_year: options.assumed_year,
        },
        label,
        observed: stats.observed,
        parsed: stats.parsed,
        ambiguous: stats.ambiguous,
        rejected: stats.rejected + stats.needs_zone + stats.needs_year + stats.needs_unit,
        coverage: ratio(stats.parsed),
        presence: ratio(stats.observed),
        formats: stats.formats.clone(),
        blocked,
        assumptions: stats.assumptions.clone(),
    })
}
