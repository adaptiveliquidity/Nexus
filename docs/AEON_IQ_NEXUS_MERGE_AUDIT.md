# AEON-IQ ↔ Nexus Integration Audit

**Date:** 2026-06-19
**Scope:** Review of the *"Cognitive Hypervisor: AEON-IQ & Nexus Integration Specification"* against the
ground truth of both repositories.
**Repos audited:**
- `adaptiveliquidity/Nexus` @ `main` (this repo — AI-native WASM snap-rollback sandbox)
- `adaptiveliquidity/AEON-IQ` @ `main` (long-term memory MMU proxy) — read via public clone

**Bottom line:** The two systems are **genuinely complementary, not overlapping**, and integrating them is
the correct functional direction. **However, "merging" in the literal sense (a monorepo / Cargo workspace) is
the *wrong* mechanism** — and the spec already says so. The spec's recommended **service-boundary bridge** is
the right call, and the evidence below supports it. The spec's *Phase-0 ground truth of AEON-IQ is accurate*,
but its **execution plan is one-sided**: it contains zero Nexus-side work even though the integration cannot
land without Nexus changes. Those gaps are enumerated in §4.

---

## 1. Verdict: is integration the correct functional direction?

**Yes — with high confidence.** The two codebases occupy disjoint responsibilities with no functional overlap
to consolidate and no competing abstractions to reconcile:

| Axis | AEON-IQ | Nexus |
| :--- | :--- | :--- |
| Role | "What the agent **knew**" — memory retrieval/extraction/versioning | "What the agent **did**" — sandboxed WASM execution |
| Shape | Stateful **HTTP proxy** (axum) | Stateless **library + Unix-socket daemon + MCP server** |
| Storage | Postgres + pgvector (`sqlx`), 21 migrations | **No database** — in-memory state + on-disk snapshots |
| Crypto | SHA-256 query hashing (`sha2`, `subtle`, `hex`) | ed25519 capsule signing + HMAC/SHA-256 redaction (`ed25519-dalek`, `hmac`, `sha2`) |
| Heavy deps | sqlx/pgvector, prometheus | wasmtime 45 + cranelift, zstd, rmcp |

They **compose** rather than merge: AEON-IQ produces an auditable record of *context injected*, Nexus produces
a signed record of *execution performed*, and a thin bridge correlates the two. This is a sound layering.

---

## 2. Repo strategy: confirm "merge" should NOT be a monorepo

The spec ranks **Staging Fork & Service Boundary** as Primary and **Cargo Workspace Monorepo** as Rejected.
The dependency evidence confirms this is correct:

- **Shared, compatible deps** (trivial to bridge): `tokio 1`, `serde 1`, `serde_json 1`, `uuid 1`,
  `chrono 0.4`, `sha2 0.10`, `rand 0.8`, `futures 0.3`, `anyhow`, `tracing`.
- **Collision / bloat risk in a workspace:** Nexus pulls `wasmtime 45` + `cranelift` + `wasmtime-wasi` +
  `zstd` + `rmcp`; AEON-IQ pulls `sqlx 0.9` + `pgvector 0.4` + `axum 0.7` + `prometheus`. Forcing these into
  one build unit would (a) multiply AEON-IQ proxy compile times with a full WASM JIT toolchain it never runs,
  and (b) prevent scaling proxy nodes independently from execution nodes. (`thiserror` even differs by major:
  Nexus `2.0`, AEON-IQ `1` — a non-issue across a service boundary, friction inside one workspace.)
- **Operational argument:** AEON-IQ is a long-lived stateful proxy; Nexus is an ephemeral execution fabric.
  Coupling their release cycles and deploy topologies has no upside.

**Recommendation: adopt the service boundary. Do not monorepo. Build `aeon_nexus_bridge` as a standalone
crate.** Keep both repos independent; the bridge owns the shared wire types.

---

## 3. Ground-truth verification of the spec's claims

### 3.1 AEON-IQ Phase-0 evidence map — **accurate**

Every claim in spec §1 was checked against source:

| Spec claim | Verified? | Evidence |
| :--- | :--- | :--- |
| RateLimiter is a `DashMap<String, Bucket>` token bucket that **never evicts** (memory leak) | ✅ **Confirmed** | `src/rate_limit.rs` — `buckets: DashMap<String, Bucket>`; `check_and_consume` only ever `entry().or_insert_with(...)`. No removal path anywhere. The leak is real. |
| Proposed `evict_stale_buckets` fix is API-correct | ✅ | `Bucket` has a `last_refill: Instant` field, so the spec's `retain(... < max_idle)` compiles against the real struct. |
| Extraction runs in volatile `tokio::spawn`, **facts lost on LLM/embed failure** | ✅ **Confirmed** | `src/memory/extraction.rs` — `extract_and_store` is documented "Entry point called from `tokio::spawn`. Logs errors, never panics." No retry/outbox. |
| Background workers spawned on the **main runtime** (no role split) | ✅ **Confirmed** | `src/main.rs` unconditionally `tokio::spawn`s `archival::run_job`, `rmk_worker::run_policy_update_job`, `run_co_access_decay_job`, `run_pressure_sweep_job`; then `TcpListener::bind` + `axum::serve` in the same process. No `ROLE` switch exists. |
| Migrations `0001`–`0021` applied at startup | ✅ | Files `0001_initial.sql` … `0021_memory_retrieval_logs.sql` present. |
| Retrieval audit log links to proof | ✅ | `0021_memory_retrieval_logs.sql` has `query_hash`, `candidate_memory_ids`, `injected_memory_ids`, `suppressed_memory_ids`, `scores JSONB`, `latency_ms`. SHA-256 hashing confirmed in `src/memory/retrieval.rs`. |
| Memory diff API `GET …/memories/diff` as-of two timestamps | ✅ | `src/api.rs:518` `memories_diff` → `store::list_latest_versions_as_of(...)` for `from`/`to`. |
| Conflict detection vs top-5 similar, stored in `memory_conflicts` | ✅ | `src/memory/conflicts.rs` — background `tokio::spawn`, top-5, `memory_conflicts` table. |
| Dashboard is Next.js **app-router** proxying backend | ✅ | `dashboard/src/app/api/.../route.ts` files present (App Router). |

**Conclusion:** the spec's audit of AEON-IQ is trustworthy. The MUST-fix items (T1 rate-limit eviction, T2
evidence contract, T6 extraction outbox) target real defects.

### 3.2 Nexus side — **stronger than the spec assumes, but missing the integration seams**

The spec treats Nexus as a black box that "emits a signed Proof Capsule containing the
`AeonNexusMemoryEvidence` digest." Reality:

- ✅ **Signed proof capsules exist.** `src/proof/schema.rs::ProofCapsule` + `src/proof/signing.rs`
  (`sign_capsule`/`verify_capsule`, ed25519, `SignatureEnvelope { signer, key_id, signature,
  signed_payload_digest }`).
- ✅ **The crypto model already matches the spec's privacy rules.** `TypedDigest { algorithm, value,
  public_recomputable }` and `DigestMode::{Sha256Public, HmacSha256Private, RedactedNoDigest}` are a direct
  fit for the contract's "SHA-256 public digest vs HMAC-SHA256 private, auditor-recomputable" requirement.
  `src/proof/redaction.rs::RedactionPolicy` already HMACs paths/tokens with an operator key. **The spec's
  "Zero-Knowledge & Privacy Rules" are essentially already implemented in Nexus.**
- ✅ **Capability denial is real.** `src/security/capability.rs` → `NexusError::CapabilityDenied`,
  subset-lattice enforcement. The Denial Negotiator (§7) has a real signal to hook.
- ✅ **Snapshot/rollback + fork-and-race are real.** `CapabilityEvidence`, `SnapshotEvidence`,
  `FailureEvidence`, `RollbackEvidence`, `BranchRaceEvidence` already exist on the capsule, and `lib.rs`
  exports `fork_and_race` — so the spec's "fork-and-race recovery timeline branch" maps to a real primitive.
- ✅ **Transport already exists.** `src/daemon/protocol.rs` is exactly the spec's "structured JSON over a
  Unix socket": `[u32 BE length][payload]` framing, 64 MiB cap, `nexus-agentd` over
  `default_socket_path()`. The bridge does **not** need a new HTTP server on the Nexus side.

---

## 4. Gaps the spec under-specifies (the real work)

These are the substantive findings. None are blockers to the *direction*; all are blockers to the *plan as
written*.

### G1 — The PR plan has **zero Nexus-side work**, but the integration requires it.
Spec §10 lists five PRs, all on `AEON-IQ` branches. Yet the handshake cannot exist without Nexus changes:

1. **`ProofCapsule` has no slot for external memory evidence.** Grepping Nexus finds no
   `memory_evidence` / `AeonNexusMemoryEvidence` field. The spec's claim that the capsule "contains the
   `AeonNexusMemoryEvidence` digest" requires adding (e.g.) an `Option<MemoryEvidenceRef>` to
   `ProofCapsule`. Nexus's own versioning note ("add fields with `#[serde(default)]`") makes this safe, but
   it is a Nexus PR that the plan omits.
2. **`DaemonRequest::Execute` has no correlation fields.** It carries `name`, `wasm_bytes/path`, `entry`,
   `input`, `auth_token` — no `agent_id`, `session_id`, or `memory_evidence_digest`. Step 4 of the flow
   ("Execute Tool Request (inject MemoryEvidence)") needs these threaded through and surfaced in the capsule.
3. **Timeline events** (`snapshot_created`, `proof_capsule_emitted`, `capability_denied`) must be **emitted
   by Nexus** (returned in the response/capsule) for AEON-IQ to persist them. Nexus has no DB, so it cannot
   write the `cognitive_hypervisor_timeline` table itself — it must push.

> **Action:** add a parallel Nexus PR track (capsule evidence field + daemon correlation fields + event
> surfacing) *before* AEON-IQ PR #3/#4 can be accepted. The bridge crate should own these shared types so
> both repos depend on one definition.

### G2 — Two different `agent_id` namespaces.
AEON-IQ `agent_id` is `TEXT` (memory owner / tenant). Nexus's only `AgentId` lives in
`src/snapshot/sync/lineage.rs` and identifies a **snapshot lineage node**, not a memory tenant. These are
unrelated today. The bridge must define an explicit mapping (and the evidence contract should digest the
AEON-IQ agent id, not assume Nexus knows it).

### G3 — "Maximum isolation" claim vs. real coupling.
The strategy table rates the service boundary "Maximum" isolation, but §6/§8 make Nexus depend on AEON-IQ
being up to record every execution event (timeline ledger lives in AEON-IQ's Postgres). That is a real
runtime coupling. Mitigation: Nexus should **return** events in the signed response and treat ledger
persistence as best-effort/async on the AEON-IQ side, so a ledger outage degrades auditability but never
blocks execution.

### G4 — Time-travel step "**invalidates and prunes all newer memory versions**" is destructive and
self-contradictory. Spec §8 step 3 says prune newer versions, while the same sentence calls it a
"fork-and-race recovery **timeline branch**." Pruning is not branching. AEON-IQ's `memory_versions` is
append-only by design (that's what makes `list_latest_versions_as_of` and the diff API work). **Recommend
branch-don't-prune:** create a new timeline branch pointer; never hard-delete history, or the audit trail the
whole project exists to provide is destroyed. Mark this MUST before any time-travel endpoint ships.

### G5 — Snapshot addressing by timestamp.
§6 path D calls `/memories/at?timestamp=…` (AEON-IQ has this) and asks Nexus to "load the WASM state snapshot
matching the target snapshot ID." Nexus snapshots are keyed by `snapshot_id`/lineage, **not wall-clock**. The
timestamp→snapshot resolution must live in the `cognitive_hypervisor_timeline` table (it already has
`nexus_snapshot_id` + `timestamp`, so this is workable — but the bridge must do the lookup; Nexus can't).

### G6 — Key management is split.
The contract assumes one rotating "operator key" for HMAC digests. Nexus actually has **two** independent key
sources: an ed25519 **signing seed** (capsule signatures) and an HMAC **redaction key**. AEON-IQ currently
hashes queries with plain SHA-256 (no HMAC, no `hmac` crate in its deps). For an auditor to cross-verify
`agent_id_digest` across both systems, **the same HMAC key must be provisioned to both**, and AEON-IQ must add
the `hmac` crate. Define one key-provisioning story in the bridge before PR #3.

---

## 5. Spec accuracy nitpicks (minor)
- `REINDEX INDEX CONCURRENTLY idx_memories_hnsw` (Wave 1.4) requires PG12+ and the spec's own VERIFY item
  (pgvector ≥ 0.5). Correctly flagged; keep it gated behind a version check.
- §9 thresholds (boot < 2 ms, rollback < 1 ms, combined roundtrip < 12 ms) are stated without a baseline in
  this repo's `BENCHMARKS.md`; treat as targets to validate, not facts.
- T14 ("Anthropic TTFT streaming irrelevant for batch WASM") is reasonable **for tool execution**, but
  AEON-IQ is a streaming proxy on the planning path; don't let the bridge break existing SSE passthrough
  (the spec's VERIFY item #3 already calls this out — keep it).

---

## 6. Recommended sequencing (amended)

The spec's wave ordering is sound for AEON-IQ hardening. The amendment is to **interleave a Nexus track**:

1. **AEON-IQ PR #1 / #2** (rate-limit eviction, reindex, extraction outbox, role split) — independent, ship first.
2. **Bridge crate `aeon_nexus_bridge`** — owns `AeonNexusMemoryEvidence`, digest/HMAC helpers, and the wire
   types shared by both repos. (Spec PR #3, expanded.)
3. **Nexus PR (new):** add `memory_evidence` field to `ProofCapsule`; add `agent_id`/`session_id`/
   `evidence_digest` to `DaemonRequest::Execute`; surface `capability_denied`/`snapshot_created`/
   `proof_capsule_emitted` in the response. Backward-compatible via `#[serde(default)]`.
4. **AEON-IQ PR #4** (timeline ledger + time-travel) — **branch-don't-prune** (G4); resolve timestamp→snapshot
   in the timeline table (G5).
5. **AEON-IQ PR #5** (Denial Negotiator) — hook `NexusError::CapabilityDenied`; the 2-round loop cap is good.

---

## 7. Summary

- **Direction: correct.** AEON-IQ (memory) and Nexus (execution) are complementary; integration adds a
  verifiable "what it knew → what it did" audit chain that neither has alone.
- **Mechanism: service boundary, not a code merge.** Dependency and operational evidence both reject a
  monorepo; the spec already lands here.
- **Spec quality: Phase-0 ground truth is accurate; the integration plan is one-sided.** Add a Nexus work
  track (G1), reconcile the two `agent_id` namespaces (G2), make ledger persistence non-blocking (G3), make
  time-travel **branch not prune** (G4), and unify key management (G6).
- **Pleasant surprise:** Nexus already implements most of the spec's cryptographic/privacy contract
  (`TypedDigest`, `DigestMode::HmacSha256Private`, `RedactionPolicy`, ed25519 signing, `fork_and_race`),
  so the bridge is mostly *wiring and identity mapping*, not net-new crypto.
