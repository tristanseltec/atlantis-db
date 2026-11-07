# 12 — Configuration Reference

Complete reference for all Atlantis DB configuration options. Configuration is defined in `atlantis-core` and exposed through the `Config` type in the public API.

## EngineConfig

**File:** [`crates/atlantis-core/src/config.rs`](../crates/atlantis-core/src/config.rs)

The top-level configuration structure:

```rust
pub struct EngineConfig {
    pub storage: StorageConfig,
    pub vector: VectorConfig,
    pub checkpoint_interval_secs: u64,
}
```

### Defaults

| Field | Default | Description |
|---|---|---|
| `storage.memory_budget_bytes` | `Some(1 GB)` | Max hot tier memory before eviction |
| `storage.eviction_policy` | `Lru` | Eviction algorithm |
| `storage.disk_path` | `None` | Disk path for persistence (set by `Engine::open`) |
| `storage.compaction_threshold` | `1000` | Delta edges per rel type before CSR compaction |
| `vector.dimensions` | `128` | Vector dimensionality |
| `vector.distance_metric` | `Cosine` | Distance metric |
| `vector.hnsw` | See below | HNSW parameters |
| `checkpoint_interval_secs` | `300` (5 min) | Auto-checkpoint interval (not yet wired) |

## Builder API

The `EngineConfigBuilder` provides a fluent API:

```rust
use atlantis_db::Config;
use atlantis_core::{DistanceMetricKind, HnswParams};

let config = Config::builder()
    .memory_budget(512 * 1024 * 1024)   // 512 MB
    .disk_path("./my_db")
    .vector_dimensions(384)
    .distance_metric(DistanceMetricKind::Cosine)
    .hnsw_params(HnswParams {
        m: 24,
        m0: 48,
        ef_construction: 300,
        ef_search: 100,
        max_level: 16,
    })
    .checkpoint_interval(60)
    .build();
```

### Builder Methods

| Method | Parameter | Description |
|---|---|---|
| `memory_budget(bytes)` | `usize` | Sets hot tier memory budget in bytes |
| `disk_path(path)` | `impl Into<PathBuf>` | Sets disk path for persistence |
| `vector_dimensions(dims)` | `usize` | Sets vector dimensionality |
| `distance_metric(metric)` | `DistanceMetricKind` | Sets distance metric |
| `hnsw_params(params)` | `HnswParams` | Sets all HNSW parameters |
| `checkpoint_interval(secs)` | `u64` | Sets checkpoint interval in seconds |
| `build()` | — | Returns the final `EngineConfig` |

## StorageConfig

```rust
pub struct StorageConfig {
    pub memory_budget_bytes: Option<usize>,
    pub eviction_policy: EvictionPolicy,
    pub disk_path: Option<PathBuf>,
    pub compaction_threshold: usize,
}
```

### Fields

#### `memory_budget_bytes`

Maximum memory the hot tier may consume before eviction kicks in.

- `Some(bytes)`: Eviction is enabled. When hot tier memory exceeds this budget, the LRU tracker selects eviction candidates.
- `None`: No limit. The hot tier grows unbounded (purely in-memory).
- Default: `Some(1_073_741_824)` (1 GB)

```rust
// 512 MB budget
.memory_budget(512 * 1024 * 1024)

// No limit (pure in-memory)
// Not directly available via builder; set via config struct
```

#### `eviction_policy`

The algorithm for selecting which nodes to evict.

```rust
pub enum EvictionPolicy {
    Lru,  // Least Recently Used (default, implemented)
    Lfu,  // Least Frequently Used (not yet implemented)
}
```

- Default: `Lru`
- Only `Lru` is currently implemented. `Lfu` is defined but not wired.

#### `disk_path`

Path for on-disk cold tier storage and checkpoints.

- `Some(path)`: Enables persistence. The cold tier file (`cold_tier.db`) and checkpoint file (`checkpoint.db`) are stored in this directory.
- `None`: In-memory only. No disk persistence.
- Default: `None` (but `Engine::open(path)` sets this automatically)

#### `compaction_threshold`

Number of delta edges per relationship type that triggers CSR compaction.

- When the delta log for a relationship type reaches this threshold, the delta is merged into the CSR.
- Lower values: More frequent compaction, smaller deltas, more CSR rebuilds.
- Higher values: Less frequent compaction, larger deltas, more delta lookups.
- Default: `1000`

## VectorConfig

```rust
pub struct VectorConfig {
    pub dimensions: usize,
    pub distance_metric: DistanceMetricKind,
    pub hnsw: HnswParams,
}
```

### Fields

#### `dimensions`

The dimensionality of vectors stored in the HNSW index. All vectors must match this dimensionality.

- Must match your embedding model's output size.
- Common values: 128, 384, 768, 1536, 3072.
- Default: `128`

```rust
.vector_dimensions(384)   // all-MiniLM-L6-v2
.vector_dimensions(1536)  // OpenAI text-embedding-3-small
```

#### `distance_metric`

```rust
pub enum DistanceMetricKind {
    Cosine,      // 1 - cosine similarity (default)
    Euclidean,   // L2 distance
    DotProduct,  // negative dot product
}
```

- Default: `Cosine`
- See [Vector Index](05-vector-index.md) for metric details.

## HnswParams

```rust
pub struct HnswParams {
    pub m: usize,
    pub m0: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub max_level: usize,
}
```

### Fields

#### `m`

Number of nearest neighbors to connect a new entry to during insertion (layers > 0). Higher values improve recall at the cost of memory and build time.

- Range: 5–48 (paper recommendation)
- Default: `16`
- For high recall: 24–48
- For memory efficiency: 8–12

#### `m0`

Maximum connections at layer 0 (the dense base layer). Should be `2 * m`.

- Default: `32` (= `2 * 16`)

#### `ef_construction`

Candidate pool size during index construction. Higher values produce a higher-quality index at the cost of slower insertion.

- Default: `200`
- For better index quality: 300–500
- For faster ingestion: 100–150

#### `ef_search`

Candidate pool size during search. Higher values improve recall at the cost of slower search. Can be overridden per-query via `SearchParams`.

- Default: `50`
- For high recall: 100–200
- For fast search: 20–50

#### `max_level`

Hard cap on the number of HNSW layers. The actual max level is determined by random assignment during insertion.

- Default: `16`
- Rarely needs changing.

## Tuning Scenarios

### High-Recall RAG

```rust
Config::builder()
    .vector_dimensions(384)
    .hnsw_params(HnswParams {
        m: 32,
        m0: 64,
        ef_construction: 400,
        ef_search: 200,
        max_level: 16,
    })
    .build()
```

Prioritizes recall over speed and memory.

### High-Throughput Ingestion

```rust
Config::builder()
    .vector_dimensions(384)
    .hnsw_params(HnswParams {
        m: 12,
        m0: 24,
        ef_construction: 100,
        ef_search: 50,
        max_level: 16,
    })
    .build()
```

Prioritizes ingestion speed. Recall may be lower.

### Memory-Constrained

```rust
Config::builder()
    .memory_budget(256 * 1024 * 1024)  // 256 MB
    .disk_path("./my_db")              // spill to disk
    .vector_dimensions(128)
    .hnsw_params(HnswParams {
        m: 8,
        m0: 16,
        ef_construction: 100,
        ef_search: 50,
        max_level: 16,
    })
    .build()
```

Small memory budget with disk fallback. Lower HNSW connectivity to save memory.

### Pure In-Memory (Testing)

```rust
let config = Config::builder()
    .vector_dimensions(64)
    .build();
let engine = Engine::open_with_config(config).await?;
```

Or simply:

```rust
let engine = Engine::open_in_memory().await?;
```

No disk path, no memory limit. Data is lost on shutdown.

## Serialization

All config types derive `Serialize` and `Deserialize`. You can serialize and restore configuration:

```rust
let config = EngineConfig::builder()
    .vector_dimensions(256)
    .build();

let serialized = bincode::serialize(&config).unwrap();
let restored: EngineConfig = bincode::deserialize(&serialized).unwrap();
assert_eq!(restored.vector.dimensions, 256);
```

## What's Not Yet Wired

The following config options are parsed and stored but not yet used at runtime:

| Option | Status |
|---|---|
| `checkpoint_interval_secs` | Stored but no background actor uses it. Checkpointing is manual via `engine.checkpoint()` or on shutdown. |
| `EvictionPolicy::Lfu` | Defined but not implemented. Only `Lru` is supported. |
| `EvictionPolicy` in general | Eviction happens inline, not via a background actor. |

See [Implementation Roadmap](13-implementation-roadmap.md) for the plan.

## What's Next

- **[01 — Getting Started](01-getting-started.md)** — Using config in practice.
- **[05 — Vector Index](05-vector-index.md)** — HNSW algorithm details.
- **[04 — Storage Engine](04-storage-engine.md)** — How memory budget affects tiering.
