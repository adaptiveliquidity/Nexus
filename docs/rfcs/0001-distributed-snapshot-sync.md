# RFC 0001 — Distributed Snapshot Synchronization

- **Status:** Draft (research / design only — no production code)
- **Roadmap:** P3, Research
- **Author:** Nexus
- **Supersedes / relates to:** `docs/phase_d_distributed_snapshot_fabric.md` (if present)

## 1. Summary

Define a protocol for replicating Nexus snapshots across multiple hosts so that
a snapshot taken on node A can be restored on node B. This enables multi-host
rollback, agent migration (move a running agent's state to another node), and
replicated checkpoints for durability.

This is a **design document**. It proposes a data model, wire format, consistency
model, and a phased implementation plan. It does not change production code.

## 2. Context — what a snapshot actually is today

From `src/snapshot/manager.rs`, a snapshot is a self-contained, serializable value:

```text
Snapshot {
    id: Uuid,
    timestamp: DateTime<Utc>,
    memory: Vec<u8>,            // zstd level-3 compressed WASM linear memory
    memory_checksum: String,    // SHA-256 hex of the *decompressed* memory
    fs_changes: FilesystemDiff, // created/modified/deleted files + dirs
    execution_state: ExecutionState,
    metadata: SnapshotMetadata,
    original_size: usize,
    compressed_size: usize,
}

ExecutionState {
    captured_globals: Vec<GlobalSnapshot>,  // name, GlobalValue{I32|I64|F32|F64}, mutable
    captured_tables:  Vec<TableSnapshot>,   // name, size: u32
}

SnapshotMetadata {
    operation_name: String,
    input_hash: String,
    creation_time_us: u64,
    memory_pages: u32,
    preconditions: Vec<String>,
}
```

Two properties make distribution tractable:

1. **It is already serializable** (`serde` on every field) and content-addressable
   (`memory_checksum` is a SHA-256 over the canonical decompressed memory).
2. **It is already compressed** (zstd) and self-describing (sizes + checksum
   travel with the bytes).

The hard parts are not the payload format — it is *which* snapshots to ship, *when*,
and *how to reconcile* concurrent rollbacks on different nodes.

## 3. Goals / Non-goals

**Goals**
- Transfer a complete, integrity-verified snapshot from one node to another.
- Support both *push* (source replicates to a peer) and *pull* (peer requests a
  snapshot by id) flows.
- Make transfers content-addressed and idempotent (re-sending the same snapshot
  is a no-op).
- Define a consistency model that is honest about what concurrent rollback means.

**Non-goals (this RFC)**
- Live, sub-millisecond memory migration of a *running* instance (the snapshot is
  taken at safe points; we ship snapshots, not live `Store` state).
- Byzantine fault tolerance / untrusted peers (assume mutually authenticated nodes
  in a trust domain; cross-trust-domain sync is future work and overlaps RFC 0003).
- Restoring a live WASM call stack (not captured today — see RFC 0002).

## 4. Data model on the wire

Snapshots are content-addressed by a **digest** derived from the existing
`memory_checksum` plus a hash of the remaining fields:

```text
snapshot_digest = SHA-256(
    memory_checksum_bytes
    || canonical_cbor(execution_state)
    || canonical_cbor(fs_changes)
    || canonical_cbor(metadata)
)
```

Rationale: `memory` is large and already hashed via `memory_checksum`; we avoid
re-hashing megabytes by reusing it and only hashing the small structured tail.
Canonical CBOR (deterministic map ordering) makes the digest reproducible across
nodes and language versions.

Wire frame reuses the daemon's existing length-prefixed framing
(`src/daemon` — `[u32 BE length][payload]`), extended with a small typed envelope:

```text
SyncEnvelope {
    proto_version: u16,
    kind: Advertise | Want | Snapshot | Ack | Nack,
    body: <kind-specific, serde/bincode or CBOR>,
}
```

- **Advertise** `{ digests: Vec<Digest>, metadata_summaries }` — "I have these."
- **Want** `{ digests: Vec<Digest> }` — "send me these."
- **Snapshot** `{ digest, Snapshot }` — the payload; receiver verifies
  `memory_checksum` after decompression and recomputes `snapshot_digest` before
  accepting.
- **Ack/Nack** `{ digest, reason? }`.

Integrity: a received snapshot is only admitted to the local `SnapshotManager`
ring buffer if (a) decompressed memory hashes to `memory_checksum`, and (b) the
recomputed `snapshot_digest` matches the advertised one. Mismatch ⇒ Nack + drop.

## 5. Consistency model

The honest framing: **snapshots are immutable, content-addressed objects; the
mutable thing is "which snapshot is the current head for a given agent/lineage."**

- **Snapshot objects:** *strongly consistent by construction* — they are immutable
  and content-addressed, so two nodes either have byte-identical objects under a
  digest or they have different digests. No conflict is possible on the objects
  themselves.
- **Lineage head pointer** (`current_snapshot` per agent, today a
  `RwLock<Option<Snapshot>>` in the hypervisor): this is the contended state.
  Two nodes can independently roll an agent back to different snapshots.

We propose **eventual consistency with explicit conflict surfacing**, not silent
last-writer-wins:

- Each lineage head update carries a Lamport/HLC timestamp and the originating
  node id.
- Concurrent divergent heads (neither is a causal ancestor of the other) are
  **not auto-merged**. They are recorded as a *fork* and surfaced to the operator
  / policy layer, because "which rollback wins" is a semantic decision the runtime
  cannot make safely. (A CRDT could pick a deterministic winner, but for
  execution state that risks resurrecting a state the operator deliberately rolled
  away from.)

This mirrors how `requires_rollback()` already keeps rollback decisions explicit
rather than automatic.

## 6. Transport options

| Option | Pros | Cons | Verdict |
|--------|------|------|---------|
| gRPC streaming (tonic) | Mature, backpressure, TLS/mTLS built in, bidi streams fit Advertise/Want | New heavy dep tree; HTTP/2 overhead for LAN | **Recommended for v1** |
| QUIC (quinn) | Lower latency, multiplexed, great for WAN/lossy links | Younger ecosystem; more to get right | Future / WAN profile |
| Custom binary over the existing daemon framing | Reuses `[u32 BE len][payload]`; zero new deps; consistent with `nexus-agentd` | We re-implement auth, flow control, retries | Good for a minimal embedded mode |

Recommendation: ship a **transport-agnostic core** (digest/advertise/want/verify
state machine) with a `SyncTransport` trait, and provide two impls: the existing
daemon framing (zero new deps, LAN/embedded) first, then gRPC for the networked
profile. Authentication: mutual TLS for gRPC; for the daemon-framing mode, a
pre-shared node key with an HMAC over each frame.

## 7. Failure modes

- **Partial transfer / connection drop:** transfers are atomic at the
  `SnapshotManager` boundary — a snapshot is admitted only after full receipt +
  verification. No partial snapshot is ever installed.
- **Digest collision / corruption:** SHA-256 over memory + canonical-CBOR tail;
  verification on receipt. Corruption ⇒ Nack, no install.
- **Version skew:** `proto_version` in the envelope; `GlobalValue`/`Capability`
  enums must be versioned (additive only) so an older node rejects unknown
  variants with a clear Nack rather than mis-deserializing.
- **Ring-buffer eviction races:** the source may evict a snapshot (capacity-bound
  `SnapshotRingBuffer`) between Advertise and Want. Want for an evicted digest ⇒
  Nack(`gone`); requester falls back to another advertiser or gives up.
- **Lineage fork:** surfaced, not hidden (see §5).
- **Clock skew:** use HLC (hybrid logical clocks), not wall clock, for head
  ordering, since `timestamp` is `Utc::now()` and nodes will drift.

## 8. Security considerations

- Snapshots contain raw guest memory — treat them as **confidential**. Transport
  must be encrypted (mTLS / authenticated framing). At rest, the existing
  persistence path should be the boundary; replication must not weaken it.
- A malicious/buggy peer could flood Advertise. Rate-limit and cap inbound
  pending Wants.
- `fs_changes` paths are attacker-influenced data; restoring them on a new node
  must re-run the *same* capability authorization the original execution required
  (`metadata.preconditions`) — never replay filesystem effects unconditionally.
  This is the cross-cutting tie-in to the capability model (and RFC 0003).

## 9. Phased implementation plan

1. **Phase 1 — Object sync core (no transport).** `snapshot_digest`, canonical
   CBOR encoding, a `SyncTransport` trait, and an in-memory transport for tests.
   Property test: round-trip any `Snapshot` ⇒ identical digest + integrity pass.
2. **Phase 2 — Daemon-framing transport.** Advertise/Want/Snapshot/Ack over the
   existing length-prefixed protocol with HMAC auth. Two `nexus-agentd` instances
   replicate a snapshot on a loopback socket.
3. **Phase 3 — Lineage heads + HLC.** Track per-agent head with hybrid logical
   clocks; detect and surface forks; expose head state via the daemon API.
4. **Phase 4 — gRPC transport profile.** tonic-based `SyncTransport` impl with
   mTLS for the networked deployment.
5. **Phase 5 — Restore-with-authorization.** On restore of a replicated snapshot,
   re-validate `metadata.preconditions` before any `fs_changes` are applied.

## 10. Open questions

- Do we need delta sync between snapshots of the same lineage (ship only changed
  pages)? The codebase already has `compute_dirty_pages` / `DiffSnapshot`
  (`src/snapshot/compression.rs`) — distributed delta sync could reuse it, but it
  adds protocol complexity. Defer to a follow-up once full-object sync works.
- Where does the lineage-head authority live — fully peer-to-peer, or a single
  elected coordinator per lineage?

## 11. Prior art

- **Firecracker snapshot/restore + live-migration** — file-based, full-memory
  snapshots with an explicit "load and resume" boundary; informs our
  "ship-immutable-object, restore-at-safe-point" model.
  <https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md>
- **CRIU (Checkpoint/Restore In Userspace)** — process-tree checkpointing and the
  hard problems of restoring external resources (files, sockets); motivates §8's
  "re-authorize side effects on restore."
- **CRDTs (Shapiro et al.)** — considered for head reconciliation and explicitly
  *rejected* for execution-state heads in favor of surfaced forks (§5).
