# Multi-Cluster / Geo-Replication Design Document

**Status:** Proposal  
**Author:** Design session  
**Date:** 2026-04-05  
**Crate scope:** `orlando-cluster`, `orlando-core`, `orlando-persistence`

---

## Table of Contents

1. [Problem Statement](#1-problem-statement)
2. [How Orleans Does It](#2-how-orleans-does-it)
3. [Three Options for Orlando](#3-three-options-for-orlando)
4. [Recommendation](#4-recommendation)
5. [Implementation Phases](#5-implementation-phases)
6. [Key Types and Traits](#6-key-types-and-traits)
7. [Open Questions](#7-open-questions)

---

## 1. Problem Statement

Orlando today operates as a single cluster: a set of silos in the same failure
domain that share one `HashRing`, one SWIM membership protocol, and one
`GrainDirectory`. Every grain has exactly one activation somewhere in the
cluster. This is correct and sufficient for many workloads, but it breaks down
under three real-world pressures.

### 1.1 Latency

A single-cluster deployment in `us-east-1` means that every grain call from a
user in Tokyo traverses the Pacific Ocean twice (request + response). At ~150ms
round-trip latency per ocean crossing, even a simple two-hop grain interaction
costs 300ms of network overhead before any business logic runs. For interactive
applications (chat, gaming, collaborative editing), this is unacceptable.

Multi-cluster lets you place grain activations geographically close to the
users that call them most frequently. A grain activated in `ap-northeast-1`
handles Tokyo users at single-digit millisecond latency while the same grain
type serves US users from `us-east-1`.

### 1.2 Data Residency and Sovereignty

GDPR (EU), LGPD (Brazil), PIPL (China), and similar regulations require that
certain categories of personal data never leave a geographic jurisdiction.
A single cluster deployed in one region cannot satisfy these requirements.

Multi-cluster enables data-pinned grains: a `UserProfileGrain` for an EU user
is activated exclusively in an EU cluster, and its persisted state never
replicates outside the EU. The system must enforce this at the placement layer,
not rely on application code to get it right.

### 1.3 Disaster Recovery and High Availability

A single cluster is a single failure domain. If the AWS region hosting the
cluster suffers an outage (which happens roughly once per year at major cloud
providers), all grain activations are lost and the system is down until the
region recovers.

Multi-cluster with state replication enables failover: when the primary cluster
for a grain goes down, a secondary cluster can promote itself, restore the
last-replicated state, and resume serving requests. The trade-off is between
how much data loss is tolerable (RPO) and how quickly the failover happens
(RTO).

### 1.4 What "Multi-Cluster" Means in This Document

A **cluster** is a set of silos that share a SWIM membership protocol, a
`HashRing`, and a `GrainDirectory`. Clusters operate independently: they have
separate failure detection, separate rebalancing, and separate placement
decisions.

**Multi-cluster** is the layer above: a set of clusters that are aware of each
other and cooperate on grain placement, state replication, or both. The
communication between clusters crosses geographic boundaries and carries
fundamentally higher latency than intra-cluster communication.

```
                     +-----------+
                     | Clusters  |
                     | Registry  |
                     +-----+-----+
                           |
              +------------+------------+
              |            |            |
         +----+----+  +----+----+  +----+----+
         | us-east |  | eu-west |  | ap-ne-1 |
         |  Silo A |  |  Silo D |  |  Silo G |
         |  Silo B |  |  Silo E |  |  Silo H |
         |  Silo C |  |  Silo F |  |         |
         +---------+  +---------+  +---------+
```

Each box above is a full Orlando cluster with its own `HashRing` and SWIM
protocol. The "Clusters Registry" is the new component this design introduces.

---

## 2. How Orleans Does It

Orleans implemented multi-cluster support in version 2.x and iterated on it
through 3.x. Understanding their design decisions, what worked, and what they
deprecated is critical before charting Orlando's path.

### 2.1 Two Activation Policies

Orleans introduced two policies at the grain class level:

#### GlobalSingleInstance (GSI)

- Exactly one activation of a grain exists across all clusters, globally.
- Every cluster maintains a **registration entry** in a shared
  multi-cluster-aware grain directory.
- When a grain is accessed in a cluster where it is not currently activated,
  the system consults the global directory. If an activation exists elsewhere,
  the request is forwarded cross-cluster. If not, the local cluster activates
  it and registers itself as owner.
- **Conflict resolution:** If two clusters simultaneously try to activate the
  same grain (split-brain scenario), Orleans uses a **log-view consistency
  model** where the shared storage acts as the arbiter. The first registration
  to persist wins; the loser deactivates its duplicate activation and retries.
- **Pros:** Strong consistency, simple mental model, no state divergence.
- **Cons:** Cross-region latency for every non-local access. Under partition,
  the "ownership check" may stall, causing activation delays or brief
  unavailability.

#### OneInstancePerCluster (OIPC)

- Each cluster is allowed to have its own independent activation of the same
  grain.
- There is no global directory coordination; each cluster manages grains
  autonomously.
- State diverges across clusters and must be reconciled.
- Orleans used a **log-view consistency model** for reconciliation: state
  changes are written to a shared append-only log, and activations read from
  the log to merge divergent state.

### 2.2 Log-View Consistency Model

The heart of Orleans' multi-cluster state reconciliation is the **log-view
provider**, a pluggable abstraction built on top of the persistence layer.

The model works as follows:

1. Each grain activation reads from and writes to a shared log (not the grain's
   mailbox channel -- this is a persistence-layer log).
2. Writes append **events** (deltas) rather than full state snapshots.
3. On activation (or periodically), a grain reads all unread log entries and
   **folds** them into its local state.
4. The log provider supports pluggable backends: Azure Table Storage,
   DynamoDB, custom implementations.

The key insight is that this separates the **consistency mechanism** (the log)
from the **conflict resolution strategy** (the fold function). Grain authors
choose what happens when concurrent writes are merged.

In practice, this is a form of **event sourcing with multi-master writes**,
which only works well when the fold function is commutative, associative, and
idempotent (i.e., when the state is a CRDT or can be made to behave like one).

### 2.3 Multi-Cluster Gossip Channels

Orleans introduced a dedicated gossip channel between clusters, separate from
the intra-cluster SWIM protocol. This channel carried:

- **Cluster status updates:** which clusters are alive, which are draining.
- **Grain directory hints:** optional hints about where a grain is activated,
  to reduce cross-cluster lookups.
- **Configuration changes:** cluster membership (add/remove entire clusters).

The gossip channel used a dedicated gRPC service with its own heartbeat
interval tuned for cross-region latency (typically 10-30 seconds, vs.
1-2 seconds for intra-cluster SWIM).

### 2.4 What Orleans Deprecated and Why

Orleans significantly reworked multi-cluster in the transition from 2.x to 3.x,
and by Orleans 7.x (the current stable release as of this writing), the
multi-cluster feature is in maintenance mode with limited investment. Key
lessons:

1. **Log-view providers were too complex.** The abstraction required grain
   authors to understand event sourcing, commutative operations, and the
   subtleties of eventual consistency. Most application developers wanted
   simpler guarantees. The log-view became a "power user" feature that few
   teams used correctly.

2. **OneInstancePerCluster was rarely the right choice.** Most applications
   either needed strong consistency (GSI) or were better served by external
   CRDT databases (Redis CRDT, CockroachDB). The middle ground of "you write
   your own merge function" was error-prone.

3. **The global directory for GSI was a bottleneck.** Relying on shared
   storage (e.g., Azure Table Storage) for cross-cluster grain registration
   introduced a dependency on a single storage system's availability and
   latency. If the storage was slow or unavailable, grain activation stalled
   globally.

4. **Gossip at the cluster level added operational complexity.** Operators had
   to configure and monitor two separate gossip systems (intra-cluster SWIM +
   inter-cluster gossip), with different health checks and failure modes.

5. **The Orleans team recommended Kubernetes-native solutions.** For many
   deployments, Kubernetes federation plus external load balancing (e.g.,
   Istio, Linkerd) replaced the need for framework-level multi-cluster. The
   framework moved toward being a better single-cluster solution and letting
   infrastructure handle cross-region routing.

The takeaway: multi-cluster is genuinely useful, but Orleans' approach was
over-engineered for what most users needed. A simpler model with fewer
knobs would serve 90% of use cases.

---

## 3. Three Options for Orlando

### 3.1 Option A: Global Single Instance (GSI)

**Summary:** One activation per grain across all clusters. If the grain is
not local, the request is forwarded to the owning cluster.

#### How It Works

```
User (Tokyo) ──> ap-ne-1 cluster ──> "CounterGrain/room-42 is on us-east"
                                          │
                                          ▼  (cross-cluster gRPC)
                                     us-east cluster
                                     CounterGrain/room-42
                                          │
                                          ▼
                                     response
                                          │
User (Tokyo) <── ap-ne-1 cluster <────────┘
```

1. Each cluster maintains its own `HashRing` for intra-cluster placement as
   today.
2. A new **cluster-level directory** maps `GrainId -> ClusterId`. This is
   either stored in shared external storage (e.g., Redis, PostgreSQL) or
   maintained via inter-cluster gossip.
3. When a grain is first accessed, the local cluster checks the cluster-level
   directory:
   - If the grain is registered to another cluster, forward the request.
   - If the grain is not registered anywhere, activate it locally and register.
   - If the directory is unavailable, activate locally (availability over
     consistency fallback).
4. Conflict resolution: if two clusters register simultaneously, a compare-and-
   swap (CAS) operation on the shared storage picks the winner. The loser
   deactivates its activation.

#### Pros

- **Simple consistency model.** One activation, one state, no merge logic.
- **Minimal changes to the grain programming model.** Grain authors write
  handlers exactly as they do today.
- **No new traits for grain state.** The persistence layer works unchanged.
- **Straightforward implementation path.** Most of the work is in routing and
  the cross-cluster directory, not in new grain semantics.

#### Cons

- **Cross-region latency for every non-local grain call.** A Tokyo user
  calling a US-based grain pays ~300ms round-trip, every time.
- **Single point of failure per grain.** If the owning cluster goes down, the
  grain is unavailable until the directory entry expires and another cluster
  claims it.
- **Directory as a bottleneck.** The cluster-level directory becomes a critical
  shared resource. Its availability directly determines grain activation
  availability.
- **Partition sensitivity.** During a network partition between clusters, the
  directory may be unreachable, causing activation storms or split-brain.

#### Implementation Scope

| Component | Work |
|-----------|------|
| `ClusterId` type | New type in `orlando-core` (~30 LOC) |
| `ClusterDirectory` trait + Redis/Postgres impl | New trait in `orlando-cluster` (~400 LOC) |
| Cross-cluster gRPC forwarding | Extend `GrainTransportService::invoke` (~200 LOC) |
| `ClusterSiloBuilder` changes | Add cluster registry config (~100 LOC) |
| Proto changes | New `ClusterDirectory` service (~50 LOC proto, ~200 LOC Rust) |
| **Total** | **~1000 LOC** |

#### Estimated Effort

4-6 weeks for one developer, including tests and a working two-cluster
example.

---

### 3.2 Option B: Active-Passive with Async Replication

**Summary:** One cluster is the primary (write) owner for each grain. Secondary
clusters receive asynchronous state replication and can serve stale reads.
On primary failure, a secondary promotes itself.

#### How It Works

```
Primary (us-east)                  Secondary (eu-west)
┌──────────────────┐              ┌──────────────────┐
│ CounterGrain/42  │──replicate──>│ CounterGrain/42  │
│ state: { n: 15 } │              │ state: { n: 14 } │  (one event behind)
│ [accepts writes] │              │ [read-only copy] │
└──────────────────┘              └──────────────────┘

Write from EU user:
  eu-west ──forward write──> us-east ──execute──> replicate──> eu-west

Read from EU user:
  eu-west ──serve locally from replica──> (fast, possibly stale)
```

1. Each grain has a **primary cluster** determined by a placement policy
   (configurable: region pinning, hash-based, or manual override).
2. The primary cluster activates the grain normally and handles all writes.
3. After each state mutation (handler completion), the primary appends a
   **replication entry** to a replication log.
4. Secondary clusters consume the log asynchronously and update their local
   read-only copies of the grain state.
5. **Read requests** at a secondary can be served from the local copy (stale)
   or forwarded to the primary (consistent). This is a per-grain-type
   configuration.
6. **Write requests** at a secondary are always forwarded to the primary.
7. **Failover:** When the primary cluster is detected as down (via inter-
   cluster health checks), a secondary promotes itself:
   - Stop replication from the old primary.
   - Mark itself as the new primary in the cluster directory.
   - Resume accepting writes.
   - Begin replicating to remaining secondaries.

#### Replication Log Design

The replication log is an ordered sequence of `ReplicationEntry` values (see
Section 6.5) storing full-state snapshots or deltas per grain. Storage options:

- **Shared storage** (PostgreSQL, Redis Streams): simplest, delegates cross-
  region replication to the database.
- **Direct gRPC streaming:** lower latency, more complex failure handling.

#### Pros

- **Simple consistency for writers.** Single-writer semantics: no merge
  conflicts, no CRDTs, no event sourcing. The primary is authoritative.
- **Good read latency.** Secondaries serve stale reads locally. For read-heavy
  workloads (which most grain workloads are), this is a significant win.
- **Familiar model.** Active-passive replication is well-understood by
  operators. PostgreSQL streaming replication, MySQL replication, and Redis
  Sentinel all work this way.
- **Incremental adoption.** Grains that do not need multi-cluster can be left
  unchanged. Only grains that opt in (by implementing a replication trait)
  participate.

#### Cons

- **Write latency for non-primary regions.** Writes from a secondary must
  cross regions to the primary, same as Option A.
- **Failover is not instant.** Detecting primary failure takes time (seconds
  to tens of seconds). During failover, writes to affected grains are
  unavailable.
- **Stale reads.** Secondaries serve state that may be one or more
  replication entries behind. For some grains (e.g., account balance), this
  is unacceptable.
- **Replication lag monitoring.** Operators must monitor replication lag per
  grain per cluster. If the secondary falls too far behind, a failover may
  lose significant state.
- **Primary election.** When the primary fails, secondaries must agree on who
  becomes the new primary. This requires a consensus mechanism (or external
  coordination via shared storage).

#### Implementation Scope

| Component | Work |
|-----------|------|
| `ReplicationEntry` types | New types in `orlando-core` (~80 LOC) |
| `ReplicationLog` trait + storage backends | New module in `orlando-persistence` (~600 LOC) |
| Replication producer (primary side) | Hook into mailbox loop after handler (~300 LOC) |
| Replication consumer (secondary side) | New task per replicated grain (~400 LOC) |
| `ReplicatedGrain` trait | New trait in `orlando-core` (~60 LOC) |
| Failover detector + promotion protocol | New module in `orlando-cluster` (~500 LOC) |
| Cross-cluster health check service | gRPC service + proto (~300 LOC) |
| Read routing (stale vs. forwarded) | Extend `ClusterGrainRef` (~200 LOC) |
| **Total** | **~2500 LOC** |

#### Estimated Effort

8-12 weeks for one developer, including a replication backend (PostgreSQL or
Redis Streams), a failover protocol, and a multi-cluster integration test.

---

### 3.3 Option C: Active-Active with CRDTs

**Summary:** Each cluster independently activates its own instance of a grain.
State changes replicate to all clusters asynchronously. Conflicts are resolved
automatically using CRDT (Conflict-free Replicated Data Type) semantics.

#### How It Works

```
us-east                         eu-west                        ap-ne-1
┌────────────────┐             ┌────────────────┐             ┌────────────────┐
│ Counter/42     │             │ Counter/42     │             │ Counter/42     │
│ GCounter {     │<--merge---->│ GCounter {     │<--merge---->│ GCounter {     │
│  us: 5, eu: 0 │             │  us: 3, eu: 7  │             │  us: 5, eu: 7  │
│  ap: 0        }│             │  ap: 0        }│             │  ap: 3        }│
│ value() = 5    │             │ value() = 10   │             │ value() = 15   │
└────────────────┘             └────────────────┘             └────────────────┘
       │                              │                              │
       └──────────────────────────────┼──────────────────────────────┘
                                      │
                            (async replication, merges)
```

1. Each cluster has its own activation of the grain. There is no global
   single instance.
2. The grain's state type must implement a `Crdt` trait that defines a
   deterministic `merge` operation.
3. After each handler invocation, the local activation broadcasts its state
   delta (or full state) to all other clusters.
4. On receiving a remote state update, the local activation calls `merge`
   to incorporate the remote changes.
5. CRDTs guarantee that all replicas converge to the same state regardless
   of message ordering or delivery timing.

#### CRDT Types That Would Be Supported

| CRDT Type | Use Case | Grain Example |
|-----------|----------|---------------|
| GCounter | Monotonic count | Page views, likes |
| PNCounter | Inc/dec counter | Inventory, balance |
| GSet / ORSet | Set operations | Tags, shopping cart |
| LWWRegister | Last-writer-wins | User profile fields |
| MVRegister | Multi-value concurrent writes | Collaborative edits |

See Section 6.9 for the `Crdt` trait definition.

#### Pros

- **Low latency everywhere.** Every region activates and serves grains
  locally. No cross-region forwarding for reads or writes.
- **No single point of failure.** Every cluster is autonomous. If one cluster
  goes down, the others continue operating without interruption or failover
  protocol.
- **Partition tolerant.** During a network partition between clusters, each
  cluster continues to serve requests. When connectivity restores, states
  merge.
- **No coordination required.** No distributed consensus, no leader election,
  no global directory. This is the simplest operationally at the cluster
  level.

#### Cons

- **Complex for grain authors.** Not all state naturally fits a CRDT. A bank
  account balance cannot be a simple counter (what if the balance goes
  negative?). Grain authors must understand CRDT semantics and choose
  appropriate types.
- **Eventual consistency only.** Reads at different clusters may return
  different values until replication converges. There is no way to get a
  globally consistent read without coordination (which defeats the purpose).
- **Limited state types.** Grains with complex state (nested structs, graphs,
  arbitrary business logic) may not be expressible as CRDTs without
  significant refactoring.
- **Replication overhead.** Full-state CRDTs (like ORSet with tombstones) can
  grow without bound. Delta-state CRDTs help, but add implementation
  complexity.
- **Debugging is hard.** When state diverges unexpectedly, tracing the cause
  through concurrent merge operations across clusters is non-trivial.
- **Fundamental constraint: changes the grain programming model.** Handlers
  can no longer think in terms of "read state, modify state, done." They
  must think in terms of "apply a CRDT operation that will eventually merge
  with concurrent operations from other clusters."

#### Implementation Scope

| Component | Work |
|-----------|------|
| `Crdt` trait | New trait in `orlando-core` (~40 LOC) |
| Built-in CRDT types (GCounter, PNCounter, GSet, ORSet, LWWRegister) | New module `orlando-crdt` (~1500 LOC) |
| `CrdtGrain` trait (extends `Grain` with merge semantics) | New trait in `orlando-core` (~80 LOC) |
| Replication broadcaster (push state to peers) | New task in `orlando-cluster` (~400 LOC) |
| Replication receiver (receive + merge) | Hook into mailbox loop (~300 LOC) |
| Inter-cluster replication gRPC service | Proto + Rust (~400 LOC) |
| Merge injection into mailbox loop | Extend mailbox loop to accept merge messages (~200 LOC) |
| Cluster registry (which clusters exist) | Shared with Options A/B (~200 LOC) |
| **Total** | **~3100 LOC** |

#### Estimated Effort

12-16 weeks for one developer, including the CRDT library, replication
protocol, and at least three CRDT grain examples.

---

### 3.4 Comparison Matrix

| Criterion | A: Global Single Instance | B: Active-Passive | C: Active-Active CRDT |
|-----------|---------------------------|--------------------|-----------------------|
| **Read latency** | High (forwarded) | Low (stale local) | Low (local) |
| **Write latency** | High (forwarded) | High (forwarded to primary) | Low (local) |
| **Consistency** | Strong | Eventual (reads), Strong (writes) | Eventual |
| **Grain author complexity** | None | Low (opt-in trait) | High (CRDT design) |
| **Operator complexity** | Medium (directory) | Medium (replication monitoring) | Low (no coordination) |
| **Partition tolerance** | Poor | Medium (primary available) | Excellent |
| **Failover** | Re-register in directory | Promotion protocol | Not needed |
| **State types** | Any | Any | CRDT-compatible only |
| **Implementation effort** | ~1000 LOC / 4-6 weeks | ~2500 LOC / 8-12 weeks | ~3100 LOC / 12-16 weeks |
| **Risk** | Low | Medium | High |

---

## 4. Recommendation

**Implement Option B (Active-Passive with Async Replication) first, with Option
A as a stepping stone and Option C as a future evolution.**

### Rationale

The three options form a natural progression, not a fork:

```
Option A (GSI)  ──>  Option B (Active-Passive)  ──>  Option C (Active-Active)
    │                       │                              │
    │  Cluster directory    │  + Replication log            │  + CRDT merge
    │  Cross-cluster gRPC   │  + Read replicas              │  + Multi-activation
    │  ~1000 LOC            │  + Failover                   │  + Convergence
    │                       │  ~1500 LOC incremental        │  ~600 LOC incremental
    └───────────────────────┴──────────────────────────────-┘
```

The cluster directory and cross-cluster gRPC transport from Option A are
prerequisites for both B and C. The replication log from Option B is a
prerequisite for C (CRDTs need a mechanism to ship state between clusters).

Therefore, the implementation path is:

1. **Build Option A first** (4-6 weeks). This gives us a working multi-cluster
   system with the simplest possible semantics. It is immediately useful for
   data residency use cases (pin a grain to a specific cluster, forward
   requests there).

2. **Layer Option B on top** (6-8 weeks incremental). Add replication and read
   replicas. This addresses the read latency problem without changing the
   consistency model for writes.

3. **Offer Option C as an opt-in advanced feature** (6-8 weeks incremental).
   For grains whose state is naturally CRDT-shaped, allow active-active
   operation. This is additive and does not change the default behavior.

### Why Not Start with C?

Active-active CRDTs solve the most problems but are the hardest to get right.
They require:
- A correct CRDT library (subtle edge cases in tombstone management, delta
  propagation, and clock drift).
- Grain authors to understand a fundamentally different programming model.
- Thorough testing of convergence under adversarial network conditions.

Starting with C would delay the first useful multi-cluster deployment by
months. Starting with A gives us value in weeks.

### Why Not Just A?

Option A alone does not solve the latency problem, which is the most common
motivation for multi-cluster. A system that forwards every non-local grain
call across the ocean is only marginally better than a single cluster with a
global load balancer. Option B (read replicas) is the minimum viable solution
for latency.

### The Recommended Default

- **Default grain behavior:** Global Single Instance (Option A). One
  activation, forwarded requests. No configuration needed.
- **Opt-in for read replicas:** Grains that implement `ReplicatedGrain` get
  async replication to secondary clusters, with configurable staleness
  tolerance.
- **Opt-in for active-active:** Grains that implement both `CrdtGrain` and
  have a `Crdt` state type get per-cluster activations with automatic merge.

This layering means that the simplest grains work correctly across clusters
with zero additional code, while advanced grains can opt into progressively
more sophisticated replication strategies.

---

## 5. Implementation Phases

### Phase 5.1: Cluster Identity and Discovery

**Duration:** 2 weeks  
**Crates affected:** `orlando-core`, `orlando-cluster`

#### Changes

1. **Add `ClusterId`** to `orlando-core` (see Section 6.1).
2. **Add `ClusterConfig` and `MultiClusterConfig`** to `orlando-cluster`
   (see Section 6.2).
3. **Extend `ClusterSiloBuilder`** with `.multi_cluster(config)` method.
4. **Inter-cluster health checking.** Background task pings peer clusters at
   `health_check_interval`, maintains `ClusterStatus` (`Healthy`, `Degraded`,
   `Unreachable`) per peer. Reuses existing `ConnectionPool`.

#### Files

| File | Action | Est. LOC |
|------|--------|----------|
| `crates/orlando-core/src/cluster_id.rs` | New | 30 |
| `crates/orlando-cluster/src/multi_cluster_config.rs` | New | 120 |
| `crates/orlando-cluster/src/cluster_health.rs` | New | 200 |
| `crates/orlando-cluster/src/cluster_silo.rs` | Modify | +60 |
| `crates/orlando-cluster/src/lib.rs` | Modify | +10 |
| `crates/orlando-cluster/proto/orlando.proto` | Modify | +20 |

#### Testing

- Unit tests: `ClusterId` equality, `MultiClusterConfig` construction.
- Integration test: two `ClusterSilo` instances in different `tokio::test`
  tasks, configured as peers, verifying health check success/failure.
- No real network needed: bind to `127.0.0.1` with different ports.

---

### Phase 5.2: Cross-Cluster Grain Directory (Option A Complete)

**Duration:** 3 weeks  
**Crates affected:** `orlando-cluster`, `orlando-persistence`

#### Changes

1. **`CrossClusterDirectory` trait** (see Section 6.3 for full definition).
   CAS semantics: `register` returns the actual owner (may differ from
   requested).

2. **In-memory implementation** (`DashMap<GrainId, ClusterId>`) for tests.

3. **PostgreSQL implementation.** `INSERT ON CONFLICT DO NOTHING RETURNING *`
   for CAS. Schema: `(grain_type, grain_key) -> cluster_id + epoch`.

4. **Redis implementation.** `SET grain:{type}/{key} NX EX ttl` for CAS with
   automatic TTL cleanup. Re-register periodically to maintain ownership.

5. **Cross-cluster forwarding.** Extend `GrainTransportService::invoke`:
   check directory before local activation. If another cluster owns the
   grain, forward to its gateway. If unowned, activate locally and register.

6. **`ClusterGateway` proto service** (see Section 6.8). Handles
   `ForwardInvoke`, `ClusterPing`, and `NotifyDrain`.

#### Files

| File | Action | Est. LOC |
|------|--------|----------|
| `crates/orlando-cluster/src/cross_cluster_directory.rs` | New | 100 (trait + in-memory) |
| `crates/orlando-cluster/src/directory_pg.rs` | New | 200 |
| `crates/orlando-cluster/src/directory_redis.rs` | New | 150 |
| `crates/orlando-cluster/src/cluster_gateway.rs` | New | 300 |
| `crates/orlando-cluster/src/transport.rs` | Modify | +100 |
| `crates/orlando-cluster/src/cluster_silo.rs` | Modify | +80 |
| `crates/orlando-cluster/proto/orlando.proto` | Modify | +25 |

#### Testing

- Unit tests: in-memory directory CAS semantics (concurrent register).
- Integration test: two clusters (different ports on localhost), grain
  accessed from non-owning cluster verifies forwarding.
- Test: simultaneous registration from two clusters, verify only one wins.
- Test: cluster A activates grain, cluster A goes down (simulated by
  `shutdown()`), cluster B re-registers and activates.

---

### Phase 5.3: Replication Log and Read Replicas (Option B)

**Duration:** 4 weeks  
**Crates affected:** `orlando-core`, `orlando-cluster`, `orlando-persistence`

#### Changes

1. **Define `ReplicatedGrain` and `ReplicationLog` traits** (see Sections 6.6
   and 6.7 for full definitions).

2. **PostgreSQL replication log backend.**

   ```sql
   CREATE TABLE replication_log (
       id BIGSERIAL PRIMARY KEY,
       grain_type TEXT NOT NULL, grain_key TEXT NOT NULL,
       sequence BIGINT NOT NULL, source_cluster TEXT NOT NULL,
       entry_type SMALLINT NOT NULL, payload BYTEA NOT NULL,
       created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
       UNIQUE (grain_type, grain_key, sequence)
   );
   ```

3. **Replication producer.** After handler completion on the primary, the
   mailbox loop serializes state and sends a `ReplicationEntry` to a
   background task (non-blocking, fire-and-forget) that appends to the log.

4. **Replication consumer.** Each secondary runs a background task per
   replicated grain that polls the log (or receives push via
   `LISTEN/NOTIFY`) and applies state updates locally.

5. **Read routing in `ClusterGrainRef`.** Introduce `ReadOnlyMessage` marker
   trait. Read-only messages served from local replica if staleness is within
   `max_staleness()`; otherwise forwarded to primary.

#### Files

| File | Action | Est. LOC |
|------|--------|----------|
| `crates/orlando-core/src/replication.rs` | New | 120 (types + traits) |
| `crates/orlando-persistence/src/replication_log.rs` | New | 100 (trait) |
| `crates/orlando-persistence/src/replication_pg.rs` | New | 250 |
| `crates/orlando-cluster/src/replication_producer.rs` | New | 200 |
| `crates/orlando-cluster/src/replication_consumer.rs` | New | 300 |
| `crates/orlando-cluster/src/cluster_grain_ref.rs` | Modify | +150 |
| `crates/orlando-core/src/message.rs` | Modify | +20 |

#### Testing

- Unit test: replication log append + read_from round-trip.
- Integration test: two clusters, primary writes, secondary reads.
  Use `tokio::time::pause()` to deterministically advance time and verify
  replication lag.
- Test: primary goes down, verify secondary has state up to last replicated
  sequence.
- Test: staleness threshold exceeded, verify secondary forwards to primary
  instead of serving stale data.

---

### Phase 5.4: Failover Protocol

**Duration:** 3 weeks  
**Crates affected:** `orlando-cluster`

#### Changes

1. **Failover state machine:** `Monitoring -> PrimaryUnreachable -> Promoting
   -> Promoted`. Grace period (default 30s) prevents premature promotion on
   transient blips.

2. **Promotion steps:** (a) re-verify primary unreachable, (b) CAS-register
   in directory (prevents two secondaries promoting simultaneously),
   (c) on success: activate grain, load state from last replicated entry,
   (d) on failure: stand down and follow the new primary.

3. **Fence tokens:** Each registration carries a monotonic epoch. Stale
   primaries' writes are rejected if their epoch is lower than the current
   directory epoch. Uses `GrainOwnership { cluster_id, epoch, registered_at }`.

4. **Drain notification:** Graceful shutdown sends `DrainNotification` to
   peers, triggering orderly promotion without waiting for the grace period.

#### Files

| File | Action | Est. LOC |
|------|--------|----------|
| `crates/orlando-cluster/src/failover.rs` | New | 400 |
| `crates/orlando-cluster/src/cross_cluster_directory.rs` | Modify | +60 (epochs) |
| `crates/orlando-cluster/src/cluster_health.rs` | Modify | +80 (failover trigger) |
| `crates/orlando-cluster/proto/orlando.proto` | Modify | +15 (drain notification) |

#### Testing

- Integration test: three clusters (A=primary, B=secondary, C=secondary).
  Shut down A. Verify exactly one of B or C promotes.
- Test: A goes down, B promotes, A comes back up. Verify A does not
  reclaim ownership (fenced by epoch).
- Test: graceful drain. A sends drain notification, B promotes without waiting
  for grace period.
- Test: network partition (simulated by dropping gRPC connections). A is alive
  but unreachable from B. B promotes. A's subsequent writes are rejected
  (stale epoch).
- All tests run on localhost with `tokio::time::pause()`.

---

### Phase 5.5: Data Residency Placement Constraints (Parallel Track)

**Duration:** 2 weeks (can run in parallel with Phases 5.3-5.4)  
**Crates affected:** `orlando-core`, `orlando-cluster`

This phase adds placement constraints that restrict which clusters a grain
can be activated in. It is independent of replication and can be implemented
as soon as Phase 5.2 (cross-cluster directory) is complete.

#### Changes

1. **Placement constraints on the `Grain` trait.**

   ```rust
   pub trait Grain: Send + 'static {
       // ... existing methods ...

       /// Clusters where this grain type may be activated.
       /// Returns `None` for no restriction (any cluster).
       /// Returns `Some(vec!["eu-west"])` to pin to EU only.
       fn allowed_clusters() -> Option<Vec<&'static str>> {
           None
       }
   }
   ```

2. **Enforcement in `CrossClusterDirectory`.**

   Before registering a grain, check `G::allowed_clusters()`. If the local
   cluster is not in the allowed list, reject the activation and forward
   to an allowed cluster.

3. **Enforcement in `ClusterGrainRef`.**

   When constructing a `ClusterGrainRef`, if the grain has cluster
   constraints, route to an allowed cluster immediately instead of activating
   locally.

#### Files

| File | Action | Est. LOC |
|------|--------|----------|
| `crates/orlando-core/src/grain.rs` (trait definition) | Modify | +15 |
| `crates/orlando-cluster/src/cross_cluster_directory.rs` | Modify | +30 |
| `crates/orlando-cluster/src/cluster_gateway.rs` | Modify | +40 |

#### Testing

- Test: grain with `allowed_clusters = ["eu-west"]` accessed from `us-east`.
  Verify request is forwarded to `eu-west`.
- Test: grain with `allowed_clusters = ["eu-west"]` accessed from `eu-west`.
  Verify local activation.
- Test: grain with no constraint. Verify normal behavior (activate anywhere).

---

## 6. Key Types and Traits

This section drafts the core abstractions that will be added across the
implementation phases. These are the public API surface that grain authors
and operators will interact with.

### 6.1 Cluster Identity

```rust
// crates/orlando-core/src/cluster_id.rs

/// Identifies a cluster within a multi-cluster deployment.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClusterId(pub String);

impl ClusterId {
    pub fn new(id: impl Into<String>) -> Self { Self(id.into()) }
    pub fn as_str(&self) -> &str { &self.0 }
}
```

### 6.2 Multi-Cluster Configuration

```rust
// crates/orlando-cluster/src/multi_cluster_config.rs

pub struct MultiClusterConfig {
    pub local: ClusterConfig,
    pub peers: Vec<PeerClusterEndpoint>,
    pub directory: DirectoryBackendConfig,
    pub health_check_interval: Duration,      // default: 10s
    pub failover_grace_period: Duration,      // default: 30s
}

pub struct ClusterConfig {
    pub cluster_id: ClusterId,
    pub region: Option<String>,               // e.g., "us-east-1"
    pub zone: Option<String>,                 // e.g., "us-east-1a"
}

pub struct PeerClusterEndpoint {
    pub cluster_id: ClusterId,
    pub gateway_endpoint: String,             // host:port of peer gateway
    pub region: Option<String>,
}

pub enum DirectoryBackendConfig {
    InMemory,                                 // testing only
    Postgres { connection_string: String },   // recommended for production
    Redis { url: String, ttl: Duration },     // alternative, TTL-based cleanup
}
```

### 6.3 Cross-Cluster Directory Trait

```rust
// crates/orlando-cluster/src/cross_cluster_directory.rs

/// Tracks which cluster owns each grain activation globally.
/// Implementations must provide CAS semantics: only one cluster can own
/// a grain at a time. Concurrent registrations resolved by first-writer-wins.
#[async_trait]
pub trait CrossClusterDirectory: Send + Sync + 'static {
    async fn lookup(&self, grain_id: &GrainId) -> Result<Option<GrainOwnership>, DirectoryError>;

    /// CAS register. Returns actual owner (may differ from requested if another cluster won).
    async fn register(&self, grain_id: &GrainId, cluster_id: &ClusterId, epoch: u64)
        -> Result<GrainOwnership, DirectoryError>;

    async fn deregister(&self, grain_id: &GrainId, cluster_id: &ClusterId)
        -> Result<(), DirectoryError>;

    /// Extend TTL. No-op for backends without TTL (e.g., PostgreSQL).
    async fn renew(&self, grain_id: &GrainId, cluster_id: &ClusterId)
        -> Result<(), DirectoryError> { Ok(()) }
}

pub struct GrainOwnership {
    pub cluster_id: ClusterId,
    pub epoch: u64,
    pub registered_at: SystemTime,
}

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("directory backend unavailable: {0}")]
    Unavailable(String),
    #[error("stale epoch: current is {current}, requested {requested}")]
    StaleEpoch { current: u64, requested: u64 },
    #[error("directory backend error: {0}")]
    Backend(String),
}
```

### 6.4 Cross-Cluster Transport Trait

```rust
// crates/orlando-cluster/src/cross_cluster_transport.rs

/// Transport for grain invocations between clusters. Separate from
/// intra-cluster GrainTransport due to different latency, auth, and
/// connection pooling characteristics.
#[async_trait]
pub trait CrossClusterTransport: Send + Sync + 'static {
    /// Forward a grain invocation to a peer cluster's gateway silo.
    async fn forward(&self, target: &ClusterId, req: CrossClusterInvokeRequest)
        -> Result<CrossClusterInvokeResponse, CrossClusterError>;

    /// Health check a peer cluster.
    async fn ping(&self, target: &ClusterId)
        -> Result<ClusterStatus, CrossClusterError>;
}

pub struct CrossClusterInvokeRequest {
    pub grain_type: String,
    pub grain_key: String,
    pub message_type: String,
    pub message_version: u32,
    pub payload: Vec<u8>,
    pub encoding: i32,
    pub source_cluster: ClusterId,
    pub epoch: u64,
    pub request_context: HashMap<String, String>,
}

pub struct CrossClusterInvokeResponse {
    pub payload: Vec<u8>,
    pub error: String,
    pub encoding: i32,
}

pub enum ClusterStatus {
    Healthy { cluster_id: ClusterId, active_grains: u32, silo_count: u32 },
    Draining { cluster_id: ClusterId },
}

#[derive(Debug, thiserror::Error)]
pub enum CrossClusterError {
    #[error("peer cluster {0} is unreachable")]
    Unreachable(ClusterId),
    #[error("connection to cluster {0} failed: {1}")]
    Connection(ClusterId, String),
    #[error("invocation on cluster {0} failed: {1}")]
    Invocation(ClusterId, String),
    #[error("stale epoch for grain {grain_type}/{grain_key}: {details}")]
    StaleEpoch { grain_type: String, grain_key: String, details: String },
}
```

### 6.5 Replication Types

```rust
// crates/orlando-core/src/replication.rs

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplicationEntry {
    pub grain_id: GrainId,
    pub sequence: u64,                    // monotonic per grain
    pub timestamp: SystemTime,
    pub source_cluster: ClusterId,
    pub entry_type: ReplicationEntryType,
    pub payload: Vec<u8>,                 // serialized state
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplicationEntryType {
    FullState,    // complete snapshot, replaces replica state entirely
    Delta,        // incremental change (future optimization)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplicationMode {
    Immediate,                            // replicate after every handler
    Batched { interval: Duration },       // batch at fixed interval
}
```

### 6.6 Replication Log Trait

```rust
// crates/orlando-persistence/src/replication_log.rs

/// Append-only log of state changes per grain. Primary appends after handler
/// invocations; secondaries read to maintain replicas. Must be durable.
#[async_trait]
pub trait ReplicationLog: Send + Sync + 'static {
    /// Append entry, returns assigned sequence number. Sequences must be
    /// strictly monotonic per grain.
    async fn append(&self, entry: ReplicationEntry) -> Result<u64, ReplicationError>;

    /// Read entries after a sequence number, capped by limit.
    async fn read_from(&self, grain_id: &GrainId, after_sequence: u64, limit: usize)
        -> Result<Vec<ReplicationEntry>, ReplicationError>;

    /// Latest sequence number for a grain (0 if none).
    async fn latest_sequence(&self, grain_id: &GrainId) -> Result<u64, ReplicationError>;

    /// Delete entries before a sequence. Returns count deleted.
    async fn truncate(&self, grain_id: &GrainId, before_sequence: u64)
        -> Result<u64, ReplicationError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ReplicationError {
    #[error("replication log backend unavailable: {0}")]
    Unavailable(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("deserialization error: {0}")]
    Deserialization(String),
    #[error("sequence conflict: expected {expected}, got {actual}")]
    SequenceConflict { expected: u64, actual: u64 },
    #[error("replication backend error: {0}")]
    Backend(String),
}
```

### 6.7 Replicated Grain Trait

```rust
// crates/orlando-core/src/replicated_grain.rs

/// Marker trait for grains with state replicated across clusters.
/// Primary handles writes; secondaries maintain read-only replicas.
///
/// Usage: implement PersistentGrain + ReplicatedGrain, then optionally
/// mark read-only messages with ReadOnlyMessage for local serving.
pub trait ReplicatedGrain: crate::Grain
where
    Self::State: Serialize + DeserializeOwned,
{
    /// Max replication lag for local reads. ZERO = always forward,
    /// MAX = always serve stale. Default: 5 seconds.
    fn max_staleness() -> Duration { Duration::from_secs(5) }

    /// How state ships to secondaries. Default: after every handler.
    fn replication_mode() -> ReplicationMode { ReplicationMode::Immediate }

    /// Periodic full-state snapshot interval. Default: 60 seconds.
    fn snapshot_interval() -> Duration { Duration::from_secs(60) }
}
```

### 6.8 Proto Changes

New services and messages added to `orlando.proto`:

```protobuf
// Cross-cluster gateway. Each cluster exposes this to peer clusters.
service ClusterGateway {
  rpc ForwardInvoke(CrossClusterInvokeRequest) returns (CrossClusterInvokeResponse);
  rpc ClusterPing(ClusterPingRequest) returns (ClusterPingResponse);
  rpc NotifyDrain(DrainNotification) returns (DrainAck);
}

// Replication stream from primary to secondary clusters.
service ReplicationService {
  rpc Subscribe(ReplicationSubscribeRequest) returns (stream ReplicationEntryProto);
}

// Key new messages (abbreviated):
message CrossClusterInvokeRequest {
  string grain_type = 1; string grain_key = 2; string message_type = 3;
  uint32 message_version = 4; bytes payload = 5; int32 encoding = 6;
  string source_cluster = 7; uint64 epoch = 8;
  map<string, string> request_context = 9;
}

message ClusterPingResponse {
  string cluster_id = 1; uint32 active_grains = 2;
  uint32 silo_count = 3; ClusterState state = 4;
}

enum ClusterState { HEALTHY = 0; DRAINING = 1; JOINING = 2; }

message DrainNotification {
  string cluster_id = 1;
  repeated GrainOwnershipEntry grains = 2; // empty = all grains
}

message GrainOwnershipEntry {
  string grain_type = 1; string grain_key = 2;
  uint64 epoch = 3; uint64 latest_sequence = 4;
}

message ReplicationSubscribeRequest {
  string grain_type = 1; string grain_key = 2;
  uint64 after_sequence = 3; string subscriber_cluster = 4;
}

message ReplicationEntryProto {
  string grain_type = 1; string grain_key = 2; uint64 sequence = 3;
  int64 timestamp_millis = 4; string source_cluster = 5;
  int32 entry_type = 6; bytes payload = 7;
}
```

### 6.9 Future: CRDT Trait (Option C)

Would live in a future `orlando-crdt` crate. Not implemented until Options A
and B are stable.

```rust
/// Conflict-free replicated data type. merge must be commutative,
/// associative, and idempotent to guarantee replica convergence.
pub trait Crdt: Sized + Send + Clone + 'static {
    fn merge(&mut self, remote: &Self);
}

/// Grow-only counter. Each cluster maintains its own sub-counter.
pub struct GCounter { counts: HashMap<ClusterId, u64> }
impl GCounter {
    pub fn increment(&mut self, cluster: &ClusterId) { /* per-cluster +1 */ }
    pub fn value(&self) -> u64 { self.counts.values().sum() }
}
impl Crdt for GCounter {
    fn merge(&mut self, remote: &Self) {
        for (k, &v) in &remote.counts {
            let local = self.counts.entry(k.clone()).or_insert(0);
            *local = (*local).max(v);
        }
    }
}

// Also planned: PNCounter, GSet, ORSet, LWWRegister, MVRegister
```

---

## 7. Open Questions

### 7.1 Directory Backend: PostgreSQL vs. Redis

| | PostgreSQL | Redis |
|---|---|---|
| **Durability** | WAL-backed, crash-safe | Configurable, possible data loss |
| **CAS** | `INSERT ON CONFLICT` | `SET NX` |
| **TTL** | Manual cleanup | Built-in expiry |
| **Cross-region** | Aurora Global / Citus | ElastiCache Global |
| **Latency** | Higher (disk) | Lower (memory) |

**Recommendation:** Support both via `CrossClusterDirectory` trait. Also
consider etcd (CAS via transactions, TTL via leases, already in most K8s).

### 7.2 Replication: Log-Based vs. Direct Streaming

**Log-based** (shared DB, poll): simpler failure handling, higher lag.
**Direct gRPC streaming** (push): lower lag, complex reconnection logic.

**Recommendation:** Log-based first (Phase 5.3). The `ReplicationLog` trait
abstracts storage, so a gRPC-streaming backend can be added later.

### 7.3 Epoch Fencing vs. Lease-Based Ownership

**Epoch fencing:** monotonic epoch per registration, stale writes rejected.
**Lease-based:** periodic renewal, failover when lease expires.

**Recommendation:** Use both. Leases for normal operation (low overhead),
epochs as a safety net during the partition-to-detection window.

### 7.4 How Many Clusters Is "Multi"?

Practical: 2-5 clusters. Directory scales with grains (not clusters).
Replication fan-out grows linearly with secondaries. Health checks are O(N^2)
naive; for large N, switch to gossip-based cluster health.

**Open:** Cap at N clusters in v1, or leave unbounded and document scaling?

### 7.5 Interaction with Existing Placement Strategies

Current `PlacementStrategy` selects a silo within one cluster. Multi-cluster
adds a layer above: selecting which cluster first.

**Recommendation:** Separate `CrossClusterPlacement` trait. Cluster selection
(geographic, policy) and silo selection (hash, load) are different enough
that combining them would be confusing.

### 7.6 Testing Without Multiple Datacenters

All tests run on localhost. Strategies: multiple `ClusterSilo` on different
ports, injected latency via test transport wrapper, partition simulation by
dropping connections, `tokio::time::pause()/advance()` for deterministic
failover timing.

**Recommendation:** Provide a `TestMultiCluster` builder (like
`FakeGrainContext`) that spins up N clusters with configurable latency and
partition simulation.

### 7.7 Backward Compatibility

Must not break single-cluster deployments:

- `GrainId` unchanged -- `ClusterId` tracked in directory, not embedded.
- Gateway gRPC service only registered when `MultiClusterConfig` is provided.
- `ClusterGrainRef` unchanged -- multi-cluster routing activates only with config.

### 7.8 Monitoring and Observability

Key metrics to expose via the `metrics` crate: replication lag per grain per
cluster, cross-cluster forwarding count, directory lookup latency, failover
events, epoch mismatch count.

**Open:** Define exact metric names and labels, following `orlando-runtime`
conventions.

### 7.9 Wire Protocol Versioning

Clusters may run different Orlando versions during rolling deploys. The
existing `message_version` handles message schemas; the cluster protocol also
needs versioning. **Recommendation:** Add `protocol_version` to
`ClusterPingRequest/Response` so clusters can negotiate compatibility.

### 7.10 Security Between Clusters

Cross-cluster needs: mTLS with per-cluster certificates, cluster-level auth
tokens (separate from silo-level), encryption of replication payloads at rest.

**Open:** Transport-layer (TLS) vs. application-layer (encrypted payloads)
encryption. TLS protects in transit only; application-layer protects at rest
too but adds key management.

---

## Appendix A: Glossary

| Term | Definition |
|------|------------|
| **Cluster** | A set of silos that share a SWIM membership protocol, HashRing, and GrainDirectory. Operates as a single failure domain. |
| **Multi-cluster** | A set of clusters that cooperate on grain placement, replication, and failover across geographic regions. |
| **Primary cluster** | The cluster that owns a grain activation and handles all writes. |
| **Secondary cluster** | A cluster that maintains a read-only replica of a grain's state, received via replication. |
| **Cross-cluster directory** | A shared registry mapping GrainId to the ClusterId that currently owns the grain's activation. |
| **Replication log** | An ordered, append-only sequence of state changes for a grain. Used to ship state from primary to secondary clusters. |
| **Epoch** | A monotonically increasing number associated with a grain's ownership. Used to fence stale primaries after failover. |
| **Gateway silo** | The silo within a cluster that handles incoming cross-cluster traffic. May be any silo or a designated entry point. |
| **GSI (Global Single Instance)** | Activation policy where exactly one activation of a grain exists across all clusters. |
| **CRDT** | Conflict-free Replicated Data Type. A state type with a deterministic merge operation that guarantees convergence. |
| **Fence token** | A value (epoch) attached to operations that prevents stale actors from performing writes after ownership has transferred. |
| **RPO (Recovery Point Objective)** | Maximum acceptable data loss during failover, measured in time or replication entries. |
| **RTO (Recovery Time Objective)** | Maximum acceptable downtime during failover, measured from detection to resumption of service. |

---

## Appendix B: Reference Material

- **Orleans multi-cluster:** https://learn.microsoft.com/en-us/dotnet/orleans/ (see `src/Orleans.EventSourcing/` and `src/Orleans.Runtime/MultiClusterNetwork/`)
- **CRDTs:** Shapiro et al., "A comprehensive study of Convergent and Commutative Replicated Data Types" (2011) -- https://hal.inria.fr/inria-00555588/document
- **Delta CRDTs:** Almeida, Shoker, Baquero (2018) -- https://arxiv.org/abs/1603.01529
- **Rust `crdts` crate:** https://crates.io/crates/crdts
- **DDIA:** Kleppmann, "Designing Data-Intensive Applications" (2017), Ch. 5 (Replication) and Ch. 9 (Consensus)
- **SWIM paper:** Das, Gupta, Stelling (2002)
- **Rust deps:** `tonic` (gRPC streaming), `sqlx` (PostgreSQL), `fred` (Redis)
