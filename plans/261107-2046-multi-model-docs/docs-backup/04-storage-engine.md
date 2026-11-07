# 04 — Storage Engine

**Crate:** `atlantis-storage`

This chapter documents the hybrid tiered storage engine: the in-memory hot tier, the on-disk cold tier, the tiering manager, and the persistence format.

## Overview

Atlantis DB uses a hybrid tiered storage architecture. Frequently accessed data stays in memory for sub-millisecond access. Infrequently accessed data is evicted to disk. This lets datasets exceed available RAM without sacrificing hot-path performance.

```
┌──────────────────────────────────────────────┐
│              GraphStore (facade)               │
│                                               │
│  ┌──────────────┐     ┌──────────────────┐   │
│  │   Hot Tier    │     │    Cold Tier      │   │
│  │  (in memory)  │     │   (on disk)       │   │
│  │               │     │                   │   │
│  │  Nodes[Vec]   │     │  cold_tier.db     │   │
│  │  Edges[Vec]   │     │  (append-only)    │   │
│  │  CSR adjacency│     │                   │   │
│  │  Delta log    │     │  Page index:      │   │
│  │  Label index  │     │  NodeId→Location  │   │
│  │  LRU tracker   │     │                   │   │
│  └───────┬───────┘     └────────┬──────────┘   │
│          │    TieringManager     │              │
│          └──────────┬────────────┘              │
│                     │                           │
│            Persistence (checkpoint)             │
│            checkpoint.db (bincode)              │
└──────────────────────────────────────────────┘
```

## Hot Tier

**File:** [`crates/atlantis-storage/src/hot_tier.rs`](../crates/atlantis-storage/src/hot_tier.rs)

The hot tier is the in-memory store for frequently accessed data. It combines several data structures for efficient access:

### Node and Edge Storage

Nodes and edges are stored in `Vec<Option<Node>>` and `Vec<Option<Relationship>>`, indexed by `NodeId` and `EdgeId` respectively. This gives O(1) direct access by ID. Deleted entries are tracked via a tombstone `HashSet<NodeId>` and their slots are marked `None` for reuse.

```rust
pub struct HotTier {
    nodes: Vec<Option<Node>>,
    edges: Vec<Option<Relationship>>,
    free_node_slots: Vec<usize>,   // reusable slots after deletion
    free_edge_slots: Vec<usize>,
    deleted_nodes: HashSet<NodeId>, // tombstones
    next_node_id: u64,
    next_edge_id: u64,
    // ... adjacency, indexes, tracker
}
```

### CSR + Delta Adjacency

Adjacency is stored using a combination of CSR (Compressed Sparse Row) and a delta log:

- **CSR** (`CsrStore`): Read-optimized, contiguous arrays for cache-friendly traversal. One `CsrAdjacency` per relationship type.
- **Delta log** (`DeltaLog`): Incremental edge insertions that haven't been compacted into CSR yet.

When you query neighbors, the hot tier merges results from both the delta log and the CSR. When the delta for a relationship type exceeds the compaction threshold, it is merged into the CSR.

```
Insert edge → DeltaLog
                    ↓ (compaction threshold reached)
              Compaction: merge delta + CSR → rebuild CSR
```

This avoids rebuilding the CSR on every edge insertion while still supporting incremental updates.

### CSR Structure

**File:** [`crates/atlantis-storage/src/csr.rs`](../crates/atlantis-storage/src/csr.rs)

```rust
pub struct CsrAdjacency {
    offsets: Vec<u64>,     // offsets[node_id] → start index in targets
    targets: Vec<NodeId>,  // target node IDs, contiguous per source
    edge_ids: Vec<EdgeId>,  // edge IDs, parallel to targets
}
```

- `offsets[node_id]` to `offsets[node_id + 1]` gives the range of outgoing edges for that node.
- Out-degree = `offsets[node_id + 1] - offsets[node_id]`.
- Building CSR from an edge list: sort by source, count out-degrees, prefix-sum offsets, fill targets.

`CsrStore` holds one `CsrAdjacency` per relationship type, stored in a `HashMap<RelType, CsrAdjacency>`.

### Delta Log

**File:** [`crates/atlantis-storage/src/delta.rs`](../crates/atlantis-storage/src/delta.rs)

```rust
pub struct DeltaLog {
    entries: HashMap<NodeId, Vec<(NodeId, EdgeId)>>,
}
```

Operations: `insert(from, to, edge_id)`, `get(from) -> &[(NodeId, EdgeId)]`, `drain() -> Vec<(from, to, edge_id)>` (for compaction), `remove_node`, `remove_edge`.

### Label Index

A `HashMap<Label, Vec<NodeId>>` maps each label to all node IDs with that label. This enables fast `NodeByLabelScan` in the query executor. The index is maintained on insert and delete.

### LRU Tracker

**File:** [`crates/atlantis-storage/src/lru.rs`](../crates/atlantis-storage/src/lru.rs)

An LRU access tracker for tiering decisions:

```rust
pub struct LruTracker {
    access_order: VecDeque<NodeId>,    // most recent at back
    positions: HashMap<NodeId, usize>, // quick position lookup
}
```

When the hot tier exceeds its memory budget, the LRU tracker provides eviction candidates (least recently used nodes). The tiering manager evicts these to the cold tier.

### Compaction

Compaction merges the delta log for a relationship type into the CSR:

1. Collect all existing CSR edges (filtering out deleted nodes).
2. Drain all delta edges (filtering out deleted nodes).
3. Rebuild the CSR from the combined edge list.
4. Clear the delta.

Compaction is triggered automatically when the delta for a relationship type exceeds the `compaction_threshold` (default: 1000). It can also be triggered manually via `GraphStore::compact_all()`.

### Memory Estimation

`HotTier::memory_usage()` provides a rough byte estimate based on node and edge sizes. This is used by the tiering manager to decide when to evict.

## Cold Tier

**File:** [`crates/atlantis-storage/src/cold_tier.rs`](../crates/atlantis-storage/src/cold_tier.rs)

The cold tier stores evicted nodes on disk using a simple append-only file with bincode serialization:

```rust
pub struct ColdTier {
    file: File,                              // append-only page file
    page_index: HashMap<NodeId, PageLocation>, // NodeId → file location
    base_path: PathBuf,
}

pub struct PageLocation {
    pub offset: u64,
    pub size: u32,
}
```

- **Write**: Serialize the node with bincode, append to the file, record the location in the page index.
- **Read**: Look up the location, seek to the offset, read `size` bytes, deserialize.
- **Contains**: Check the page index.
- **Remove**: Delete the entry from the page index (data remains in the file but is unreferenced).

This is a simple, durable format. More sophisticated page-based or LSM-tree formats are future options (see [Implementation Roadmap](13-implementation-roadmap.md)).

## Tiering Manager

**File:** [`crates/atlantis-storage/src/tiering.rs`](../crates/atlantis-storage/src/tiering.rs)

The tiering manager moves data between hot and cold tiers:

```rust
pub fn maybe_evict(&mut self) -> Result<usize, AtlantisError>
pub fn promote(&mut self, node_id: NodeId) -> Result<Option<Node>, AtlantisError>
pub fn is_on_disk(&self, node_id: NodeId) -> bool
```

**Eviction** (`maybe_evict`):
1. Check if hot tier memory usage exceeds the configured budget.
2. If over budget, get eviction candidates from the LRU tracker (least recently used).
3. Evict each candidate: remove from hot tier, write to cold tier.
4. Returns the number of nodes evicted.

**Promotion** (`promote`):
1. Read the node from the cold tier.
2. Remove it from the cold tier index.
3. Return the node data (caller re-associates it with the hot tier).

Note: In the current implementation, promotion does not fully re-insert into the hot tier with the original node ID, because IDs are allocated sequentially. This is a known limitation documented in the [Implementation Roadmap](13-implementation-roadmap.md).

## GraphStore Facade

**File:** [`crates/atlantis-storage/src/store.rs`](../crates/atlantis-storage/src/store.rs)

`GraphStore` is the unified API that combines the hot and cold tiers:

```rust
pub struct GraphStore {
    hot: HotTier,
    cold: Option<ColdTier>,
    config: StorageConfig,
}
```

Key operations:

| Method | Description |
|---|---|
| `new_in_memory()` | Create with no disk persistence |
| `with_config(config)` | Create with a storage config (opens cold tier if disk path set) |
| `open(path)` | Open from disk, restoring from checkpoint if it exists |
| `insert_node(labels, properties)` | Insert a node, returns `NodeId` |
| `insert_edge(from, to, rel_type, properties)` | Insert an edge, returns `EdgeId` |
| `get_node_owned(id)` | Get an owned copy of a node |
| `get_node_hot(id)` | Get a node from the hot tier only (no promotion) |
| `get_edge(id)` | Get a reference to an edge |
| `get_neighbors(from, rel_type)` | Get neighbors via a relationship type |
| `get_nodes_by_label(label)` | Get all node IDs with a label |
| `get_all_nodes()` | Get all node IDs (for `AllNodesScan`) |
| `delete_node(id)` | Delete a node (hot + cold) |
| `delete_edge(id)` | Delete an edge |
| `set_property(node_id, key, value)` | Set/update a property |
| `remove_property(node_id, key)` | Remove a property |
| `checkpoint()` | Persist to disk |
| `maybe_evict()` | Trigger tiering eviction if over budget |
| `compact_all()` | Compact all dirty relationship types |

The `get_node_hot` method is used by the query executor because it only requires a shared reference (no promotion needed for read-only queries).

## Persistence

**File:** [`crates/atlantis-storage/src/persistence.rs`](../crates/atlantis-storage/src/persistence.rs)

Persistence serializes the entire hot tier state to a binary file:

```rust
pub struct SerializedStore {
    pub magic: [u8; 8],           // b"ATLANTIS"
    pub version: u32,              // format version (currently 1)
    pub nodes: Vec<Node>,
    pub edges: Vec<Relationship>,
    pub label_index: HashMap<Label, Vec<NodeId>>,
    pub next_node_id: u64,
    pub next_edge_id: u64,
    pub cold_index: HashMap<NodeId, PageLocation>,
}
```

- **Checkpoint**: Serialize all live nodes, edges, label index, and ID counters to `checkpoint.db` via bincode.
- **Restore**: Read the file, validate magic bytes and version, rebuild the hot tier by re-inserting all nodes and edges.

The magic bytes (`b"ATLANTIS"`) and format version allow future format evolution. The cold tier page index is also saved and restored.

## Data Flow Example

Creating a node and relationship:

```
engine.execute("CREATE (a:Person {name: 'Alice'})-[:KNOWS]->(b:Person {name: 'Bob'})", &[])
  │
  ▼
GraphStore::insert_node(["Person"], {name: "Alice"})  →  NodeId(0)
  │  HotTier::insert_node:
  │    - allocate NodeId(0)
  │    - store in nodes[0]
  │    - update label_index["Person"] → [NodeId(0)]
  │    - touch LRU tracker
  ▼
GraphStore::insert_node(["Person"], {name: "Bob"})    →  NodeId(1)
  ▼
GraphStore::insert_edge(NodeId(0), NodeId(1), "KNOWS", {})
  │  HotTier::insert_edge:
  │    - allocate EdgeId(0)
  │    - store in edges[0]
  │    - add to delta["KNOWS"]: NodeId(0) → [(NodeId(1), EdgeId(0))]
  │    - touch LRU tracker
  ▼
  (if delta["KNOWS"].len() >= compaction_threshold)
    → HotTier::compact("KNOWS"): merge delta + CSR → rebuild CSR
```

Querying neighbors:

```
GraphStore::get_neighbors(NodeId(0), &RelType::from("KNOWS"))
  │
  ▼
HotTier::get_neighbors:
  - check delta["KNOWS"].get(NodeId(0))  → [(NodeId(1), EdgeId(0))]
  - check CSR["KNOWS"].neighbor_targets(NodeId(0))  → (empty or compacted)
  - merge results, filter deleted nodes
  → [(NodeId(1), EdgeId(0))]
```

## Design Decisions

**Why CSR + delta instead of pure CSR or pure adjacency lists?**
CSR gives O(1) access to a node's neighbor slice with excellent cache locality — critical for traversal-heavy workloads. But rebuilding CSR on every insert is expensive. The delta log allows O(1) incremental inserts while periodic compaction keeps the CSR up to date. This is a well-known technique from graph database research.

**Why an append-only file for the cold tier?**
Simplicity and durability. Appending is fast, and bincode serialization is compact. Page-based formats with mmap or LSM-trees are more complex and are future options. The current format is sufficient for v1's cold tier needs.

**Why LRU as the default eviction policy?**
LRU is simple to implement and works well for most access patterns. The `EvictionPolicy` config enum supports `Lru` and `Lfu`, though the current implementation only uses LRU. Adaptive policies are a future option.

**Why are node IDs sequential and not UUIDs?**
Sequential `u64` IDs are compact (8 bytes), enable CSR indexing by array position, and are cheap to pass by value (they're `Copy`). The trade-off is that IDs are not stable across checkpoints in all cases — this is a known limitation being addressed in the roadmap.

## What's Next

- **[05 — Vector Index](05-vector-index.md)** — The HNSW vector index that powers similarity search.
- **[07 — Query Planner & Executor](07-query-planner-executor.md)** — How the executor reads from the storage engine.
- **[12 — Configuration Reference](12-configuration.md)** — Storage config options.
