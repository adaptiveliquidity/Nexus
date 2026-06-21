# NexusIQ Security Review — P10 Findings

**Scope:** aeon-memory integration path (src/aeon.rs, src/bin/nexus_mcp.rs, src/security/negotiator.rs)
**Review date:** 2026-06-21
**Status:** All HIGH findings remediated for v1.0

## Findings

| ID | Severity | Title | File | Status |
|----|----------|-------|------|--------|
| H1 | HIGH | SSRF via unsanitized AEON_BASE_URL scheme | src/aeon.rs | Fixed: scheme validated in from_env() before struct construction |
| H2 | HIGH | Allowlist check after AEON memory recall | src/bin/nexus_mcp.rs | Fixed: allowlist check moved to top of do_nexus_iq_execute |
| H3 | MEDIUM | debug_assert! skipped in release builds | src/security/negotiator.rs | Fixed: changed to assert! |
| H4 | MEDIUM | HMAC key accepted below 32-byte minimum | src/aeon.rs | Fixed: from_env() rejects keys shorter than 32 bytes |
| H5 | MEDIUM | AeonConfig derived Debug exposes secrets | src/aeon.rs | Fixed: custom Debug impl redacts management_key and hmac_key |
| H6 | LOW | TimelineDeliveryStatus::Queued misleading name | src/aeon.rs | Fixed: renamed to FireAndForget; serde serializes as fire_and_forget |
| H7 | INFO | Behavior verified correct | src/security/negotiator.rs | No action: already correct |

## Residual and Out-of-Scope

The following items were identified but are explicitly out of scope for v1.0:

- **Token TOCTOU re-check at sandbox entry**: capability tokens are checked at request time; a re-check immediately before WASM instantiation would catch late-revocation. Deferred to post-v1.0.
- **preview_base64 explicit permission gate**: base64 previews in proof capsules are not gated on a separate explicit capability. Deferred to post-v1.0.

## References

- Full threat model: `docs/AEON_NEXUS_THREAT_MODEL.md`
- Proof capsule limitations: `docs/NEXUSIQ_PROOF_LIMITATIONS.md`
