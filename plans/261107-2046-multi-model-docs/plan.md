---
title: Multi-model documentation restructure
status: completed
created: 2026-11-07
---

# Multi-model documentation restructure

## Outcome

The `docs/` set describes Atlantis DB as a full multi-model database (graph, vector,
document, relational, key-value, time-series, full-text, geospatial, live queries),
deployable embedded, as a standalone server, and as a distributed cluster. The docs
are inspired by SurrealDB's scope but keep Atlantis DB's own identity: **Atlantis
Cypher** (Cypher + extensions) is the one query language.

## Locked user decisions

| Decision | Choice |
|---|---|
| Query language | Extend Cypher. No second language. Follow Neo4j 5 / openCypher / GQL syntax wherever it already exists; add extensions only where Cypher has nothing. |
| Models in scope | Document + relational, key-value + time-series, full-text + geospatial, live queries + events (plus existing graph + vector). |
| Deployment scope | Full platform: embedded, standalone server (HTTP / WebSocket RPC / optional Bolt), auth & permissions, multi-tenant namespaces, client SDKs, distributed storage backend. |
| Doc layout | Restructure in place. Clearly separate "implemented today" from "target design". |

## Non-goals

- No code changes. Docs only.
- Do not copy SurrealDB text, SurrealQL syntax, or branding. SurrealDB is a scope reference only.
- Do not touch `.kiro/spec/`.

## Acceptance criteria

1. Every chapter opens with the status block (see "Status convention") and every
   unimplemented feature is visibly marked with its phase.
2. Statements about "implemented today" are verified against `crates/*/src`.
3. All chapters use the vocabulary, syntax and crate names in this brief, so there is
   no contradiction between chapters.
4. All internal links resolve to the new file names. No file exceeds 800 lines.
5. `docs/README.md` indexes every chapter, and `docs/25-implementation-roadmap.md`
   holds the phased plan.

## Backup

The original docs and root README are in `docs-backup/` next to this plan.

---

# Design brief (single source of truth for all chapters)

## Status convention

Every chapter starts, immediately under the H1, with:

```markdown
> **Status:** Implemented | Partially implemented | Planned — Phase N
> **Crates:** `atlantis-xxx`, `atlantis-yyy` (mark new crates as "(planned)")
```

In mixed chapters, mark each unimplemented section heading with a suffix, e.g.
`## Secondary Indexes (Planned — Phase 2)`. In tables use a Status column with
`Implemented`, `Partial`, or `Phase N`. Never describe planned behaviour in the present
tense without a marker. Prefer a short "Implemented today" subsection followed by a
"Target design" subsection when a chapter covers both.

## Chapter layout (final file names)

| # | File | Kind | Content |
|---|---|---|---|
| 01 | `01-getting-started.md` | update | Embedded quickstart (works today) + server quickstart (planned) + multi-model taste |
| 02 | `02-architecture.md` | rewrite | Layered multi-model architecture, target crate map, deployment modes, request flow |
| 03 | `03-data-model.md` | rewrite | Unified record model, namespaces/databases/tables, RecordId, full Value type system; current graph types kept as "Implemented today" |
| 04 | `04-storage-engine.md` | update | Current hot/cold tier + target transactional KV layer, key encoding, backends, CSR adjacency cache |
| 05 | `05-transactions-consistency.md` | new | ACID, MVCC, isolation levels, explicit transactions, durability, conflict handling |
| 06 | `06-query-language.md` | update (was 06-cypher-query-language) | Atlantis Cypher: supported subset today + full extension catalogue overview |
| 07 | `07-schema-ddl.md` | new | Namespaces, databases, tables, fields, types, assertions, defaults, computed fields, constraints, index DDL |
| 08 | `08-document-relational.md` | new | Nested documents, record links, joins, aggregation/grouping, secondary indexes, relational patterns |
| 09 | `09-graph-model.md` | new | Edges as records, traversal, variable-length paths, shortest path, path functions, graph algorithms |
| 10 | `10-vector-index.md` | update (was 05) | HNSW today + vector index DDL, persistence, filtering, quantization, hybrid search |
| 11 | `11-full-text-search.md` | new | Analyzers, BM25, full-text index DDL, highlighting, hybrid text+vector (RRF) |
| 12 | `12-geospatial.md` | new | Geometry types, geo functions, spatial index |
| 13 | `13-key-value-time-series.md` | new | KV API, TTL, range scans; time-series tables, composite keys, bucketing, retention, downsampling |
| 14 | `14-live-queries-events.md` | new | Change feeds, LIVE queries, events/triggers |
| 15 | `15-query-planner-executor.md` | update (was 07) | Current planner/executor + target cost-based optimizer, join operators, index selection |
| 16 | `16-engine-runtime.md` | update (was 08) | Actor model today + target: commit sequencer, concurrent snapshot readers, background workers |
| 17 | `17-server-protocols.md` | new | `atlantis` server and CLI, HTTP REST, WebSocket RPC, optional Bolt, import/export, backup |
| 18 | `18-security-auth.md` | new | System users, roles, access methods, JWT, record-level permissions, audit |
| 19 | `19-client-sdks.md` | new | Connection URLs, Rust/Python/JS-WASM/Go/Java SDKs, shared method surface |
| 20 | `20-distributed-architecture.md` | new | Compute/storage separation, TiKV backend, cluster membership, distributed live queries, vector/text index distribution, ops |
| 21 | `21-api-reference.md` | update (was 09) | Rust API today + planned API surface marked |
| 22 | `22-configuration.md` | update (was 12) | Current config + planned storage/server/auth/cluster config |
| 23 | `23-query-cookbook.md` | update (was 10) | Recipes per model; mark planned recipes |
| 24 | `24-rag-patterns.md` | update (was 11) | Add hybrid text+vector, document-chunk tables, agent memory with TTL/live queries |
| 25 | `25-implementation-roadmap.md` | rewrite (was 13) | Current status + phased plan to multi-model |

Old→new link map: `05-vector-index`→`10-vector-index`, `06-cypher-query-language`→
`06-query-language`, `07-query-planner-executor`→`15-...`, `08-engine-runtime`→`16-...`,
`09-api-reference`→`21-...`, `10-query-cookbook`→`23-...`, `11-rag-patterns`→`24-...`,
`12-configuration`→`22-...`, `13-implementation-roadmap`→`25-...`.

## What exists today (verify in code before claiming)

Crates: `atlantis-core`, `atlantis-storage` (hot tier CSR+delta, cold tier append-only,
LRU tiering), `atlantis-vector` (HNSW, persistence stub), `atlantis-cypher` (lexer,
parser, planner; optimizer is pass-through), `atlantis-engine` (single-writer tokio
actor, executor; background actors stubbed; VarLenExpand and CreateRelationship TODO),
`atlantis-db` (facade). `Value` today: `Null, Bool, Integer, Float, String, Array,
Vector`. Vector search via `CALL db.vector.search(...)`. Embedded only.

## Target architecture

```
 Clients: Rust embedded | SDKs (Rust/Python/JS/Go/Java) | CLI | HTTP | WS RPC | Bolt
                                │
 atlantis-server (planned) ── protocols, sessions, auth handshake
                                │
 atlantis-engine ── session, transactions, permissions enforcement,
                    executor, live-query & event dispatch
                                │
 atlantis-cypher ── lexer → parser → AST → logical plan → cost-based optimizer
                                │
 Index layer: atlantis-index (B-tree / unique / composite)   atlantis-vector (HNSW)
              atlantis-text (full-text, BM25)                atlantis-geo (spatial)
                                │
 atlantis-storage ── record store, edge store, CSR adjacency cache, tiering/caching
                                │
 atlantis-kv ── transactional ordered KV trait, key encoding, MVCC
     backends: mem | rocksdb (default persistent) | tikv (distributed)
                                │
 atlantis-core ── Value, RecordId, Geometry, Datetime, errors, config
```

Target crates. Existing ones are marked (exists), new ones (planned):

| Crate | Role |
|---|---|
| `atlantis-core` (exists) | Types: `Value`, `RecordId`, `Geometry`, `Datetime`, `Duration`, errors, config |
| `atlantis-kv` (planned) | `KvStore`/`Transaction` traits, key encoding, MVCC; features `kv-mem`, `kv-rocksdb`, `kv-tikv` |
| `atlantis-storage` (exists → evolves) | Record and edge stores on top of `atlantis-kv`; CSR+delta becomes an adjacency cache; tiering becomes cache management |
| `atlantis-index` (planned) | Secondary B-tree, unique, composite indexes; index manager and background index builds |
| `atlantis-vector` (exists) | HNSW, persisted through `atlantis-kv` |
| `atlantis-text` (planned) | Analyzers, inverted index, BM25 |
| `atlantis-geo` (planned) | Geometry ops, spatial index (geohash / S2-style cells over KV) |
| `atlantis-cypher` (exists → extended) | Dialect extensions, cost-based optimizer |
| `atlantis-engine` (exists → extended) | Transactions, permissions, live queries, events, change feeds |
| `atlantis-auth` (planned) | Users, roles, access methods, JWT, password hashing (Argon2id) |
| `atlantis-server` (planned) | HTTP, WebSocket RPC, optional Bolt; binary `atlantis` |
| `atlantis-db` (exists → extended) | Public facade; one `Engine` API for embedded and remote connections |

Deployment modes: **Embedded** (library, `mem://` or `rocksdb://`), **Server** (single
node `atlantis start`), **Cluster** (stateless `atlantis` compute nodes over `tikv://`).

## Unified record model

- Hierarchy: **Namespace → Database → Table → Record**. Embedded default:
  namespace `atlantis`, database `default`.
- Every record lives in exactly one **table** and has a **RecordId** = `table` + `key`.
  Text form `Person:alice`, `Person:42`, `Reading:['sensor-1', d'2026-11-07T00:00:00Z']`.
- Key kinds: integer, string, UUID, ULID (default auto-generated), array (composite,
  ordered; used for time-series and range scans).
- **Graph mapping**: a node is a record. The node's **first label is its table**;
  further labels are secondary labels (indexed tag set). A relationship is a record in an
  **edge table** named after its type (`WORKS_AT`), with mandatory `in`/`out`
  (alias `startNode`/`endNode`) RecordIds. Edge tables can carry schema and indexes.
- **Document mapping**: records are nested documents (maps of maps/arrays), schemaless
  by default, schemafull when declared.
- **Relational mapping**: tables + typed fields + record links (fields of type
  `record<Customer>`) + secondary indexes + joins planned by the optimizer.
- In Cypher, `id(n)` returns the RecordId; `rid('Person', 'alice')` constructs one; the
  canonical in-query form is a parameter or `rid()` call. RecordId is a `Value`.

### Value type system (target)

`Null`, `Bool`, `Integer(i64)`, `Float(f64)`, `Decimal` (128-bit), `String`, `Bytes`,
`Datetime` (UTC, ns), `Duration`, `Uuid`, `Array`, `Object` (ordered map, nested
documents), `Vector(Vec<f32>)` (exists), `Geometry` (Point, LineString, Polygon,
MultiPoint, MultiLineString, MultiPolygon, Collection; WGS84), `RecordId`, `Range`.
Type names in DDL: `bool, int, float, decimal, string, bytes, datetime, duration, uuid,
array<T>, object, vector<f32, N>, point, geometry, record<Table>, any, option<T>`.

## Atlantis Cypher extensions (canonical syntax)

Use these exact forms everywhere.

Context & tenancy:
```cypher
CREATE NAMESPACE acme;  CREATE DATABASE acme.shop;  USE acme.shop;
```

Tables & fields (`07-schema-ddl.md` owns the full grammar):
```cypher
CREATE TABLE Person SCHEMAFULL;
CREATE TABLE Event SCHEMALESS;
CREATE TABLE WORKS_AT TYPE EDGE FROM Person TO Company;
CREATE TABLE Reading TYPE TIMESERIES KEY (sensor, ts) RETENTION 30d;
CREATE FIELD email ON Person TYPE string ASSERT value =~ '.+@.+';
CREATE FIELD age ON Person TYPE option<int> DEFAULT 0 ASSERT value >= 0;
CREATE FIELD fullName ON Person COMPUTED p.first + ' ' + p.last;
ALTER TABLE Person CHANGEFEED 7d;   DROP TABLE Event;
```

Indexes & constraints (Neo4j 5 forms, plus extensions):
```cypher
CREATE INDEX person_email FOR (p:Person) ON (p.email);
CREATE CONSTRAINT person_email_unique FOR (p:Person) REQUIRE p.email IS UNIQUE;
CREATE VECTOR INDEX chunk_embedding FOR (c:Chunk) ON (c.embedding)
  OPTIONS {dimension: 1536, metric: 'cosine', m: 16, efConstruction: 200};
CREATE FULLTEXT INDEX doc_text FOR (d:Doc) ON EACH [d.title, d.body]
  OPTIONS {analyzer: 'english'};
CREATE POINT INDEX place_location FOR (p:Place) ON (p.location);
CREATE ANALYZER english TOKENIZER unicode FILTERS [lowercase, ascii, snowball('english')];
```

Document access: `p.address.city`, `SET p.address.city = 'Hue'`, `SET p += {...}`,
list ops. A field of type `record<T>` auto-dereferences on property access
(`o.customer.name`), and `FETCH` is not needed.

Graph: `MATCH (a)-[:KNOWS*1..3]->(b)`, `shortestPath(...)`, `allShortestPaths(...)`,
`CALL algo.pagerank(...)`, `CALL algo.wcc(...)` (graph algorithms are Phase 3).

Search procedures (all `YIELD` style, consistent with `db.vector.search`):
```cypher
CALL db.vector.search('chunk_embedding', $vec, 10) YIELD node, score
CALL db.text.search('doc_text', 'graph database', 10) YIELD node, score
CALL db.search.hybrid({text: 'doc_text', vector: 'chunk_embedding'},
                      'graph database', $vec, 10) YIELD node, score   // RRF
```
Function namespaces: `text.highlight`, `point({longitude, latitude})`,
`point.distance`, `geo.within`, `geo.intersects`, `geo.polygon`, `ts.bucket`,
`time.now()`, `duration('PT5M')`, `crypto.argon2.compare`, and `db.kv.*` procedures.

Key-value: `CALL db.kv.set($key, $value, {ttl: '1h'})`, `db.kv.get`, `db.kv.delete`,
`db.kv.scan($prefix)`; Rust: `engine.kv().set(...)`.

Live queries & events:
```cypher
LIVE MATCH (o:Order) WHERE o.status = 'open' RETURN o;   // returns live id
KILL LIVE $liveId;
CALL db.changes('Order', {since: $versionstamp}) YIELD change;
CREATE EVENT order_shipped ON Order
  WHEN $event = 'UPDATE' AND $before.status <> 'shipped' AND $after.status = 'shipped'
  THEN { CREATE (:Notification {order: $after.id, at: time.now()}) };
```

Transactions: `BEGIN; ... COMMIT;` / `ROLLBACK;` in multi-statement queries, plus
`engine.transaction(|tx| ...)` in Rust and `tx` objects in SDKs.

Security:
```cypher
CREATE USER admin ON ROOT PASSWORD $pw ROLES [owner];
CREATE USER analyst ON DATABASE PASSWORD $pw ROLES [viewer];
CREATE ACCESS account ON DATABASE TYPE RECORD
  SIGNUP { CREATE (u:User {email: $email, pass: crypto.argon2.generate($pass)}) RETURN u }
  SIGNIN { MATCH (u:User {email: $email}) WHERE crypto.argon2.compare(u.pass, $pass) RETURN u }
  DURATION FOR TOKEN 15m, FOR SESSION 24h;
ALTER TABLE Post PERMISSIONS
  FOR read WHERE p.published OR p.author = $auth.id
  FOR create, update, delete WHERE p.author = $auth.id;
```
Roles: `owner`, `editor`, `viewer` at ROOT / NAMESPACE / DATABASE level. Session
params: `$auth`, `$session`, `$access`, `$token`.

## Storage & transactions design

- `atlantis-kv` exposes an ordered, transactional KV API: `begin(read_only)`, `get`,
  `set`, `delete`, `scan(range)`, `commit`, `cancel`, with snapshot reads.
- Key encoding (byte-ordered, memcomparable): prefixes
  `/ns/{ns}/db/{db}/tb/{tb}/r/{key}` (record),
  `.../g/{from}/{dir}/{edgeTable}/{edgeKey}` (adjacency entries, `dir` ∈ out|in),
  `.../ix/{index}/{encodedValues}/{key}` (secondary index),
  `.../ft/{index}/...` (full-text postings), `.../vx/{index}/...` (HNSW graph and vectors),
  `.../cf/{tb}/{versionstamp}` (change feed), `/sys/...` (catalog, users, cluster).
- MVCC with **snapshot isolation by default** and optional **serializable**
  (read-set validation). Optimistic concurrency, conflict → `Error::TxConflict`, which
  clients may retry.
- Embedded/single-node: one commit sequencer per database (the evolution of today's
  single-writer actor); readers run concurrently on snapshots.
- Durability: backend WAL (RocksDB) with `sync` modes `every|interval|never`.
- Backends: `mem` (tests and ephemeral), `rocksdb` (default persistent), `tikv`
  (distributed, Phase 7). Today's hot/cold tier remains during the migration and is
  documented as "Implemented today".
- CSR+delta adjacency stays as an in-memory **adjacency cache** rebuilt from `g/` keys.

## Server, protocols, SDKs

- Binary: `atlantis` with `start`, `shell`, `import`, `export`, `backup`, `restore`,
  `upgrade`, `version`, `isready`.
  `atlantis start --bind 0.0.0.0:7700 --user root --pass $PW rocksdb://data`.
- Default port **7700**. Bolt (optional, feature `bolt`) on 7687.
- HTTP: `POST /query` (Cypher + params), `GET|POST|PUT|PATCH|DELETE /key/{table}[/{id}]`,
  `POST /signup`, `POST /signin`, `GET /health`, `GET /version`, `POST /import`,
  `GET /export`. Headers `Atlantis-NS`, `Atlantis-DB`, `Authorization: Bearer`.
- WebSocket RPC at `/rpc`, JSON or CBOR, request `{id, method, params}`. Methods:
  `use, signup, signin, authenticate, invalidate, info, query, select, create, insert,
  update, merge, patch, delete, relate, live, kill, let, unset, begin, commit, cancel`.
  Live notifications are pushed as `{result: {id, action, record}}`.
- Connection URLs: `mem://`, `rocksdb://path`, `ws://host:7700`, `wss://`, `http://`,
  `https://`, `tikv://pd1:2379,pd2:2379`.
- SDKs: Rust (`atlantis-db`, embedded and remote, same `Engine` API), Python
  (`atlantisdb`), JavaScript/TypeScript (`atlantisdb.js`, plus WASM embedded build),
  Go, Java. All share the method surface of the RPC.

## Distributed design (Phase 7)

Stateless `atlantis` compute nodes over TiKV (Raft, multi-region transactional KV).
Node registration and heartbeats in `/sys/cluster`. Live queries are registered in KV,
and changes fan out through per-node notification queues. HNSW and full-text indexes are
stored in KV, and each node keeps a lazily rebuilt in-memory cache with version checks.
Later: a native Raft backend is a future option, not committed.

## Phased roadmap (owned by `25-implementation-roadmap.md`)

| Phase | Name | Scope |
|---|---|---|
| 0 | Complete graph + vector core | HNSW persistence, CREATE relationships, stable-ID promotion, DETACH DELETE, var-length expand, MERGE, aggregation, background actors |
| 1 | Transactional storage foundation | `atlantis-kv`, key encoding, MVCC, `mem` + `rocksdb` backends, migrate graph store, explicit transactions |
| 2 | Unified record & document/relational model | RecordId, extended Value types, namespaces/databases/tables, schema DDL, fields, constraints, `atlantis-index`, record links, joins, cost-based optimizer |
| 3 | Search & specialized models | Full-text (`atlantis-text`), geospatial (`atlantis-geo`), KV API, time-series tables, vector index DDL + filtering, graph algorithms |
| 4 | Reactive | Change feeds, events, live queries |
| 5 | Server & security | `atlantis-server`, HTTP/WS RPC, `atlantis-auth`, permissions, CLI, import/export, backup |
| 6 | SDKs | Rust remote, Python, JS/TS + WASM, Go, Java; optional Bolt |
| 7 | Distributed | TiKV backend, stateless compute cluster, distributed live queries, ops tooling |

## Writing rules for chapter authors

- Read the existing chapter (if any) first, and keep accurate existing content about
  today's code. Re-verify code claims against `crates/`.
- Complete sentences, concise, with practical examples. Each chapter ≤ 800 lines (target 250–550).
- End each chapter with a short "Design Decisions" section (where useful) and "What's Next" links.
- Use only the syntax, names, ports, crate names and phases above. If you need a new
  construct, keep it consistent with these patterns and mention it in your final report.
- Use relative links `NN-name.md` only to files in the chapter layout table.

## Addendum: verified code facts and resolved questions (2026-11-07 20:58)

Verified by the storage/engine writer. Every chapter must reflect these facts when it describes "implemented today":

- The workspace currently **does not compile**: `crates/atlantis-core/src/config.rs` uses `AtlantisError` without importing it. Do not claim tests pass.
- Multi-hop MATCH is broken. In `(a)-[:KNOWS]->(b:Person)`, `b` is bound to every `Person`, not to `a`'s neighbours.
- `Expand` follows outgoing edges only, and a relationship pattern without a type returns no rows.
- `MATCH ... SET` updates nothing, because the planner builds `SetProperty` on an empty input.
- `UNWIND` or `RETURN` as the first clause (e.g. `RETURN 1`) returns no rows.
- The HNSW index is **never populated** by the engine, so `db.vector.search` returns nothing today and its label argument is ignored.
- Eviction and auto-checkpoint never run at runtime. `checkpoint_interval_secs` is unused.
- Delete only tombstones a node. Restoring a checkpoint gives nodes new IDs while edges keep the old IDs.
- MERGE, REMOVE and OPTIONAL MATCH are silently ignored by the planner.
- `CREATE` skips relationships and returns only the last node created.
- Phase 0 must list all of the above as fixes.

Resolved decisions:
- **Events** run synchronously inside the triggering transaction. They are atomic: an event failure aborts the transaction. Live notifications and change-feed entries are emitted only after commit.
- **Transactions in Rust:** `engine.transaction(|tx| async move { ... }).await -> Result<T>`. It does not retry automatically. `engine.transaction_with_retry(max_attempts, f)` retries on `Error::TxConflict`. Target error type is `Error` (today's `AtlantisError` becomes `atlantis_db::Error`).
- **Path features:** `shortestPath`/`allShortestPaths` are Phase 3, together with the `algo.*` procedures. Variable-length expand is Phase 0.
- **Config keys:** `transaction.isolation` (`snapshot`|`serializable`), `query.timeout` (duration, default `30s`), `storage.sync` (`every`|`interval`|`never`), `storage.url`. In `atlantis.toml` and as env vars `ATLANTIS_TRANSACTION_ISOLATION`, `ATLANTIS_QUERY_TIMEOUT`, `ATLANTIS_STORAGE_SYNC`, `ATLANTIS_STORAGE_URL`.
- **Physical operator names:** `IndexSeek`, `IndexRangeScan`, `KeyRangeScan`, `VectorTopK`, `TextSearch`, `SpatialScan`.
- **Live queries:** each live query has a bounded notification queue. If the queue overflows, the live query is killed, and the client catches up via `db.changes`.
- Vector search today: `CALL db.vector.search(label, vec, k[, threshold])` with literal args only. The label is ignored. The first YIELD column holds the internal integer index ID, not a node. `score = 1 - distance`. `serialize_index` is a stub. Target signature: `db.vector.search(indexName, vec, k[, thresholdOrOptions])`.
- Vector metric names: `'cosine'`, `'euclidean'`, `'dot_product'`.
- Added key prefix: `/ns/{ns}/db/{db}/kv/d/{key}` (KV data) and `.../kv/x/{expiresAt}/{key}` (TTL expiry queue). Vector sub-keys: `vx/{index}/meta`, `v/{key}`, `n/{level}/{key}`.
- Planned Rust API additions: `engine.kv()` (`set`, `set_with_ttl`, `get`, `delete`, `scan`), `tx.kv()`, `engine.live(query, params) -> Stream` (dropping the stream kills the live query).
- Background workers: TTL expiry, time-series retention, change-feed GC, vector compaction, index builds, checkpoint/tiering (Phase 0).
- Live queries in Phase 4 are limited to one table (no aggregation, ORDER BY or LIMIT) and send no initial snapshot. A record that stops matching is notified as DELETE.
- **Record-bound variable in clauses:** in `PERMISSIONS`, `COMPUTED` and table-level `ASSERT`, the current record is bound to the single free variable that is used (conventionally the table's initial, e.g. `p`). `this` is always accepted as an alias. In field `ASSERT`, `value` is the field's value. 07 owns this rule.
- **Explicit RecordId on CREATE:** `CREATE (:Person {id: 'alice'})` or `{id: rid('Person','alice')}` sets the key. Without `id`, a ULID is generated. Exports use this form.
- **Default permissions:** a table without a PERMISSIONS clause gives `NONE` to record/JWT (application) users. System users bypass table permissions. Events run with owner privileges. `PERMISSIONS FOR <action> FULL | NONE | WHERE <expr>`.
- **Delivery semantics:** live notifications are at-most-once. Change feeds are the durable, ordered path.
- **Target error type** is `atlantis_db::Error`, with variants including `TxConflict`, `PermissionDenied`, `SchemaViolation`, `ParseError`, `AuthenticationFailed`, `NotFound`, `Timeout`.
- **Server defaults:** bind `127.0.0.1:7700`; `--user`/`--pass` create the root user on first start only; `mem://` if no storage URL. Flags: `--tls-cert`, `--tls-key`, `--allow-origin`, `--bolt`, `--log`, `--audit-log`. `ATLANTIS_*` env vars mirror the flags. 22 owns the config keys.
- **Live notification actions** are `CREATE`, `UPDATE`, `DELETE` and `KILLED`. `KILLED` is the final notification when the server ends a live query.
- **Change feeds:** `db.changes` is open to application users, filtered by the table's `FOR read` rule.
- **Retries:** non-Rust SDKs take a `retries` option on `transaction(...)`, which mirrors `transaction_with_retry`.
- **Distributed live fan-out** happens after commit, in a separate transaction. Delivery is at most once.
- **Unlabeled nodes:** `CREATE ({...})` with no label goes into the reserved table `_default` (schemaless). Table names starting with `_` are reserved.
- **Geometry phases:** the `Geometry` *value type* is Phase 2, together with the other extended Value types. Geo functions and the spatial index (`atlantis-geo`) are Phase 3.
- **Additional Phase 0 fixes:**
  - `crates/atlantis-db/src/config.rs` has an inherent `impl Config` on a foreign type alias (E0116), which also clashes with `EngineConfig::builder`.
  - That file re-exports an `EngineConfigBuilder` that `atlantis-core` does not export.
  - `Sort` with a descending key ignores the secondary sort keys.
- **Parser and executor gaps** verified by running queries (see chapter 06) are Phase 0 fixes. Path variables and path functions are Phase 0. Multi-statement `;` scripts are Phase 1.
- `EXPLAIN <query>` (Phase 2) returns the physical plan. Chapter 15 owns it.
- Keywords such as `ORDER` must be accepted as identifiers in label and table positions (e.g. the table `Order`).
