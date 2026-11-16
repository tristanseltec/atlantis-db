# 22 — Configuration

> **Status:** Partially implemented — embedded `EngineConfig` (directory, sync mode, checkpoint interval, vector defaults) exists; storage backend URL, transaction, server, auth and cluster settings are Planned — Phases 1–7
> **Crates:** `atlantis-core`, `atlantis-db`, `atlantis-kv`, `atlantis-server` (planned), `atlantis-auth` (planned)

This chapter lists every configuration setting. "Implemented today" covers the
`EngineConfig` struct in `atlantis-core`. "Target design" covers the settings that arrive
with each roadmap phase, plus the `atlantis.toml` file and `ATLANTIS_*` environment
variables used by the server binary.

## Implemented today

**Files:** [`crates/atlantis-core/src/config.rs`](../crates/atlantis-core/src/config.rs),
[`crates/atlantis-db/src/config.rs`](../crates/atlantis-db/src/config.rs)

`atlantis_db::Config` is a type alias for `atlantis_core::EngineConfig`. All config types
derive `Serialize` and `Deserialize`. The crate's own tests round-trip them through
`bincode`.

### EngineConfig

```rust
pub struct EngineConfig {
    pub storage: StorageConfig,
    pub vector: VectorConfig,
    pub checkpoint_interval_secs: u64,
}
```

| Field | Default | Effect |
|---|---|---|
| `storage.disk_path` | `None` (`Engine::open(path)` sets it) | Database directory; `None` is in-memory only |
| `storage.sync` | `SyncMode::Every` | When commits are forced to disk |
| `vector.dimensions` | `128` | Default `dimension` of a vector index |
| `vector.distance_metric` | `Cosine` | Default `metric` of a vector index |
| `vector.hnsw` | see [HnswParams](#hnswparams) | Default HNSW parameters of a vector index |
| `vector.build_threads` | `0` | Threads that build a vector index from its records; `0` uses one per available CPU |
| `checkpoint_interval_secs` | `300` | Period of automatic checkpoints; `0` disables them |

Every field is used at runtime. The settings `memory_budget_bytes`, `eviction_policy`
and `compaction_threshold`, and their builder methods, were removed with the legacy
graph store.

### Builder

`EngineConfig::builder()` returns an `EngineConfigBuilder`. `build()` runs `validate()`
and returns `Result<EngineConfig, AtlantisError>`.

```rust
use atlantis_core::{DistanceMetricKind, EngineConfig, HnswParams, SyncMode};

let config = EngineConfig::builder()
    .disk_path("./my_db")
    .sync_mode(SyncMode::Every)
    .vector_dimensions(384)
    .distance_metric(DistanceMetricKind::Cosine)
    .hnsw_params(HnswParams { m: 24, m0: 48, ef_construction: 300, ef_search: 100, max_level: 16 })
    .checkpoint_interval(60)
    .build()?;
```

| Method | Parameter | Sets |
|---|---|---|
| `disk_path(path)` | `impl Into<PathBuf>` | `storage.disk_path` |
| `sync_mode(sync)` | `SyncMode` | `storage.sync` |
| `vector_dimensions(dims)` | `usize` | `vector.dimensions` |
| `distance_metric(metric)` | `DistanceMetricKind` | `vector.distance_metric` |
| `hnsw_params(params)` | `HnswParams` | `vector.hnsw` |
| `vector_build_threads(n)` | `usize` | `vector.build_threads` |
| `checkpoint_interval(secs)` | `u64` | `checkpoint_interval_secs` |
| `build()` | — | Validates, then returns the config |

`Config::builder()` resolves to `EngineConfig::builder()`, because `Config` is a type
alias.

### Validation rules

`EngineConfig::validate()` returns `AtlantisError::Config` when any rule fails:

- `vector.dimensions` is greater than 0.
- `m`, `m0`, `ef_construction`, `ef_search` and `max_level` are each greater than 0.
- `m0 >= m`.

### StorageConfig

```rust
pub struct StorageConfig {
    pub disk_path: Option<PathBuf>,
    pub sync: SyncMode,                    // Every (default) | Never
}
```

- **`disk_path`**: the directory holding `records.kv` (checkpoint), `records.wal` (and,
  during a checkpoint, `records.wal.old`) and the `LOCK` file that keeps a second engine
  from opening the same directory. `None` means in-memory only; `checkpoint()` is then a
  no-op. See [04 — Storage Engine](04-storage-engine.md).
- **`sync`**: `SyncMode::Every` fsyncs the write-ahead log before each commit returns;
  `SyncMode::Never` leaves flushing to the OS, so a machine crash (not a process crash)
  can lose recent commits. A missing `sync` in serialized config deserializes as
  `Every`. See [05 — Transactions & Consistency](05-transactions-consistency.md).

`checkpoint_interval_secs` sets how often a background thread folds the log into
`records.kv`. Checkpoints bound the log size and recovery time; durability does not
depend on them.

### VectorConfig

```rust
pub struct VectorConfig {
    pub dimensions: usize,                    // default 128
    pub distance_metric: DistanceMetricKind,  // Cosine (default) | Euclidean | DotProduct
    pub hnsw: HnswParams,
}
```

These are the defaults for `CREATE VECTOR INDEX` options that a statement leaves out
(`dimension`, `metric`, `m`, `efConstruction`); `ef_search` is the default search pool.
Set `dimensions` to your embedding model, for example 384 for all-MiniLM-L6-v2 or 1536
for text-embedding-3-small, or give `dimension` per index. Metric details are in
[10 — Vector Index](10-vector-index.md).

### HnswParams

| Field | Default | Effect |
|---|---|---|
| `m` | `16` | Links per node on layers above 0. Higher improves recall and costs memory. |
| `m0` | `32` | Maximum links on layer 0. Conventionally `2 * m`, and must be `>= m`. |
| `ef_construction` | `200` | Candidate pool during insert. Higher gives a better graph and slower ingest. |
| `ef_search` | `50` | Default candidate pool during search. `db.vector.search(..., {efSearch: n})` overrides it per call. |
| `max_level` | `16` | Hard cap on layers. Rarely changed. |

### Tuning presets

| Goal | `m` / `m0` | `ef_construction` | `ef_search` |
|---|---|---|---|
| High recall (RAG) | 32 / 64 | 400 | 200 |
| Fast ingestion | 12 / 24 | 100 | 50 |
| Low memory | 8 / 16 | 100 | 50 |

## Target design

### Configuration surfaces

| Surface | Used by | Status |
|---|---|---|
| `EngineConfig` and its builder | Embedded Rust | Implemented; new sections added per phase |
| Connection URL | `Engine::connect`, SDKs, `atlantis start` | Phase 1 (embedded), Phase 6 (remote) |
| `atlantis.toml` | `atlantis` binary | Phase 5 |
| `ATLANTIS_*` environment variables | `atlantis` binary | Phase 5 |
| `atlantis start` flags | `atlantis` binary | Phase 5 |

Precedence for the binary, highest first: command-line flag, environment variable,
`atlantis.toml`, built-in default. The binary reads `./atlantis.toml` unless
`--config <path>` or `ATLANTIS_CONFIG` names another file.

Every key maps to an environment variable: `ATLANTIS_` plus the upper-cased dotted
path, with `.` replaced by `_`. For example, `transaction.isolation` becomes
`ATLANTIS_TRANSACTION_ISOLATION`.

Credentials (the root password, token signing keys) belong in environment variables or
in files with restricted permissions that the config points to. Never put credential
values in `atlantis.toml` or in version control.

### Full example

```toml
# atlantis.toml

[storage]
url = "rocksdb://./data"          # mem:// | rocksdb://path | tikv://pd1:2379,pd2:2379
sync = "every"                    # every | interval | never
sync_interval = "200ms"           # used when sync = "interval"

[transaction]
isolation = "snapshot"            # snapshot | serializable

[query]
timeout = "30s"

[cache]
record_cache_size = "1GiB"
adjacency_cache_size = "512MiB"
vector_cache_size = "1GiB"

[index]
workers = 2

[live]
queue_size = 1024

[server]
bind = "127.0.0.1:7700"
tls_cert = "/etc/atlantis/tls/cert.pem"
tls_key = "/etc/atlantis/tls/key.pem"
allow_origin = ["https://app.example.com"]
bolt = false                      # needs the `bolt` feature
bolt_bind = "127.0.0.1:7687"
max_body_size = "16MiB"
max_connections = 10000

[auth]
root_user = "root"                # password from ATLANTIS_AUTH_ROOT_PASS or --pass
token_key_file = "/etc/atlantis/token.key"
token_duration = "1h"
session_duration = "12h"
argon2_memory = "19MiB"
argon2_iterations = 2
argon2_parallelism = 1

[cluster]
pd_endpoints = ["pd1:2379", "pd2:2379"]
node_id = "auto"
heartbeat_interval = "5s"
heartbeat_expiry = "30s"

[log]
level = "info"
audit = "/var/log/atlantis/audit.log"
```

### Storage (Partially implemented — `sync` modes `every`/`never`; the rest Phase 1, `tikv` in Phase 7)

| Key | Env var | Default | Meaning |
|---|---|---|---|
| `storage.url` | `ATLANTIS_STORAGE_URL` | `mem://` | Selects the `atlantis-kv` backend. `mem` needs feature `kv-mem`, `rocksdb` needs `kv-rocksdb`, `tikv` needs `kv-tikv`. The positional `<url>` of `atlantis start` sets it. |
| `storage.sync` | `ATLANTIS_STORAGE_SYNC` | `every` | `every` fsyncs the WAL on each commit. `interval` fsyncs every `sync_interval`, so a crash can lose that window. `never` leaves fsync to the OS. `every` and `never` exist today as `StorageConfig::sync`. |
| `storage.sync_interval` | `ATLANTIS_STORAGE_SYNC_INTERVAL` | `200ms` | Flush period for `sync = "interval"`. |

The server falls back to `mem://` when no storage URL is given, so data does not
survive a restart. Always pass a URL in production. URL forms are shared with clients
([19 — Client SDKs](19-client-sdks.md)). Durability trade-offs are explained in
[05 — Transactions and Consistency](05-transactions-consistency.md). Today's
`disk_path` maps to `rocksdb://<disk_path>` once the `rocksdb` backend lands.

### Transactions and queries (Planned — Phase 1)

There is no isolation or timeout setting today. Transactions use snapshot isolation
unless the caller passes `Isolation::Serializable` to
`Engine::transaction_with_isolation` ([21 — API Reference](21-api-reference.md)), and
queries have no row or time limit.

| Key | Env var | Default | Meaning |
|---|---|---|---|
| `transaction.isolation` | `ATLANTIS_TRANSACTION_ISOLATION` | `snapshot` | Default isolation for new transactions. `serializable` adds read-set validation at commit and raises more `TxConflict` errors. |
| `query.timeout` | `ATLANTIS_QUERY_TIMEOUT` | `30s` | Wall-clock limit per statement. Exceeding it returns `Error::Timeout` and rolls back an implicit transaction. |

### Caches (Planned — Phase 1, vector cache in Phase 3)

| Key | Default | Meaning |
|---|---|---|
| `cache.record_cache_size` | `1GiB` | Decoded records kept in memory. |
| `cache.adjacency_cache_size` | `512MiB` | Adjacency cache rebuilt from `g/` keys. |
| `cache.vector_cache_size` | `1GiB` | In-memory HNSW graphs loaded from `vx/` keys. |

Per-index HNSW settings already live in `CREATE VECTOR INDEX ... OPTIONS {...}`
([10 — Vector Index](10-vector-index.md)); `VectorConfig` is the default for indexes
that omit options.

### Index workers (Planned — Phase 2)

| Key | Default | Meaning |
|---|---|---|
| `index.workers` | `2` | Background tasks that build new indexes and backfill existing records. |

### Live queries (Planned — Phase 4)

| Key | Default | Meaning |
|---|---|---|
| `live.queue_size` | `1024` | Bounded notification queue per live query. On overflow the live query is killed, and the client catches up with `db.changes`. |

See [14 — Live Queries and Events](14-live-queries-events.md).

### Server (Planned — Phase 5, Bolt in Phase 6)

| Key | Flag | Default | Meaning |
|---|---|---|---|
| `server.bind` | `--bind` | `127.0.0.1:7700` | HTTP and WebSocket RPC (`/rpc`) listener. |
| `server.tls_cert`, `server.tls_key` | `--tls-cert`, `--tls-key` | unset | PEM files. Setting both enables `https://` and `wss://`. |
| `server.allow_origin` | `--allow-origin` | `[]` | Allowed browser origins for CORS. An empty list disables CORS. |
| `server.bolt` | `--bolt` | `false` | Enables the Bolt listener. Needs the `bolt` feature. |
| `server.bolt_bind` | `--bolt-bind` | `127.0.0.1:7687` | Bolt listener address. |
| `server.max_body_size` | — | `16MiB` | Largest HTTP request body, including `POST /import`. |
| `server.max_connections` | — | `10000` | Concurrent HTTP and WebSocket connections. |
| `server.insecure_no_auth` | `--insecure-no-auth` | `false` | Serves requests without authentication. Refused unless `server.bind` is a loopback address. |
| `log.level` | `--log` | `info` | Log verbosity: `error`, `warn`, `info`, `debug`, `trace`. |
| `log.audit` | `--audit-log` | unset | File for the audit log described in [18 — Security and Auth](18-security-auth.md). |

Endpoints and headers are documented in
[17 — Server and Protocols](17-server-protocols.md).

### Auth (Planned — Phase 5)

| Key | Flag | Default | Meaning |
|---|---|---|---|
| `auth.root_user` | `--user` | unset | Creates this ROOT `owner` user on first start only, when no system user exists. Ignored afterwards. |
| `auth.root_pass` | `--pass` | unset | Its password. Supply it through `ATLANTIS_AUTH_ROOT_PASS` or `--pass`, never the file. |
| `auth.token_key_file` | — | generated and stored under `/sys` | Signing key for issued JWTs. Set it so tokens stay valid across nodes and restarts. |
| `auth.token_duration` | — | `1h` | Token lifetime for system users. Record access methods set their own with `DURATION FOR TOKEN`. |
| `auth.session_duration` | — | `12h` | Session lifetime for system users. Record access methods set their own with `FOR SESSION`. |
| `auth.argon2_memory` | — | `19MiB` | Argon2id memory cost for password hashing. |
| `auth.argon2_iterations` | — | `2` | Argon2id time cost. |
| `auth.argon2_parallelism` | — | `1` | Argon2id lanes. |

The Argon2id defaults follow the OWASP minimum. Raise them on servers with headroom.
Existing hashes keep the parameters they were created with. Roles and access methods are
in [18 — Security and Auth](18-security-auth.md).

### Cluster (Planned — Phase 7)

| Key | Default | Meaning |
|---|---|---|
| `cluster.pd_endpoints` | taken from a `tikv://` URL | TiKV placement-driver endpoints. |
| `cluster.node_id` | `auto` | Identity in `/sys/cluster`. `auto` generates and persists a ULID. |
| `cluster.heartbeat_interval` | `5s` | How often a node refreshes its registration. |
| `cluster.heartbeat_expiry` | `30s` | A node whose last heartbeat is older than this is treated as dead. Its live queries are removed. |

See [20 — Distributed Architecture](20-distributed-architecture.md).

### Example start command

```bash
ATLANTIS_AUTH_ROOT_PASS="$ROOT_PW" atlantis start \
  --bind 0.0.0.0:7700 --user root \
  --tls-cert cert.pem --tls-key key.pem \
  rocksdb://data
```

### Embedded builder additions (Planned)

Embedded applications set the same values through `EngineConfig`, with no file and no
environment lookup:

```rust
let config = EngineConfig::builder()
    .sync_mode(SyncMode::Every)                                // implemented (Every | Never); Interval is Phase 1
    .isolation(Isolation::Snapshot)                            // Phase 1
    .query_timeout(Duration::from_secs(30))                    // Phase 1
    .adjacency_cache_size(512 << 20)                           // Phase 1
    .index_workers(2)                                          // Phase 2
    .build()?;
let engine = Engine::connect_with_config("rocksdb://./data", config).await?;
```

## Design Decisions

- **One key namespace for file, environment and flags.** The same dotted path works in
  all three, so operators do not learn the settings twice.
- **Credentials stay out of the config file.** The file may be committed or copied;
  secrets come from the environment or a key file with restricted permissions.
- **Per-index settings live in DDL.** HNSW, analyzer and spatial options belong to the
  index definition, so two indexes in one database can differ. Global config only holds
  defaults.

## What's Next

- [21 — API Reference](21-api-reference.md) — where `EngineConfig` is passed in.
- [04 — Storage Engine](04-storage-engine.md) — how storage settings behave.
- [17 — Server and Protocols](17-server-protocols.md) — the `atlantis` binary.
