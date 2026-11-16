# Atlantis Retrieval Bench (`rbench`)

Step F1 of the [roadmap priorities](../../docs/25-implementation-roadmap.md#priorities):
a reproducible harness for the workload described in
[26 — Positioning & Performance Targets](../../docs/26-positioning-and-targets.md#the-benchmark-atlantis-retrieval-bench).
Every later step reports against the baselines it records in `benches/results/`.

```bash
cargo run --release -p atlantis-retrieval-bench -- --scale small --out benches/results
cargo run --release -p atlantis-retrieval-bench -- --scale tiny --duration 2   # smoke test
```

## Corpus

Deterministic (seeded SplitMix64; the same corpus on every machine):

| Scale | Chunks | Dim | Queries |
|---|---|---|---|
| `tiny` | 2,000 | 64 | 100 |
| `small` | 20,000 | 768 | 300 |
| `medium` | 100,000 | 768 | 500 |
| `full` | 1,000,000 | 768 | 1,000 |

- Embeddings: unit vectors from a 256-cluster Gaussian mixture.
- `user`: 100 users, so the filter keeps 1% of the corpus. It is independent of the
  embedding, which is the hard case for filtered search.
- Text: topic words of the chunk's cluster plus Zipf-distributed general words, for the
  BM25 half of the hybrid query once an adapter supports it.
- Graph: chunks / 10 documents (`PART_OF`), chunks / 20 entities (`MENTIONS`), four
  `RELATED` edges per entity. About 2.2 edges per chunk.
- Ground truth: exact top-k by brute force, unfiltered and filtered to the query's user.

## Workloads

| Name | What it measures |
|---|---|
| `load` | Bulk load (records + edges) rate, then time until the vector index answers (always runs) |
| `vector` | Unfiltered top-k latency and recall@k |
| `filtered` | Top-k filtered to the query's user: latency, recall@k, and queries that came back with fewer than k rows |
| `graphrag` | Filtered top-k → document → mentioned entity → related entities, one thread |
| `concurrent` | The graph-RAG query on `--threads` threads for `--duration` seconds: QPS and latency |
| `mixed` | `concurrent` while a writer adds memories at `--write-rate` per second: reader latency, achieved write rate, write latency |
| `freshness` | Writes a memory and searches for it in the same transaction, then rolls back; the share of probes where search saw the write |
| `reopen` | With `--dir`: close, reopen, time to the first answer (index rebuild included) |

Options: `--chunks --dim --users --queries` override the preset, `--k` (10),
`--overfetch F` (filter after an unfiltered top-`k × F` search instead of inside the index; for comparison), `--threads` (8), `--duration` (10 s), `--write-rate` (2000),
`--probes` (20), `--dir PATH` (persistent engine; must be empty), `--out DIR` (writes
`<date>-<scale>-<mem|dir>.json` and `.md`), `--workloads a,b,c`.

## Adapters

An adapter implements `Adapter` in `src/adapter.rs` and declares its `Capabilities`
(filter inside the search, BM25 hybrid, search reads own writes, graph in the same
query, embedded). The report prints them next to the numbers, so a system that does
part of the workload outside the database is never compared silently.

| Adapter | Status |
|---|---|
| Atlantis DB, embedded (in-memory or `--dir`) | Implemented. Filters run inside the vector index (`YIELD ... WHERE`); `--overfetch F` measures the old over-fetch-then-filter path. No BM25 yet. |
| SurrealDB (embedded) | Planned |
| LadybugDB (embedded) | Planned |
| FalkorDB (server) | Planned |
| Neo4j (server) | Planned |
| Qdrant + Neo4j (the multi-store stack) | Planned |

Server adapters will run against a container on the same machine, with client time
included in latency and the setup recorded in the report.

## Rules

- Compare systems only at the same scale, hardware and recall.
- Record the commit, the machine and the exact command with every published result.
- A result file is never edited by hand; rerun instead.
