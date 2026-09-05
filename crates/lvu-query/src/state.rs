use crate::{
    BatchQuery, BatchResult, BatchValidity, CompiledDefinition, EnrichmentStage, TextSearch,
    execute_batch,
};
use polars::prelude::DataFrame;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Debug)]
pub struct DefinitionState {
    pub draft: String,
    pub applied: Option<CompiledDefinition>,
    pub definition_generation: u64,
}
impl DefinitionState {
    pub fn new() -> Self {
        Self {
            draft: String::new(),
            applied: None,
            definition_generation: 0,
        }
    }
    pub fn edit(&mut self, draft: String) {
        self.draft = draft;
    }
    pub fn apply(&mut self, compiled: CompiledDefinition) {
        self.draft = compiled.source.clone();
        self.applied = Some(compiled);
        self.definition_generation = self.definition_generation.saturating_add(1);
    }
}
impl Default for DefinitionState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryProgress {
    pub generation: u64,
    pub completed_batches: usize,
    pub total_batches: Option<usize>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryCommitMetadata {
    pub generation: u64,
    pub definition_generation: u64,
    pub batch_count: usize,
    pub matched_count: usize,
}

#[derive(Clone, Default)]
pub struct QueryCancellation(Arc<AtomicBool>);
impl QueryCancellation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Default)]
struct GenerationInner {
    current: u64,
    committed: Option<QueryCommitMetadata>,
}
pub struct QueryGenerationState {
    inner: Mutex<GenerationInner>,
}
impl QueryGenerationState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(GenerationInner::default()),
        }
    }
    pub fn begin(&self) -> u64 {
        let mut inner = self.inner.lock().expect("generation mutex poisoned");
        inner.current = inner.current.saturating_add(1);
        inner.current
    }
    pub fn current(&self) -> u64 {
        self.inner
            .lock()
            .expect("generation mutex poisoned")
            .current
    }
    pub fn committed(&self) -> Option<QueryCommitMetadata> {
        self.inner
            .lock()
            .expect("generation mutex poisoned")
            .committed
            .clone()
    }
    pub fn committed_generation(&self) -> Option<u64> {
        self.committed().map(|value| value.generation)
    }
    fn commit_with(
        &self,
        metadata: QueryCommitMetadata,
        cancelled: &QueryCancellation,
        publish: impl FnOnce(),
    ) -> bool {
        let mut inner = self.inner.lock().expect("generation mutex poisoned");
        if inner.current != metadata.generation || cancelled.is_cancelled() {
            return false;
        }
        publish();
        inner.committed = Some(metadata);
        true
    }
}
impl Default for QueryGenerationState {
    fn default() -> Self {
        Self::new()
    }
}

pub trait BatchResultSink {
    fn store_candidate(&mut self, result: BatchResult) -> Result<(), String>;
    fn publish(&mut self);
    fn abort(&mut self);
}
pub struct BoundedPageSink {
    capacity: usize,
    candidate: VecDeque<BatchResult>,
    published: VecDeque<BatchResult>,
}
impl BoundedPageSink {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            capacity,
            candidate: VecDeque::new(),
            published: VecDeque::new(),
        }
    }
    pub fn published(&self) -> &VecDeque<BatchResult> {
        &self.published
    }
}
impl BatchResultSink for BoundedPageSink {
    fn store_candidate(&mut self, result: BatchResult) -> Result<(), String> {
        if self.candidate.len() == self.capacity {
            self.candidate.pop_front();
        }
        self.candidate.push_back(result);
        Ok(())
    }
    fn publish(&mut self) {
        self.published = std::mem::take(&mut self.candidate);
    }
    fn abort(&mut self) {
        self.candidate.clear();
    }
}

pub struct QueryPlan<'a> {
    pub definition_generation: u64,
    pub stages: &'a [EnrichmentStage],
    pub filter: Option<&'a CompiledDefinition>,
    pub text_search: Option<&'a TextSearch>,
    pub colors: &'a [(String, CompiledDefinition)],
}
pub struct QueryExecution<'a> {
    pub generation: u64,
    pub total_batches: Option<usize>,
    pub plan: QueryPlan<'a>,
    pub cancellation: &'a QueryCancellation,
}
pub fn execute_bounded_batches(
    generations: &QueryGenerationState,
    batches: impl IntoIterator<Item = DataFrame>,
    execution: QueryExecution<'_>,
    sink: &mut impl BatchResultSink,
    mut progress: impl FnMut(QueryProgress),
) -> bool {
    let QueryExecution {
        generation,
        total_batches,
        plan,
        cancellation,
    } = execution;
    if cancellation.is_cancelled() || generation != generations.current() {
        sink.abort();
        return false;
    }
    let mut batch_count = 0;
    let mut matched_count = 0;
    for batch in batches {
        if cancellation.is_cancelled() || generation != generations.current() {
            sink.abort();
            return false;
        }
        let result = execute_batch(
            &batch,
            BatchQuery {
                generation,
                definition_generation: plan.definition_generation,
                stages: plan.stages,
                filter: plan.filter,
                text_search: plan.text_search,
                colors: plan.colors,
            },
        );
        if result.validity != BatchValidity::Valid {
            sink.abort();
            return false;
        }
        batch_count += 1;
        matched_count += result.matched_ids.len();
        if sink.store_candidate(result).is_err() {
            sink.abort();
            return false;
        }
        progress(QueryProgress {
            generation,
            completed_batches: batch_count,
            total_batches,
        });
    }
    if cancellation.is_cancelled() {
        sink.abort();
        return false;
    }
    let metadata = QueryCommitMetadata {
        generation,
        definition_generation: plan.definition_generation,
        batch_count,
        matched_count,
    };
    let committed = generations.commit_with(metadata, cancellation, || sink.publish());
    if !committed {
        sink.abort();
    }
    committed
}
