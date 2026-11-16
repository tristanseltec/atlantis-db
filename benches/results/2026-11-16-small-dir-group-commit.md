## Retrieval Bench — atlantis (embedded, directory) — small

20000 chunks × 768d, 100 users (filter selectivity 1.00%), 44000 edges, 300 queries, k = 10. linux x86_64 with 12 CPUs; commit 0e6fe36; 2026-11-16.

Capabilities: filter in search yes, BM25 hybrid no, search reads own writes no, graph in query yes, embedded yes. Notes: filtered search = filter pushed into the vector index (YIELD ... WHERE); efSearch default; no BM25 (graph-RAG is vector-only).

| Workload | p50 ms | p99 ms | Mean ms | Other |
|---|---|---|---|---|
| Load | | | | 67000 items in 2.4s (28027/s); index build 5.81s |
| Vector top-k | 0.268 | 0.439 | 0.279 | recall@k 1.000 |
| Graph-RAG, 8 threads | 1.452 | 4.116 | 1.547 | 5171 QPS; 0 errors |
| Graph-RAG, 8 threads + writes | 1.518 | 3.895 | 1.692 | 4725 QPS; writes 135/s of 2000 target |
| Write (mixed) | 6.992 | 13.123 | 7.420 | 0 errors |
| Ingest, 8 writers, 1 record/tx | 9.420 | 10.984 | 9.029 | 886 records/s; 0 errors |
