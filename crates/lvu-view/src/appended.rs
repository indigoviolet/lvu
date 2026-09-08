//! A published sequence that a refresh extends rather than rebuilds.
//!
//! Membership is published as immutable snapshots that readers hold for as long
//! as they like, so a refresh cannot mutate what it already published. Rebuilding
//! the whole sequence to add the handful of records that just arrived satisfies
//! that, but costs what the view already holds on every refresh — a cost that
//! grows with the view while the work that caused it does not.
//!
//! So the snapshot is a list of immutable chunks instead of one array. Appending
//! adds a chunk and copies only the pointers to the others. Left alone that
//! would accumulate a chunk per refresh, and the pointer copy — and every
//! lookup's search for the right chunk — would grow instead. Merging keeps the
//! chunk sizes strictly decreasing, which bounds the count at `log2(len)`: the
//! same trick a binary counter plays, and it costs each element `O(log n)`
//! copies over its whole life rather than one per refresh.

use std::sync::Arc;

/// An append-only sequence, cheap to extend and cheap to share.
#[derive(Clone, Debug)]
pub(crate) struct Appended<T> {
    /// Oldest first, sizes strictly decreasing except where a merge is pending.
    chunks: Vec<Arc<[T]>>,
    /// `starts[i]` is the index `chunks[i]` begins at. Same length as `chunks`.
    starts: Vec<usize>,
    len: usize,
}

impl<T> Default for Appended<T> {
    fn default() -> Self {
        Self {
            chunks: Vec::new(),
            starts: Vec::new(),
            len: 0,
        }
    }
}

impl<T: Clone> Appended<T> {
    pub(crate) fn from_vec(values: Vec<T>) -> Self {
        let mut value = Self::default();
        value.extend(values);
        value
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn get(&self, index: usize) -> Option<&T> {
        let chunk = self.chunk_of(index)?;
        self.chunks[chunk].get(index - self.starts[chunk])
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        self.chunks.iter().flat_map(|chunk| chunk.iter())
    }

    /// The chunk holding `index`, by binary search over the starts.
    fn chunk_of(&self, index: usize) -> Option<usize> {
        if index >= self.len {
            return None;
        }
        Some(match self.starts.binary_search(&index) {
            Ok(exact) => exact,
            Err(after) => after - 1,
        })
    }

    /// Remove and return the last value.
    ///
    /// Chunk sizes decrease towards the back, so the chunk this rebuilds is the
    /// smallest one. It exists for the one caller that has to reopen what it
    /// published — display grouping, whose run can continue across the boundary
    /// between one refresh and the next — and nothing else should need it.
    pub(crate) fn pop_last(&mut self) -> Option<T> {
        let last = self.chunks.last()?.last()?.clone();
        let chunk = self.chunks.pop().expect("checked");
        let start = self.starts.pop().expect("same length as chunks");
        self.len -= chunk.len();
        if chunk.len() > 1 {
            let kept: Arc<[T]> = chunk[..chunk.len() - 1].to_vec().into();
            self.starts.push(start);
            self.len += kept.len();
            self.chunks.push(kept);
        }
        Some(last)
    }

    /// Append `values`, merging trailing chunks so their sizes stay decreasing.
    pub(crate) fn extend(&mut self, values: Vec<T>) {
        if values.is_empty() {
            return;
        }
        let mut pending: Arc<[T]> = values.into();
        // Merge while the chunk being added is at least as large as the one
        // before it. Sizes are decreasing, so this stops after `O(log n)` steps
        // and each step at least doubles what it produced.
        while self
            .chunks
            .last()
            .is_some_and(|last| last.len() <= pending.len())
        {
            let last = self.chunks.pop().expect("checked");
            self.starts.pop();
            self.len -= last.len();
            let mut merged = Vec::with_capacity(last.len() + pending.len());
            merged.extend(last.iter().cloned());
            merged.extend(pending.iter().cloned());
            pending = merged.into();
        }
        self.starts.push(self.len);
        self.len += pending.len();
        self.chunks.push(pending);
    }
}

impl<T: Clone> Appended<T> {
    /// Index of the first value for which `predicate` is false, over a sequence
    /// partitioned by it. The same contract as `slice::partition_point`.
    pub(crate) fn partition_point(&self, predicate: impl Fn(&T) -> bool) -> usize {
        let mut low = 0usize;
        let mut high = self.len;
        while low < high {
            let middle = low + (high - low) / 2;
            match self.get(middle) {
                Some(value) if predicate(value) => low = middle + 1,
                _ => high = middle,
            }
        }
        low
    }
}

impl<T: Clone + Ord> Appended<T> {
    /// `Ok(index)` when `value` is present. The whole sequence is ascending, so
    /// each chunk is ascending and the chunks are ascending between them.
    pub(crate) fn binary_search(&self, value: &T) -> Result<usize, usize> {
        let mut low = 0usize;
        let mut high = self.chunks.len();
        while low < high {
            let middle = (low + high) / 2;
            match self.chunks[middle][0].cmp(value) {
                std::cmp::Ordering::Greater => high = middle,
                _ => low = middle + 1,
            }
        }
        if low == 0 {
            return Err(0);
        }
        let chunk = low - 1;
        match self.chunks[chunk].binary_search(value) {
            Ok(index) => Ok(self.starts[chunk] + index),
            Err(index) => Err(self.starts[chunk] + index),
        }
    }
}

/// The worker's side of an append-only sequence: what was published last time,
/// plus what this refresh has matched so far.
///
/// A scan pushes record by record, so the new records accumulate in a plain
/// `Vec` and become one chunk at publication. That keeps a full scan exactly as
/// cheap as it was — one contiguous build — while a refresh over a settled view
/// leaves the published chunks untouched.
#[derive(Debug)]
pub(crate) struct AppendedBuilder<T> {
    base: Appended<T>,
    added: Vec<T>,
}

impl<T: Clone> AppendedBuilder<T> {
    pub(crate) fn new(base: Appended<T>) -> Self {
        Self {
            base,
            added: Vec::new(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.base.len() + self.added.len()
    }

    /// The most recently appended value, whether it arrived this pass or last.
    pub(crate) fn last(&self) -> Option<&T> {
        self.added
            .last()
            .or_else(|| self.base.get(self.base.len().checked_sub(1)?))
    }

    pub(crate) fn push(&mut self, value: T) {
        self.added.push(value);
    }

    /// The last value, mutable.
    ///
    /// Reopening a value that was already published means taking it back out of
    /// the published chunks first, which is why this is not free and why only
    /// display grouping uses it: a run of repeated records can begin before a
    /// refresh and continue after it, and the group that describes it has to
    /// grow rather than be split at a boundary the user cannot see.
    pub(crate) fn last_mut(&mut self) -> Option<&mut T> {
        if self.added.is_empty()
            && let Some(reopened) = self.base.pop_last()
        {
            self.added.push(reopened);
        }
        self.added.last_mut()
    }

    pub(crate) fn finish(mut self) -> Appended<T> {
        self.base.extend(self.added);
        self.base
    }
}

impl<T: Clone> FromIterator<T> for Appended<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from_vec(iter.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flattened(value: &Appended<u64>) -> Vec<u64> {
        value.iter().copied().collect()
    }

    #[test]
    fn extending_preserves_order_and_indexing() {
        let mut value = Appended::default();
        let mut expected = Vec::new();
        for round in 0..64u64 {
            let batch: Vec<u64> = (0..=round).map(|offset| round * 100 + offset).collect();
            expected.extend(batch.iter().copied());
            value.extend(batch);
            assert_eq!(value.len(), expected.len());
            assert_eq!(flattened(&value), expected);
            for (index, wanted) in expected.iter().enumerate() {
                assert_eq!(value.get(index), Some(wanted), "index {index}");
            }
            assert_eq!(value.get(expected.len()), None);
        }
    }

    #[test]
    fn chunk_count_stays_logarithmic_under_one_at_a_time_appends() {
        let mut value = Appended::default();
        for index in 0..4096u64 {
            value.extend(vec![index]);
        }
        assert_eq!(value.len(), 4096);
        // 4096 single appends would be 4096 chunks without merging.
        assert!(value.chunks.len() <= 13, "chunks: {}", value.chunks.len());
    }

    #[test]
    fn appending_copies_what_arrived_not_what_is_held() {
        // The merge rule is what bounds this: over `n` single appends the total
        // element copying is `O(n log n)`, so the average per append is a log
        // factor rather than the length of the sequence.
        let mut value: Appended<u64> = Appended::default();
        let mut copies = 0usize;
        for index in 0..8192u64 {
            let before: usize = value.chunks.iter().map(|chunk| chunk.len()).sum();
            value.extend(vec![index]);
            let after: usize = value.chunks.iter().map(|chunk| chunk.len()).sum();
            // Every merge rewrites the chunks it consumed.
            copies += after.saturating_sub(before);
        }
        assert!(copies < 8192 * 16, "total copies {copies}");
    }

    #[test]
    fn binary_search_spans_chunks() {
        let mut value = Appended::default();
        for round in 0..40u64 {
            value.extend(vec![round * 2, round * 2 + 1]);
        }
        for wanted in 0..80u64 {
            assert_eq!(value.binary_search(&wanted), Ok(wanted as usize));
        }
        assert_eq!(value.binary_search(&80), Err(80));
        assert_eq!(value.binary_search(&1000), Err(80));
    }

    #[test]
    fn an_empty_extend_changes_nothing() {
        let mut value: Appended<u64> = Appended::from_vec(vec![1, 2, 3]);
        value.extend(Vec::new());
        assert_eq!(flattened(&value), vec![1, 2, 3]);
        assert_eq!(value.len(), 3);
    }

    #[test]
    fn a_builder_reads_across_what_it_inherited_and_what_it_added() {
        let mut builder = AppendedBuilder::new(Appended::from_vec(vec![1u64, 2, 3]));
        assert_eq!(builder.len(), 3);
        assert_eq!(builder.last(), Some(&3));
        builder.push(4);
        builder.push(5);
        assert_eq!(builder.len(), 5);
        assert_eq!(builder.last(), Some(&5));
        let finished = builder.finish();
        assert_eq!(
            finished.iter().copied().collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn reopening_the_last_value_takes_it_out_of_what_was_published() {
        let mut builder = AppendedBuilder::new(Appended::from_vec(vec![1u64, 2, 3]));
        *builder.last_mut().expect("a last value") = 30;
        builder.push(4);
        let finished = builder.finish();
        assert_eq!(
            finished.iter().copied().collect::<Vec<_>>(),
            vec![1, 2, 30, 4]
        );
        assert_eq!(finished.len(), 4);
        assert_eq!(finished.get(2), Some(&30));
    }

    #[test]
    fn reopening_repeatedly_reopens_the_same_value_once() {
        // Grouping calls this for every record it folds into the run, so the
        // second and later calls must find the value already reopened rather
        // than take another one out of what was published.
        let mut base = Appended::default();
        for round in 0..50u64 {
            base.extend(vec![round]);
        }
        let mut builder = AppendedBuilder::new(base);
        for _ in 0..5 {
            *builder.last_mut().expect("a last value") += 1000;
        }
        assert_eq!(builder.len(), 50);
        let finished = builder.finish();
        assert_eq!(finished.len(), 50);
        let values: Vec<u64> = finished.iter().copied().collect();
        assert_eq!(values[49], 49 + 5000);
        for (index, value) in values.iter().enumerate().take(49) {
            assert_eq!(*value, index as u64);
        }
    }

    #[test]
    fn partition_point_matches_the_slice_form() {
        let mut value = Appended::default();
        let mut flat = Vec::new();
        for round in 0..30u64 {
            let batch: Vec<u64> = (0..=round).map(|offset| round * 50 + offset).collect();
            flat.extend(batch.iter().copied());
            value.extend(batch);
        }
        for probe in [0u64, 1, 49, 50, 700, 1_499, 5_000] {
            assert_eq!(
                value.partition_point(|item| *item <= probe),
                flat.partition_point(|item| *item <= probe),
                "probe {probe}"
            );
        }
    }

    #[test]
    fn popping_the_last_value_empties_cleanly() {
        let mut value = Appended::from_vec(vec![7u64]);
        assert_eq!(value.pop_last(), Some(7));
        assert!(value.is_empty());
        assert_eq!(value.pop_last(), None);
        assert_eq!(value.get(0), None);
    }

    #[test]
    fn a_builder_over_nothing_reports_nothing() {
        let builder: AppendedBuilder<u64> = AppendedBuilder::new(Appended::default());
        assert_eq!(builder.len(), 0);
        assert_eq!(builder.last(), None);
        assert!(builder.finish().is_empty());
    }

    #[test]
    fn an_empty_sequence_answers_without_panicking() {
        let value: Appended<u64> = Appended::default();
        assert!(value.is_empty());
        assert_eq!(value.get(0), None);
        assert_eq!(value.binary_search(&5), Err(0));
        assert_eq!(flattened(&value), Vec::<u64>::new());
    }
}
