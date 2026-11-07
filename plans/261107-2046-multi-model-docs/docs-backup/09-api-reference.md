# 09 — API Reference

**Crate:** `atlantis-db` (re-exports from `atlantis-core` and `atlantis-engine`)

This chapter is the complete reference for the public API that users interact with.

## Engine

**File:** [`crates/atlantis-db/src/engine.rs`](../crates/atlantis-db/src/engine.rs)

`Engine` is the main entry point. It is cheaply clonable and safe to share across async tasks.

```rust
#[derive(Clone)]
pub struct Engine { /* ... */ }
```

### Methods

#### `Engine::open`

```rust
pub async fn open(path: impl AsRef<Path>) -> Result<Self, AtlantisError>
```

Opens or creates a database at the given path. If a checkpoint exists, the database state is restored. Uses default configuration with the disk path set to the given path.

```rust
let engine = Engine::open("./my_db").await?;
```

#### `Engine::open_with_config`

```rust
pub async fn open_with_config(config: EngineConfig) -> Result<Self, AtlantisError>
```

Opens or creates a database with a custom configuration. See [Configuration Reference](12-configuration.md).

```rust
let config = Config::builder()
    .disk_path("./my_db")
    .vector_dimensions(384)
    .build();
let engine = Engine::open_with_config(config).await?;
```

#### `Engine::open_in_memory`

```rust
pub async fn open_in_memory() -> Result<Self, AtlantisError>
```

Opens an ephemeral in-memory database with no disk persistence. Data is lost when the engine is dropped or shut down. Ideal for testing.

```rust
let engine = Engine::open_in_memory().await?;
```

#### `Engine::execute`

```rust
pub async fn execute(
    &self,
    cypher: &str,
    params: &[(&str, Value)],
) -> Result<ResultSet, AtlantisError>
```

Executes a Cypher query with parameters. Returns a `ResultSet` containing the result columns and rows.

The `params` slice contains `(name, value)` pairs that substitute `$name` references in the query. This enables parameterized queries to prevent injection and improve performance.

```rust
let result = engine.execute(
    "MATCH (n:Person) WHERE n.name = $name RETURN n.age AS age",
    &[("name", "Alice".into())],
).await?;
```

#### `Engine::checkpoint`

```rust
pub async fn checkpoint(&self) -> Result<(), AtlantisError>
```

Triggers a checkpoint, flushing in-memory state to disk. Requires a disk path to be configured. Returns an error if no disk path is set.

```rust
engine.checkpoint().await?;
```

#### `Engine::shutdown`

```rust
pub async fn shutdown(&self) -> Result<(), AtlantisError>
```

Shuts down the engine, flushing in-memory state to disk if a disk path is configured. After shutdown, further operations return `AtlantisError::Shutdown`.

```rust
engine.shutdown().await?;
```

## Config

**File:** [`crates/atlantis-db/src/config.rs`](../crates/atlantis-db/src/config.rs)

`Config` is an alias for `EngineConfig`. See [Configuration Reference](12-configuration.md) for all options.

```rust
pub type Config = EngineConfig;
```

### Config::builder

```rust
pub fn builder() -> EngineConfigBuilder
```

Creates a builder for constructing a `Config` with overrides:

```rust
let config = Config::builder()
    .memory_budget(512 * 1024 * 1024)  // 512 MB
    .disk_path("./my_db")
    .vector_dimensions(384)
    .distance_metric(DistanceMetricKind::Cosine)
    .hnsw_params(HnswParams { m: 24, m0: 48, ..Default::default() })
    .checkpoint_interval(120)
    .build();
```

## ResultSet

**File:** [`crates/atlantis-db/src/result_set.rs`](../crates/atlantis-db/src/result_set.rs)

The result of a query execution.

```rust
pub struct ResultSet {
    /* columns: Vec<String>, rows: Vec<Vec<Value>> */
}
```

### Methods

| Method | Return Type | Description |
|---|---|---|
| `columns()` | `&[String]` | Column names from the RETURN clause |
| `row_count()` | `usize` | Number of rows |
| `is_empty()` | `bool` | True if no rows |
| `rows()` | `impl Iterator<Item = &Row>` | Iterate over rows |

### Usage

```rust
let result = engine.execute("MATCH (n:Person) RETURN n.name AS name, n.age AS age", &[]).await?;

println!("Columns: {:?}", result.columns());
for row in result.rows() {
    println!("{}: {}", row.get_string("name").unwrap_or(""), row.get_int("age").unwrap_or(0));
}
```

## Row

A borrowed view into a single result row.

```rust
pub struct Row<'a> { /* columns: &'a [String], values: &'a [Value] */ }
```

### Methods

| Method | Return Type | Description |
|---|---|---|
| `get(column)` | `Option<&Value>` | Get a value by column name |
| `get_string(column)` | `Option<&str>` | Get a string value |
| `get_int(column)` | `Option<i64>` | Get an integer value |
| `get_float(column)` | `Option<f64>` | Get a float value (widens integers) |
| `get_bool(column)` | `Option<bool>` | Get a boolean value |
| `get_vector(column)` | `Option<&[f32]>` | Get a vector value |
| `values()` | `&[Value]` | Raw value slice |
| `column_count()` | `usize` | Number of columns in this row |

## Params

**File:** [`crates/atlantis-db/src/params.rs`](../crates/atlantis-db/src/params.rs)

A helper for constructing query parameters. Typically used implicitly via the `&[(&str, Value)]` slice in `Engine::execute`.

```rust
pub struct Params(HashMap<String, Value>);
```

The `From<&[(&str, Value)]` implementation converts a slice of tuples into a `Params`:

```rust
// These are equivalent:
engine.execute("MATCH (n) WHERE n.age > $min RETURN n", &[("min", 25i64.into())]).await?;
```

For programmatic construction:

```rust
let mut params = Params::new();
params.insert("name", "Alice");
params.insert("limit", 10i64);
```

## Value

**File:** [`crates/atlantis-core/src/value.rs`](../crates/atlantis-core/src/value.rs)

Re-exported from `atlantis-core`. See [Data Model](03-data-model.md) for the full type documentation.

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

### Common Conversions

```rust
// From Rust types to Value:
Value::from(true)                    // Bool
Value::from(42i64)                   // Integer
Value::from(3.14f64)                 // Float
Value::from("hello")                 // String
Value::from(vec![0.1f32, 0.2, 0.3])  // Vector

// From Value to Rust types (via accessors):
value.as_string()    // Option<&str>
value.as_int()       // Option<i64>
value.as_float()      // Option<f64>  (widens Integer)
value.as_bool()       // Option<bool>
value.as_vector()     // Option<&[f32]>
```

## Error Handling

**File:** [`crates/atlantis-core/src/error.rs`](../crates/atlantis-core/src/error.rs)

All fallible operations return `Result<T, AtlantisError>`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum AtlantisError {
    Parse(String),
    InvalidQuery(String),
    NodeNotFound(NodeId),
    Storage(String),
    Vector(String),
    DimensionMismatch { expected: usize, actual: usize },
    Io(#[from] std::io::Error),
    Serialization(String),
    Config(String),
    Shutdown,
    Channel(String),
}
```

### Common Errors

| Variant | When | Example |
|---|---|---|
| `Parse` | Invalid Cypher syntax | `engine.execute("MATCH (", &[])` |
| `InvalidQuery` | Valid syntax, invalid semantics | `WHERE without MATCH` |
| `DimensionMismatch` | Vector dimension doesn't match config | Inserting a 64-dim vector into a 128-dim index |
| `Storage` | Disk I/O or storage error | Corrupt checkpoint file |
| `Vector` | Vector index error | No vector index available |
| `Shutdown` | Engine has been shut down | Calling execute after shutdown |
| `Io` | Filesystem error | Disk full, permission denied |

### Error Handling Pattern

```rust
match engine.execute("MATCH (n) RETURN n", &[]).await {
    Ok(result) => { /* process results */ }
    Err(AtlantisError::Parse(msg)) => eprintln!("Syntax error: {msg}"),
    Err(AtlantisError::Shutdown) => eprintln!("Engine is shut down"),
    Err(e) => eprintln!("Error: {e}"),
}
```

## Re-exports

The `atlantis-db` crate re-exports the most commonly used types:

```rust
pub use atlantis_core::Value;
pub use config::Config;
pub use engine::Engine;
pub use params::Params;
pub use result_set::{ResultSet, Row};
```

Additional types from `atlantis-core` are available through `Config` (which is `EngineConfig`):

```rust
use atlantis_db::Config;
use atlantis_core::{DistanceMetricKind, HnswParams, StorageConfig, VectorConfig};
```

## What's Next

- **[10 — Query Cookbook](10-query-cookbook.md)** — Practical query examples using this API.
- **[11 — RAG Patterns](11-rag-patterns.md)** — RAG-specific usage patterns.
- **[12 — Configuration Reference](12-configuration.md)** — All configuration options.
