# 20 — Distributed Architecture

> **Status:** Planned — Phase 7
> **Crates:** `atlantis-kv` (planned, feature `kv-tikv`), `atlantis-server` (planned), `atlantis-engine`, `atlantis-vector`, `atlantis-text` (planned), `atlantis-storage`

This chapter describes the **Cluster** deployment mode: many stateless `atlantis` compute
nodes sharing one TiKV cluster as their storage. It covers the separation of compute and
storage, how Atlantis keys map onto TiKV regions, the transaction model, cluster
membership, distributed live queries, index caches, scaling, multi-region deployment,
failure behaviour and operations.

## Implemented Today

Nothing in this chapter is implemented. Atlantis DB runs in one process today, with a
single-writer tokio actor over an in-memory hot tier and an append-only cold tier on
local disk (see [04-storage-engine.md](04-storage-engine.md) and
[16-engine-runtime.md](16-engine-runtime.md)). There is no `atlantis-kv` layer yet, and
no network protocol.

Cluster mode depends on the earlier phases: the transactional KV layer and key encoding
(Phase 1), the record model and indexes (Phases 2–3), live queries and change feeds
(Phase 4), and the server (Phase 5). Phase 7 adds the `tikv` backend and the cluster
behaviour described below.

## Why Compute/Storage Separation (Planned — Phase 7)

```
            clients (SDKs, HTTP, WS RPC, Bolt)
                        │
                  load balancer
          ┌─────────────┼─────────────┐
     atlantis node  atlantis node  atlantis node      ← stateless compute
     (parse, plan,  (…)            (…)                  caches only
      execute,
      permissions,
      live queries)
          └─────────────┼─────────────┘
                        │  transactional KV (atlantis-kv, kv-tikv)
       ┌────────────────┼────────────────┐
     PD (placement driver, timestamps)   TiKV stores (Raft groups per region)
```

Atlantis does not implement its own replication or consensus for the cluster mode.
TiKV already provides a sorted, transactional, Raft-replicated key-value store with
automatic sharding and rebalancing. Because every Atlantis model is encoded as ordered
keys in `atlantis-kv` (see [04-storage-engine.md](04-storage-engine.md)), the same engine
that runs over `rocksdb://` runs over `tikv://` with a different backend.

Consequences:

- **Compute nodes hold no authoritative state.** Any node can serve any request. A node
  can be added, removed or replaced without moving data.
- **Storage scales independently.** Add TiKV stores for capacity and write throughput;
  add `atlantis` nodes for query CPU, connections and live-query fan-out.
- **One code path.** Embedded, single-server and cluster modes share the engine,
  planner, permissions and index code. Only the `KvStore` implementation and the cluster
  coordination described here are cluster-specific.

Start a node:

```bash
atlantis start --bind 0.0.0.0:7700 tikv://pd1:2379,pd2:2379,pd3:2379
# or: ATLANTIS_STORAGE_URL=tikv://pd1:2379,pd2:2379,pd3:2379 atlantis start --bind 0.0.0.0:7700
```

The PD endpoints locate the cluster. The first node to start initializes the catalog
under `/sys/...`; root credentials are created as described in
[18-security-auth.md](18-security-auth.md).

## Key Ranges and Regions (Planned — Phase 7)

TiKV splits the key space into contiguous **regions** (ranges of keys), each replicated
by its own Raft group, and moves regions between stores to balance load. Atlantis does
not choose region boundaries; it chooses key layout, and the layout decides locality.

Atlantis keys are memcomparable and hierarchical (the encoding is owned by
[04-storage-engine.md](04-storage-engine.md)):

| Prefix | Contents | Locality consequence |
|---|---|---|
| `/ns/{ns}/db/{db}/tb/{tb}/r/{key}` | Records | A table is a contiguous range; scans of one table touch few regions |
| `.../g/{from}/{dir}/{edgeTable}/{edgeKey}` | Adjacency entries | All edges of one node are adjacent, so one-hop expansion is a short range scan |
| `.../ix/{index}/{encodedValues}/{key}` | Secondary indexes | Index range scans are contiguous |
| `.../ft/{index}/...`, `.../vx/{index}/...` | Full-text postings, HNSW graph and vectors | Each index is its own range |
| `.../cf/{tb}/{versionstamp}` | Change feed | Appends at the end of a range |
| `.../kv/d/{key}`, `.../kv/x/{expiresAt}/{key}` | Key-value data and TTL expiry queue | KV data is one ordered range per database, so prefix scans stay local; the expiry queue is time-ordered and has a hot tail like the change feed |
| `/sys/...` | Catalog, users, cluster membership, live-query registry | Small, hot, read through caches |

Two layout risks matter in a cluster:

- **Hot tails.** Monotonically increasing keys (change-feed versionstamps, time-series
  keys led by a timestamp) concentrate writes on the last region of a range. Time-series
  tables should lead their composite key with a series identifier, as in
  `KEY (sensor, ts)`, so writes spread across series (see
  [13-key-value-time-series.md](13-key-value-time-series.md)). The default ULID record
  key is time-ordered too, but its random suffix and per-table prefix limit the effect;
  tables with very high insert rates can use UUID keys instead.
- **Supernodes.** A node with millions of edges has a long adjacency range that may span
  regions. Traversals still work, but they become multi-region scans.

## Transaction Model over TiKV (Planned — Phase 7)

Atlantis transactions in cluster mode use TiKV's optimistic transaction protocol, a
Percolator-style two-phase commit with timestamps from PD:

1. **Begin.** The node obtains a start timestamp from PD. All reads in the transaction
   see the snapshot at that timestamp.
2. **Execute.** Reads go to TiKV at the start timestamp; writes are buffered on the
   compute node.
3. **Prewrite.** On commit, the node writes locks and data for every buffered key, with
   one key chosen as the primary. A key written by another transaction after the start
   timestamp is a conflict.
4. **Commit.** The node obtains a commit timestamp and commits the primary key. That
   single write is the commit point; secondary keys are committed asynchronously and
   resolved by readers if a node dies mid-way.

Mapping to the Atlantis transaction contract in
[05-transactions-consistency.md](05-transactions-consistency.md):

| Contract | In cluster mode |
|---|---|
| Snapshot isolation (default) | Native: start-timestamp snapshot plus write-write conflict detection at prewrite |
| Serializable (optional) | The node also records its read set and validates it before commit, failing with `TxConflict` if any read key changed; more expensive across regions |
| Conflict | `TxConflict`, retried by the SDK `transaction(...)` helpers (see [19-client-sdks.md](19-client-sdks.md)) |
| Durability | A commit is acknowledged after Raft replication to a majority of the primary key's region; the RocksDB `storage.sync` setting does not apply |
| Versionstamps for change feeds | Derived from the commit timestamp, so they are globally ordered |

The single-node **commit sequencer** of embedded and server mode (see
[16-engine-runtime.md](16-engine-runtime.md)) does not exist in cluster mode. Ordering
comes from PD timestamps, and conflict detection from TiKV.

Large transactions are bounded by TiKV's transaction size limits. Bulk loads such as
`atlantis import` therefore commit in batches, which the Cypher-script export format
already does (see [17-server-protocols.md](17-server-protocols.md)).

## Cluster Membership (Planned — Phase 7)

Compute nodes coordinate only through KV. There is no separate coordination service.

- On start, a node writes a registration record under `/sys/cluster/nodes/{nodeId}` with
  its address, version and start time.
- It refreshes a heartbeat timestamp in that record at a fixed interval.
- A node whose heartbeat is older than the expiry window is considered dead. Any live
  node may garbage-collect a dead node's registration and the state it owned (live-query
  registrations, notification queue).
- Background work that must run once per cluster (change-feed cleanup, time-series
  retention and downsampling, index builds) is claimed through a lease record under
  `/sys/cluster/leases/{task}`, written transactionally. A node holds a lease only while
  its heartbeat is current; if it dies, another node takes the lease after expiry.

Leases rely on transactional compare-and-set, not on clocks agreeing. The expiry window
is chosen well above expected clock skew and pause times.

## Distributed Live Queries (Planned — Phase 7)

A client subscribes on one node, but the matching write may commit on any other node.

1. **Registration.** `LIVE MATCH ...` on node A writes the live query, with its filter,
   subscriber identity and owning node, to `/sys/...` under the database, in the same
   transaction that returns the live id.
2. **Matching after commit.** Every node keeps a cached copy of the live-query registry
   per table, refreshed when the registry's version changes. Events have already run
   inside the write's transaction. Only after node B's write to table `Order` commits
   does B evaluate the registered filters and permission predicates for that table
   against the changed records, so no notification describes an uncommitted change.
3. **Fan-out.** For each match, B appends a notification, tagged with the commit
   versionstamp, to the owning node's **notification queue** under
   `/sys/cluster/nodes/{nodeId}/...` in a separate transaction.
4. **Delivery.** Node A drains its queue in versionstamp order and moves each
   notification into the in-memory, bounded queue of its live query. It pushes
   `{result: {id, action, record}}` messages to the subscriber's socket, then deletes the
   delivered entries. If a live query's bounded queue overflows because the subscriber
   is too slow, A kills that live query, and the client catches up through `db.changes`.

If node A dies, its sockets close and its clients reconnect to another node, which
re-registers their live queries (see the reconnect behaviour in
[19-client-sdks.md](19-client-sdks.md)). The dead node's queue and registrations are
garbage-collected. If B crashes after commit but before fan-out, those notifications are
lost while the write itself stays committed. Notifications are therefore delivered at
most once per registration; consumers that need every change use change feeds (`db.changes`), which are durable and replayable
(see [14-live-queries-events.md](14-live-queries-events.md)).

Matching on the writing node adds work to every write to a table with live queries. The
per-table registry keeps tables without subscribers free of that cost.

## Index Caches (Planned — Phase 7)

All index data is stored in KV, so every node can read it. Some indexes are fast only
when held in memory:

| Index | Stored in KV | In-memory on each node |
|---|---|---|
| Secondary B-tree (`atlantis-index`) | `ix/` entries | None needed; range scans go to TiKV |
| HNSW (`atlantis-vector`) | `vx/` graph and vectors | Graph cache for search |
| Full-text (`atlantis-text`) | `ft/` postings and statistics | Hot postings and BM25 statistics |
| Adjacency (`atlantis-storage`) | `g/` entries | CSR adjacency cache |
| Spatial (`atlantis-geo`) | Cell entries | None needed |

Caches are kept correct with **version checks**. Each cached index records the version of
the index metadata key it was built from. Writes that change the index bump that version
in the same transaction. Before a query uses a cache, the node compares versions:

- equal: use the cache;
- newer in KV and the change log since the cached version is short: apply the delta;
- otherwise: mark the cache stale, serve the query from KV directly (slower, still
  correct), and rebuild the cache lazily in the background.

Correctness never depends on the cache: a cache miss or a stale cache costs latency, not
wrong results. Within a transaction, the engine also overlays the transaction's own
uncommitted index changes, so a transaction sees its own writes.

Large HNSW indexes are the most expensive cache to rebuild. Nodes can be configured to
warm specific indexes at startup, and deployments with very large vector sets may dedicate
a subset of nodes, behind a separate load-balancer pool, to search-heavy traffic so that
not every node must hold every graph.

## Scaling (Planned — Phase 7)

| Bottleneck | Scale by |
|---|---|
| Query CPU, connections, live-query sockets | More `atlantis` nodes |
| Storage capacity, write throughput, read I/O | More TiKV stores; PD rebalances regions |
| Timestamp allocation | PD batches timestamps; rarely a limit |
| Single hot record or hot key range | Data-model change (spread keys, avoid counters on one record) |
| Memory for vector and full-text caches | Larger nodes, or routing search traffic to dedicated nodes |

Contended single records (a global counter, a popular post's like count) serialize under
any optimistic protocol and will produce `TxConflict` retries. Model such values as
per-writer records aggregated at read time, or as time-series rows.

## Multi-Region Notes (Planned — Phase 7)

TiKV placement rules decide where replicas live. Atlantis adds no multi-region logic of
its own; the notes below describe the expected trade-offs.

- **Single region, multiple zones** is the recommended starting point: replicas across
  three availability zones, compute nodes in every zone.
- **Stretched across regions:** every commit waits for a cross-region Raft majority and
  every transaction needs PD timestamps, so write latency rises to inter-region round
  trips. Reads can be served by follower or stale reads where the application accepts
  bounded staleness; this would be exposed as an explicit read option, never a default.
- **Data residency:** placement rules can pin key ranges, such as one namespace's
  `/ns/{ns}/...` prefix, to stores in a given region. Because namespaces are key
  prefixes, tenant-per-namespace designs map naturally onto residency rules.

Active-active replication between independent clusters is not planned.

## Failure Modes (Planned — Phase 7)

| Failure | Effect | Recovery |
|---|---|---|
| Compute node crash | Its sessions, open transactions and live-query sockets are lost; committed data is safe | Clients reconnect elsewhere; SDKs re-register live queries; dead-node state is garbage-collected after heartbeat expiry |
| Node dies mid-commit | Primary key committed or not decides the outcome | Readers resolve leftover locks via the primary; no partial transaction becomes visible |
| TiKV store loss (minority of a region's replicas) | Brief latency spike during Raft leader election | TiKV re-replicates; no Atlantis action |
| Region loses majority | Keys in that region unavailable; affected statements fail with a storage error | Restore TiKV quorum; Atlantis nodes recover automatically |
| PD unavailable | No new transactions can start | Restore PD quorum |
| Network partition between nodes and storage | Partitioned nodes fail requests; `GET /health` returns `503` | Load balancer drains them via health checks |
| Stale index cache | Slower queries from KV | Background rebuild |

`GET /health` reports `503` whenever the node cannot reach TiKV, so load balancers stop
routing to it (see [17-server-protocols.md](17-server-protocols.md)).

## Operations (Planned — Phase 7)

### Rolling Upgrades

Because compute nodes are stateless, upgrades replace nodes one at a time: drain a node
from the load balancer, stop it, start the new version, wait for `atlantis isready`, and
continue. To make this safe:

- each node records its version in `/sys/cluster/nodes/{nodeId}`;
- a new version must read and write the storage format of the previous version;
- storage-format changes are switched on only after every registered node runs a version
  that understands them, so a cluster never mixes incompatible writers;
- `atlantis upgrade tikv://...` performs that switch explicitly, and refuses while older
  nodes are still registered.

TiKV and PD are upgraded with their own rolling procedures.

### Backup and Restore

`atlantis backup` and `atlantis restore` do not copy TiKV data themselves in cluster
mode. Physical backups use TiKV's backup and restore tooling, which produces a consistent
snapshot at a timestamp across all regions. Logical, per-database backups use
`atlantis export` and `atlantis import`, which work identically in every mode (see
[17-server-protocols.md](17-server-protocols.md)). After a physical restore, nodes rebuild
their index caches lazily.

### Monitoring

Each node exposes `GET /metrics` (see [17-server-protocols.md](17-server-protocols.md)).
In a cluster, the most useful Atlantis signals are `TxConflict` rate, KV request latency,
notification-queue depth per node, and index-cache rebuilds. TiKV and PD export their own
metrics for region health, Raft and storage.

## Future Option: Native Raft Backend (not committed)

A built-in Raft-replicated backend, letting a small cluster run from `atlantis` binaries
alone without a separate TiKV and PD deployment, is a possible future option. It is not
part of the roadmap. If pursued, it would be another `atlantis-kv` backend behind the
same `KvStore`/`Transaction` traits, and nothing above `atlantis-kv` would change.

## Design Decisions

- **Reuse TiKV instead of building consensus.** Distributed transactions, Raft, region
  splitting and rebalancing are years of work; TiKV provides them behind an interface that
  matches `atlantis-kv`.
- **Stateless compute.** Simplifies scaling, upgrades and failure handling; the cost is
  that in-memory accelerators are caches that must be rebuilt and version-checked.
- **Coordination through KV.** Membership, leases and live-query fan-out use the same
  transactional store, so no extra coordination service is needed.
- **Notifications after commit, change feeds for completeness.** Fan-out runs after
  commit and never slows or aborts the write. The price is at-most-once live delivery,
  and `db.changes` covers that gap.

## What's Next

- [04-storage-engine.md](04-storage-engine.md) — `atlantis-kv`, key encoding, backends
- [05-transactions-consistency.md](05-transactions-consistency.md) — isolation and conflicts
- [14-live-queries-events.md](14-live-queries-events.md) — live-query semantics
- [22-configuration.md](22-configuration.md) — cluster configuration
- [25-implementation-roadmap.md](25-implementation-roadmap.md) — Phase 7 scope
