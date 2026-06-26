# AEON-IQ x Nexus Threat Model

**Status:** Phase 10 release hardening
**Feature gate:** `aeon-memory` only; default builds do not compile the AEON-IQ integration path.

This threat model covers the service-boundary integration between the Nexus hypervisor, the AEON-IQ proxy, and AEON-IQ Postgres. Nexus remains the execution authority. AEON-IQ remains the memory and timeline authority. The shared bridge crate carries only wire-level evidence records and digest helpers.

## Trust Boundaries

| Boundary | Threat | Mitigation | Residual risk |
| --- | --- | --- | --- |
| Nexus hypervisor to AEON-IQ proxy | AEON-IQ is unavailable, slow, or returns malformed memory data, causing execution to fail or block. | `src/aeon.rs` makes the memory client fail open: transport, timeout, HTTP-status, and response-shape failures return empty/no-op results. The integration exists behind `#[cfg(feature = "aeon-memory")]`. | Auditability can be reduced when memory recall or storage fails. The execution decision still proceeds according to Nexus policy and proof honesty records the memory mode. |
| AEON-IQ proxy to Postgres | The timeline or memory ledger is unavailable or stale. | Nexus returns execution events and proof evidence to the caller; AEON-IQ is responsible for persisting them to Postgres, including the `cognitive_hypervisor_timeline` table from AEON-IQ migration `0023`. | A ledger outage can leave gaps in AEON-IQ's historical timeline until the caller retries or compensates. Nexus does not use Postgres as an execution dependency. |
| Nexus to shared bridge crate | Shared evidence encoding diverges between the two systems. | `crates/aeon_nexus_bridge` owns `MemoryEvidenceRef`, `AgentSessionMapping`, canonical JSON bytes, SHA-256 digests, and HMAC-SHA256 agent handles. | Both systems must continue using the same bridge version or compatible wire schema during staged deploys. |

## HMAC Key Provisioning

| Threat | Mitigation | Residual risk |
| --- | --- | --- |
| A hardcoded, logged, or mismatched HMAC key makes memory evidence unverifiable or exposes the tenant identifier. | Nexus reads the shared HMAC key only from `NEXUS_AEON_HMAC_KEY` in `src/aeon.rs`. The key is parsed as hex and is never generated from a fallback constant. When absent or empty, `build_memory_evidence_ref` returns `MemoryAttestationMode::Absent` with no evidence reference. AEON-IQ must be provisioned with the same key for its side of migration `0023` timeline/evidence verification. | Operators can still misconfigure one side during rotation. In that case Nexus fails open and records `Absent` or `Degraded` instead of pretending evidence is attested. |
| The management API key is confused with the HMAC key. | `NEXUS_AEON_MANAGEMENT_KEY` authorizes calls to AEON-IQ management endpoints; `NEXUS_AEON_HMAC_KEY` binds proof evidence. They are separate environment variables and should be rotated independently. | A leaked management key can allow unauthorized AEON-IQ management calls, but it does not let an attacker recompute HMAC evidence without the HMAC key. |

## Agent And Session Correlation

| Threat | Mitigation | Residual risk |
| --- | --- | --- |
| Raw AEON-IQ tenant agent identifiers leak through proof evidence. | `crates/aeon_nexus_bridge::AgentSessionMapping` computes an HMAC-SHA256 `agent_handle`. The proof-facing `MemoryEvidenceRef` carries the digest and optional session id, not the raw AEON-IQ agent id as the evidence handle. Runtime management and timeline API calls still address the AEON-IQ tenant by agent id, so those channels must stay authenticated operational traffic. | The optional session id remains visible in proof evidence. Operators should choose non-secret session ids or treat proof capsules as sensitive audit artifacts. |
| Nexus and AEON-IQ use different meanings for `agent_id`. | The bridge explicitly maps Nexus local context to AEON-IQ's memory-tenant namespace instead of reusing snapshot-lineage identifiers. `src/aeon.rs` currently builds the mapping from the configured AEON-IQ `agent_id` and the tool/config session id. | The mapping remains an operational contract: callers must pass the intended AEON-IQ agent/session values when using daemon or MCP correlation paths. |

## Denial Negotiation Loop

| Threat | Mitigation | Residual risk |
| --- | --- | --- |
| Memory recall suggests broader permissions or an infinite negotiation loop after `CapabilityDenied`. | `src/security/negotiator.rs` caps negotiation at `MAX_NEGOTIATION_ROUNDS = 2`, builds candidates only from the original capability set, requires a strict subset, rejects unchanged attempts, and returns `None` on exhaustion. `src/hypervisor/mod.rs` only accepts a negotiated outcome if the caller's existing tokens authorize that narrowed subset. | The negotiator can decline a useful path if memory recall is incomplete or ambiguous. Exhaustion fails closed for the denied capability request by returning the original `CapabilityDenied`. |
| AEON-IQ memory becomes an escalation oracle. | The negotiator never mints new capabilities and never accepts suggestions outside the original requirement set. It narrows only. | A malicious memory entry can still influence which allowed subset is attempted, so operators should treat AEON-IQ memory as advisory input, not policy authority. |

## Fail-Open Ledger

| Threat | Mitigation | Residual risk |
| --- | --- | --- |
| AEON-IQ timeline persistence blocks Nexus execution. | Nexus emits events such as `capability_denied`, `snapshot_created`, and `proof_capsule_emitted` through `src/daemon/mod.rs` and the MCP `nexus_aeon_execute_timeline` tool, but persistence to AEON-IQ's `POST /agents/:id/timeline` endpoint is best-effort outside the Nexus hot path. | If the caller never forwards or retries events, AEON-IQ's timeline can be incomplete. The Nexus proof capsule still exists independently. |
| Time-travel lookup selects the wrong snapshot for a timestamp. | AEON-IQ's `GET /agents/:id/timeline/at` endpoint should resolve timestamp to timeline rows that include Nexus snapshot/proof identifiers from migration `0023`; Nexus snapshots remain id-addressed. | Clock skew and incomplete timeline ingestion can make timestamp lookups stale or ambiguous. Consumers should prefer exact snapshot/proof ids when available. |

## Timeline Chain Integrity Boundary (v1.0.0)

The timeline event chain (`TimelineEventBody.prev_event_digest`) uses unkeyed SHA-256 to link consecutive events. This provides tamper-evidence against accidental corruption and passive observers who cannot recompute hashes. It does NOT provide tamper-proof integrity against an active adversary with write access to the timeline spool: such an attacker can recompute all SHA-256 digests and re-link a fully valid forged chain undetectably. Additionally, the genesis event carries `prev_event_digest = None`, providing no external anchor for the chain head.

This limitation is acceptable for v1.0.0 because the timeline is fire-and-forget advisory (non-blocking, non-authoritative). Operators who require stronger guarantees should treat the spool as an untrusted append-only log and anchor the chain head to an external commitment. HMAC-signing the chain head using `NEXUS_AEON_HMAC_KEY` is planned for v1.1.0.

## Proof Honesty

| Threat | Mitigation | Residual risk |
| --- | --- | --- |
| A proof capsule overstates AEON-IQ memory integrity. | `src/proof/schema.rs` records `MemoryAttestationMode`: `Advisory`, `Attested`, `Degraded`, or `Absent`. `src/aeon.rs` returns `Attested` only when an HMAC-bound `MemoryEvidenceRef` is built; missing HMAC returns `Absent`; construction failures return `Degraded`; AEON context without evidence remains `Advisory`. | Verifiers must check `memory_mode` and not infer attestation from AEON fields alone. |
| Default users unknowingly depend on AEON-IQ behavior. | All AEON-specific fields, daemon correlation, negotiator behavior, and MCP timeline execution are compiled only with `aeon-memory`; default builds omit the fields entirely. | Feature-enabled deployments must still configure keys and endpoints correctly. The conformance suite in `tests/aeon_conformance.rs` guards the default-off shape and fail-open proof behavior. |
