# NexusIQ Build Plan — AEON-IQ × Nexus Integration (Phases 4–10)

**Date:** 2026-06-20  
**Status:** Active — Phases 0–3 complete, Phase 4 in execution  
**Canonical audit:** [`docs/AEON_IQ_NEXUS_MERGE_AUDIT.md`](AEON_IQ_NEXUS_MERGE_AUDIT.md)  
**Repository:** `adaptiveliquidity/Nexus`

---

## What "done" means

A demonstrable end-to-end loop: *agent stores memory → executes in Nexus with recall → the signed proof capsule carries a verifiable memory-evidence digest → a capability denial triggers a bounded negotiation → time-travel branches the timeline without destroying history → all of it usable over the MCP wire protocol.*

Three invariants hold throughout:
1. **Default build byte-identical** — every change is feature-gated under `aeon-memory` (default off).
2. **Fail-open** — a memory/ledger outage degrades auditability, never blocks execution.
3. **Honest proof** — the capsule explicitly records when memory was missing or degraded ("advisory vs attested" modes).

---

## Completed (Phases 0–3, merged to main)

| Phase | PR | What shipped |
|---|---|---|
| Phase P (AEON-IQ readiness) | AEON-IQ #21–#25 | RateLimiter eviction, extraction outbox+retry, proxy/worker split (`MEMORYOS_ROLE`), HNSW runbook |
| Phase 0 (`aeon-memory` feature) | Nexus #110 | `AeonConfig` in `src/aeon.rs`; route ai-recovery LLM calls through AEON-IQ — default off (Invariant 1) |
| Phase 1 (audit doc) | Nexus #109 | `docs/AEON_IQ_NEXUS_MERGE_AUDIT.md` — canonical gap analysis G1–G6, sequencing §6 |
| Phase 2 (`AeonMemoryClient`) | Nexus #111 | Fail-open management API client; `health`, `search`, `store` methods; `MemoryHit`; env `NEXUS_AEON_*` |
| Phase 2b (wiring) | Nexus #112 | Recall before LLM recovery; fire-and-forget capture on exec failure; contract fixes (similarity field, create_memory body) |
| Phase 3 (bridge crate) | Nexus #113 | `crates/aeon_nexus_bridge` — `MemoryEvidence`, `MemoryEvidenceRef`, `AgentSessionMapping`, HMAC/SHA-256 helpers |

---

## Remaining Phases

### Phase 4 — ProofCapsule memory-evidence binding + HMAC unification
**Risk:** HIGH (crypto — proof signing, trust root)  
**Audit gaps closed:** G1.1, G6  
**Dispatch:** `return_to_claude` (Claude commits + opens PR)

What ships:
- `memory_evidence: Option<MemoryEvidenceRef>` added to `ProofCapsule` (`src/proof/schema.rs`) via `#[serde(default)]` — backward-compatible.
- `MemoryAttestationMode` enum: `Advisory` (memory searched; outcome uncertain) vs `Attested` (memory injected; evidence verifiable).
- `NEXUS_AEON_HMAC_KEY` env var in `AeonConfig` — feeds the bridge's `AgentSessionMapping.agent_handle()` so auditors can cross-verify `agent_id_digest` across both systems (G6).
- Helper in `src/aeon.rs` (feature-gated): build `MemoryEvidenceRef` from recall results, ready to embed in capsule.
- `aeon_nexus_bridge` added as optional Cargo dep, enabled by `aeon-memory` feature.

Accept: sign/verify round-trips; default build byte-identical; capsule schema versions cleanly.  
**Depends on:** Phase 3 ✅

---

### Phase 5 — Daemon execution correlation fields
**Risk:** MEDIUM (protocol change)  
**Audit gaps closed:** G1.2, G2  
**Dispatch:** `return_to_claude`

What ships:
- `DaemonRequest::Execute` gains optional `agent_id: Option<String>`, `session_id: Option<String>`, `memory_evidence_digest: Option<String>` — `#[serde(default)]` throughout; old clients unaffected.
- Fields threaded through execute → `ProofCapsule` subject/evidence.
- G2 resolved: bridge's `AgentSessionMapping` is the canonical mapping between AEON-IQ tenant `agent_id` and Nexus lineage; the HMAC digest (not the raw id) crosses the wire.

Accept: protocol round-trip test; old-client backward-compat; 64 MiB framing intact.  
**Depends on:** Phase 4

---

### Phase 6 — Event surfacing + non-blocking ledger
**Risk:** MEDIUM  
**Audit gaps closed:** G1.3, G3  
**Dispatch:** `return_to_claude`

What ships:
- Daemon response surfaces events: `capability_denied`, `snapshot_created`, `proof_capsule_emitted` (Nexus has no DB; it must push these for AEON-IQ to persist them).
- G3 resolved: Nexus returns events in the signed response; AEON-IQ persistence is best-effort/async — ledger outage degrades auditability, never blocks execution.

Accept: events present in response; execution unaffected when the consumer is down (fail-open test).  
**Depends on:** Phase 5

---

### Phase 7 — AEON-IQ timeline ledger + time-travel (branch-don't-prune)
**Risk:** HIGH (data integrity — audit trail)  
**Audit gaps closed:** G4, G5  
**Dispatch:** `return_to_claude` (AEON-IQ side — requires AEON-IQ in lab sandbox)

What ships (AEON-IQ repo):
- `cognitive_hypervisor_timeline` Postgres table: `agent_id`, `session_id`, `nexus_snapshot_id`, `capsule_digest`, `timestamp`, `event_type`.
- Time-travel endpoint — **branch, don't prune**: new branch pointer in `memory_versions`; history is append-only. `memory_versions` is never hard-deleted (that's the audit trail the project exists to provide).
- G5 resolved: timestamp→snapshot resolution uses the timeline table (`nexus_snapshot_id` + `timestamp` columns); the bridge does the lookup, not Nexus.

**Prerequisite:** AEON-IQ cloned to `/home/ahpsi/codex-clones/AEON-IQ` (→ `/projects/AEON-IQ` in lab).  
Accept: time-travel creates a branch (history intact verified by row count); `/memories/at?timestamp=` resolves to a real `snapshot_id`.  
**Depends on:** Phase 6. **Parallel with Phase 8.**

---

### Phase 8 — Denial Negotiator (2-round cap)
**Risk:** HIGH (capability boundary)  
**Audit gaps closed:** G2 hook  
**Dispatch:** `return_to_claude`

What ships (Nexus):
- Hook `NexusError::CapabilityDenied` (surfaced in Phase 6) → bounded **2-round** negotiation.
- Round 1: consult AEON-IQ memory search for a narrower capability set that may succeed.
- Round 2: retry with narrowed set. If denied again — stop. Hard cap enforced.
- No privilege escalation; denial still fails closed if negotiation exhausts.

Accept: hard 2-round cap enforced (test with a deny-all policy); no escalation path; retry counter visible in `CapabilityEvidence`.  
**Depends on:** Phase 6. **Parallel with Phase 7.**

---

### Phase 9 — MCP surfacing + end-to-end demo + conformance suite
**Risk:** MEDIUM  
**Dispatch:** `return_to_claude` for MCP hunk; `prepare_pr` acceptable for demo/tests

What ships:
- Memory-bound execution and time-travel exposed through the **MCP server** (`src/bin/nexus_mcp.rs`) — full chain usable over the wire protocol (continues the Secure MCP Runtime milestone).
- End-to-end demo script: agent stores memory → executes → capsule carries evidence → capability denial → negotiation → time-travel branch.
- **Conformance/regression suite** locking the three invariants as permanent tests: default-off byte-identical, fail-open, advisory-vs-attested, branch-don't-prune. These become CI gates.

Accept: demo script runs end-to-end; conformance suite green; all three invariants tested.  
**Depends on:** Phases 4–8 (both P7 and P8 must be complete).

---

### Phase 10 — Release hardening + final gate
**Risk:** MEDIUM (release/version)  
**Dispatch:** `return_to_claude` (release/version is sensitive)

What ships:
- Perf validation vs audit §9 targets (boot <2 ms, rollback <1 ms, combined roundtrip <12 ms — these are targets to validate, not assumed facts).
- Full cross-system threat model (HMAC key provisioning, correlation, negotiation loop).
- Documentation: merge-audit status updated to "closed"; integration runbook; key-provisioning guide.
- `CHANGELOG.md` updated; version bump; tag.

Accept: all audit targets met or variance documented; threat model reviewed; tag pushed.  
**Depends on:** Phase 9.

---

## Dependency graph

```
#113 ✅ ──▶ P4 ──▶ P5 ──▶ P6 ──┬──▶ P7 (AEON timeline) ──┐
            (crypto)  (corr)  (events) └──▶ P8 (denial)  ──┴──▶ P9 ──▶ P10
                                              P7 ∥ P8              (MCP)  (release)
```

Critical path: **P4 → P5 → P6 → P7 → P9 → P10**. P8 runs in parallel with P7.

---

## Operating constraints (established lab policy)

- **Sensitive paths → `return_to_claude`.** All phases here touch proof-signing, capability, secure-runtime, or release. Lab does impl + review; Claude commits and opens PR.
- **`auto_pr` is OFF** unless explicitly authorized per-run.
- **Consensus for HIGH-risk phases.** The autonomous workflow adds independent security-auditor + verification-qa + claude-reviewer votes (decorrelation by design).
- **Branch isolation:** lab branches off `/projects/Nexus-main` (GitHub origin, synced to main at #113). Never branch from local clones lagging GitHub.
- **PR runner:** fetch origin → branch from origin/main → push → open PR targeting main → return URL. Never push to main, never force-push.
- **AEON-IQ lab access (P7/P8):** requires AEON-IQ cloned to `/home/ahpsi/codex-clones/AEON-IQ` before Phase 7 fires.
