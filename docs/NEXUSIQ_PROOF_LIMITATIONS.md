# NexusIQ Proof Capsule — Honest Limitations

**Status:** v1.0 — accurate as of this release

## What a Proof Capsule IS

A proof capsule is a signed, redacted attestation artifact that records observable
facts about a single WASM tool execution inside the Nexus sandbox. It provides:

- The tool identity, public module digest, input digest treatment, and capability
  policy profile in effect
- The execution outcome as captured by optional failure evidence, rollback
  evidence, or branch race evidence; successful runs have no failure evidence
- A redaction report listing which fields were hashed, HMAC-bound, truncated, or
  removed
- A non-empty `limitations` array describing what the capsule does not prove
- An optional Ed25519 signature binding the capsule payload to a proof key
- If `aeon-memory` is enabled: `MemoryAttestationMode` and, when available, an
  HMAC-bound `MemoryEvidenceRef` for AEON-IQ memory evidence

The proof digest types support public SHA-256, private HMAC-SHA-256, and
redacted/no-digest modes. In the current proof execution path, WASM input bytes
are treated as sensitive: they are HMAC-SHA-256 when a proof HMAC key is
configured, and redacted when no proof HMAC key is available. The capsule does
not include raw input.

With `aeon-memory`, memory evidence can be reported as `Advisory`, `Attested`,
`AttestedNoHit`, `AttestedWithRecall`, `Degraded`, or `Absent`. `Attested` is
used when a precomputed HMAC-bound memory evidence digest is attached from an
execution receipt. `AttestedNoHit` and `AttestedWithRecall` distinguish whether
a successful AEON-IQ recall or evidence-building path had zero hits or one or
more hits; when those hits are attached to a capsule as `MemoryEvidenceRef`, the
reference is HMAC-bound. `Degraded` means Nexus attempted the memory-evidence
path but could not fully attest it. `Absent` means the memory sidecar or required
HMAC evidence was not configured or not available.

## What a Proof Capsule IS NOT

| Claim | Reality |
|-------|---------|
| Proves correct execution | Records observable runtime facts; does not verify program logic correctness |
| Formal proof of behavior | Not a theorem-proving artifact; the sandbox observes, it does not verify |
| SLSA-compliant | Not SLSA-compliant; no build provenance chain, no hermetic build |
| Enables deterministic replay | The input digest treatment is present; full input is not stored in the capsule |
| Proves runtime integrity | The signature proves Nexus emitted the capsule with the configured proof key; it does not prove the runtime binary is uncompromised |
| Zero-knowledge | All proofs are record-and-redact, not zero-knowledge; RFC 0003 documents the ZK option as not recommended at this time |
| Cryptographically proves memory truth | HMAC-bound memory evidence proves Nexus attached an AEON-IQ memory reference; it does not prove the underlying memory content is accurate or complete |

## Scoring Ceiling

An independently deployed, self-certified system can reach approximately
9.7-9.9 out of 10 on a NexusIQ completeness rubric. A literal 10/10 requires
third-party benchmark reproduction, external users, and an independent security
review, which are out of scope for self-work.
