# RFC 0005 — Nexus Runtime Proof Capsules

**Status:** Proposed  
**Author:** Nexus Team  
**Created:** 2026-06-17  
**Depends on:** RFC 0004 (Capability Profile Manifests)

---

## 1. Summary

This RFC introduces the **Nexus Runtime Proof Capsule**: a signed, redacted attestation artifact that records observable facts about a single tool execution inside the Nexus WASM sandbox. It is a Nexus-native runtime attestation, inspired by software provenance standards and designed to be Sigstore/in-toto compatible in later versions. A proof capsule does **not** prove correct execution, does not enable deterministic replay, and is not SLSA-compliant; it is an auditable record bounded by the Nexus runtime and the proof key.

---

## 2. Motivation

AI agents that execute untrusted WASM tools need an audit trail: what ran, with what input, under what capability policy, with what outcome. Existing telemetry (logs, traces) is operator-internal; there is no portable, verifiable artifact that a downstream system can inspect without trusting the operator's runtime. A proof capsule provides:

- **Auditability** — an operator or auditor can verify what the runtime recorded about an execution.
- **Capability evidence** — which capabilities were required and granted, and whether there was a mismatch.
- **Failure and rollback evidence** — typed failure category and whether rollback occurred, without leaking secrets.
- **Redaction by default** — host paths, tokens, env values, and error strings are redacted before signing so the capsule is safe to share.
- **MCP composability** — the MCP server can return a `McpProofReference` alongside normal tool output, enabling downstream verification without changing the existing `ToolOutput` contract.

---

## 3. Frozen Schema

Schema field names are frozen after Wave 1 and may not be changed without a versioned RFC amendment. All types derive `Serialize` and `Deserialize`; no `deny_unknown_fields` (forward-compatible).

### 3.1 Top-level capsule

```
ProofCapsule {
    version: String,                       // always "1"
    capsule_id: Uuid,
    subject: ProofSubject,
    tool: ToolIdentity,
    input: InputIdentity,
    policy: PolicyProfileRef,
    capabilities: CapabilityEvidence,
    snapshot: Option<SnapshotEvidence>,
    failure: Option<FailureEvidence>,
    rollback: Option<RollbackEvidence>,
    branches: Option<BranchRaceEvidence>,
    redaction: RedactionReport,
    limitations: Vec<String>,              // mandatory, non-empty
    signature: Option<SignatureEnvelope>,
}
```

### 3.2 Subject and identity

```
ProofSubject { run_id, tool_name, started_at, finished_at, duration_ms }
ToolIdentity { module_digest: TypedDigest, module_name, entrypoint }
InputIdentity { digest: TypedDigest, media_type, raw_included: bool }
  // raw_included is always false in v1; field is present for forward-compat
```

### 3.3 Digest types

```
TypedDigest { algorithm: String, value: String, public_recomputable: bool }
DigestMode { Sha256Public | HmacSha256Private | RedactedNoDigest }
```

`public_recomputable = false` for any HMAC'd field. `RedactedNoDigest` is used when no key is available and the value is low-entropy (must never be SHA-256'd directly).

### 3.4 Evidence types

```
SnapshotEvidence { snapshot_id, snapshot_kind: SnapshotKind, memory_digest, original_size, compressed_size }
SnapshotKind { LatestRuntime | EmptyBaseline | Diff }
FailureEvidence { failure_category, requires_rollback, deterministic: Option<bool>, error_summary }
RollbackEvidence { occurred, from_snapshot_id: Option<Uuid>, reason: Option<String> }
BranchRaceEvidence { source_snapshot_id: Option<Uuid>, winner_branch_id, branches_tried, branches_succeeded }
  // v1: winner + counts only; loser branch details NOT retained
CapabilityEvidence { required: Vec<String>, granted: Vec<String>, mismatch: Option<Vec<String>> }
RedactionReport { hashed_fields, truncated_fields, removed_fields, hmac_fields }
SignatureEnvelope { signer, key_id, signature: String, signed_payload_digest: TypedDigest }
```

### 3.5 Receipt and mode types

```
ExecutionReceipt {
    run_id, started_at, finished_at, tool_name, entrypoint,
    module_sha256, input_sha256, input_bytes_len,
    required_caps, granted_caps,
    policy_mode: PolicyEnforcementMode,
    profile: Option<(name, toml_sha256)>,
    snapshot: Option<SnapshotEvidence>,
    failure: Option<FailureModeLite>,
    rollback: Option<(bool, Uuid, String)>,
    branches: Option<BranchRaceEvidence>,
}
ProofCaptureMode { Disabled | ReceiptOnly }
ProofHmacKey { Disabled | FromEnv(env_var_name) | EphemeralTestOnly }
McpProofReference { capsule_digest: TypedDigest, artifact_id: Option<String>, inline_summary: ProofScorecard }
ActiveCapabilityProfile { manifest_name, source_digest: TypedDigest, source_path_redacted: Option<String> }
```

---

## 4. Redaction Model

The capsule is redacted **before** it is signed. The signed payload is the redacted capsule (with `signature: None`). Raw values are never included.

| Field class | Treatment | DigestMode |
|---|---|---|
| Host filesystem path | HMAC (with key) or placeholder string | `HmacSha256Private` / `RedactedNoDigest` |
| Capability token / secret | ID-only string (e.g. `token:<id>`) | — |
| Environment variable value | Removed entirely | `RedactedNoDigest` |
| Error string | Truncated to 256 chars | — |
| WASM input bytes | SHA-256 (public) | `Sha256Public` |
| WASM module bytes | SHA-256 (public) | `Sha256Public` |
| Snapshot memory | SHA-256 (public, from `Snapshot.memory_checksum`) | `Sha256Public` |

The `RedactionReport` records which fields were hashed, truncated, removed, or HMAC'd.

**Critical:** raw prompt content, raw error messages beyond 256 chars, `preview_base64`, and any value that could leak a credential must never appear in an emitted capsule. See §8 for the `preview_base64` specific hazard.

---

## 5. Signing Model

- **Algorithm:** Ed25519 (via `ed25519-dalek 2.1`).
- **Key source:** A **dedicated proof key** separate from the capability signing key. Provided as a base64-encoded 32-byte seed via `NEXUS_PROOF_SIGNING_KEY` env var, `--proof-signing-key` CLI flag, or verified with `--proof-public-key`. Must not reuse the capability key.
- **What is signed:** Canonical JSON of `ProofCapsule { signature: None }` (i.e., the capsule with the signature field set to `None` before serialization). The `signature` field is excluded from the digest.
- **Canonical JSON:** stable key ordering over the entire capsule structure.
- **`SignatureEnvelope`:** contains `signer`, `key_id`, `signature` (base64), and `signed_payload_digest` (SHA-256 of the signed bytes).
- **Verification:** recompute canonical JSON → SHA-256 → Ed25519 verify against public key; also verify that `required_caps ⊆ granted_caps`.

---

## 6. HMAC Key Policy

`ProofHmacKey` controls how low-entropy sensitive values (paths, prompt fragments, env var names) are handled:

| Key state | Low-entropy value treatment |
|---|---|
| `Disabled` | `RedactedNoDigest` — value replaced with placeholder, `public_recomputable: false` |
| `FromEnv(var_name)` | `HmacSha256Private` — HMAC-SHA-256 with key from env var; `public_recomputable: false` |
| `EphemeralTestOnly` | Same as `FromEnv` but key is generated in-process; not stable across runs |

**Invariant:** A low-entropy secret (path, prompt, env value, token) MUST NEVER be passed directly to SHA-256. SHA-256 on a low-entropy value is not a redaction — it is a preimage-attack risk. Use HMAC or `RedactedNoDigest`.

---

## 7. PolicyEnforcementMode

Records what enforcement was actually active at runtime. Truthful mapping:

| Mode | When emitted | Meaning |
|---|---|---|
| `UnprofiledDev` | CLI without `--profile` | No profile loaded; no capability enforcement beyond runtime defaults |
| `ProfileValidatedOnly` | Profile parsed but not enforced | Profile was validated at load time only |
| `ProfileLoadedMcp` | MCP `nexus_execute` with active profile | Profile loaded; advisory check performed |
| `ProfileEnforcedMcpCapabilitiesOnly` | MCP `nexus_execute_wasi` with active profile | Capability allowlist enforced; tool-name list not enforced |
| `ProfileEnforcedMcpToolAndCapability` | **RESERVED — never emit in v1** | Both tool names and capabilities enforced; not implemented |
| `ProfileEnforcedRuntime` | **RESERVED — never emit in v1** | Runtime-level enforcement; not implemented |

The two RESERVED modes must not be emitted by any v1 code path. Their presence in the enum is for forward-compatibility in deserialization only.

---

## 8. `preview_base64` Handling

**⚠ Important:** `preview_base64` is **not** confined to private-debug surfaces in the current codebase. It is a field on `RestoredMemorySummary` (`src/bin/nexus_mcp.rs:291-296`) and is reachable through the public MCP `nexus_snapshot_rollback` response when `include_restored_state: true` is requested (`:79-87`, `:276-281`).

Any code that constructs a `ProofCapsule` or `McpProofReference` **must actively exclude** `preview_base64`:
- It must not appear in any capsule field.
- It must not appear in `McpProofReference.inline_summary` or any returned JSON.
- The `RedactionReport.removed_fields` list should record it if it was present in the source data and had to be suppressed.

The forbidden-field test suite (Wave 2, `nexus_security_auditor`) must assert that `preview_base64` is absent from all serialized capsule output.

---

## 9. `ProofCaptureMode::Disabled` Guarantee

`ProofCaptureMode::Disabled` is the **default**. When active:

- `execute_tool` (the existing public function) is unchanged in signature and semantics.
- No `ExecutionReceipt` is constructed.
- No `ProofCapsule` is built, signed, or emitted.
- The `ToolOutput` returned by `execute_tool` is identical in shape and content.
- Callers of `execute_tool` who do not opt into proof capture are unaffected.

Proof capture is only activated by calling `execute_tool_proof` (returns `(ToolOutput, ExecutionReceipt)`) with `ProofCaptureMode::ReceiptOnly`. The existing `execute_tool` entry point must not be altered.

---

## 10. Limitations and Threat Boundary

### Mandatory limitations (all must appear in every emitted capsule)

```
"does_not_prove_external_side_effects_absent"
"does_not_include_raw_snapshot_memory"
"does_not_restore_stack_or_registers"
"execution_state_is_memory_globals_and_table_metadata"
"blocked_sync_wasi_io_cancellation_is_cooperative"
"proof_trusts_nexus_runtime_and_host_boundary"
```

### Threat boundary

The capsule is trustworthy only within the following boundary:

- **Trusted:** the Nexus runtime process, the proof key (NEXUS_PROOF_SIGNING_KEY), and the WASM sandbox.
- **Not covered:** the host OS, the filesystem beyond the WASM sandbox, external network calls made by the tool, or any side effects that occur outside the sandbox.
- **Not covered:** correctness of the tool's logic — only that the tool ran with the recorded inputs, capabilities, and produced the recorded outcome category.

A valid signature proves the capsule was produced by a Nexus process holding the proof key. It does not prove the tool is correct, safe, or side-effect-free.

---

## 11. Future Directions (v2)

These are **not** part of v1 and must not be claimed as current functionality:

- **Sigstore/cosign signing** — replace the direct Ed25519 key with a Sigstore bundle, enabling keyless signing via OIDC.
- **in-toto predicate v2** — wrap the capsule as an in-toto attestation predicate for supply-chain integration.
- **OpenTelemetry trace export** — map `ExecutionReceipt` fields to OTel span attributes for observability pipeline integration.
- **Loser branch evidence** — retain `BranchOutcome` for all branches in `BranchRaceEvidence`, not just the winner.
- **Streaming capsule** — emit partial receipts for long-running tools without waiting for completion.

---

*External standard references (informational only — not compliance claims):*  
*[SLSA provenance](https://slsa.dev/provenance) · [in-toto attestation](https://github.com/in-toto/attestation/blob/main/spec/README.md) · [Sigstore/cosign](https://docs.sigstore.dev/quickstart/quickstart-cosign/) · [OpenTelemetry traces](https://opentelemetry.io/docs/concepts/signals/traces/) · [MCP security best practices](https://modelcontextprotocol.io/docs/tutorials/security/security_best_practices)*