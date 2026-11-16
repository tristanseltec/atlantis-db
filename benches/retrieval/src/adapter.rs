//! The interface every system under test implements.
//!
//! Adapters run the same logical workload in their own query language. An
//! adapter that cannot do part of a workload natively says so in
//! [`Capabilities`] and the report shows it, instead of the harness hiding
//! the difference.

use std::time::Duration;

use serde::Serialize;

use crate::dataset::{Dataset, Query};

/// What the system does natively. Reported next to every result.
#[derive(Clone, Debug, Serialize)]
pub struct Capabilities {
    /// The filter is applied inside the vector search (not after it).
    pub filter_in_search: bool,
    /// BM25 full-text search fused with vector search in one query.
    pub hybrid_text: bool,
    /// A write is visible to vector search in the same transaction.
    pub search_reads_own_writes: bool,
    /// Graph expansion in the same query as the search.
    pub graph_in_query: bool,
    /// Runs in the application process.
    pub embedded: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct LoadStats {
    /// Records plus edges written.
    pub items: usize,
    pub load: Duration,
    /// Time until the vector index answers its first query.
    pub index_build: Duration,
}

/// A new memory written during the mixed workload.
pub struct Memory<'a> {
    pub id: u64,
    pub user: u32,
    pub vector: &'a [f32],
    pub text: &'a str,
    pub entity: u32,
}

pub trait Adapter: Send + Sync {
    fn name(&self) -> String;
    fn capabilities(&self) -> Capabilities;

    /// Loads the corpus and builds the vector index.
    fn load(&self, ds: &Dataset) -> anyhow::Result<LoadStats>;

    /// Top-k chunk ids for `q`, filtered to `q.user` when `filtered`.
    fn vector_topk(&self, q: &Query, k: usize, filtered: bool) -> anyhow::Result<Vec<u64>>;

    /// The graph-RAG query: filtered top-k chunks, fused with BM25 on `q.text`
    /// when the system can, then the chunk's document and the entities related
    /// to what it mentions. Returns the number of rows.
    fn graph_rag(&self, q: &Query, k: usize) -> anyhow::Result<usize>;

    /// Writes one memory (a chunk with its embedding and a `MENTIONS` edge)
    /// in one transaction.
    fn write_memory(&self, m: &Memory) -> anyhow::Result<()>;

    /// Writes a memory and, in the same transaction, runs a filtered search
    /// for its own vector. True when the search returns it. Must leave no
    /// trace (roll back).
    fn reads_own_write(&self, m: &Memory) -> anyhow::Result<bool>;

    /// Closes and reopens a persistent database; returns the time until the
    /// first query is answered. `None` for in-memory runs.
    fn reopen(&self) -> anyhow::Result<Option<Duration>>;
}
