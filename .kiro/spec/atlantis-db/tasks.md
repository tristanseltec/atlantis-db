# Atlantis DB — Implementation Tasks

> **Workflow**: Tasks are designed to be incremental and test-driven. Each task builds on the previous one. Write tests first, then implementation, then verify with `cargo test` and `cargo clippy`.

---

## Phase 1: Foundation

### Task 1: Cargo workspace scaffolding

Create the Cargo workspace root and all six crate skeletons with proper `Cargo.toml` files, `lib.rs` entry points, and shared dependency versions.

**Files to create:**
- `Cargo.toml` (workspace root with `[workspace]` section, shared dependency table, `rust-version = "1.75"`, `edition = "2021"`)
- `crates/atlantis-core/Cargo.toml` + `src/lib.rs`
- `crates/atlantis-storage/Cargo.toml` + `src/lib.rs`
- `crates/atlantis-vector/Cargo.toml` + `src/lib.rs`
- `crates/atlantis-cypher/Cargo.toml` + `src/lib.rs`
- `crates/atlantis-engine/Cargo.toml` + `src/lib.rs`
- `crates/atlantis-db/Cargo.toml` + `src/lib.rs`
- `rustfmt.toml` (project formatting config)
- `clippy.toml` (or `.cargo/config.toml` with `-D warnings`)

**Dependencies per crate:**
- `atlantis-core`: serde, thiserror, no other deps
- `atlantis-storage`: atlantis-core, serde, bincode
- `atlantis-vector`: atlantis-core, serde
- `atlantis-cypher`: atlantis-core, chumsky, serde
- `atlantis-engine`: atlantis-core, atlantis-storage, atlantis-vector, atlantis-cypher, tokio, tracing
- `atlantis-db`: atlantis-engine, atlantis-core, tokio

**Verification:** `cargo build` compiles all crates. `cargo fmt --check` passes. `cargo clippy -- -D warnings` passes with no warnings.

---

### Task 2: Core types — Value and primitives

Implement the fundamental type system in `atlantis-core`: the `Value` enum, `NodeId`/`EdgeId` newtypes, `Label` and `RelType` types, `PropertyMap`, and `LabelSet`.

**Files to create:**
- `crates/atlantis-core/src/value.rs` — `Value` enum with variants: `Null`, `Bool`, `Integer(i64)`, `Float(f64)`, `String(String)`, `Array(Vec<Value>)`, `Vector(Vec<f32>)`. Implement `Display`, `PartialEq`, `PartialOrd`, `Clone`, `serde::Serialize`, `serde::Deserialize`. Add type accessor methods (`as_string()`, `as_int()`, `as_float()`, `as_vector()`, `as_bool()`, `is_null()`).
- `crates/atlantis-core/src/node.rs` — `NodeId(u64)` newtype (Copy, Hash, Eq, Ord). `Node` struct with `id`, `labels: LabelSet`, `properties: PropertyMap`.
- `crates/atlantis-core/src/relationship.rs` — `EdgeId(u64)` newtype (Copy, Hash, Eq, Ord). `Relationship` struct with `id`, `from: NodeId`, `to: NodeId`, `rel_type: RelType`, `properties: PropertyMap`. `Direction` enum (`Outgoing`, `Incoming`, `Both`).
- `crates/atlantis-core/src/label.rs` — `Label(Arc<str>)` with cheap clone, `LabelSet` as `BTreeSet<Label>`.
- `crates/atlantis-core/src/rel_type.rs` — `RelType(Arc<str>)` with cheap clone.
- `crates/atlantis-core/src/property.rs` — `PropertyMap` as `HashMap<String, Value>` with helper methods (`get`, `insert`, `remove`, `contains`).
- `crates/atlantis-core/src/error.rs` — `AtlantisError` enum using `thiserror`. Include variants: `Parse`, `InvalidQuery`, `NodeNotFound`, `Storage`, `Vector`, `DimensionMismatch`, `Io`, `Serialization`, `Config`, `Shutdown`.
- `crates/atlantis-core/src/lib.rs` — Re-export all public types.

**Tests (in `#[cfg(test)] mod tests` within each file):**
- `value.rs`: Test all `as_*` accessors return correct values. Test `PartialOrd` ordering for numbers. Test `Vector` vs `Array` are distinct variants. Test serde round-trip for each variant.
- `node.rs`: Test `Node` construction and property access.
- `relationship.rs`: Test `Relationship` construction, `Direction` variants.
- `label.rs`: Test `Label` equality and `LabelSet` ordering.

**Verification:** `cargo test -p atlantis-core` passes. `cargo clippy -p atlantis-core -- -D warnings` passes.

---

### Task 3: Core types — Config and engine configuration

Implement configuration types that control storage, vector index, and engine behavior.

**Files to create:**
- `crates/atlantis-core/src/config.rs` — `EngineConfig` struct with nested configs:
  - `StorageConfig`: `memory_budget_bytes: Option<usize>` (default: 1GB), `eviction_policy: EvictionPolicy` (enum: `Lru`, `Lfu`, default `Lru`), `disk_path: Option<PathBuf>`, `compaction_threshold: usize` (default: 1000 delta edges per rel type).
  - `VectorConfig`: `dimensions: usize`, `distance_metric: DistanceMetricKind` (enum: `Cosine`, `Euclidean`, `DotProduct`, default `Cosine`), `hnsw: HnswParams` (m=16, m0=32, ef_construction=200, ef_search=50, max_level=16).
  - `EngineConfig`: `storage: StorageConfig`, `vector: VectorConfig`, `checkpoint_interval_secs: u64` (default: 300).
- Implement `Default` for all config structs with sensible defaults.
- Implement `serde::Serialize`/`Deserialize` for persistence.

**Tests:**
- Test `Default::default()` produces expected values for each config.
- Test serde round-trip for `EngineConfig`.
- Test custom config construction via builder pattern or struct update syntax.

**Verification:** `cargo test -p atlantis-core` passes.

---

## Phase 2: Storage Engine

### Task 4: CSR adjacency implementation

Implement the Compressed Sparse Row adjacency structure and the delta log for incremental updates.

**Files to create:**
- `crates/atlantis-storage/src/csr.rs` — `CsrAdjacency` struct with `offsets: Vec<u64>`, `targets: Vec<NodeId>`, `edge_ids: Vec<EdgeId>`. Methods: `build(edges: &[(NodeId, NodeId, EdgeId)])` to construct from sorted edges, `neighbors(node_id) -> &[(NodeId, EdgeId)]` to get a node's outgoing edges as a slice, `out_degree(node_id) -> usize`. Implement `Clone` and `serde::Serialize`/`Deserialize`.
- `crates/atlantis-storage/src/delta.rs` — `DeltaLog` struct with `entries: HashMap<NodeId, Vec<(NodeId, EdgeId)>>`. Methods: `insert(from, to, edge_id)`, `get(from) -> &[(NodeId, EdgeId)]`, `len() -> usize`, `drain() -> Vec<(NodeId, NodeId, EdgeId)>` to extract all entries for compaction, `clear()`.

**Tests:**
- `csr.rs`: Build CSR from a small edge set, verify `neighbors()` returns correct slices. Test that a node with no edges returns an empty slice. Test `out_degree()`. Test serde round-trip.
- `delta.rs`: Insert several entries, verify `get()` returns correct results. Test `drain()` empties the log. Test `clear()`.

**Verification:** `cargo test -p atlantis-storage` passes. `cargo clippy -p atlantis-storage -- -D warnings` passes.

---

### Task 5: Hot tier — in-memory graph store

Implement the in-memory hot tier with CSR + delta adjacency, node/edge stores, label index, and LRU access tracking.

**Files to create:**
- `crates/atlantis-storage/src/hot_tier.rs` — `HotTier` struct (as designed in D3). Methods:
  - `insert_node(labels, properties) -> NodeId` — allocates a NodeId, stores the node, updates label index.
  - `get_node(id) -> Option<&Node>`
  - `get_node_mut(id) -> Option<&mut Node>`
  - `insert_edge(from, to, rel_type, properties) -> EdgeId` — stores edge, adds to delta log.
  - `get_edge(id) -> Option<&Relationship>`
  - `get_neighbors(id, rel_type) -> Vec<(NodeId, EdgeId)>` — reads CSR + delta, merges results.
  - `get_nodes_by_label(label) -> &[NodeId]` — returns slice from label index.
  - `delete_node(id)` — removes node, removes from label index, marks edges for deletion.
  - `delete_edge(id)` — removes from edge store, (delta entries cleaned on compaction).
  - `set_property(node_id, key, value)` / `remove_property(node_id, key)`
  - `compact(rel_type)` — merges delta into CSR for a relationship type.
  - `needs_compaction(rel_type) -> bool` — checks delta size against threshold.
  - `memory_usage() -> usize` — estimates current memory consumption.
- `crates/atlantis-storage/src/lru.rs` — `LruTracker` struct tracking node access order. Methods: `touch(id)`, `evict_candidates(budget) -> Vec<NodeId>`.

**Tests:**
- Insert 100 nodes with labels, verify label index. Insert edges, verify `get_neighbors()` returns correct results from delta. Trigger compaction, verify CSR is rebuilt and neighbors still correct. Test delete node removes from label index. Test delete edge. Test `memory_usage()` increases after insert.

**Verification:** `cargo test -p atlantis-storage` passes.

---

### Task 6: Cold tier and tiering manager

Implement the on-disk cold tier and the tiering manager that moves data between hot and cold tiers.

**Files to create:**
- `crates/atlantis-storage/src/cold_tier.rs` — `ColdTier` struct (as designed in D3). Methods:
  - `new(path) -> Result<Self>` — opens or creates the page file.
  - `write_node(node) -> Result<PageLocation>` — serializes node with bincode, writes to page file.
  - `read_node(loc) -> Result<Node>` — reads and deserializes.
  - `remove(loc)` — marks page as free.
  - `node_locations() -> &HashMap<NodeId, PageLocation>`
- `crates/atlantis-storage/src/tiering.rs` — `TieringManager` struct. Methods:
  - `new(hot, cold, config)` — takes references to both tiers and config.
  - `maybe_evict()` — if hot tier memory exceeds budget, select LRU victims, write to cold tier, remove from hot tier, record in ghost set.
  - `promote(node_id) -> Result<Node>` — if node is in cold tier, read it, load into hot tier, return.
  - `is_on_disk(node_id) -> bool`

**Tests:**
- Write a node to cold tier, read it back, verify equality. Test eviction: insert enough nodes to exceed memory budget, call `maybe_evict()`, verify some nodes moved to cold tier. Test promotion: access an evicted node, verify it's loaded back into hot tier.

**Verification:** `cargo test -p atlantis-storage` passes.

---

### Task 7: GraphStore facade and persistence

Implement the `GraphStore` facade combining hot and cold tiers, plus serialization/deserialization for checkpointing.

**Files to create:**
- `crates/atlantis-storage/src/store.rs` — `GraphStore` struct owning `HotTier`, `ColdTier`, config. Methods:
  - `new(config) -> Result<Self>`
  - `get_node(id) -> Option<&Node>` (checks hot, promotes from cold on miss)
  - `get_node_owned(id) -> Option<Node>` (returns owned for cross-actor sending)
  - `insert_node(...) -> NodeId`
  - `insert_edge(...) -> EdgeId`
  - `get_neighbors(id, rel_type) -> Vec<(NodeId, EdgeId)>`
  - `get_nodes_by_label(label) -> Vec<NodeId>`
  - `delete_node(id)`, `delete_edge(id)`
  - `set_property(...)`, `remove_property(...)`
  - `checkpoint() -> Result<()>` — serialize entire store to disk
  - `restore(path) -> Result<Self>` — deserialize from disk
- `crates/atlantis-storage/src/persistence.rs` — Serialization format as designed in D11. Functions: `serialize_store(store) -> Vec<u8>`, `deserialize_store(data) -> (HotTier, indexes)`.

**Tests:**
- Create a store, insert nodes and edges, checkpoint to disk, drop the store, restore from disk, verify all data is intact. Test that CSR adjacency is restored. Test that label index is restored.

**Verification:** `cargo test -p atlantis-storage` passes.

---

## Phase 3: Vector Index

### Task 8: Distance metrics and vector store

Implement the distance metric trait and the compact vector store.

**Files to create:**
- `crates/atlantis-vector/src/distance.rs` — `DistanceMetric` trait with `distance(&self, a: &[f32], b: &[f32]) -> f32`. Implement `CosineDistance`, `EuclideanDistance`, `DotProductDistance`. Add a factory function `from_kind(kind: DistanceMetricKind) -> Box<dyn DistanceMetric>`.
- `crates/atlantis-vector/src/vector_store.rs` — `VectorStore` struct with `vectors: Vec<Vec<f32>>`, `dimensions: usize`. Methods: `new(dimensions)`, `insert(vec) -> VectorId`, `get(id) -> &[f32]`, `remove(id)`, `len() -> usize`, `dimensions() -> usize`. Pre-allocate capacity for efficiency.

**Tests:**
- `distance.rs`: Test cosine distance between identical vectors is ~0. Test cosine distance between orthogonal vectors is ~1. Test Euclidean distance matches manual calculation. Test dot product.
- `vector_store.rs`: Insert vectors, retrieve by ID, verify correct values. Test that inserting a vector with wrong dimensions returns an error or panics (design choice). Test `remove()`.

**Verification:** `cargo test -p atlantis-vector` passes. `cargo clippy -p atlantis-vector -- -D warnings` passes.

---

### Task 9: HNSW index — insertion and search

Implement the core HNSW algorithm: layer assignment, insertion with neighbor selection, and k-NN search.

**Files to create:**
- `crates/atlantis-vector/src/hnsw.rs` — `HnswIndex` struct (as designed in D4). Methods:
  - `new(params, distance_metric) -> Self`
  - `insert(&mut self, node_id: NodeId, vector: Vec<f32>)` — full insertion algorithm:
    1. Assign random level using `floor(-ln(unif(0,1)) * (1.0 / ln(M)))`.
    2. Greedy search from entry point down to `level + 1`.
    3. For each layer from `level` to 0: search with `ef_construction`, select M nearest, add bidirectional edges, prune.
    4. Update entry point if new max level.
  - `search(&self, query: &[f32], k: usize, ef_search: Option<usize>) -> Vec<(NodeId, f32)>` — returns top-k nearest with distances.
  - `remove(&mut self, node_id: NodeId)` — mark as deleted (tombstone).
  - `len() -> usize` — number of active vectors.
  - `params() -> &HnswParams`
- `crates/atlantis-vector/src/params.rs` — `HnswParams` struct with `Default` implementation.

**Tests:**
- Insert 1000 random vectors (128-dim), search for top-10, compare with brute-force results. Verify recall > 90% (configurable threshold in test). Test that inserting a vector with wrong dimensions returns error. Test search on empty index returns empty. Test that entry point is set after first insert. Test `remove()` makes the node not appear in search results.

**Verification:** `cargo test -p atlantis-vector` passes.

---

### Task 10: HNSW persistence

Implement serialization of the HNSW structure for checkpointing.

**Files to create:**
- `crates/atlantis-vector/src/persistence.rs` — Functions to serialize/deserialize the entire HNSW index:
  - `serialize_index(index) -> Vec<u8>` — serialize layers, adjacency, entry point, max level, vector store, params.
  - `deserialize_index(data) -> HnswIndex`
- Add `serde::Serialize`/`Deserialize` derives on internal HNSW types, or implement manual serialization for the layer adjacency (HashMap of Vecs).

**Tests:**
- Build an HNSW index with 500 vectors, serialize to bytes, deserialize, run the same search on both, verify results are identical. Test that round-trip preserves params and entry point.

**Verification:** `cargo test -p atlantis-vector` passes.

---

## Phase 4: Cypher Query Language

### Task 11: Cypher lexer

Implement the tokenizer that converts Cypher text into a stream of tokens.

**Files to create:**
- `crates/atlantis-cypher/src/lexer/mod.rs` — module root, re-exports.
- `crates/atlantis-cypher/src/lexer/token.rs` — `Token` enum (as designed in D5), `SpannedToken` struct.
- `crates/atlantis-cypher/src/lexer/tokenizer.rs` — `tokenize(input: &str) -> Result<Vec<SpannedToken>, AtlantisError>` function. Handle:
  - Keywords (case-insensitive): MATCH, OPTIONAL, WHERE, RETURN, CREATE, MERGE, DELETE, SET, REMOVE, WITH, ORDER, BY, LIMIT, SKIP, AND, OR, NOT, CALL, YIELD, DISTINCT, AS, ON, UNION, NULL, TRUE, FALSE, UNWIND, IN, IS, STARTS, ENDS, CONTAINS, XNOR, XOR.
  - Punctuation: `() {} [] : , . | = <> < > <= >= + - * / % <- -> <-> --`.
  - Literals: integers, floats, strings (single + double quotes), booleans, null.
  - Identifiers: alphanumeric + underscore.
  - Parameters: `$name`.
  - Comments: `//` line comments and `/* */` block comments.
  - Function call paths: `db.vector.search` (dotted identifiers).

**Tests:**
- Tokenize each keyword and verify correct `Token` variant. Tokenize numbers, strings, parameters. Tokenize a full MATCH query and verify the token sequence. Test error on unterminated string. Test comment stripping. Test that identifiers are case-sensitive but keywords are case-insensitive.

**Verification:** `cargo test -p atlantis-cypher` passes. `cargo clippy -p atlantis-cypher -- -D warnings` passes.

---

### Task 12: Cypher AST types

Define the AST node types that the parser will produce.

**Files to create:**
- `crates/atlantis-cypher/src/parser/ast.rs` — All AST types (as designed in D6):
  - `Statement`, `Query`, `Clause` (enum), `MatchClause`, `CreateClause`, `MergeClause`, `DeleteClause`, `SetClause`, `RemoveClause`, `WithClause`, `UnwindClause`, `CallClause`.
  - `Pattern`, `NodePattern`, `RelationshipPattern`, `Direction` (reuse from core or re-define).
  - `ReturnClause` with `ReturnItem` (expression + optional alias).
  - `Expression` (enum), `BinOp`, `UnOp`, `PropertyAccess`, `Range`.
  - `PropertyMap` literal (inline `{key: value}` in patterns).
  - Derive `Debug`, `Clone`, `PartialEq` on all types.

**Tests:**
- Construct AST nodes manually and verify `Debug` output. Test that `Expression::BinaryOp` can represent `n.age > 30`.

**Verification:** `cargo test -p atlantis-cypher` passes.

---

### Task 13: Cypher parser with chumsky — core clauses

Implement the chumsky parser for the core Cypher clauses: MATCH, WHERE, RETURN, CREATE.

**Files to create:**
- `crates/atlantis-cypher/src/parser/mod.rs` — Top-level `parse(input: &str) -> Result<Statement, AtlantisError>` function. Tokenize first, then parse token stream with chumsky.
- `crates/atlantis-cypher/src/parser/statement.rs` — Statement parser: sequence of clauses ending with RETURN. Uses `choice()` of clause parsers.
- `crates/atlantis-cypher/src/parser/pattern.rs` — Node pattern parser: `(var:Label {props})`. Relationship pattern parser: `[var:TYPE *min..max]` with direction arrows (`-`/`->`/`<-`/`<->`). Pattern parser: chains of node + relationship patterns.
- `crates/atlantis-cypher/src/parser/expression.rs` — Expression parser with operator precedence: OR > AND > NOT > comparison > additive > multiplicative > property access > literal/variable/parameter/function call. Use chumsky's `pratt` module for precedence parsing if available, or manual recursive descent combinators.
- `crates/atlantis-cypher/src/parser/clause.rs` — Clause parsers:
  - `match_clause()`: `MATCH` pattern (optionally `OPTIONAL`)
  - `where_clause()`: `WHERE` expression
  - `return_clause()`: `RETURN` [DISTINCT] return_items [ORDER BY] [SKIP] [LIMIT]
  - `create_clause()`: `CREATE` pattern

**Tests:**
- Parse `MATCH (n:Person) RETURN n` → verify AST has one MatchClause with NodePattern{var:"n", labels:["Person"]} and ReturnClause with one item.
- Parse `MATCH (a)-[:KNOWS]->(b) WHERE a.name = 'Alice' RETURN b.name` → verify relationship pattern, WHERE predicate, RETURN projection.
- Parse `CREATE (n:Person {name: 'Bob', age: 30})` → verify node creation with inline properties.
- Parse `MATCH (n) WHERE n.age > 25 AND n.active = true RETURN n.name, n.age ORDER BY n.age DESC LIMIT 10` → verify boolean expression, ORDER BY, LIMIT.
- Test error messages on malformed queries (missing RETURN, unbalanced parens).

**Verification:** `cargo test -p atlantis-cypher` passes. Test that valid Cypher parses without error and produces correct AST.

---

### Task 14: Cypher parser — remaining clauses and vector extensions

Extend the parser with MERGE, DELETE, SET, REMOVE, WITH, UNWIND, CALL (vector), and ORDER BY/SKIP/LIMIT.

**Files to create/modify:**
- `crates/atlantis-cypher/src/parser/clause.rs` (extend) — Add parsers for:
  - `merge_clause()`: `MERGE` pattern (with ON CREATE / ON MATCH SET clauses)
  - `delete_clause()`: `DELETE` expression-list (`DETACH DELETE` for cascading)
  - `set_clause()`: `SET` node.prop = value, `SET` node:Label
  - `remove_clause()`: `REMOVE` node.prop, `REMOVE` node:Label
  - `with_clause()`: `WITH` [DISTINCT] return_items [ORDER BY] [SKIP] [LIMIT] [WHERE]
  - `unwind_clause()`: `UNWIND` expression `AS` var
  - `call_clause()`: `CALL db.vector.search(label, $vec, $k, $threshold) YIELD node, score` — parse function path, arguments, YIELD variables.
- `crates/atlantis-cypher/src/parser/expression.rs` (extend) — Add function call parser for `db.vector.search` and `db.vector.similarity` with dotted path support. Add `IN`, `STARTS WITH`, `ENDS WITH`, `CONTAINS` operators. Add list literal `[1, 2, 3]` and map literal `{key: value}`.

**Tests:**
- Parse `MERGE (n:Person {email: 'x@y.com'}) ON CREATE SET n.created = timestamp() RETURN n` → verify merge with ON CREATE.
- Parse `MATCH (n) DETACH DELETE n` → verify DETACH DELETE.
- Parse `MATCH (n) SET n.age = 31, n:Adult RETURN n` → verify SET with multiple assignments.
- Parse `WITH n.name AS name, count(*) AS cnt ORDER BY cnt DESC LIMIT 5` → verify WITH chaining.
- Parse `CALL db.vector.search('Embedding', $query_vec, 10, 0.7) YIELD node, score RETURN node, score` → verify CALL clause with YIELD.
- Parse `MATCH (n) WHERE db.vector.similarity(n.vec, $q) > 0.8 RETURN n` → verify vector function in WHERE.

**Verification:** `cargo test -p atlantis-cypher` passes. All supported Cypher syntax parses correctly.

---

## Phase 5: Query Planner

### Task 15: Logical plan and planner

Implement the logical plan operator tree and the planner that converts AST to logical plan.

**Files to create:**
- `crates/atlantis-cypher/src/planner/logical.rs` — `LogicalOperator` enum (as designed in D7). All operator variants with their data fields.
- `crates/atlantis-cypher/src/planner/mod.rs` — `plan(statement: &Statement) -> Result<LogicalOperator, AtlantisError>` function. Planning logic:
  1. For each MATCH clause: generate `NodeByLabelScan` (if label specified) or `AllNodesScan`, then `Expand` for each relationship in the pattern.
  2. For WHERE: wrap previous plan in `Filter`.
  3. For WITH: wrap in `Apply` (pipelining) with projection.
  4. For CREATE: generate `CreateNode` / `CreateRelationship`.
  5. For DELETE: generate `Delete`.
  6. For SET/REMOVE: generate `SetProperty` / `RemoveProperty`.
  7. For CALL (vector search): generate `VectorSearch`.
  8. For RETURN: generate `Project` with optional `Sort`, `Skip`, `Limit`.
  - Maintain a symbol table mapping variable names to their types (node, relationship, value) for validation.

**Tests:**
- Plan `MATCH (n:Person) RETURN n.name` → verify `Project(Filter(NodeByLabelScan))`.
- Plan `MATCH (a)-[:KNOWS]->(b) WHERE a.name = 'Alice' RETURN b.name` → verify `Project(Filter(Expand(NodeByLabelScan)))`.
- Plan `CALL db.vector.search('Embedding', $q, 10, 0.7) YIELD node, score RETURN node, score` → verify `Project(VectorSearch)`.
- Plan a hybrid query: `CALL db.vector.search(...) YIELD node MATCH (node)-[:EMBEDS]->(doc) RETURN doc.title` → verify `Project(Expand(VectorSearch))`.

**Verification:** `cargo test -p atlantis-cypher` passes.

---

### Task 16: Rule-based optimizer

Implement the query optimizer that transforms logical plans for better performance.

**Files to create:**
- `crates/atlantis-cypher/src/planner/optimizer.rs` — `optimize(plan: LogicalOperator) -> LogicalOperator` function. Apply rules bottom-up:
  1. **Filter pushdown**: Move `Filter` operators inside `Expand` and `Project` when the filter only references variables available at the inner level.
  2. **Limit pushdown**: Push `Limit` through `Project` when no aggregation is present.
  3. **Dead variable elimination**: Remove projected variables not used by downstream operators.
  4. **Vector index detection**: If a `Filter` contains a `db.vector.similarity` call, and the left operand is a known vector property, replace with a `VectorSearch` operator.

**Tests:**
- Test filter pushdown: `Filter(Project(Scan))` becomes `Project(Filter(Scan))` when filter references scan variables.
- Test that a filter referencing a variable from a later Expand stays in place.
- Test dead variable elimination removes unused projections.
- Test vector similarity detection transforms a scan + filter into a vector search.

**Verification:** `cargo test -p atlantis-cypher` passes.

---

## Phase 6: Engine

### Task 17: Actor-based engine core

Implement the core engine actor, command enum, and engine handle.

**Files to create:**
- `crates/atlantis-engine/src/command.rs` — `Command` enum (as designed in D8) with all command variants and oneshot responders.
- `crates/atlantis-engine/src/handle.rs` — `EngineHandle` struct (clonable, wraps `mpsc::Sender<Command>`). Async methods: `query()`, `create_node()`, `get_node()`, `shutdown()`. Each sends a command and awaits the oneshot response.
- `crates/atlantis-engine/src/actor.rs` — `CoreEngineActor` struct owning `GraphStore`, `HnswIndex`, `QueryExecutor`, config. `run()` method: `while let Some(cmd) = rx.recv().await { ... }` processing loop. Spawns background tiering and checkpoint actors on start.
- `crates/atlantis-engine/src/lib.rs` — Re-exports. `Engine::open()` function that creates the actor, spawns it, returns the handle.

**Tests:**
- Create an engine, send a `CreateNode` command, verify response contains a valid NodeId. Send a `GetNode` command with the returned ID, verify the node is returned. Send `Shutdown`, verify clean shutdown. Test that multiple cloned handles can send commands concurrently.

**Verification:** `cargo test -p atlantis-engine` passes.

---

### Task 18: Query executor

Implement the executor that walks the logical plan and produces result rows.

**Files to create:**
- `crates/atlantis-engine/src/executor.rs` — `QueryExecutor` struct (as designed in D9). `execute(&mut self, plan: &LogicalOperator) -> Result<Vec<Row>, AtlantisError>` method with recursive dispatch on each operator variant:
  - `NodeByLabelScan`: iterate label index, produce rows binding the variable.
  - `AllNodesScan`: iterate all nodes.
  - `Expand`: for each input row, traverse neighbors via rel_type, produce new rows with the connected variable bound.
  - `VarLenExpand`: traverse variable-length paths (BFS up to max depth).
  - `VectorSearch`: call HNSW search, produce rows binding node + score.
  - `Filter`: evaluate predicate expression per row, keep matching.
  - `Project`: evaluate each projection expression, produce new rows.
  - `Sort`: collect all rows, sort by keys.
  - `Limit`/`Skip`: apply pagination.
  - `CreateNode`/`CreateRelationship`: perform mutations, produce rows.
  - `Delete`: perform deletions.
  - `SetProperty`/`RemoveProperty`: perform updates.
  - `Apply`: pipeline left into right.
- `Row` type: `HashMap<String, Value>` with accessors.
- Expression evaluation: `eval(expr: &Expression, row: &Row) -> Result<Value, AtlantisError>` handling all expression variants including function calls (including `db.vector.similarity`).

**Tests:**
- Execute `MATCH (n:Person) RETURN n.name` on a graph with 3 Person nodes, verify 3 rows. Execute `MATCH (a)-[:KNOWS]->(b) WHERE a.name = 'Alice' RETURN b.name`, verify correct filtered traversal. Execute a vector search query, verify results include score. Execute `CREATE (n:Person {name: 'Test'}) RETURN n`, verify node created and returned. Test ORDER BY and LIMIT.

**Verification:** `cargo test -p atlantis-engine` passes.

---

### Task 19: Background actors (tiering and checkpointing)

Implement the tiering and checkpointing background actors.

**Files to create:**
- `crates/atlantis-engine/src/tiering_actor.rs` — `TieringActor` that periodically checks memory usage and evicts cold data. Runs on a tokio interval (configurable). Communicates with core actor via channel to request eviction. On eviction, calls `TieringManager::maybe_evict()`.
- `crates/atlantis-engine/src/checkpoint_actor.rs` — `CheckpointActor` that periodically calls `GraphStore::checkpoint()`. Runs on a tokio interval (default: 300s). Also handles a "flush on shutdown" signal.

**Tests:**
- Insert enough nodes to exceed memory budget, wait for tiering interval, verify some nodes are on disk. Insert nodes, wait for checkpoint interval, verify checkpoint file exists. Verify that after shutdown + restart, all data is intact.

**Verification:** `cargo test -p atlantis-engine` passes.

---

## Phase 7: Public API & Integration

### Task 20: Public API facade (atlantis-db crate)

Implement the user-facing `Engine`, `Config`, `ResultSet`, and `Params` types.

**Files to create:**
- `crates/atlantis-db/src/lib.rs` — Re-export all public types. Crate-level documentation with examples.
- `crates/atlantis-db/src/engine.rs` — `Engine` struct wrapping `EngineHandle`. `open()`, `open_with_config()`, `execute()`, `shutdown()`. Implement `Clone` (delegates to handle clone). Rustdoc on every method.
- `crates/atlantis-db/src/config.rs` — Public `Config` builder with ergonomic construction. Re-export `EngineConfig` fields as a flat config with defaults.
- `crates/atlantis-db/src/result_set.rs` — `ResultSet` (columns + rows), `Row` with typed accessors (`get_string`, `get_int`, `get_float`, `get_vector`, `get_bool`, `get_node`). Implement `IntoIterator` for `ResultSet`.
- `crates/atlantis-db/src/params.rs` — `Params` type wrapping `HashMap<String, Value>`. Implement `From<&[(&str, Value)]>` for ergonomic construction.

**Tests:**
- Open an in-memory engine, execute a CREATE + MATCH, verify ResultSet has correct columns and values. Test typed accessors on Row. Test parameterized query. Test that `Engine` is cloneable and clones share the same database.

**Verification:** `cargo test -p atlantis-db` passes. `cargo doc -p atlantis-db` builds without warnings.

---

### Task 21: Integration tests

Write end-to-end integration tests exercising the full stack through the public API.

**Files to create:**
- `tests/basic_crud.rs` — Create nodes/relationships, read them back, update properties, delete, verify gone.
- `tests/cypher_queries.rs` — Test MATCH patterns (node, relationship, variable-length), WHERE filters, RETURN projections, ORDER BY, LIMIT, WITH chaining.
- `tests/vector_search.rs` — Insert embedding nodes + content nodes, run vector search, verify top-k results. Run hybrid query (vector search + graph traversal), verify results include traversed content.
- `tests/persistence.rs` — Open engine, write data, shutdown. Reopen, verify all data present. Test that vector index is restored.
- `tests/example_dataset.rs` — Load a small movie graph (like Neo4j's sample: Person, Movie, ACTED_IN, DIRECTED). Run several Cypher queries against it, verify correctness.

**Tests:**
- Each test file contains multiple `#[tokio::test]` functions. Use a temp directory for persistence tests (`tempfile` dev-dependency). Verify query results match expected values.

**Verification:** `cargo test` (all tests across workspace) passes.

---

### Task 22: Criterion benchmarks

Set up the benchmark crate and write benchmarks for the critical paths.

**Files to create:**
- `benches/Cargo.toml` — Depends on atlantis-db, criterion, tokio.
- `benches/src/storage_bench.rs` — Benchmarks for: node insertion (100, 1000, 10000 nodes), edge insertion, CSR compaction, neighbor traversal (1-hop, 2-hop), label scan.
- `benches/src/vector_bench.rs` — Benchmarks for: HNSW insertion (100, 1000, 10000 vectors, 128-dim), HNSW search (top-10, top-100, varying ef_search), recall vs brute force.
- `benches/src/query_bench.rs` — Benchmarks for: full MATCH+WHERE+RETURN query, hybrid vector+graph query, parameterized query overhead.
- `benches/Cargo.toml` — `[[bench]]` entries for each benchmark file.

**Tests/Verification:**
- `cargo bench` runs all benchmarks and produces timing reports. Verify benchmarks complete without errors. Record baseline numbers.

**Verification:** `cargo bench` runs successfully.

---

## Phase 8: Rust Best Practices Steering

### Task 23: Kiro steering/skills files for Rust best practices

Create Kiro steering files in `.kiro/steering/` that codify Rust best practices for this project, ensuring all code generation follows consistent conventions.

**Files to create:**
- `.kiro/steering/rust-conventions.md` — Always-included steering covering:
  - Rust 2021 edition, pinned toolchain version.
  - Formatting: `rustfmt.toml` settings (max_width, use_field_init_shorthand, etc.).
  - Linting: `clippy -D warnings` enforced, specific clippy lints to enable/disable.
  - Error handling: `thiserror` for library errors, `anyhow` for binaries only, no `unwrap()`/`expect()` in library code, use `?` operator.
  - Crate selection: prefer well-maintained crates, pin versions with `=`, avoid yanked crates.
  - Naming conventions: snake_case for functions/variables, CamelCase for types, SCREAMING_SNAKE for constants.
  - Documentation: rustdoc on all public items, `///` comments, module-level `//!` docs.
  - Unsafe policy: avoid unless necessary, document with `// SAFETY:` comments.
  - Testing: `#[cfg(test)]` inline tests, `proptest` for invariants, criterion for benchmarks.
  - Dependencies: workspace-level dependency management, minimal dependency tree.
- `.kiro/steering/atlantis-db-architecture.md` — Always-included steering covering:
  - Crate structure and responsibilities.
  - Actor model patterns (mpsc + oneshot).
  - CSR + delta storage pattern.
  - HNSW index conventions.
  - Cypher parser conventions (chumsky usage).
  - File naming conventions.

**Verification:** Files exist and are valid markdown. Content covers all the specified areas.

---

## Task Dependency Graph

```
Task 1 (scaffolding)
├── Task 2 (core types - Value)
│   ├── Task 3 (core types - Config)
│   ├── Task 4 (CSR + delta)
│   │   └── Task 5 (hot tier)
│   │       └── Task 6 (cold tier + tiering)
│   │           └── Task 7 (GraphStore + persistence)
│   ├── Task 8 (distance + vector store)
│   │   └── Task 9 (HNSW insert + search)
│   │       └── Task 10 (HNSW persistence)
│   ├── Task 11 (lexer)
│   │   └── Task 12 (AST types)
│   │       └── Task 13 (parser - core clauses)
│   │           └── Task 14 (parser - remaining + vector)
│   │               └── Task 15 (logical plan + planner)
│   │                   └── Task 16 (optimizer)
│   │                       └── Task 18 (executor)
│   ├── Task 17 (actor engine)  ← depends on Tasks 7, 10, 16
│   │   ├── Task 18 (executor)
│   │   └── Task 19 (background actors)
│   └── Task 20 (public API)  ← depends on Task 17
│       ├── Task 21 (integration tests)
│       └── Task 22 (benchmarks)
└── Task 23 (steering files)  ← independent, can be done anytime
```

**Critical path**: Task 1 → 2 → 4 → 5 → 6 → 7 → 17 → 20 → 21

**Parallelizable**: Tasks 8-10 (vector) and Tasks 11-16 (Cypher) can proceed in parallel once Task 2 is done. Task 23 can be done at any time.
