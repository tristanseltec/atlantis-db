# 21 — API Reference

> **Status:** Partially implemented — the embedded Rust facade exists; the multi-model API surface is Planned — Phases 1–7
> **Crates:** `atlantis-db`, `atlantis-core`, `atlantis-engine`, `atlantis-kv` (planned), `atlantis-auth` (planned), `atlantis-server` (planned)

This chapter is the reference for the public Rust API. The first half documents the
API that exists in `crates/atlantis-db/src` today. The second half documents the target
API surface, with the phase that delivers each part. Cypher syntax lives in
[06 — Query Language](06-query-language.md); configuration lives in
[22 — Configuration](22-configuration.md).

## Implemented today

The facade crate `atlantis-db` re-exports these items from
[`crates/atlantis-db/src/lib.rs`](../crates/atlantis-db/src/lib.rs):

```rust
pub use atlantis_core::Value;
pub use config::Config;
pub use engine::Engine;
pub use params::Params;
pub use result_set::{ResultSet, Row};
```

Configuration types come from `atlantis-core`:

```rust
use atlantis_core::{DistanceMetricKind, EngineConfig, HnswParams, StorageConfig, VectorConfig};
use atlantis_core::config::{EngineConfigBuilder, EvictionPolicy};
```

### Known build defects (Phase 0)

The facade source describes the intended embedded API, but the workspace does not
compile as committed. Phase 0 in [25 — Implementation Roadmap](25-implementation-roadmap.md)
fixes these first. Until then, treat the signatures below as the documented contract
rather than a usable build.

| Location | Defect |
|---|---|
| `atlantis-core/src/config.rs` | `AtlantisError` is used without an import, so `atlantis-core` fails to build. |
| `atlantis-db/src/config.rs` | Imports `atlantis_core::EngineConfigBuilder`, which is not re-exported at the crate root. |
| `atlantis-db/src/config.rs` | `impl Config { fn builder() }` adds an inherent method to a type alias of a foreign type, which the orphan rule forbids. Use `EngineConfig::builder()`. |
| `atlantis-db/src/engine.rs` | Imports `atlantis_engine::spawn_engine`, which `atlantis-engine` does not re-export (it lives in `atlantis_engine::actor`). |
| `atlantis-db/src/result_set.rs` | `rows()` declares `Iterator<Item = &Row>` but yields owned `Row` values. |

### Engine

**File:** [`crates/atlantis-db/src/engine.rs`](../crates/atlantis-db/src/engine.rs)

`Engine` wraps a clonable handle to the single-writer engine actor (see
[16 — Engine Runtime](16-engine-runtime.md)). Clones share the same actor and are safe to
move across tasks. Every method is `async` and needs a Tokio runtime.

| Method | Signature | Behaviour |
|---|---|---|
| `open` | `async fn open(path: impl AsRef<Path>) -> Result<Engine, AtlantisError>` | Builds a default config with `disk_path` set to `path`, then calls `open_with_config`. Restores `checkpoint.db` from that directory when it exists. |
| `open_with_config` | `async fn open_with_config(config: EngineConfig) -> Result<Engine, AtlantisError>` | Spawns the engine actor with the given config. |
| `open_in_memory` | `async fn open_in_memory() -> Result<Engine, AtlantisError>` | Uses `EngineConfig::default()` (no disk path). Data is lost on drop or shutdown. |
| `execute` | `async fn execute(&self, cypher: &str, params: &[(&str, Value)]) -> Result<ResultSet, AtlantisError>` | Parses, plans and executes one Cypher statement. `params` binds `$name` references. |
| `checkpoint` | `async fn checkpoint(&self) -> Result<(), AtlantisError>` | Writes the hot tier and cold-tier page index to `checkpoint.db`. Returns `Storage` when no disk path is configured. |
| `shutdown` | `async fn shutdown(&self) -> Result<(), AtlantisError>` | Checkpoints when a disk path is configured, then stops the actor. Later calls return `Shutdown`. |

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

Two behaviours matter when you read results today:

- `RETURN n` for a node variable returns the internal node id as `Value::Integer`, not a
  node map. Return properties (`n.name`) to get data.
- Only `disk_path` and the vector settings from `EngineConfig` reach the engine. The
  actor opens storage with `GraphStore::open(path)` or `GraphStore::new_in_memory()`,
  both of which use default `StorageConfig` values. See
  [22 — Configuration](22-configuration.md#what-is-not-wired-yet).

### Config

**File:** [`crates/atlantis-db/src/config.rs`](../crates/atlantis-db/src/config.rs)

`Config` is a type alias for `atlantis_core::EngineConfig`. Build one with
`EngineConfig::builder()`. `build()` validates the values and returns
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
| `rows()` | iterator of `Row` | Borrowed views; see the build defect above. |

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

```rust
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Array(Vec<Value>),
    Vector(Vec<f32>),
}
```

| Conversion | Result |
|---|---|
| `From<bool>` | `Bool` |
| `From<i64>`, `From<i32>` | `Integer` |
| `From<f64>`, `From<f32>` | `Float` |
| `From<&str>`, `From<String>` | `String` |
| `From<Vec<f32>>` | `Vector` |
| `From<Vec<Value>>` | `Array` |

Accessors: `as_string`, `as_int`, `as_float` (widens `Integer`), `as_bool`, `as_vector`,
`is_null`, `is_vector`, `type_name`. The full type discussion is in
[03 — Data Model](03-data-model.md).

### AtlantisError

**File:** [`crates/atlantis-core/src/error.rs`](../crates/atlantis-core/src/error.rs)

Every fallible method returns `Result<T, AtlantisError>`. `atlantis-db/src/lib.rs`
does not re-export the type, so import it from `atlantis_core::AtlantisError`.

| Variant | Raised when |
|---|---|
| `Parse(String)` | The Cypher text does not parse, including unsupported clauses such as `MERGE`. |
| `InvalidQuery(String)` | The statement parses but cannot be planned, e.g. `WHERE` with no preceding clause, an unknown procedure, or a non-literal `k` in `db.vector.search`. |
| `NodeNotFound(NodeId)` | A referenced node does not exist. |
| `Storage(String)` | Storage failure, e.g. `checkpoint` without a disk path. |
| `Vector(String)` | Vector index failure, e.g. the query argument is not a vector. |
| `DimensionMismatch { expected, actual }` | A vector does not match the index dimension. |
| `Io(std::io::Error)` | Filesystem error. |
| `Serialization(String)` | Encoding or decoding failure. |
| `Config(String)` | Config validation failure from `build()` or `validate()`. |
| `Shutdown` | The actor has stopped or its channel is closed. |
| `Channel(String)` | Internal channel failure. |

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
| Embedded open | `open`, `open_with_config`, `open_in_memory`, `execute`, `checkpoint`, `shutdown` | Implemented (Phase 0 fixes the build) |
| URL connections | `Engine::connect("mem://")`, `Engine::connect("rocksdb://path")` | Phase 1 |
| Transactions | `engine.transaction(f)`, `engine.transaction_with_retry(max_attempts, f)`, `Error::TxConflict` | Phase 1 |
| Session context | `use_ns`, `use_db`, `set_var`, `unset_var` | Phase 2 |
| Record identity | `RecordId`, extended `Value` variants including `Geometry` | Phase 2 |
| Record helpers | `select`, `create`, `insert`, `update`, `merge`, `patch`, `delete`, `relate` | Phase 2 |
| Schema errors | `Error::SchemaViolation` | Phase 2 |
| Key-value | `engine.kv()`, `tx.kv()` | Phase 3 |
| Live queries | `engine.live(query, params)` stream | Phase 4 |
| Auth | `signin`, `signup`, `authenticate`, `invalidate`, `Credentials`, `Error::PermissionDenied` | Phase 5 |
| Remote connections | `Engine::connect("ws://...")`, `wss://`, `http://`, `https://`; cargo features `protocol-ws`, `protocol-http` | Phase 6 |
| Distributed | `Engine::connect("tikv://pd1:2379,pd2:2379")` | Phase 7 |

The signatures below are design targets. They will be confirmed against the code when
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

### Namespace and database (Planned — Phase 2)

```rust
engine.use_ns("acme").use_db("shop").await?;
```

This sets the session context, the same as `USE acme.shop;` in Cypher.
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

### Transactions (Planned — Phase 1)

```rust
pub async fn transaction<T, F, Fut>(&self, f: F) -> Result<T, Error>
where
    F: FnOnce(Transaction) -> Fut,
    Fut: Future<Output = Result<T, Error>>;

pub async fn transaction_with_retry<T, F, Fut>(&self, max_attempts: u32, f: F) -> Result<T, Error>
where
    F: FnMut(Transaction) -> Fut,
    Fut: Future<Output = Result<T, Error>>;
```

```rust
engine.transaction(|tx| async move {
    tx.execute("MATCH (a:Account {id: $from}) SET a.balance = a.balance - $amt", &p).await?;
    tx.execute("MATCH (a:Account {id: $to})   SET a.balance = a.balance + $amt", &p).await?;
    Ok(())
}).await?;

// Re-runs the closure on Error::TxConflict, up to 5 attempts in total.
engine.transaction_with_retry(5, |tx| async move { /* same body */ Ok(()) }).await?;
```

The closure runs against one snapshot. Returning `Ok` commits and returning `Err`
rolls back. `transaction` never retries: a conflict at commit returns
`Error::TxConflict`. `transaction_with_retry` re-runs the closure on `TxConflict` only,
so the closure must be safe to run more than once. Isolation levels and durability are
covered in [05 — Transactions and Consistency](05-transactions-consistency.md).

### Record helpers (Planned — Phase 2)

These helpers mirror the WebSocket RPC methods in
[17 — Server and Protocols](17-server-protocols.md). Each one compiles to Atlantis
Cypher, so permissions and events apply in the same way.

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
let alice: Value = engine.create(RecordId::new("Person", "alice"),
                                 json!({"name": "Alice", "age": 30})).await?;
engine.merge(RecordId::new("Person", "alice"), json!({"address": {"city": "Hue"}})).await?;
engine.relate(RecordId::new("Person", "alice"), "WORKS_AT",
              RecordId::new("Company", "acme"), json!({"since": 2024})).await?;
```

`json!` here stands for any value that converts into `Value::Object`.

### RecordId (Planned — Phase 2)

```rust
pub struct RecordId { /* table: String, key: RecordKey */ }
pub enum RecordKey { Integer(i64), String(String), Uuid(Uuid), Ulid(Ulid), Array(Vec<Value>) }

let a = RecordId::new("Person", "alice");      // Person:alice
let b: RecordId = "Person:42".parse()?;        // integer key
assert_eq!(a.table(), "Person");
```

In Cypher, `id(n)` returns the `RecordId` and `rid('Person', 'alice')` constructs one.

### Extended Value (Planned — Phase 2)

The target `Value` adds variants without removing any of today's:

| Variant | Status |
|---|---|
| `Null`, `Bool`, `Integer(i64)`, `Float(f64)`, `String`, `Array`, `Vector(Vec<f32>)` | Implemented |
| `Decimal`, `Bytes`, `Datetime`, `Duration`, `Uuid`, `Object`, `RecordId`, `Range` | Phase 2 |
| `Geometry` (Point, LineString, Polygon, Multi*, Collection) | Phase 2 (geo functions and spatial index in Phase 3) |

Matching accessors (`as_object`, `as_record_id`, `as_datetime`, ...) and `From`
conversions follow the existing pattern. DDL type names are listed in
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
engine.transaction(|tx| async move {
    tx.kv().set("counter:orders", Value::from(1i64)).await?;
    tx.execute("CREATE (:Order {total: 10})", &[]).await?;
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

### Error type (Planned)

Today's `AtlantisError` becomes `atlantis_db::Error`, re-exported from the facade. New
variants:

| Variant | Raised when | Status |
|---|---|---|
| `ParseError` | The query does not parse (replaces `Parse`). | Phase 2 |
| `TxConflict` | A transaction lost a write-write or serializable read-set check. Retryable. | Phase 1 |
| `Timeout` | A statement exceeded `query.timeout`. | Phase 1 |
| `NotFound` | A referenced record, table or index does not exist (replaces `NodeNotFound`). | Phase 2 |
| `SchemaViolation` | A write breaks a field type, `ASSERT`, or a constraint. | Phase 2 |
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
