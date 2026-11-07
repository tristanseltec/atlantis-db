# 08 — Engine Runtime

**Crate:** `atlantis-engine`

This chapter documents the actor-based runtime: the core engine actor, command protocol, engine handle, and background actors.

## Actor Model

Atlantis DB uses a single-writer actor model built on tokio's async runtime. The core engine actor owns all mutable state (the `GraphStore` and `HnswIndex`) and processes one command at a time. This eliminates data races without any locks.

```
┌─────────────────────────────────────────────────────┐
│                 User Application                      │
│                                                      │
│    Engine (clone)   Engine (clone)   Engine (clone)   │
│         │                │                │           │
│         └────────────────┼────────────────┘           │
│                          │                            │
│                  ┌───────▼────────┐                   │
│                  │  EngineHandle   │                   │
│                  │ (mpsc::Sender)  │                   │
│                  └───────┬────────┘                   │
│                          │ Commands                     │
│                  ┌───────▼────────┐                   │
│                  │  CoreEngineActor│                   │
│                  │                 │                   │
│                  │  GraphStore     │                   │
│                  │  HnswIndex      │                   │
│                  │  QueryExecutor  │                   │
│                  │                 │                   │
│                  │  Processes one  │                   │
│                  │  command at a   │                   │
│                  │  time (serialized)│                  │
│                  └─────────────────┘                   │
└─────────────────────────────────────────────────────┘
```

## CoreEngineActor

**File:** [`crates/atlantis-engine/src/actor.rs`](../crates/atlantis-engine/src/actor.rs)

The core actor owns all mutable state:

```rust
pub struct CoreEngineActor {
    store: GraphStore,
    vector_index: Option<HnswIndex>,
    config: EngineConfig,
}
```

### Initialization

```rust
pub fn new(config: EngineConfig) -> Result<Self, AtlantisError>
```

On creation:
- If a disk path is configured, open the `GraphStore` from that path (restoring from checkpoint if it exists).
- If no disk path, create an in-memory store.
- Create the `HnswIndex` with the configured parameters, dimensions, and distance metric.

### Run Loop

```rust
pub async fn run(mut self, mut rx: mpsc::Receiver<Command>)
```

The actor enters a loop, awaiting commands from the channel:

```rust
while let Some(cmd) = rx.recv().await {
    match cmd {
        Command::Query { cypher, params, respond } => {
            let result = self.execute_query(&cypher, &params);
            let _ = respond.send(result);
        }
        Command::CreateNode { labels, properties, respond } => { ... }
        Command::GetNode { id, respond } => { ... }
        Command::Checkpoint { respond } => { ... }
        Command::Shutdown { respond } => {
            if self.config.storage.disk_path.is_some() {
                let _ = self.store.checkpoint();
            }
            let _ = respond.send(Ok(()));
            break;
        }
    }
}
```

The loop exits when:
- The channel closes (all handles dropped), or
- A `Shutdown` command is received.

On shutdown, the actor flushes in-memory state to disk if a disk path is configured.

### Spawning

```rust
pub fn spawn_engine(config: EngineConfig) -> Result<EngineHandle, AtlantisError>
```

Creates the actor, opens an `mpsc::channel(128)`, spawns the actor on the tokio runtime via `tokio::spawn`, and returns an `EngineHandle` wrapping the channel sender.

## Command Protocol

**File:** [`crates/atlantis-engine/src/command.rs`](../crates/atlantis-engine/src/command.rs)

Commands are the messages sent to the core actor. Each command carries a `oneshot::Sender` for the response:

```rust
pub enum Command {
    Query {
        cypher: String,
        params: HashMap<String, Value>,
        respond: oneshot::Sender<Result<QueryResult, AtlantisError>>,
    },
    CreateNode {
        labels: Vec<String>,
        properties: HashMap<String, Value>,
        respond: oneshot::Sender<Result<NodeId, AtlantisError>>,
    },
    GetNode {
        id: NodeId,
        respond: oneshot::Sender<Result<Option<Node>, AtlantisError>>,
    },
    Checkpoint {
        respond: oneshot::Sender<Result<(), AtlantisError>>,
    },
    Shutdown {
        respond: oneshot::Sender<Result<(), AtlantisError>>,
    },
}
```

### QueryResult

```rust
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}
```

This is the internal representation of query results. The public API converts it to a `ResultSet` (see [API Reference](09-api-reference.md)).

## EngineHandle

**File:** [`crates/atlantis-engine/src/handle.rs`](../crates/atlantis-engine/src/handle.rs)

The handle is a cheaply clonable wrapper around the channel sender:

```rust
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<Command>,
}
```

Each method sends a command and awaits the `oneshot` response:

```rust
pub async fn query(&self, cypher: &str, params: HashMap<String, Value>)
    -> Result<QueryResult, AtlantisError>
{
    let (tx, rx) = oneshot::channel();
    self.tx.send(Command::Query { cypher: cypher.to_string(), params, respond: tx })
        .await
        .map_err(|_| AtlantisError::Shutdown)?;
    rx.await.map_err(|_| AtlantisError::Shutdown)?
}
```

If the channel is closed (actor shut down), methods return `AtlantisError::Shutdown`.

| Method | Description |
|---|---|
| `query(cypher, params)` | Execute a Cypher query |
| `create_node(labels, properties)` | Create a node directly (bypassing Cypher) |
| `get_node(id)` | Get a node by ID |
| `checkpoint()` | Trigger a disk checkpoint |
| `shutdown()` | Shut down the engine |

## Public API Layer

**Crate:** `atlantis-db`

The public `Engine` type wraps `EngineHandle` and provides an ergonomic API:

```rust
#[derive(Clone)]
pub struct Engine {
    handle: EngineHandle,
}
```

See [API Reference](09-api-reference.md) for the full `Engine` API.

The `execute` method converts the `&[(&str, Value)]` parameter slice into a `HashMap` before sending:

```rust
pub async fn execute(&self, cypher: &str, params: &[(&str, Value)])
    -> Result<ResultSet, AtlantisError>
{
    let params = Params::from(params).into_map();
    let result = self.handle.query(cypher, params).await?;
    Ok(ResultSet::new(result.columns, result.rows))
}
```

## Background Actors

The engine design includes background actors for tiering and checkpointing. In the current implementation, these are **placeholder stubs**:

### Tiering Actor

**File:** [`crates/atlantis-engine/src/tiering_actor.rs`](../crates/atlantis-engine/src/tiering_actor.rs)

```rust
pub async fn run_tiering_loop() {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        // Tiering is handled inline by the store in v1.
    }
}
```

In v1, tiering eviction happens inline when `GraphStore::maybe_evict()` is called (after insertions). The background actor is a placeholder for future work where tiering runs asynchronously and communicates with the core actor via a channel.

### Checkpoint Actor

**File:** [`crates/atlantis-engine/src/checkpoint_actor.rs`](../crates/atlantis-engine/src/checkpoint_actor.rs)

```rust
pub async fn run_checkpoint_loop() {
    let mut interval = tokio::time::interval(Duration::from_secs(300));
    loop {
        interval.tick().await;
        // Checkpointing is handled explicitly by the user or on shutdown in v1.
    }
}
```

In v1, checkpointing is triggered explicitly by `engine.checkpoint()` or on shutdown. The `checkpoint_interval_secs` config option is parsed but not yet used by a background actor. The placeholder exists for future automatic periodic checkpointing.

See [Implementation Roadmap](13-implementation-roadmap.md) for the plan to wire these actors.

## Concurrency Guarantees

- **No data races**: The core actor owns all mutable state and processes commands sequentially. Multiple `Engine` handles can send commands concurrently, but they are processed one at a time.
- **Cheap cloning**: `Engine` and `EngineHandle` are `Clone` (backed by `mpsc::Sender` which is cheaply clonable). Share across async tasks freely.
- **Graceful shutdown**: Send `Shutdown` to flush state and stop the actor. After shutdown, further commands return `AtlantisError::Shutdown`.
- **Channel capacity**: The command channel has a capacity of 128 messages. If the channel is full, `send().await` will backpressure the caller until the actor processes messages.

## Design Decisions

**Why a single-writer actor instead of `RwLock` or `Mutex`?**
Locks introduce complexity: deadlock potential, lock contention, and the need for careful lock ordering. The actor model serializes all operations through a single channel, which is simpler and eliminates an entire class of concurrency bugs. For an embedded database where the caller is in the same process, the throughput cost of serialization is acceptable — the bottleneck is typically I/O and computation, not lock acquisition.

**Why `mpsc` with capacity 128?**
A bounded channel provides backpressure: if the actor is slow, senders await instead of buffering unbounded messages. 128 is enough to smooth out bursty workloads without excessive memory use. The capacity is a compile-time constant; making it configurable is a future option.

**Why `oneshot` channels for responses instead of a shared result?**
`oneshot::Sender`/`Receiver` is the idiomatic tokio pattern for request-response. It's efficient (single message, no buffering) and naturally handles the case where the receiver is dropped (the send fails silently).

**Why not spawn background actors in v1?**
Tiering and checkpointing work correctly when done inline. Spawning separate actors adds complexity (channel communication, coordination) without functional benefit in v1. The placeholder loops exist so the architecture is ready for async background processing when the workload demands it.

## What's Next

- **[09 — API Reference](09-api-reference.md)** — The public API that wraps this runtime.
- **[12 — Configuration Reference](12-configuration.md)** — Engine configuration including checkpoint intervals.
- **[13 — Implementation Roadmap](13-implementation-roadmap.md)** — Background actor implementation plan.
