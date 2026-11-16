## Retrieval Bench — atlantis (embedded, in-memory) — small

20000 chunks × 768d, 100 users (filter selectivity 1.00%), 44000 edges, 300 queries, k = 10. linux x86_64 with 12 CPUs; commit 7271661; 2026-11-16.

Capabilities: filter in search yes, BM25 hybrid no, search reads own writes no, graph in query yes, embedded yes. Notes: filtered search = filter pushed into the vector index (YIELD ... WHERE); efSearch default; no BM25 (graph-RAG is vector-only).

| Workload | p50 ms | p99 ms | Mean ms | Other |
|---|---|---|---|---|
| Load | | | | 67000 items in 0.6s (115254/s); index build 5.18s |
| Vector top-k | 0.296 | 0.561 | 0.315 | recall@k 1.000 |
| Filtered vector top-k | 0.146 | 0.332 | 0.486 | recall@k 1.000; 0 of 300 queries short |
| Graph-RAG, 1 thread | 0.832 | 1.352 | 0.875 |  |
| Graph-RAG, 8 threads | 1.419 | 4.716 | 1.634 | 4895 QPS; 0 errors |
| Graph-RAG, 8 threads + writes | 2.397 | 7.907 | 2.783 | 2874 QPS; writes 323/s of 2000 target |
| Write (mixed) | 2.601 | 8.633 | 3.096 | 0 errors |
