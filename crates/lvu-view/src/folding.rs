//! Repeated-pattern folding.
//!
//! A flood of near-identical events (retry storms, reconnect loops, one stack
//! trace repeating every 50ms) buries everything else in a live view. This
//! module collapses such runs into a single foldable entry carrying a count and
//! the first/last occurrence, without touching the underlying records.
//!
//! Invariants this module maintains:
//!
//! * Folding is derived, reversible presentation. Every constituent event stays
//!   individually addressable by its stable [`RowId`]; nothing is dropped,
//!   reordered, merged or rewritten, and no filter behaviour changes because a
//!   fold exists. Expanding all entries reproduces the input exactly
//!   ([`expand_entries`]).
//! * Decisions depend only on the prefix of events already seen, so a feed
//!   partitioned into arbitrary batches folds identically to a whole-frame feed.
//! * Every accumulator is capped: tracked patterns, run length, retained
//!   entries, retained member references and retained sample characters.
//!   Eviction is deterministic and documented on [`FoldConfig`].
//! * Text is normalised and truncated by characters, never by byte slicing, and
//!   truncation never splits a base character from its combining marks.
//!
//! Folding is off by default ([`FoldConfig::default`] has `enabled: false`).
//!
//! # The fold key is one column
//!
//! A run is a maximal group of consecutive events sharing one key, and the key
//! is the value of exactly one column ([`FoldKey`]). The default column is a
//! derived one, `pattern`: the event's text with volatile substrings replaced
//! and the level prefixed ([`pattern_key`]). Any other column — including an
//! enrichment column the user built — supplies its value **as-is**: no
//! normalisation, no level prefix, no substitution. Folding on several fields
//! is therefore not a second mechanism here; it is an enrichment column that
//! concatenates them, and this module still sees one column.
//!
//! Consequences worth stating, because they are the whole difference:
//!
//! * [`FoldConfig::aggressiveness`] applies to the derived `pattern` column and
//!   to nothing else. A column key is never rewritten, so a field the user did
//!   not ask about is never replaced.
//! * An event whose row does not carry the key column at all does not fold. It
//!   becomes its own entry, exactly as it would with folding disabled, rather
//!   than joining every other event that is missing the same column.
//! * A column value is still truncated to [`FoldConfig::maximum_key_chars`] on
//!   a character boundary, so the retained key stays bounded.

use lvu::{DisplayRow, RowId};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// How aggressively volatile substrings are replaced before comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Normalisation {
    /// Only unambiguous machine identifiers: timestamps, UUIDs, IPv4
    /// addresses, `0x`-prefixed hex and numbers.
    Conservative,
    /// Adds quoted values, filesystem paths and bare long hex identifiers.
    Standard,
    /// Adds any mixed letter/digit token, so `worker-7a` and `worker-31b`
    /// share a pattern.
    Aggressive,
}

impl Normalisation {
    fn quotes_and_paths(self) -> bool {
        !matches!(self, Normalisation::Conservative)
    }

    fn bare_hex(self) -> bool {
        !matches!(self, Normalisation::Conservative)
    }

    fn mixed_tokens(self) -> bool {
        matches!(self, Normalisation::Aggressive)
    }
}

/// Which earlier events a new event may join.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FoldScope {
    /// Only an immediately preceding run of the same pattern folds. Any
    /// intervening event of a different shape ends the run.
    Adjacent,
    /// A run stays open while at most `n` events of other shapes intervene, so
    /// two interleaved floods still fold. `Lookback(0)` equals [`Self::Adjacent`].
    Lookback(usize),
}

impl FoldScope {
    /// Maximum distance in event positions between a run's last member and a
    /// new member that may join it.
    fn gap_limit(self) -> u64 {
        match self {
            FoldScope::Adjacent => 1,
            FoldScope::Lookback(window) => (window as u64).saturating_add(1),
        }
    }
}

/// Which column supplies the fold key.
///
/// One per view. Not a field of [`FoldConfig`], which stays `Copy` because
/// every other knob is a number or a flag; a key names a column, and the name
/// travels beside the numbers rather than making all of them allocate.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum FoldKey {
    /// The derived `pattern` column: the event text normalised per
    /// [`FoldConfig::aggressiveness`], with a non-empty level prefixed. This is
    /// the default, and it is what folding did before a key could be chosen.
    #[default]
    Pattern,
    /// A named column of the row. Its value is the key **as-is**: no
    /// normalisation, no level prefix. A row that does not carry the column
    /// does not fold.
    Column(String),
}

impl FoldKey {
    /// The column name shown to a user, and the token persisted for it.
    pub fn column(&self) -> Option<&str> {
        match self {
            FoldKey::Pattern => None,
            FoldKey::Column(name) => Some(name.as_str()),
        }
    }

    /// `None`, an empty name and a name of only whitespace all mean the derived
    /// pattern column, so a stored blank can never select a column that cannot
    /// exist.
    pub fn from_column(name: Option<&str>) -> Self {
        match name.map(str::trim) {
            None | Some("") => FoldKey::Pattern,
            Some(name) => FoldKey::Column(name.to_owned()),
        }
    }
}

/// Folding policy. All caps are hard; see field docs for the eviction rule that
/// applies when each is reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FoldConfig {
    /// Off by default. When disabled every event becomes its own unfolded
    /// entry and no normalisation work is done.
    pub enabled: bool,
    /// Smallest member count that renders as a fold. Runs shorter than this
    /// stay visible as individual entries. Values below 2 are treated as 2.
    pub minimum_run: usize,
    /// Whether only adjacent runs fold, or a bounded lookback window applies.
    pub scope: FoldScope,
    /// Normalisation aggressiveness used to derive the pattern key.
    pub aggressiveness: Normalisation,
    /// Maximum simultaneously open (extendable) patterns. When exceeded the
    /// least recently extended run is closed; it keeps its members and stays
    /// visible, it simply can no longer absorb new events.
    pub maximum_patterns: usize,
    /// Maximum members in one fold. On overflow the run is closed and a new
    /// entry with the same pattern starts, so counts stay bounded and no member
    /// is lost.
    pub maximum_run: usize,
    /// Maximum retained entries. On overflow the oldest entries are evicted
    /// from the front until the count drops to 3/4 of this cap.
    pub maximum_entries: usize,
    /// Maximum retained member references across all entries. On overflow the
    /// oldest entries are evicted from the front until the total drops to 3/4
    /// of this cap.
    pub maximum_members: usize,
    /// Characters retained per stored sample. Each entry keeps two samples
    /// (first and last occurrence).
    pub maximum_sample_chars: usize,
    /// Characters retained in a pattern key. Longer keys are truncated, so two
    /// events sharing a long normalised prefix fold together.
    pub maximum_key_chars: usize,
    /// Characters of input text examined when deriving a key. Text beyond this
    /// point is ignored, bounding per-event normalisation cost.
    pub maximum_scan_chars: usize,
}

impl Default for FoldConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            minimum_run: 3,
            scope: FoldScope::Adjacent,
            aggressiveness: Normalisation::Standard,
            maximum_patterns: 256,
            maximum_run: 100_000,
            maximum_entries: 8_192,
            maximum_members: 65_536,
            maximum_sample_chars: 512,
            maximum_key_chars: 256,
            maximum_scan_chars: 4_096,
        }
    }
}

impl FoldConfig {
    /// Enabled policy with defaults, for callers that only want to switch it on.
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    fn effective_minimum_run(&self) -> usize {
        self.minimum_run.max(2)
    }
}

/// One constituent event of an entry, in arrival order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FoldMember {
    pub id: RowId,
    /// Position of this event in the engine's overall input sequence. Merging
    /// members from several entries by ascending position reproduces the
    /// original order.
    pub position: u64,
    pub timestamp_unix_nanos: Option<i64>,
}

/// A presentation entry: either a single event or a collapsed run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FoldEntry {
    /// Normalised shape shared by every member. Empty when folding is disabled.
    pub pattern: Arc<str>,
    /// Members in original order. Never empty.
    pub members: Vec<FoldMember>,
    /// True once the member count reaches the configured minimum run.
    pub folded: bool,
    /// Truncated display text of the first occurrence.
    pub first_sample: String,
    /// Truncated display text of the most recent occurrence.
    pub last_sample: String,
}

impl FoldEntry {
    pub fn count(&self) -> usize {
        self.members.len()
    }

    pub fn first(&self) -> &FoldMember {
        self.members.first().expect("entries always have a member")
    }

    pub fn last(&self) -> &FoldMember {
        self.members.last().expect("entries always have a member")
    }

    /// Constituent record identities in original order.
    pub fn expand(&self) -> Vec<RowId> {
        self.members
            .iter()
            .map(|member| member.id.clone())
            .collect()
    }

    pub fn contains(&self, id: &RowId) -> bool {
        self.members.iter().any(|member| &member.id == id)
    }

    fn retained_bytes(&self) -> usize {
        let member_bytes: usize = self
            .members
            .iter()
            .map(|member| size_of::<FoldMember>() + member.id.source_id.len())
            .sum();
        self.pattern.len() + self.first_sample.len() + self.last_sample.len() + member_bytes
    }
}

/// Bounded accounting for diagnostics and tests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FoldStats {
    pub entries: usize,
    pub retained_members: usize,
    pub open_patterns: usize,
    /// Entries evicted from the front because a retention cap was reached.
    pub evicted_entries: u64,
    /// Member references dropped with those entries.
    pub evicted_members: u64,
    /// Events observed since construction or the last reset.
    pub observed_events: u64,
    /// Approximate retained heap bytes for entries.
    pub retained_bytes: usize,
}

/// One event offered to the engine.
#[derive(Clone, Copy, Debug)]
pub struct FoldEvent<'a> {
    pub id: &'a RowId,
    pub text: &'a str,
    pub level: &'a str,
    pub timestamp_unix_nanos: Option<i64>,
    /// The row's named column values, in the row's own order, for a
    /// [`FoldKey::Column`] key. Empty is a row with no columns, which folds
    /// only under [`FoldKey::Pattern`].
    pub columns: &'a [(String, String)],
}

impl<'a> FoldEvent<'a> {
    /// This event's raw value for `column`, if the row carries it.
    fn column(&self, column: &str) -> Option<&'a str> {
        self.columns
            .iter()
            .find(|(name, _)| name == column)
            .map(|(_, value)| value.as_str())
    }
}

impl<'a> From<&'a DisplayRow> for FoldEvent<'a> {
    fn from(row: &'a DisplayRow) -> Self {
        Self {
            id: &row.id,
            text: &row.text,
            level: &row.level,
            timestamp_unix_nanos: row.captured_at_unix_nanos,
            columns: &row.fields,
        }
    }
}

/// An immutable snapshot the UI can render from.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FoldFrame {
    pub entries: Vec<FoldEntry>,
    pub stats: FoldStats,
}

impl FoldFrame {
    /// Index of the entry containing `id`, if it is still retained.
    pub fn entry_of(&self, id: &RowId) -> Option<usize> {
        self.entries.iter().position(|entry| entry.contains(id))
    }

    /// Every retained record identity in original order.
    pub fn expand(&self) -> Vec<RowId> {
        expand_entries(&self.entries)
    }

    /// Total retained events across entries.
    pub fn total_members(&self) -> usize {
        self.entries.iter().map(FoldEntry::count).sum()
    }
}

struct OpenRun {
    serial: u64,
    last_position: u64,
}

/// Incremental folding state machine.
///
/// [`FoldEngine::push`] and [`FoldEngine::extend`] fold new arrivals onto
/// existing state without recomputing history. Because every decision looks
/// only at events already seen, batch boundaries cannot change the result.
pub struct FoldEngine {
    config: FoldConfig,
    /// Which column the key comes from. Constant for the engine's life: a
    /// changed key is a different partition of the same stream, so the caller
    /// builds a new engine rather than mutating this one.
    key: FoldKey,
    entries: Vec<FoldEntry>,
    /// Serial of `entries[0]`; serials are stable across front eviction.
    base_serial: u64,
    open: HashMap<Arc<str>, OpenRun>,
    /// last_position -> pattern, so the least recently extended open run is the
    /// first entry. Positions are unique per event, so this is injective.
    recency: BTreeMap<u64, Arc<str>>,
    /// Position given to the first event, so a reset resumes the same numbering.
    first_position: u64,
    next_position: u64,
    retained_members: usize,
    stats: FoldStats,
}

impl FoldEngine {
    /// An engine keyed on the derived `pattern` column, numbering from zero.
    pub fn new(config: FoldConfig) -> Self {
        Self::with_key(config, FoldKey::Pattern)
    }

    /// An engine keyed on `key`, numbering from zero.
    pub fn with_key(config: FoldConfig, key: FoldKey) -> Self {
        Self::starting_at(config, key, 0)
    }

    /// An engine keyed on `key` whose first event is numbered `first_position`.
    ///
    /// A caller that does not consume its stream from the beginning — one that
    /// folds the window a user is looking at before the millions of rows in
    /// front of it — needs member positions it can compare against its own
    /// stream. Numbering is the only thing this changes: which events share a
    /// run, and every cap and eviction rule, are exactly as they are from zero.
    pub fn starting_at(config: FoldConfig, key: FoldKey, first_position: u64) -> Self {
        Self {
            config,
            key,
            entries: Vec::new(),
            base_serial: 0,
            open: HashMap::new(),
            recency: BTreeMap::new(),
            first_position,
            next_position: first_position,
            retained_members: 0,
            stats: FoldStats::default(),
        }
    }

    /// The position the first event fed to this engine was numbered with.
    pub fn first_position(&self) -> u64 {
        self.first_position
    }

    pub fn config(&self) -> &FoldConfig {
        &self.config
    }

    pub fn key(&self) -> &FoldKey {
        &self.key
    }

    /// Discard all derived state. Records are untouched; the caller re-feeds.
    pub fn reset(&mut self) {
        self.entries.clear();
        self.base_serial = 0;
        self.open.clear();
        self.recency.clear();
        self.next_position = self.first_position;
        self.retained_members = 0;
        self.stats = FoldStats::default();
    }

    pub fn entries(&self) -> &[FoldEntry] {
        &self.entries
    }

    pub fn stats(&self) -> FoldStats {
        let mut stats = self.stats;
        stats.entries = self.entries.len();
        stats.retained_members = self.retained_members;
        stats.open_patterns = self.open.len();
        stats.retained_bytes = self.entries.iter().map(FoldEntry::retained_bytes).sum();
        stats
    }

    pub fn frame(&self) -> FoldFrame {
        FoldFrame {
            entries: self.entries.clone(),
            stats: self.stats(),
        }
    }

    /// Every retained record identity in original order.
    pub fn expand(&self) -> Vec<RowId> {
        expand_entries(&self.entries)
    }

    pub fn extend<'a, I>(&mut self, events: I)
    where
        I: IntoIterator<Item = FoldEvent<'a>>,
    {
        for event in events {
            self.push(event);
        }
    }

    pub fn extend_rows<'a, I>(&mut self, rows: I)
    where
        I: IntoIterator<Item = &'a DisplayRow>,
    {
        for row in rows {
            self.push(FoldEvent::from(row));
        }
    }

    pub fn push(&mut self, event: FoldEvent<'_>) {
        let position = self.next_position;
        self.next_position = self.next_position.saturating_add(1);
        self.stats.observed_events = self.stats.observed_events.saturating_add(1);

        if !self.config.enabled {
            self.start_entry(Arc::from(""), event, position, false);
            self.enforce_retention();
            return;
        }

        // A row that does not carry the key column has no key, so it cannot
        // join or start a run: it becomes its own entry, exactly as it would
        // with folding off. Grouping every such row together would fold on the
        // *absence* of a value, which is not what the user asked to fold on.
        let Some(key) = fold_key(&event, &self.key, &self.config) else {
            self.start_entry(Arc::from(""), event, position, false);
            self.enforce_retention();
            return;
        };
        let pattern: Arc<str> = Arc::from(key);
        self.close_stale(position);

        if let Some(run) = self.open.get(&pattern) {
            let serial = run.serial;
            if let Some(index) = self.index_of(serial)
                && self.entries[index].members.len() < self.config.maximum_run
            {
                self.append_member(index, event, position);
                self.touch(&pattern, position);
                self.enforce_retention();
                return;
            }
            self.close_pattern(&pattern);
        }

        self.start_entry(pattern, event, position, true);
        self.enforce_retention();
    }

    fn index_of(&self, serial: u64) -> Option<usize> {
        let offset = serial.checked_sub(self.base_serial)? as usize;
        (offset < self.entries.len()).then_some(offset)
    }

    fn append_member(&mut self, index: usize, event: FoldEvent<'_>, position: u64) {
        let minimum = self.config.effective_minimum_run();
        let sample = truncate_chars(event.text, self.config.maximum_sample_chars);
        let entry = &mut self.entries[index];
        entry.members.push(FoldMember {
            id: event.id.clone(),
            position,
            timestamp_unix_nanos: event.timestamp_unix_nanos,
        });
        entry.last_sample = sample;
        entry.folded = entry.members.len() >= minimum;
        self.retained_members += 1;
    }

    fn start_entry(&mut self, pattern: Arc<str>, event: FoldEvent<'_>, position: u64, open: bool) {
        let serial = self.base_serial + self.entries.len() as u64;
        let sample = truncate_chars(event.text, self.config.maximum_sample_chars);
        self.entries.push(FoldEntry {
            pattern: Arc::clone(&pattern),
            members: vec![FoldMember {
                id: event.id.clone(),
                position,
                timestamp_unix_nanos: event.timestamp_unix_nanos,
            }],
            folded: false,
            first_sample: sample.clone(),
            last_sample: sample,
        });
        self.retained_members += 1;
        if open {
            self.close_pattern(&pattern);
            self.open.insert(
                Arc::clone(&pattern),
                OpenRun {
                    serial,
                    last_position: position,
                },
            );
            self.recency.insert(position, pattern);
            self.enforce_pattern_cap();
        }
    }

    fn touch(&mut self, pattern: &Arc<str>, position: u64) {
        if let Some(run) = self.open.get_mut(pattern) {
            self.recency.remove(&run.last_position);
            run.last_position = position;
            self.recency.insert(position, Arc::clone(pattern));
        }
    }

    fn close_pattern(&mut self, pattern: &str) {
        if let Some(run) = self.open.remove(pattern) {
            self.recency.remove(&run.last_position);
        }
    }

    /// Close runs whose last member is further back than the scope allows.
    fn close_stale(&mut self, position: u64) {
        let limit = self.config.scope.gap_limit();
        let threshold = position.saturating_sub(limit);
        while let Some((&last, _)) = self.recency.iter().next() {
            if last >= threshold {
                break;
            }
            if let Some(pattern) = self.recency.remove(&last) {
                self.open.remove(&pattern);
            }
        }
    }

    /// Deterministic pattern eviction: least recently extended run first.
    fn enforce_pattern_cap(&mut self) {
        let cap = self.config.maximum_patterns.max(1);
        while self.open.len() > cap {
            let Some((&last, _)) = self.recency.iter().next() else {
                break;
            };
            if let Some(pattern) = self.recency.remove(&last) {
                self.open.remove(&pattern);
            }
        }
    }

    /// Deterministic retention eviction: oldest entries first, in batches down
    /// to 3/4 of the exceeded cap so eviction stays amortised constant.
    fn enforce_retention(&mut self) {
        let entry_cap = self.config.maximum_entries.max(1);
        let member_cap = self.config.maximum_members.max(1);
        if self.entries.len() <= entry_cap && self.retained_members <= member_cap {
            return;
        }
        let entry_target = entry_cap - entry_cap / 4;
        let member_target = member_cap - member_cap / 4;
        let mut evicted = 0usize;
        while evicted < self.entries.len()
            && (self.entries.len() - evicted > entry_target
                || self.retained_members > member_target)
        {
            let entry = &self.entries[evicted];
            self.retained_members -= entry.members.len();
            self.stats.evicted_members = self
                .stats
                .evicted_members
                .saturating_add(entry.members.len() as u64);
            evicted += 1;
        }
        if evicted == 0 {
            return;
        }
        let closed: Vec<Arc<str>> = self
            .entries
            .drain(..evicted)
            .map(|entry| entry.pattern)
            .collect();
        for pattern in closed {
            self.close_pattern(&pattern);
        }
        self.base_serial += evicted as u64;
        self.stats.evicted_entries = self.stats.evicted_entries.saturating_add(evicted as u64);
    }
}

/// Fold a bounded, ordered slice in one pass on the derived `pattern` column.
/// Equivalent to feeding the same rows to a fresh [`FoldEngine`].
pub fn fold_rows(config: FoldConfig, rows: &[DisplayRow]) -> FoldFrame {
    fold_rows_by(config, FoldKey::Pattern, rows)
}

/// [`fold_rows`] keyed on an arbitrary column.
pub fn fold_rows_by(config: FoldConfig, key: FoldKey, rows: &[DisplayRow]) -> FoldFrame {
    let mut engine = FoldEngine::with_key(config, key);
    engine.extend_rows(rows);
    engine.frame()
}

/// Merge the constituents of several entries back into original order.
///
/// With [`FoldScope::Adjacent`] the entries are already contiguous; with a
/// lookback window they interleave, and the recorded positions restore the
/// original sequence exactly.
pub fn expand_entries(entries: &[FoldEntry]) -> Vec<RowId> {
    let mut merged: Vec<(u64, &RowId)> = entries
        .iter()
        .flat_map(|entry| {
            entry
                .members
                .iter()
                .map(|member| (member.position, &member.id))
        })
        .collect();
    merged.sort_by_key(|(position, _)| *position);
    merged
        .into_iter()
        .map(|(_, id)| id.clone())
        .collect::<Vec<_>>()
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// The fold key for one event under `key`, or `None` when the event has no key
/// and therefore cannot fold.
///
/// [`FoldKey::Pattern`] derives it with [`pattern_key`] and always answers.
/// [`FoldKey::Column`] answers with the column's value **unchanged** — no
/// normalisation, no level prefix, nothing substituted — truncated to
/// [`FoldConfig::maximum_key_chars`] on a character boundary so the retained
/// key stays bounded. A row without the column answers `None`.
///
/// An *empty* value is a value: rows whose key column is present but blank
/// share a key and fold together, because that is what the column says about
/// them. A row where the column is absent is a different statement and gets a
/// different answer.
pub fn fold_key(event: &FoldEvent<'_>, key: &FoldKey, config: &FoldConfig) -> Option<String> {
    match key {
        FoldKey::Pattern => Some(pattern_key(event.text, event.level, config)),
        FoldKey::Column(column) => event
            .column(column)
            .map(|value| truncate_chars(value, config.maximum_key_chars)),
    }
}

// ---------------------------------------------------------------------------
// Normalisation
// ---------------------------------------------------------------------------

/// Derive the pattern key for one event.
///
/// Rules are applied in a fixed order at each character position; the first
/// match wins and consumes its span:
///
/// 1. timestamps — `YYYY-MM-DD[T ]HH:MM:SS[.frac][Z|±HH:MM]` and bare
///    `HH:MM:SS[.frac]` become `<ts>`
/// 2. UUIDs — `8-4-4-4-12` hex become `<uuid>`
/// 3. IPv4 addresses become `<ip>`
/// 4. quoted values — `"…"` / `'…'` become `<str>` (Standard, Aggressive)
/// 5. filesystem paths — tokens starting `/`, `./`, `../`, `~/` become `<path>`
///    (Standard, Aggressive)
/// 6. hex identifiers — `0x…` at any level, and bare hex tokens of eight or
///    more characters containing a letter at Standard and Aggressive, become
///    `<hex>`
/// 7. tokens mixing letters and digits become `<tok>` (Aggressive only);
///    separators such as `-` still split tokens, so `worker-7a` normalises to
///    `worker-<tok>` rather than collapsing wholesale
/// 8. numbers — optionally signed, decimal point and exponent — become `<num>`
/// 9. runs of whitespace collapse to one space
/// 10. any other character is copied through
///
/// Rules 1–3 and 5–7 only match at a token boundary. A non-empty `level` is
/// prefixed as `level|` so that the same shape at different severities does not
/// collapse. Input beyond `maximum_scan_chars` is ignored and the result is
/// truncated to `maximum_key_chars`.
pub fn pattern_key(text: &str, level: &str, config: &FoldConfig) -> String {
    let chars: Vec<char> = text.chars().take(config.maximum_scan_chars).collect();
    let mode = config.aggressiveness;
    let mut out = String::with_capacity(chars.len().min(config.maximum_key_chars) + 8);
    if !level.is_empty() {
        for character in level.chars().take(32) {
            out.push(character);
        }
        out.push('|');
    }

    let mut i = 0usize;
    while i < chars.len() {
        let boundary = i == 0 || !is_token_char(chars[i - 1]);

        if chars[i].is_whitespace() {
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            continue;
        }

        if boundary {
            if let Some(end) = match_timestamp(&chars, i) {
                out.push_str("<ts>");
                i = end;
                continue;
            }
            if let Some(end) = match_uuid(&chars, i) {
                out.push_str("<uuid>");
                i = end;
                continue;
            }
            if let Some(end) = match_ipv4(&chars, i) {
                out.push_str("<ip>");
                i = end;
                continue;
            }
        }

        if mode.quotes_and_paths() {
            if let Some(end) = match_quoted(&chars, i) {
                out.push_str("<str>");
                i = end;
                continue;
            }
            if boundary && let Some(end) = match_path(&chars, i) {
                out.push_str("<path>");
                i = end;
                continue;
            }
        }

        if boundary && let Some(end) = match_hex(&chars, i, mode.bare_hex()) {
            out.push_str("<hex>");
            i = end;
            continue;
        }

        if mode.mixed_tokens()
            && boundary
            && let Some(end) = match_mixed_token(&chars, i)
        {
            out.push_str("<tok>");
            i = end;
            continue;
        }

        if let Some(end) = match_number(&chars, i, boundary) {
            out.push_str("<num>");
            i = end;
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }

    truncate_chars(out.trim(), config.maximum_key_chars)
}

fn is_token_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn is_hex(character: char) -> bool {
    character.is_ascii_hexdigit()
}

fn run_len(chars: &[char], start: usize, predicate: impl Fn(char) -> bool) -> usize {
    let mut end = start;
    while end < chars.len() && predicate(chars[end]) {
        end += 1;
    }
    end - start
}

fn exact_digits(chars: &[char], start: usize, count: usize) -> Option<usize> {
    if start + count > chars.len() {
        return None;
    }
    if chars[start..start + count]
        .iter()
        .all(|character| character.is_ascii_digit())
    {
        Some(start + count)
    } else {
        None
    }
}

fn literal(chars: &[char], start: usize, expected: char) -> Option<usize> {
    (chars.get(start) == Some(&expected)).then_some(start + 1)
}

fn match_clock(chars: &[char], start: usize) -> Option<usize> {
    let mut at = exact_digits(chars, start, 2)?;
    at = literal(chars, at, ':')?;
    at = exact_digits(chars, at, 2)?;
    at = literal(chars, at, ':')?;
    at = exact_digits(chars, at, 2)?;
    if chars.get(at) == Some(&'.') || chars.get(at) == Some(&',') {
        let fraction = run_len(chars, at + 1, |character| character.is_ascii_digit());
        if fraction > 0 {
            at += 1 + fraction;
        }
    }
    Some(at)
}

fn match_zone(chars: &[char], start: usize) -> usize {
    match chars.get(start) {
        Some('Z') | Some('z') => start + 1,
        Some('+') | Some('-') => {
            let Some(mut at) = exact_digits(chars, start + 1, 2) else {
                return start;
            };
            if chars.get(at) == Some(&':') {
                at += 1;
            }
            exact_digits(chars, at, 2).unwrap_or(start)
        }
        _ => start,
    }
}

fn match_timestamp(chars: &[char], start: usize) -> Option<usize> {
    if let Some(end) = match_iso_datetime(chars, start) {
        return Some(end);
    }
    let end = match_clock(chars, start)?;
    // A bare clock must not be the head of a longer identifier.
    (!chars.get(end).copied().is_some_and(is_token_char)).then_some(end)
}

fn match_iso_datetime(chars: &[char], start: usize) -> Option<usize> {
    let mut at = exact_digits(chars, start, 4)?;
    at = literal(chars, at, '-')?;
    at = exact_digits(chars, at, 2)?;
    at = literal(chars, at, '-')?;
    at = exact_digits(chars, at, 2)?;
    let separated = match chars.get(at) {
        Some('T') | Some('t') | Some(' ') => Some(at + 1),
        _ => None,
    };
    if let Some(after) = separated
        && let Some(clock_end) = match_clock(chars, after)
    {
        return Some(match_zone(chars, clock_end));
    }
    Some(at)
}

fn match_uuid(chars: &[char], start: usize) -> Option<usize> {
    let groups = [8usize, 4, 4, 4, 12];
    let mut at = start;
    for (index, size) in groups.iter().enumerate() {
        if index > 0 {
            at = literal(chars, at, '-')?;
        }
        if at + size > chars.len() || !chars[at..at + size].iter().all(|c| is_hex(*c)) {
            return None;
        }
        at += size;
    }
    (!chars.get(at).copied().is_some_and(is_token_char)).then_some(at)
}

fn match_ipv4(chars: &[char], start: usize) -> Option<usize> {
    let mut at = start;
    for index in 0..4 {
        if index > 0 {
            at = literal(chars, at, '.')?;
        }
        let digits = run_len(chars, at, |character| character.is_ascii_digit());
        if !(1..=3).contains(&digits) {
            return None;
        }
        at += digits;
    }
    (!chars.get(at).copied().is_some_and(is_token_char)).then_some(at)
}

fn match_quoted(chars: &[char], start: usize) -> Option<usize> {
    let quote = *chars.get(start)?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let mut at = start + 1;
    while at < chars.len() {
        match chars[at] {
            '\\' => at += 2,
            character if character == quote => return Some(at + 1),
            _ => at += 1,
        }
    }
    None
}

fn is_path_char(character: char) -> bool {
    character.is_alphanumeric()
        || matches!(
            character,
            '/' | '.' | '_' | '-' | '+' | '@' | '%' | '~' | '*'
        )
}

fn match_path(chars: &[char], start: usize) -> Option<usize> {
    let head = *chars.get(start)?;
    let anchored = match head {
        '/' => true,
        '.' | '~' => {
            let mut probe = start + 1;
            if head == '.' && chars.get(probe) == Some(&'.') {
                probe += 1;
            }
            chars.get(probe) == Some(&'/')
        }
        _ => false,
    };
    if !anchored {
        return None;
    }
    let end = start + run_len(chars, start, is_path_char);
    (end - start >= 2).then_some(end)
}

fn match_hex(chars: &[char], start: usize, bare: bool) -> Option<usize> {
    if chars.get(start) == Some(&'0')
        && matches!(chars.get(start + 1), Some('x') | Some('X'))
        && chars.get(start + 2).copied().is_some_and(is_hex)
    {
        let digits = run_len(chars, start + 2, is_hex);
        return Some(start + 2 + digits);
    }
    if !bare {
        return None;
    }
    let digits = run_len(chars, start, is_hex);
    if digits < 8
        || chars
            .get(start + digits)
            .copied()
            .is_some_and(is_token_char)
    {
        return None;
    }
    let has_letter = chars[start..start + digits]
        .iter()
        .any(|character| character.is_ascii_alphabetic());
    has_letter.then_some(start + digits)
}

fn match_mixed_token(chars: &[char], start: usize) -> Option<usize> {
    let length = run_len(chars, start, is_token_char);
    if length == 0 {
        return None;
    }
    let span = &chars[start..start + length];
    let has_digit = span.iter().any(|character| character.is_ascii_digit());
    let has_letter = span.iter().any(|character| character.is_alphabetic());
    (has_digit && has_letter).then_some(start + length)
}

fn match_number(chars: &[char], start: usize, boundary: bool) -> Option<usize> {
    let mut at = start;
    if boundary && matches!(chars.get(at), Some('+') | Some('-')) {
        at += 1;
    }
    let integer = run_len(chars, at, |character| character.is_ascii_digit());
    if integer == 0 {
        return None;
    }
    at += integer;
    if chars.get(at) == Some(&'.') {
        let fraction = run_len(chars, at + 1, |character| character.is_ascii_digit());
        if fraction > 0 {
            at += 1 + fraction;
        }
    }
    if matches!(chars.get(at), Some('e') | Some('E')) {
        let mut probe = at + 1;
        if matches!(chars.get(probe), Some('+') | Some('-')) {
            probe += 1;
        }
        let exponent = run_len(chars, probe, |character| character.is_ascii_digit());
        if exponent > 0 {
            at = probe + exponent;
        }
    }
    Some(at)
}

// ---------------------------------------------------------------------------
// Unicode-safe truncation
// ---------------------------------------------------------------------------

/// Approximate combining-mark test used to keep truncation on cluster
/// boundaries. It covers the general combining blocks, variation selectors,
/// the zero-width joiner and emoji skin-tone modifiers; it is deliberately a
/// conservative approximation rather than a full grapheme segmenter, because
/// this crate takes no Unicode-table dependency.
fn is_combining(character: char) -> bool {
    matches!(character as u32,
        0x0300..=0x036F
        | 0x0483..=0x0489
        | 0x0591..=0x05BD
        | 0x0610..=0x061A
        | 0x064B..=0x065F
        | 0x0670
        | 0x06D6..=0x06DC
        | 0x0730..=0x074A
        | 0x07A6..=0x07B0
        | 0x093A..=0x093C
        | 0x0941..=0x0948
        | 0x094D
        | 0x0951..=0x0957
        | 0x0E31
        | 0x0E34..=0x0E3A
        | 0x0E47..=0x0E4E
        | 0x1AB0..=0x1AFF
        | 0x1DC0..=0x1DFF
        | 0x200D
        | 0x20D0..=0x20F0
        | 0xFE00..=0xFE0F
        | 0xFE20..=0xFE2F
        | 0x1F3FB..=0x1F3FF
    )
}

/// Number of user-perceived characters, counting combining marks with the base
/// character they attach to.
pub fn character_count(text: &str) -> usize {
    text.chars().filter(|c| !is_combining(*c)).count()
}

/// Truncate to at most `limit` characters without splitting a UTF-8 sequence
/// or separating a base character from its combining marks.
pub fn truncate_chars(text: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut counted = 0usize;
    for character in text.chars() {
        if is_combining(character) {
            if counted == 0 {
                continue;
            }
            out.push(character);
            continue;
        }
        if counted == limit {
            return out;
        }
        counted += 1;
        out.push(character);
    }
    out
}
