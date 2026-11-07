# 21 — API Reference

> **Status:** Partially implemented — the embedded Rust facade, record transactions and record types exist; the rest of the multi-model API surface is Planned — Phases 1–7
> **Crates:** `atlantis-db`, `atlantis-core`, `atlantis-storage`, `atlantis-kv`, `atlantis-engine`, `atlantis-auth` (planned), `atlantis-server` (planned)

This chapter is the reference for the public Rust API. The first half documents the
API that exists in `crates/atlantis-db/src` today. The second half documents the target
API surface, with the phase that delivers each part. Cypher syntax lives in
[06 — Query Language](06-query-language.md); configuration lives in
[22 — Configuration](22-configuration.md).

## Implemented today

The facade crate `atlantis-db` re-exports the engine, Cypher result types, record
types and transaction types. The `pub use` lines in
[`crates/atlantis-db/src/lib.rs`](../crates/atlantis-db/src/lib.rs) are the
authoritative list. Configuration types are re-exported from
[`crates/atlantis-db/src/config.rs`](../crates/atlantis-db/src/config.rs) and from the
root of `atlantis-core`.

`Engine` gives access to **two stores that do not share data yet**:

- the **Cypher graph store**, used by `execute`;
- the **record store**, used by `transaction`, `transaction_with_isolation`,
  `transaction_with_retry` and `read`.

A node created with Cypher `CREATE` is not visible to record transactions, and a record
is not visible to Cypher. Moving Cypher onto the record store is open Phase 1/2 work
([25 — Implementation Roadmap](25-implementation-roadmap.md)).

### Known build defects (Phase 0)

None remain: the workspace compiles. The failing unit tests that Phase 0 still has to
fix are listed in [25 — Implementation Roadmap](25-implementation-roadmap.md#build-and-tests).

### Engine

**File:** [`crates/atlantis-db/src/engine.rs`](../crates/atlantis-db/src/engine.rs)

`Engine` wraps a clonable handle to the single-writer Cypher actor (see
[16 — Engine Runtime](16-engine-runtime.md)) and a handle to the record store. Clones
share both stores and are safe to move across tasks. The open, query and transaction
methods are `async` and need a Tokio runtime. Record transactions run their closure on
the calling task, not on the actor.

| Method | Signature | Behaviour |
|---|---|---|
| `open` | `async fn open(path: impl AsRef<Path>) -> Result<Engine, AtlantisError>` | Builds a default config with `disk_path` set to `path`, then calls `open_with_config`. |
| `open_with_config` | `async fn open_with_config(config: EngineConfig) -> Result<Engine, AtlantisError>` | With a `disk_path`: takes an exclusive `LOCK` file in the directory (a second engine on it fails with `Storage`, "already open in another engine"), loads `records.kv` when it exists, and spawns the Cypher actor, which restores `checkpoint.db` when it exists. |
| `open_in_memory` | `async fn open_in_memory() -> Result<Engine, AtlantisError>` | Uses `EngineConfig::default()` (no disk path). Data is lost on drop or shutdown. |
| `database` | `fn database(&self) -> &DatabaseRef` | The namespace and database used by record transactions. Defaults to `atlantis` / `default`. |
| `use_db` | `fn use_db(&mut self, ns: &str, db: &str) -> Result<(), AtlantisError>` | Selects the namespace and database for record transactions on this handle. Other clones keep their own selection. Validates both names. Does not affect Cypher. |
| `transaction` | `async fn transaction<T, F>(&self, f: F) -> Result<T, AtlantisError>` where `F: FnOnce(&mut RecordTx) -> Result<T, AtlantisError>` | Runs `f` in a read-write, snapshot-isolated record transaction. `Ok` commits; `Err` discards all writes. Never retries. |
| `transaction_with_isolation` | `async fn transaction_with_isolation<T, F>(&self, isolation: Isolation, f: F) -> Result<T, AtlantisError>` (same `F` bound) | As `transaction`, with an explicit `Isolation`. |
| `transaction_with_retry` | `async fn transaction_with_retry<T, F>(&self, max_attempts: u32, f: F) -> Result<T, AtlantisError>` where `F: FnMut(&mut RecordTx) -> Result<T, AtlantisError>` | As `transaction`, re-running `f` while the error is retryable (`TxConflict`), up to `max_attempts` attempts in total. No backoff. |
| `read` | `async fn read<T, F>(&self, f: F) -> Result<T, AtlantisError>` (same `F` bound as `transaction`) | Runs `f` in a read-only transaction on one snapshot. Writes fail with `TxReadOnly`. |
| `execute` | `async fn execute(&self, cypher: &str, params: &[(&str, Value)]) -> Result<ResultSet, AtlantisError>` | Parses, plans and executes one Cypher statement against the graph store. `params` binds `$name` references. |
| `checkpoint` | `async fn checkpoint(&self) -> Result<(), AtlantisError>` | Writes the record store to `records.kv`, then the graph store's hot tier and cold-tier page index to `checkpoint.db`. Without a disk path the record step is a no-op and the graph step returns `Storage`. |
| `shutdown` | `async fn shutdown(&self) -> Result<(), AtlantisError>` | Writes `records.kv` and checkpoints the graph store when a disk path is configured, then stops the actor. Later Cypher calls return `Shutdown`. |

**Record durability.** The record store is in memory. It is written to `records.kv` by
`checkpoint()`, by `shutdown()`, and automatically (best effort) when the last `Engine`
clone is dropped, which also releases the `LOCK` file. Each write is atomic. There is no
write-ahead log yet, so commits made after the last checkpoint are lost on a crash; a
successful `transaction` does not mean the data is on disk.

```rust
use atlantis_db::{Engine, Value};

let engine = Engine::open("./my_db").await?;
let result = engine
    .execute(
        "MATCH (n:Person) WHERE n.name = $name RETURN n.age AS age",
        &[("name", Value::from("Alice"))],
    )
    .await?;
engine.shutdown().await?;
```

Two behaviours matter when you read Cypher results today:

- `RETURN n` for a node variable returns the internal node id as `Value::Integer`, not a
  node map. Return properties (`n.name`) to get data.
- Only `disk_path` and the vector settings from `EngineConfig` reach the Cypher engine.
  The actor opens storage with `GraphStore::open(path)` or `GraphStore::new_in_memory()`,
  both of which use default `StorageConfig` values. See
  [22 — Configuration](22-configuration.md#what-is-not-wired-yet).

### Record transactions

**File:** [`crates/atlantis-storage/src/records.rs`](../crates/atlantis-storage/src/records.rs)

The closure passed to `transaction`, `transaction_with_retry` or `read` receives a
`&mut RecordTx` scoped to the engine's current database. Every method is synchronous
and returns `Result<_, AtlantisError>`. The storage layout behind these methods is in
[04 — Storage Engine](04-storage-engine.md).

| Method | Purpose |
|---|---|
| `database()` | The `DatabaseRef` this transaction works in. |
| `define_table(name, kind)` | Defines a table as `TableKind::Normal` or `TableKind::Edge { from, to }`. Fails with `SchemaViolation` if it exists. Names starting with `_` are reserved. |
| `table(name)`, `tables()` | Reads one `TableDef`, or lists all, ordered by name. |
| `create(table, key, labels, data)` | Creates a record. The key is `key`, else the `id` field of `data`, else a new ULID. Fails with `RecordExists` if the id is taken. Tables are created schemaless on first write. |
| `create_node(labels, data)` | Creates a graph node. The first label is the table, the rest are secondary labels; no labels means the `_default` table. |
| `get(id)` | Reads a record. Edge records come back as documents with `in` and `out` fields. |
| `get_edge(id)` | Reads an `Edge`. |
| `update(id, data)` | Replaces the document. Labels and edge endpoints are kept; the id cannot change. |
| `merge(id, patch)` | Sets the top-level fields of `patch`. A `Null` value removes the field. |
| `delete(id, detach)` | Deletes a record and returns whether it existed. A node that still has edges fails with `SchemaViolation` unless `detach` is `true`, which also deletes the edges. |
| `scan(table, limit)` | Records of a table in key order. |
| `scan_range(table, from, to, limit)` | Records whose keys are in `[from, to)`, for example a composite-key time window. |
| `with_label(label)` | Records whose table is `label` or that carry it as a secondary label. |
| `relate(from, rel_type, to, data)` | Creates an `Edge` in the edge table `rel_type`. Both endpoints must exist, must not be edges (`SchemaViolation`), and must belong to the endpoint tables if the edge table restricts them. A concurrent delete of an endpoint makes one of the two transactions fail with `TxConflict`. |
| `delete_edge(id)` | Deletes an edge and its adjacency entries. |
| `edges(node, direction, rel_type)` | Edges attached to `node`, by `Direction` and optional type. |
| `neighbors(node, direction, rel_type)` | `RecordId`s of the nodes at the other end of those edges. |
| `commit()`, `cancel()` | Used by `Engine` internally; a closure does not call them. |

```rust
use atlantis_db::{Direction, Engine, Object, Value};

let engine = Engine::open_in_memory().await?;
engine.transaction(|tx| {
    let mut alice = Object::new();
    alice.insert("id".into(), Value::from("alice"));
    let alice = tx.create_node(&["Person", "Admin"], alice)?;   // Person:alice
    let bob = tx.create_node(&["Person"], Object::new())?;      // Person:<ULID>
    tx.relate(&alice.id, "KNOWS", &bob.id, Object::new())?;
    Ok(())
}).await?;

let friends = engine.read(|tx| {
    tx.neighbors(&"Person:alice".parse()?, Direction::Outgoing, Some("KNOWS"))
}).await?;
```

The integration tests in
[`crates/atlantis-db/tests/records.rs`](../crates/atlantis-db/tests/records.rs) show
checkpoint and reopen, edge-table restrictions, rollback, and conflicts.

### Config

**File:** [`crates/atlantis-db/src/config.rs`](../crates/atlantis-db/src/config.rs)

`Config` is a type alias for `atlantis_core::EngineConfig`. Build one with
`EngineConfig::builder()` (`Config::builder()` resolves to the same function). `build()` validates the values and returns
`Result<EngineConfig, AtlantisError>`.

```rust
use atlantis_core::{DistanceMetricKind, EngineConfig, HnswParams};

let config = EngineConfig::builder()
    .disk_path("./my_db")
    .vector_dimensions(384)
    .distance_metric(DistanceMetricKind::Cosine)
    .hnsw_params(HnswParams { m: 24, m0: 48, ..Default::default() })
    .build()?;
let engine = Engine::open_with_config(config).await?;
```

Every builder method and field is listed in [22 — Configuration](22-configuration.md).

### ResultSet and Row

**File:** [`crates/atlantis-db/src/result_set.rs`](../crates/atlantis-db/src/result_set.rs)

`ResultSet` holds column names (from `RETURN`) and rows of `Value`.

| `ResultSet` method | Returns | Notes |
|---|---|---|
| `columns()` | `&[String]` | `RETURN` aliases, or `var` / `var.prop` when no alias is given, otherwise `expr`. |
| `row_count()` | `usize` | |
| `is_empty()` | `bool` | |
| `rows()` | `impl Iterator<Item = Row<'_>>` | Borrowed views into the result set. |

| `Row` method | Returns | Notes |
|---|---|---|
| `get(column)` | `Option<&Value>` | Looks up by column name. |
| `get_string(column)` | `Option<&str>` | |
| `get_int(column)` | `Option<i64>` | |
| `get_float(column)` | `Option<f64>` | Widens `Integer`. |
| `get_bool(column)` | `Option<bool>` | |
| `get_vector(column)` | `Option<&[f32]>` | |
| `values()` | `&[Value]` | Raw values in column order. |
| `column_count()` | `usize` | |

```rust
for row in result.rows() {
    println!("{} is {}", row.get_string("name").unwrap_or(""), row.get_int("age").unwrap_or(0));
}
```

### Params

**File:** [`crates/atlantis-db/src/params.rs`](../crates/atlantis-db/src/params.rs)

`Params` wraps `HashMap<String, Value>`. `Engine::execute` converts its
`&[(&str, Value)]` slice through `Params::from`. Build one by hand with `Params::new()`
and `insert(key, value)`; read it with `as_map()` or `into_map()`.

### Value

**File:** [`crates/atlantis-core/src/value.rs`](../crates/atlantis-core/src/value.rs)

`Value` has the original seven variants (`Null`, `Bool`, `Integer`, `Float`, `String`,
`Array`, `Vector`) plus `Bytes`, `Datetime`, `Duration`, `Uuid`, `Object`, `Geometry`
and `RecordId`. The variant list and semantics are in
[03 — Data Model](03-data-model.md#value). Cypher produces and stores only the original
seven today; the others are available through record transactions.

| Conversion | Result |
|---|---|
| `From<bool>` | `Bool` |
| `From<i64>`, `From<i32>` | `Integer` |
| `From<f64>`, `From<f32>` | `Float` |
| `From<&str>`, `From<String>` | `String` |
| `From<Vec<f32>>` | `Vector` |
| `From<Vec<Value>>` | `Array` |
| `From<Vec<u8>>` | `Bytes` |
| `From<Datetime>`, `From<Duration>`, `From<Uuid>`, `From<Object>`, `From<Geometry>`, `From<RecordId>` | The matching variant |

Accessors: `as_string`, `as_int`, `as_float` (widens `Integer`), `as_bool`, `as_vector`,
`as_object`, `as_record_id`, `get_path("a.b")` (follows nested objects), `is_null`,
`is_vector`, `type_name`.

### Error and AtlantisError

**File:** [`crates/atlantis-core/src/error.rs`](../crates/atlantis-core/src/error.rs)

Every fallible method returns `Result<T, AtlantisError>`. The facade re-exports
`AtlantisError` and `Error`, a type alias for it, so `atlantis_db::Error` already names
the target error type. `is_retryable()` returns `true` for `TxConflict` only.

| Variant | Raised when |
|---|---|
| `Parse(String)` | The Cypher text does not parse, including unsupported clauses such as `MERGE`. |
| `InvalidQuery(String)` | The statement parses but cannot be planned, e.g. `WHERE` with no preceding clause, an unknown procedure, or a non-literal `k` in `db.vector.search`. |
| `NodeNotFound(NodeId)` | A referenced Cypher node does not exist. |
| `Storage(String)` | Storage failure, e.g. a graph-store `checkpoint` without a disk path, or a database directory already open in another engine. |
| `Vector(String)` | Vector index failure, e.g. the query argument is not a vector. |
| `DimensionMismatch { expected, actual }` | A vector does not match the index dimension. |
| `Io(std::io::Error)` | Filesystem error. |
| `Serialization(String)` | Encoding or decoding failure. |
| `Config(String)` | Config validation failure from `build()` or `validate()`. |
| `Shutdown` | The actor has stopped or its channel is closed. |
| `Channel(String)` | Internal channel failure. |
| `InvalidValue(String)` | A value is malformed, e.g. a geometry outside WGS84 bounds or an unparsable datetime. |
| `InvalidIdentifier(String)` | A namespace, database, table or label name is not a valid identifier, or a user table starts with `_`. |
| `InvalidRecordId(String)` | A record id or key cannot be parsed or used. |
| `RecordExists(String)` | A record or edge with that id already exists. |
| `RecordNotFound(String)` | A record, or an edge endpoint, does not exist. |
| `SchemaViolation(String)` | A write breaks a table rule: wrong table kind, disallowed edge endpoint, an edge used as an endpoint, changed id or endpoints, or a delete of a node with edges without `detach`. |
| `TxConflict(String)` | A concurrent commit invalidated the transaction. Retryable. |
| `TxFinished` | The transaction was already committed or cancelled. |
| `TxReadOnly` | A write was attempted in a read-only transaction. |
| `Encoding(String)` | A key cannot be encoded or decoded, e.g. a `Vector` in a key. |

```rust
match engine.execute("MATCH (n) RETURN n", &[]).await {
    Ok(result) => { /* ... */ }
    Err(AtlantisError::Parse(msg)) => eprintln!("syntax error: {msg}"),
    Err(AtlantisError::Shutdown) => eprintln!("engine stopped"),
    Err(e) => eprintln!("error: {e}"),
}
```

### Lower-level handle

`atlantis_engine::EngineHandle` also has `create_node(labels, properties)` and
`get_node(id)`, which bypass Cypher. The facade does not expose them. See
[16 — Engine Runtime](16-engine-runtime.md).

## Target design

The target keeps one `Engine` type for every deployment mode. An embedded engine and
a remote connection have the same methods, so code moves from `mem://` to `ws://`
without changes ([19 — Client SDKs](19-client-sdks.md) shares this method surface with
the other languages).

### API surface by phase

| Area | API | Status |
|---|---|---|
| Embedded open | `open`, `open_with_config`, `open_in_memory`, `execute`, `checkpoint`, `shutdown` | Implemented |
| URL connections | `Engine::connect("mem://")`, `Engine::connect("rocksdb://path")` | Phase 1 |
| Transactions | `transaction`, `transaction_with_isolation`, `transaction_with_retry`, `read`, `Error::TxConflict` | Implemented for the record store (`mem` backend). Cypher inside transactions: Phase 1 |
| Session context | `use_db(ns, db)` | Implemented (record transactions only) |
| Session context | `use_ns`, `set_var`, `unset_var`, `use_db` for Cypher | Phase 2 |
| Record identity | `RecordId`, `RecordKey`, `Record`, `Edge`, extended `Value` variants including `Geometry` | Implemented, except `Decimal` and `Range` (Phase 2) |
| Record operations in a transaction | `RecordTx` methods (`create`, `get`, `update`, `merge`, `delete`, `relate`, `scan`, ...) | Implemented |
| Record helpers on `Engine` | `select`, `create`, `insert`, `update`, `merge`, `patch`, `delete`, `relate` | Phase 2 |
| Schema errors | `Error::SchemaViolation` | Implemented for table and edge rules; field schemas Phase 2 |
| Key-value | `engine.kv()`, `tx.kv()` | Phase 3 |
| Live queries | `engine.live(query, params)` stream | Phase 4 |
| Auth | `signin`, `signup`, `authenticate`, `invalidate`, `Credentials`, `Error::PermissionDenied` | Phase 5 |
| Remote connections | `Engine::connect("ws://...")`, `wss://`, `http://`, `https://`; cargo features `protocol-ws`, `protocol-http` | Phase 6 |
| Distributed | `Engine::connect("tikv://pd1:2379,pd2:2379")` | Phase 7 |

Sections marked Planned show design targets. They will be confirmed against the code when
each phase lands.

### Connecting (Planned — Phase 1, remote in Phase 6, `tikv://` in Phase 7)

```rust
pub async fn connect(url: &str) -> Result<Engine, Error>;
pub async fn connect_with_config(url: &str, config: EngineConfig) -> Result<Engine, Error>;
```

```rust
let local  = Engine::connect("rocksdb://./data").await?;   // embedded, persistent
let tests  = Engine::connect("mem://").await?;             // embedded, ephemeral
let remote = Engine::connect("ws://localhost:7700").await?; // server, Phase 6
```

The URL scheme picks the `atlantis-kv` backend or the remote protocol. Remote schemes
need a cargo feature on `atlantis-db`: `protocol-ws` for `ws://` and `wss://`, and
`protocol-http` for `http://` and `https://`. Embedded backends use `kv-mem`,
`kv-rocksdb` and `kv-tikv`. URL forms are
listed in [19 — Client SDKs](19-client-sdks.md) and backend settings in
[22 — Configuration](22-configuration.md). `Engine::open(path)` stays as a shorthand for
`rocksdb://path` once Phase 1 replaces the hot/cold tier.

### Namespace and database (`use_db` implemented for records; rest Planned — Phase 2)

```rust
let mut engine = Engine::open_in_memory().await?;
engine.use_db("acme", "shop")?;   // implemented: selects acme.shop for record transactions
```

Today `use_db` is synchronous, takes `&mut self`, and applies only to record
transactions on that `Engine` handle. In Phase 2 the same selection also applies to
Cypher, the same as `USE acme.shop;`. A separate chained `use_ns(...).use_db(...)` form
is no longer planned.
`set_var(name, value)` defines a session parameter that every later query can read as
`$name`, and `unset_var(name)` removes it. They match the RPC `let` and `unset`
methods. Embedded engines
start in namespace `atlantis`, database `default`. The hierarchy is explained in
[03 — Data Model](03-data-model.md).

### Querying (`execute` exists; extensions planned)

`execute` remains the general entry point. From Phase 1 it accepts multi-statement
text including `BEGIN; ... COMMIT;`. From Phase 2, results may contain `Object`,
`RecordId` and the other extended values below, and `RETURN n` returns the record as an
`Object` rather than an integer id.

### Transactions (Implemented for records; Cypher statements Planned — Phase 1)

```rust
pub async fn transaction<T, F>(&self, f: F) -> Result<T, Error>
where
    F: FnOnce(&mut RecordTx) -> Result<T, Error>;

pub async fn transaction_with_isolation<T, F>(&self, isolation: Isolation, f: F) -> Result<T, Error>
where
    F: FnOnce(&mut RecordTx) -> Result<T, Error>;

pub async fn transaction_with_retry<T, F>(&self, max_attempts: u32, f: F) -> Result<T, Error>
where
    F: FnMut(&mut RecordTx) -> Result<T, Error>;

pub async fn read<T, F>(&self, f: F) -> Result<T, Error>
where
    F: FnOnce(&mut RecordTx) -> Result<T, Error>;
```

The closure is **synchronous** and borrows the transaction. An earlier draft of this
design took an `async move` closure that owned a `Transaction`; that form is no longer
the plan. The example under [Record transactions](#record-transactions) shows the
current form, and [05 — Transactions and Consistency](05-transactions-consistency.md)
has a retry example.

The closure runs against one snapshot. Returning `Ok` commits and returning `Err`
rolls back. `transaction` never retries: a conflict at commit returns
`Error::TxConflict`. `transaction_with_retry` re-runs the closure on `TxConflict` only,
so the closure must be safe to run more than once. Running Cypher inside the closure
(for example a `tx.execute(...)` method) is Phase 1, once Cypher runs on the record
store. Isolation levels and durability are
covered in [05 — Transactions and Consistency](05-transactions-consistency.md).

### Record helpers (Planned — Phase 2)

These `Engine`-level helpers mirror the WebSocket RPC methods in
[17 — Server and Protocols](17-server-protocols.md). Each one compiles to Atlantis
Cypher, so permissions and events apply in the same way. Inside a transaction, the
`RecordTx` methods already cover the same operations
([Record transactions](#record-transactions)).

| Method | Purpose |
|---|---|
| `select(target)` | Read one record (`RecordId`) or every record in a table (`&str`). |
| `create(target, content)` | Create a record. A table name gets an auto-generated ULID key. |
| `insert(table, contents)` | Create many records from an array. |
| `update(target, content)` | Replace record content. |
| `merge(target, partial)` | Deep-merge fields into existing content (like `SET p += {...}`). |
| `patch(target, ops)` | Apply JSON-Patch-style operations. |
| `delete(target)` | Delete a record or every record in a table. Deleting a node record also deletes its edges. |
| `relate(from, edge_table, to, content)` | Create an edge record with `in = from` and `out = to`. |

```rust
let alice: RecordId = "Person:alice".parse()?;
let acme: RecordId = "Company:acme".parse()?;
let created: Value = engine.create(alice.clone(), json!({"name": "Alice", "age": 30})).await?;
engine.merge(alice.clone(), json!({"address": {"city": "Hue"}})).await?;
engine.relate(alice, "WORKS_AT", acme, json!({"since": 2024})).await?;
```

`json!` here stands for any value that converts into `Value::Object`.

### RecordId (Implemented; Cypher functions Planned — Phase 2)

**File:** [`crates/atlantis-core/src/record.rs`](../crates/atlantis-core/src/record.rs)

```rust
use atlantis_db::{RecordId, RecordKey};

let a = RecordId::new("Person", RecordKey::String("alice".into()))?; // Person:alice
let b: RecordId = "Person:42".parse()?;                              // integer key
let c = RecordId::generate("Person")?;                               // ULID key
assert_eq!(a.table, "Person");
```

`RecordId::new` validates the table name. The text forms and key kinds are in
[03 — Data Model](03-data-model.md#records-and-recordid). In Cypher, `id(n)` returning the
`RecordId` and `rid('Person', 'alice')` constructing one are Phase 2.

### Extended Value (Partially implemented — Phase 2)

The target `Value` adds variants without removing any of the original ones:

| Variant | Status |
|---|---|
| `Null`, `Bool`, `Integer(i64)`, `Float(f64)`, `String`, `Array`, `Vector(Vec<f32>)` | Implemented |
| `Bytes`, `Datetime`, `Duration`, `Uuid`, `Object`, `RecordId` | Implemented in Rust; Cypher literals and functions Phase 2 |
| `Geometry` (Point, LineString, Polygon, Multi*, Collection) | Implemented as a value type; geo functions and spatial index Phase 3 |
| `Decimal`, `Range` | Phase 2 |

More accessors (for example `as_datetime`) follow the existing pattern as needed. DDL type names are listed in
[07 — Schema and DDL](07-schema-ddl.md).

### Key-value (Planned — Phase 3)

```rust
let kv = engine.kv();
kv.set("session:42", Value::from("...")).await?;
kv.set_with_ttl("cache:q:abc", Value::from("..."), Duration::from_secs(3600)).await?;
let v: Option<Value> = kv.get("session:42").await?;
let page: Vec<(String, Value)> = kv.scan("session:").await?;
kv.delete("session:42").await?;

// The same API inside a transaction commits atomically with record writes.
engine.transaction(|tx| {
    tx.kv().set("counter:orders", Value::from(1i64))?;
    tx.create("Order", None, vec![], order)?;
    Ok(())
}).await?;
```

The Cypher equivalents are the `db.kv.*` procedures. See
[13 — Key-Value and Time-Series](13-key-value-time-series.md).

### Live queries (Planned — Phase 4)

```rust
pub async fn live(&self, query: &str, params: &[(&str, Value)])
    -> Result<impl Stream<Item = Result<Notification, Error>>, Error>;
```

```rust
let mut live = engine
    .live("MATCH (o:Order) WHERE o.status = 'open' RETURN o", &[])
    .await?;
while let Some(n) = live.next().await {
    let n = n?;                                // Notification { id, action, record }
    println!("{:?} {:?}", n.action, n.record); // action: Create | Update | Delete
}
drop(live); // dropping the stream kills the live query
```

The notification shape matches the RPC push `{result: {id, action, record}}`. Phase 4
live queries cover one table with no aggregation, `ORDER BY` or `LIMIT`. They send no
initial snapshot, and a record that stops matching arrives as `Delete`. Delivery is
at-most-once. If the bounded queue overflows, the live query is killed and the stream
ends with an error. Catch up with `db.changes`. Semantics are in
[14 — Live Queries and Events](14-live-queries-events.md).

### Authentication (Planned — Phase 5)

```rust
let token = engine.signin(Credentials::root("root", &pw)).await?;
let token = engine.signup(Credentials::record("acme", "shop", "account",
                          &[("email", email.into()), ("pass", pass.into())])).await?;
let token = engine.signin(Credentials::record("acme", "shop", "account",
                          &[("email", email.into()), ("pass", pass.into())])).await?;
engine.authenticate(&token).await?;
engine.invalidate().await?;
```

`Credentials::root(user, pass)` signs in a system user. `Credentials::record(ns, db,
access, params)` runs the `SIGNUP` or `SIGNIN` block of a record access method. System
users and access methods are defined in [18 — Security and Auth](18-security-auth.md).
Embedded engines run as root unless a session signs in.

### Error type (Partially implemented)

`atlantis_db::Error` already exists as an alias of `AtlantisError`
([Error and AtlantisError](#error-and-atlantiserror)). The remaining target variants:

| Variant | Raised when | Status |
|---|---|---|
| `TxConflict` | A transaction lost a write-write or serializable read-set check. Retryable. | Implemented (record store) |
| `SchemaViolation` | A write breaks a field type, `ASSERT`, or a constraint. | Implemented for table and edge rules; field checks Phase 2 |
| `ParseError` | The query does not parse (replaces `Parse`). | Phase 2 |
| `Timeout` | A statement exceeded `query.timeout`. | Phase 1 |
| `NotFound` | A referenced record, table or index does not exist (replaces `NodeNotFound`). Records raise `RecordNotFound` today. | Phase 2 |
| `PermissionDenied` | The session lacks the role or record permission for the operation. | Phase 5 |
| `AuthenticationFailed` | Sign-in, sign-up or token validation failed. | Phase 5 |

## Design Decisions

- **One `Engine` for embedded and remote.** Applications switch deployment mode by
  changing the URL, not the code.
- **Cypher is the only query language.** Record helpers and `kv()` are thin wrappers
  that compile to Atlantis Cypher, so permissions, events and change feeds see one
  code path.
- **Retry is opt-in.** `transaction` surfaces `TxConflict`, and only
  `transaction_with_retry` re-runs the closure, because only the caller knows if the work
  can safely run twice.

## What's Next

- [22 — Configuration](22-configuration.md) — every config field, current and planned.
- [23 — Query Cookbook](23-query-cookbook.md) — recipes that use this API.
- [19 — Client SDKs](19-client-sdks.md) — the same surface in other languages.
