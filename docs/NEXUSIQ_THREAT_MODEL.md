# NexusIQ Threat Model Summary

**Status:** v1.0
**Scope:** NexusIQ execution path: caller -> `nexus_mcp` -> AEON-IQ recall ->
Nexus sandbox -> proof capsule -> AEON-IQ timeline.

This summary covers the NexusIQ release path only. The full boundary table and
long-form rationale live in `docs/AEON_NEXUS_THREAT_MODEL.md`. Nexus remains the
execution authority; AEON-IQ remains the memory and timeline authority. All
NexusIQ-specific integration behavior is compiled only with the `aeon-memory`
feature, and default builds omit the AEON-IQ path.

## Top Trust Boundaries

1. **Caller to `nexus_mcp` correlation:** callers must provide the intended
   AEON-IQ agent and session values; residual risk is wrong operational mapping
   that attaches proof or timeline records to the wrong tenant or session.
2. **`nexus_mcp` to AEON-IQ recall:** memory search fails open on transport,
   timeout, HTTP-status, and response-shape failures; residual risk is reduced
   auditability or missing recall context, not blocked Nexus execution.
3. **AEON recall to sandbox policy:** recalled memory is advisory and may only
   influence narrowing during denial negotiation; residual risk is malicious or
   stale memory steering which already-authorized subset is retried.
4. **Sandbox to proof capsule:** the capsule records observed execution facts,
   redactions, limitations, signature state, and memory mode; residual risk is a
   verifier treating those records as proof of runtime integrity or logic
   correctness.
5. **Proof and events to AEON-IQ timeline:** timeline delivery is best-effort or
   explicitly attested by mode, while the proof capsule exists independently;
   residual risk is incomplete or stale AEON-IQ history if events are not
   forwarded, retried, or resolved against exact proof/snapshot identifiers.

## P10 Mitigations

- H1: `AeonConfig::from_env()` validates `NEXUS_AEON_BASE_URL` as `http` or
  `https` before constructing the client configuration.
- H2: `do_nexus_iq_execute` checks the NexusIQ tool allowlist before AEON-IQ
  configuration, recall, decoding, or execution.
- H3: denial-negotiation invariants use `assert!` so release builds keep the
  "strict subset of original requirements" checks.
- H4: `NEXUS_AEON_HMAC_KEY` must decode to at least 32 bytes before use.
- H5: `AeonConfig` uses a custom `Debug` implementation that redacts the
  management key and HMAC key.
- H6: offline timeline status is named `FireAndForget` and serializes as
  `fire_and_forget`.
- H7: denial-negotiation behavior was reviewed and verified correct without a
  code change.

## References

- Full threat model: `docs/AEON_NEXUS_THREAT_MODEL.md`
- Security review findings: `docs/NEXUSIQ_SECURITY_REVIEW.md`
- Proof capsule limitations: `docs/NEXUSIQ_PROOF_LIMITATIONS.md`
