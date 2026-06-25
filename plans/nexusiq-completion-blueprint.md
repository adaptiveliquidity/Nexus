# NexusIQ Completion Blueprint

**Status:** Active · **Created:** 2026-06-25 · **Owner:** orchestrator (Claude) · **Impl:** Codex / ECC lab
**Basis:** Evidence-backed 4-cluster audit of the ChatGPT "remaining work" report against the live
`adaptiveliquidity/Nexus`, `adaptiveliquidity/AEON-IQ`, and `adaptiveliquidity/Nexus-IQ` repos (2026-06-25).

> **Why this doc exists.** The ChatGPT report's *framework* (test matrix, release-readiness gates,
> "installable/testable/safe/understandable") is sound, but its *status* was stale and one section was
> architecturally wrong. This blueprint keeps only the **genuinely-remaining** work, with file-level
> evidence so a fresh agent can execute each task cold.

---

## 0. Locked architectural decisions

1. **MCP is the gateway.** Do **not** build a bespoke REST gateway (`/decide /execute /simulate /policy/check`)
   or an OpenAPI contract. Integration is MCP + Docker Compose and is already built. `nexus_execute_proof`
   is `/execute`; compose healthchecks + AEON `/health` + `/api/v1/stats` cover health/metrics; policy =
   Nexus capability profiles. (Report Section C is superseded.)
2. **Build dedicated memory capabilities.** Add explicit `ReadMemory(scope)` / `WriteMemory(scope)` to the
   Nexus capability lattice (Section E stays in scope).
3. **Already shipped — do NOT rebuild:** Cognitive Denial Negotiator (`src/security/negotiator.rs`,
   `MAX_NEGOTIATION_ROUNDS=2`); MemoryEvidence binding (`crates/aeon_nexus_bridge` — `MemoryEvidenceRef`,
   HMAC IDs, no-raw-memory invariant, run linkage); AEON readiness blockers 1-3,5,6.

## 1. Ground-truth status (audit summary)

| Report § | Verdict | Real remaining |
|---|---|---|
| A. AEON-IQ blockers | 5/6 DONE | HNSW maintenance runbook/job only |
| B. Proof hardening | 1 DONE / 3 PARTIAL / 4 TODO | **The real work — Sprint 1** |
| C. REST Gateway | DIVERGED (MCP already serves it) | Nothing (dropped) |
| D. MemoryEvidence binding | 5/6 DONE | optional `post_run_memory_diff` link |
| E. Capability-gated memory | 3 DONE / 1 PARTIAL / 2 TODO | **`ReadMemory`/`WriteMemory` — Sprint 2** |
| F. Denial Negotiator | SHIPPED | Nothing |
| G. Time-travel | foundation present | future (deferred) |
| §2 Packaging | mostly DONE | docs, releases, images, HNSW runbook — Sprint 3 |

---

## SPRINT 1 — Proof-capsule hardening  *(repo: Nexus)*

**Context for all S1 tasks.** The proof scaffolding exists (`src/proof/{signing,redaction,scorecard,schema,receipt}.rs`)
but enforcement is **not wired into the capsule-construction path** in `src/hypervisor/mod.rs`
(the receipt→capsule builder around lines 880-940). Each task below wires one guard. Build/verify on WSL
`/home/ahpsi/nexus` (Windows checkout lacks the Windows SDK; CI Linux is authoritative). Dedicated proof
signing key is **already done** (`src/hypervisor/mod.rs:260`, verified by `tests/proof_signing.rs:146-164`).

### S1-T1 · Enforce mandatory non-empty limitations
- **Evidence of gap:** `src/hypervisor/mod.rs:937` sets `limitations: Vec::new()` unconditionally; `tests/proof_execution.rs:21` asserts empty is OK.
- **Do:** Populate `limitations` during capsule construction (standing caveats: sandbox scope, advisory-memory mode, non-exhaustive verification). Add a construction-time invariant rejecting empty `limitations`.
- **Gate:** Every emitted capsule has >=1 limitation; a test proves empty-limitations capsules cannot be produced.

### S1-T2 · HMAC/redact input digest for sensitive inputs
- **Evidence of gap:** `src/hypervisor/mod.rs:617` and `:892-896` always use `TypedDigest::sha256_public()`; receipt stores plain SHA256 (`src/proof/receipt.rs`).
- **Do:** For short/sensitive inputs, emit an HMAC-keyed digest (reuse the operator HMAC key used for agent IDs in `aeon_nexus_bridge`) instead of a public SHA. Public SHA only for non-sensitive/large inputs by policy.
- **Gate:** No public SHA over sensitive small values by default; test confirms HMAC path for a known sensitive input.

### S1-T3 · Redacted failure summary
- **Evidence of gap:** `src/hypervisor/mod.rs:917-920` copies `output.error` verbatim; `src/proof/redaction.rs:47-49` defines `redact_error()` which is **unused**.
- **Do:** Route `error_summary` through `redact_error()` (truncate + strip host paths/provider strings) before it lands in the capsule.
- **Gate:** Error summaries truncated/redacted; test with a host-path-laden error proves no leakage.

### S1-T4 · Populate profile_digest
- **Evidence of gap:** `src/hypervisor/mod.rs:900-903` records `policy.mode` + `profile_name` but sets `profile_digest = None`, despite `src/proof/receipt.rs:57-58` carrying `(name, toml_sha256)`.
- **Do:** Set `schema.rs` `profile_digest` (`:63`) from the receipt's `toml_sha256`.
- **Gate:** MCP proof includes a non-null `profile_digest` reflecting active profile.

### S1-T5 · MCP proof reference mode
- **Evidence of gap:** `src/bin/nexus_mcp.rs:598-600` & `:791-800` return the full `ProofCapsule` inline; `McpProofReference` (defined in `src/proof/receipt.rs`) is **never used**.
- **Do:** Default MCP responses to `McpProofReference` (proof ref + scorecard); return the full capsule only under a debug flag/env.
- **Gate:** Default `nexus_execute_proof` response carries ref+scorecard, not full capsule; debug mode returns full; test both.

### S1-T6 · No-secret construction-time tests
- **Evidence of gap:** `tests/proof_redaction.rs:140-155` only does negative serialization checks (secret not in JSON), no rejection at construction.
- **Do:** Add tests that feed API keys, absolute paths, raw tokens, and raw memory text through capsule construction and assert they are absent/redacted in the emitted capsule (depends on S1-T2/T3).
- **Gate:** Suite rejects all four secret classes; runs in CI.

**Sprint-1 dispatch note:** one Codex/lab implementation task per S1-T#, or a single batched task; each must keep `cargo test --workspace` green on Linux and not regress `tests/proof_*.rs`.

---

## SPRINT 2 — Capability-gated memory  *(repo: Nexus)*

**Context.** Capability lattice lives in `src/security/capability.rs`; the `Capability` enum (`:16-36`) currently has
`ReadFile, WriteFile, ListDirectory, HttpGet, HttpPost, ExecuteBinary, MountTmpfs, All, None` — **no memory variants**.
Attenuation (`attenuate()` `:264-308`, `is_subset_of()` `:88-96`), `nexus_attenuate_token` MCP tool
(`src/bin/nexus_mcp.rs:437-445`, impl `:983-1006`), and scope→AEON `agent_id` plumbing
(`src/daemon/mod.rs:71-81`, `crates/aeon_nexus_bridge/src/lib.rs:179-225`) are **already done** — extend, don't rebuild.

### S2-T1 · Add ReadMemory(scope) / WriteMemory(scope) variants
- **Do:** Extend `Capability` enum with `ReadMemory(MemoryScope)` and `WriteMemory(MemoryScope)`; define `MemoryScope` (agent/session/namespace selector). Wire into `is_subset_of()` lattice and `attenuate()` so sub-agents get narrower memory authority.
- **Gate:** Lattice + attenuation unit tests cover memory variants (subset, narrowing, depth cap).

### S2-T2 · MCP parsing + schema for memory capabilities
- **Evidence:** `src/bin/nexus_mcp.rs:80-96` `CapabilitySpec` parses `type` + optional `path` (file/HTTP only).
- **Do:** Extend `CapabilitySpec` parsing + the MCP tool input schema so agents can request `read_memory`/`write_memory` with a scope; document in MCP docs.
- **Gate:** An MCP client can request a memory capability; malformed/over-broad requests are rejected.

### S2-T3 · Enforce capability scope → AEON agent_id
- **Do:** Gate the `aeon_nexus_bridge` recall/write paths on a matching `ReadMemory`/`WriteMemory` capability whose scope maps to the request's `aeon_agent_id`. Cross-agent scope must deny.
- **Gate:** Recall/write without the matching memory capability is denied; cross-agent scope denied.

### S2-T4 · Memory-denial tests
- **Evidence:** `src/security/capability.rs:514-787` is the existing denial-test pattern (file/HTTP/exec).
- **Do:** Add denial tests for unauthorized memory read/write and cross-agent access.
- **Gate:** Unauthorized memory access denied in tests; runs in CI.

---

## SPRINT 3 — Finish-line packaging  *(repos: AEON-IQ, Nexus-IQ, Nexus)*

### S3-T1 · HNSW maintenance runbook + job  *(repo: AEON-IQ)*
- **Evidence:** index defined `migrations/0001_initial.sql` (`m=16, ef_construction=64`); archival in `src/archival.rs`; **no runbook/CLI**. pgvector guidance: reindex HNSW before long vacuums.
- **Do:** Add a worker-only, locked, observable maintenance job (reindex/vacuum cadence) + runbook doc. Must run only when `role.runs_workers()`.
- **Gate:** Worker-only locked maintenance job exists and is observable; runbook documents reindex-before-vacuum.

### S3-T2 · Missing docs  *(repo: Nexus-IQ; some content from Nexus)*
- **Have:** README, ARCHITECTURE, QUICKSTART, SECURITY, TROUBLESHOOTING.
- **Add:** `INSTALL.md`, `PROOF_CAPSULES.md` (schema + verification from `src/proof/`), `MEMORY_EVIDENCE.md` (from `aeon_nexus_bridge`), `POLICY.md` (capability profiles inc. new memory caps), `MCP.md` (client setup; 4 example configs already exist), `OPERATIONS.md` (Docker/keys/backups/HNSW), `CONTRIBUTING.md`.
- **Gate:** All listed docs present and accurate to current behavior.

### S3-T3 · Releases + images + version matrix  *(all repos)*
- **Do:** Publish GitHub Releases (notes + assets) for Nexus, AEON-IQ, Nexus-IQ; publish Docker images (GHCR) for `nexus`/`aeon-iq`/compose; publish a Nexus↔AEON↔Nexus-IQ version compatibility matrix. Checksums now; Sigstore later.
- **Gate:** `docker compose up` pulls published images; release notes + version matrix live.

### S3-T4 · (Optional) post_run_memory_diff → proof link  *(repo: Nexus)*
- **Evidence:** D is 5/6 done; diff is currently snapshot-based, not bound into the capsule.
- **Do:** If desired for audit completeness, add `post_run_memory_diff_id` + digest into the capsule's memory-evidence section.
- **Gate:** Capsule references the post-run memory diff; test proves linkage. *(Low priority; can defer past alpha.)*

---

## SPRINT 4 — Release gates + external alpha  *(all repos)*

### S4-T1 · CI test-matrix (from report §3 — it's good)
Wire across all three repos:
`cargo fmt --check` · `cargo clippy -- -D warnings` · `cargo test --workspace` · `cargo deny check all` ·
docker build (all images) · docker compose smoke (full stack) · no-secret scanner (logs/proofs/responses) ·
MCP smoke (`tools/list` + `nexus_execute_proof`) · proof verification (signed capsule validates).
- **Gate:** All gates green in CI on each repo.

### S4-T2 · Integration gates
AEON-unavailable advisory (execution continues, memory degraded) · AEON-unavailable attested (proof degraded/blocked) ·
Nexus-unavailable (clean error) · unsafe path (denied/negotiated) · safe workspace write (allowed) ·
tool failure (rollback + proof) · learned failure recalled next run · cross-agent memory (denied) ·
denial loop (hard stop after 2 rounds) · HNSW maintenance (worker-only, locked, observable).
- **Gate:** All integration scenarios pass against the compose stack.

### S4-T3 · External alpha
Invite 3-5 technical testers → collect install issues → record failure cases → patch docs/config → publish alpha notes.

---

## Dependency graph

```
S1 (proof hardening) ──┐
                       ├─► S4-T1/T2 (gates) ─► S4-T3 (alpha)
S2 (memory caps) ──────┤        ▲
                       │        │
S3-T1 HNSW ────────────┘        │
S3-T2 docs ────────────► (need S1/S2 done to document accurately)
S3-T3 releases ─────────────────┘ (after gates green)
S3-T4 diff link (optional, parallel)
```
- **Parallelizable now:** S1 and S2 (different modules in Nexus), S3-T1 (AEON-IQ), S3-T3 image build prep.
- **Blocked:** S3-T2 docs should follow S1/S2 (document final behavior); S3-T3 releases follow S4-T1 green.

## Out of scope (deferred — report agrees)
Dashboard UI · enterprise SSO · billing · full OpenTelemetry traces · Kubernetes/Helm · library extraction from AEON ·
full bidirectional memory rollback · hosted cloud · **bespoke REST gateway + OpenAPI** (superseded by MCP).

## Alpha readiness definition
Alpha-ready when: S1 done · S2 done · S3-T1 done · compose full-stack smoke passes · README+Quickstart accurate ·
no-secret tests pass. (Releases/images = beta gate, not alpha.)
