# NexusIQ 100% Completion Plan

**Status:** Active — adopted 2026-06-20 as the authoritative plan to take NexusIQ from the
v0.2.0 integration milestone to a production-honest v1.0 (target 9.7–9.9/10).
**Source:** operator blueprint, reconciled against two independent completion audits and
verified against the real tree at `origin/main` (`d8c378c`).
**Lane:** Codex-direct implements (off-bill); Claude reviews/verifies/PRs/merges each phase.

## Adjustments verified against the codebase
- **P1.5 dedicated proof key: already done** (#107; `proof_key_is_separate_from_capability_key` passes). Verify & check off, do not rebuild. `DigestMode {Sha256Public, HmacSha256Private, RedactedNoDigest}` already exists — apply it to the capsule input digest.
- **P6 timeline schema v2 is in the AEON-IQ repo** (`adaptiveliquidity/AEON-IQ`, migration `0024`), not this tree. Separate PR.
- **CI feature gate (P8) lands right after P1** so all later phases are CI-verified on the `aeon-memory` path.
- **Forbidden-string / no-secret scan starts in P1 tests**, not only at release.

## Invariants (hold throughout)
1. Default build byte-identical (all integration code `#[cfg(feature = "aeon-memory")]`).
2. Fail-open (memory/timeline outage degrades auditability, never blocks execution).
3. Honest proof (advisory / attested-no-hit / attested-with-recall / degraded / absent — never overclaim).

## Phases

| # | Phase | Repo | Status |
|---|---|---|---|
| 0 | Baseline freeze + evidence manifest | Nexus | pending |
| 1 | Proof-capsule honesty hardening | Nexus | in progress |
| 1b | CI `aeon-memory` feature gate (moved up from P8) | Nexus | pending |
| 2 | Automatic AEON-IQ event forwarding (`AeonTimelineSink`) | Nexus | pending |
| 3 | Daemon proof + negotiation path | Nexus | pending |
| 4 | Canonical MCP `nexus_iq_execute` full-loop tool | Nexus | pending |
| 5 | MemoryEvidence v1 schema + verifier | Nexus/bridge | pending |
| 6 | AEON-IQ timeline schema v2 (branch model, digest chain) | **AEON-IQ** | pending |
| 7 | Full-loop integration test suite (stateful mock AEON) | Nexus | pending |
| 8 | CI hardening (no-secret scan, default-off API compat) | Nexus | pending |
| 9 | Performance benchmarks (measured, not target) | Nexus | pending |
| 10 | Security review + threat-model closure + SSRF/TOCTOU/preview fixes | Nexus | pending |
| 11 | Operator UX: `nexus iq` CLI (run/verify/timeline/incident/replay) | Nexus | pending |
| 12 | Release hardening v1.0 (gates, honest-language docs, tag) | both | pending |

## Per-phase exit criteria (anchors)

- **#baseline (P0):** `artifacts/nexusiq-baseline.json` records Nexus+AEON SHAs, Cargo.lock hashes, features, PRs, gaps, test/bench commands.
- **#honest-proof (P1):** no emitted capsule passes tests without non-empty `limitations`, populated `RedactionReport`, safe (HMAC/redacted) input digest for sensitive input, redacted `error_summary`, proof-key signature; forbidden-string test proves no raw error/path/token/`preview_base64` leaks; single `ProofCapsuleBuilder` is the only construction path.
- **#ci-gate (P1b/P8):** required CI builds+tests+clippy on `--features aeon-memory --locked` and the `aeon_nexus_bridge` crate, keeping the default-build jobs; no-secret scan over emitted capsules/logs; default-off API-shape test.
- **#g3 forwarding (P2):** events reach a mock AEON timeline with no caller action; outage never blocks (fail-open test); attested mode marks proof degraded on required-but-failed delivery; `nexus aeon replay-events` is idempotent.
- **#g1 daemon (P3):** a daemon execute with proof context returns a signed capsule + memory-evidence ref + negotiation/timeline events; `aeon_memory_evidence_digest` is consumed (or removed); legacy precompiled path unchanged.
- **#phase-4 MCP (P4):** one `nexus_iq_execute` MCP call performs recall→evidence→exec→proof→negotiation→timeline sink→response; profile denies without permission.
- **#memory-evidence (P5):** versioned `MemoryEvidenceV1` with `AttestedNoHit` vs `AttestedWithRecall`; `nexus aeon verify-memory-evidence` rejects raw memory and matches capsule digest.
- **#timeline-v2 (P6, AEON-IQ):** digest-chained, branch-aware timeline; time-travel creates a branch (never deletes); resolve-at-branch works.
- **#full-loop (P7):** one suite proves success+recall+proof, failure+rollback+capture, denial+negotiation, timeline delivery, attested-blocks-missing, advisory-degrades-missing, MCP wire — under `--features aeon-memory`.
- **#perf (P9):** p50/p95/p99 for recall, execution, proof, forwarding, negotiation, full roundtrip; mock vs local vs real reported separately; no doc claims a latency number without a bench artifact.
- **#security (P10):** SSRF egress filter, token TOCTOU re-check at sandbox entry, `preview_base64` gated behind explicit permission; `docs/NEXUSIQ_THREAT_MODEL.md` + `_SECURITY_REVIEW.md` + `_PROOF_LIMITATIONS.md`.
- **#release (P12):** all gates green; honest-language docs (no "proves correct execution"/"SLSA-compliant"/"zero-knowledge"); `v1.0` tag.

## Sequencing
P0 → P1 → P1b(CI) → {P2, P3 independent} → P4 → P5 → P6(AEON-IQ) → P7(needs P2–P6) → P8 → P9 → P10 → P11 → P12.
Checkpoints: after P1b (foundation), after P7 (loop proven = 9.5), after P10 (hardened = 9.7), after P12 (released = 9.9).

## Honest scoring ceiling
9.7–9.9 achievable in-repo. A literal 10/10 needs external users, third-party benchmark reproduction, and independent security review — out of scope for self-work.
