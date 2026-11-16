## Retrieval Bench — atlantis (embedded, in-memory) — small

20000 chunks × 768d, 100 users (filter selectivity 1.00%), 44000 edges, 300 queries, k = 10. linux x86_64 with 12 CPUs; commit 3b010e2; 2026-11-16.

Capabilities: filter in search no, BM25 hybrid no, search reads own writes no, graph in query yes, embedded yes. Notes: filtered search = vector top-10×10 then WHERE; efSearch default; no BM25 (graph-RAG is vector-only).

| Workload | p50 ms | p99 ms | Mean ms | Other |
|---|---|---|---|---|
| Load | | | | 67000 items in 0.8s (84714/s); index build 177.15s |
| Vector top-k | 1.534 | 2.564 | 1.613 | recall@k 1.000 |
| Filtered vector top-k | 3.705 | 5.930 | 3.892 | recall@k 0.092; 300 of 300 queries short |
| Graph-RAG, 1 thread | 3.668 | 6.192 | 3.766 |  |
| Graph-RAG, 8 threads | 23.720 | 29.815 | 23.899 | 334 QPS; 0 errors |
| Graph-RAG, 8 threads + writes | 31.715 | 40.649 | 31.958 | 250 QPS; writes 31/s of 2000 target |
| Write (mixed) | 31.563 | 38.495 | 31.809 | 0 errors |
| Search reads own write | | | | 0% of probes |
