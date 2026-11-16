## Retrieval Bench — atlantis (embedded, in-memory) — small

20000 chunks × 768d, 100 users (filter selectivity 1.00%), 44000 edges, 300 queries, k = 10. linux x86_64 with 12 CPUs; commit ff0273a; 2026-11-16.

Capabilities: filter in search no, BM25 hybrid no, search reads own writes no, graph in query yes, embedded yes. Notes: filtered search = vector top-10×10 then WHERE; efSearch default; no BM25 (graph-RAG is vector-only).

| Workload | p50 ms | p99 ms | Mean ms | Other |
|---|---|---|---|---|
| Load | | | | 67000 items in 0.6s (103526/s); index build 20.86s |
| Vector top-k | 0.267 | 0.632 | 0.305 | recall@k 1.000 |
| Filtered vector top-k | 1.018 | 1.936 | 1.109 | recall@k 0.092; 300 of 300 queries short |
| Graph-RAG, 8 threads | 2.246 | 7.465 | 2.630 | 3041 QPS; 0 errors |
| Graph-RAG, 8 threads + writes | 4.981 | 17.597 | 5.667 | 1411 QPS; writes 162/s of 2000 target |
| Write (mixed) | 5.573 | 16.509 | 6.189 | 0 errors |
