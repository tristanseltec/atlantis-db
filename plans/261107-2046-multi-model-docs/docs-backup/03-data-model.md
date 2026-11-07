# 03 — Data Model

**Crate:** `atlantis-core`

This chapter documents the property graph data model: the core types shared across all Atlantis DB crates. These types are the foundation that storage, vector, Cypher, and the engine build upon.

## Property Graph Model

Atlantis DB implements the property graph model:

- **Nodes** are entities with zero or more labels and a map of properties.
- **Relationships** are directed edges between two nodes, with exactly one type and a map of properties.
- **Labels** categorize nodes (e.g., `:Person`, `:Embedding`). A node can have multiple labels.
- **Relationship types** categorize edges (e.g., `:KNOWS`, `:EMBEDS`). Each relationship has exactly one type.
- The graph is **schemaless** by default — labels and relationship types are created implicitly when data is inserted.

```
    (:Person {name: "Alice"})
         │
      :KNOWS {since: 2020}
         │
         ▼
    (:Person {name: "Bob"})
```

## Value

`Value` is the universal property value type, stored on nodes and relationships. It is defined in [`crates/atlantis-core/src/value.rs`](../crates/atlantis-core/src/value.rs).

```rust
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Array(Vec<Value>),      // heterogeneous array
    Vector(Vec<f32>),       // fixed-dimension embedding
}
```

The `Vector` variant is distinct from `Array` to enable type-checking and efficient HNSW storage. This is a key design decision: embeddings are first-class values, not just arrays of floats.

### Value Comparison

`Value` implements `PartialEq` and `PartialOrd` for use in `WHERE` clause comparisons:

- **Cross-type numeric comparison**: `Integer` and `Float` are comparable (`42 == 42.0` is true).
- **Null handling**: `Null` is not comparable with any non-null value — comparisons return `None`.
- **Type safety**: Different non-numeric types are not comparable (e.g., comparing a `String` to an `Integer` returns `None`).

### Convenience Conversions

`Value` implements `From` for common Rust types, making parameter passing ergonomic:

```rust
Value::from(true)           // Bool(true)
Value::from(42i64)          // Integer(42)
Value::from(42i32)          // Integer(42)  (widened)
Value::from(3.14f64)        // Float(3.14)
Value::from(3.14f32)        // Float(3.14)  (widened)
Value::from("hello")        // String("hello")
Value::from(String::from("hello"))  // String
Value::from(vec![0.1f32, 0.2, 0.3])  // Vector([0.1, 0.2, 0.3])
Value::from(vec![Value::Integer(1), Value::Bool(true)])  // Array
```

### Accessor Methods

| Method | Returns | Notes |
|---|---|---|
| `as_string()` | `Option<&str>` | For `String` values |
| `as_int()` | `Option<i64>` | For `Integer` values |
| `as_float()` | `Option<f64>` | For `Float` or `Integer` (widened) |
| `as_bool()` | `Option<bool>` | For `Bool` values |
| `as_vector()` | `Option<&[f32]>` | For `Vector` values |
| `is_null()` | `bool` | True if `Null` |
| `is_vector()` | `bool` | True if `Vector` |
| `type_name()` | `&'static str` | e.g., `"integer"`, `"vector"` |

## NodeId and EdgeId

Identifiers are lightweight `u64` wrappers, allocated sequentially by the storage engine:

```rust
pub struct NodeId(pub u64);   // Copy, Eq, Hash, Ord
pub struct EdgeId(pub u64);   // Copy, Eq, Hash, Ord
```

Both implement `Copy` for cheap passing by value, and `From<u64>` / `Into<u64>` for conversions. Display formats: `NodeId(42)` displays as `#42`, `EdgeId(42)` as `#e42`.

## Node

A graph node with labels and properties:

```rust
pub struct Node {
    pub id: NodeId,
    pub labels: LabelSet,
    pub properties: PropertyMap,
}
```

Builder pattern for construction:

```rust
let node = Node::new(NodeId(1))
    .with_label("Person")
    .with_label("Adult")
    .with_property("name", "Alice")
    .with_property("age", 30i64);
```

Query methods: `has_label(&Label) -> bool`, `get_property(&str) -> Option<&Value>`.

### NodeRef

A lightweight reference for passing node identity and labels between actors without cloning the full property map:

```rust
pub struct NodeRef {
    pub id: NodeId,
    pub labels: Arc<LabelSet>,
}
```

## Relationship

A directed edge between two nodes:

```rust
pub struct Relationship {
    pub id: EdgeId,
    pub from: NodeId,
    pub to: NodeId,
    pub rel_type: RelType,
    pub properties: PropertyMap,
}
```

Builder pattern:

```rust
let rel = Relationship::new(EdgeId(1), NodeId(0), NodeId(1), RelType::from("KNOWS"))
    .with_property("since", 2020i64)
    .with_property("strength", 0.95f64);
```

## Label and LabelSet

Labels use `Arc<str>` for cheap cloning — labels are frequently shared across nodes and passed between actors:

```rust
pub struct Label(Arc<str>);

pub struct LabelSet(BTreeSet<Label>);
```

`LabelSet` uses `BTreeSet` for deterministic iteration order, which is important for reproducible query results and consistent serialization.

Key methods:

| Method | Description |
|---|---|
| `Label::new(s)` | Create from any `AsRef<str>` |
| `Label::as_str()` | Borrow as `&str` |
| `LabelSet::insert(label)` | Add a label |
| `LabelSet::contains(&label)` | Check membership |
| `LabelSet::is_superset(&other)` | True if contains all labels in `other` |
| `LabelSet::iter()` | Iterate in sorted order |

Display format: `Label::from("Person")` displays as `:Person`.

## RelType

Relationship types also use `Arc<str>` for cheap cloning:

```rust
pub struct RelType(Arc<str>);
```

Same API shape as `Label`: `new`, `as_str`, `From<&str>`, `From<String>`. Display format: `:KNOWS`.

## PropertyMap

A map of property key-value pairs, backed by `HashMap<String, Value>`:

```rust
pub struct PropertyMap(HashMap<String, Value>);
```

Key methods: `insert`, `get`, `get_mut`, `remove`, `contains`, `len`, `is_empty`, `iter`, `clear`, `extend`. Also implements `FromIterator<(String, Value)>`.

Keys are case-sensitive strings.

## Direction

The direction of a relationship in a graph pattern, used by the parser and planner:

```rust
pub enum Direction {
    Outgoing,   // ()-[r]->()
    Incoming,   // ()<-[]-()
    Both,       // ()<-[]->()
}
```

Helper methods: `includes_outgoing()`, `includes_incoming()`, `reverse()`.

## Configuration Types

All configuration is defined in [`crates/atlantis-core/src/config.rs`](../crates/atlantis-core/src/config.rs). See [Configuration Reference](12-configuration.md) for the full details.

```rust
pub struct EngineConfig {
    pub storage: StorageConfig,
    pub vector: VectorConfig,
    pub checkpoint_interval_secs: u64,
}
```

## Error Type

The unified error type for all Atlantis DB operations, defined with `thiserror`:

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

All fallible public API operations return `Result<T, AtlantisError>`. The `Io` variant uses `#[from]` for automatic conversion from `std::io::Error`.

## Serialization

All core types derive `Serialize` and `Deserialize` via serde. The persistence layer uses bincode for compact binary serialization. This means `Node`, `Relationship`, `Value`, `LabelSet`, `PropertyMap`, and all config types can be serialized and restored from disk.

## Design Decisions

**Why `Arc<str>` for labels and relationship types?**
Labels and relationship types are shared across many nodes/edges. `Arc<str>` provides cheap cloning (reference count bump) rather than allocating a new `String` each time. This matters because labels are passed between actors and stored in indexes.

**Why `BTreeSet` for `LabelSet` instead of `HashSet`?**
Deterministic iteration order. This ensures that query results are reproducible and serialization is consistent. The overhead is negligible for the small number of labels per node.

**Why a separate `Vector` variant in `Value`?**
Type safety and storage efficiency. Distinguishing embedding vectors from generic arrays allows the HNSW index to enforce dimensionality and use compact `Vec<f32>` storage. It also makes the Cypher type system cleaner — `db.vector.search` expects a `Vector`, not an `Array`.

**Why `i64` for integers and `f64` for floats?**
These are the natural widths for 64-bit platforms. Integer property values use `i64`, and float values use `f64`. The `as_float()` accessor widens `Integer` to `f64` for cross-type comparisons.

**Why `Copy` for `NodeId` and `EdgeId`?**
IDs are `u64` — small enough to pass by value everywhere. `Copy` avoids borrow-checker friction and eliminates reference counting overhead for the most frequently passed values in the engine.

## What's Next

- **[04 — Storage Engine](04-storage-engine.md)** — How nodes, relationships, and adjacency are stored in memory and on disk.
- **[05 — Vector Index](05-vector-index.md)** — How the `Vector` value variant powers HNSW search.
- **[09 — API Reference](09-api-reference.md)** — How to use these types from the public API.
