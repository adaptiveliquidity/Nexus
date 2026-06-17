# Nexus Breakthrough Backlog

Date: 2026-06-16
Branch: `research/breakthrough-backlog`
Mode: research-only; no implementation in this PR.

## Evidence Basis

Local evidence checked before ranking:

- `src/security/capability.rs` has Ed25519 capability tokens, attenuation, path subset checks, and lifecycle tests.
- `src/bin/nexus_mcp.rs` exposes the MCP stdio tool surface and now gates `wasm_path` through `NEXUS_MCP_MODULE_DIR`.
- `src/bin/nexus_agentd.rs` supports optional `NEXUS_AGENTD_AUTH_TOKEN`, prefers `wasm_bytes`, and gates fallback `wasm_path` with `NEXUS_AGENTD_MODULE_DIR`.
- `src/sandbox/pool.rs`, `tests/sandbox_pool.rs`, and `benches/density_validation.rs` provide the opt-in pool and manual density harness.
- `src/snapshot/sync/*` and `tests/snapshot_sync_*` provide local snapshot digest, framing, lineage, protocol, and loopback coverage.
- `tests/wasm_call_stack_capture.rs` verifies diagnostic trap call-stack capture without changing snapshot digest semantics.

External primary-source anchors:

- [Wasmtime Component Model docs](https://component-model.bytecodealliance.org/running-components/wasmtime.html): Wasmtime supports WASI Preview 2 components for `wasi:cli/command` and `wasi:http/proxy`.
- [WASI.dev](https://wasi.dev/): WASI starts with no ambient authority and hosts grant capabilities explicitly.
- [Wasmtime pooling allocator docs](https://docs.wasmtime.dev/api/wasmtime/struct.PoolingAllocationConfig.html): pooling preallocates instance resources to improve instantiation speed and parallel scalability.
- [Bytecode Alliance Wizer](https://github.com/bytecodealliance/wizer): pre-initializes a Wasm module and snapshots the initialized state into a new module.
- [Model Context Protocol specification](https://modelcontextprotocol.io/specification/2025-06-18): MCP is an open protocol for connecting LLM apps to external data sources and tools.

## Ranking Method

Items are ordered by lowest risk/work and highest likely product impact first. "Breakthrough" here means a capability that materially improves Nexus' defensibility as an agent runtime, not a speculative rewrite.

| Rank | Idea | Impact | Work | Risk | Why this first |
| --- | --- | --- | --- | --- | --- |
| 1 | Capability Profile Manifests | Very high | Low | Low | Converts security posture into testable, reviewable artifacts without changing runtime semantics. |
| 2 | Runtime Proof Capsule | Very high | Low-Med | Low | Makes executions auditable: Wasm digest, inputs, tokens, trace, and environment in one signed/verifiable bundle. |
| 3 | MCP Security Conformance Suite | High | Low | Low | Directly hardens the live agent surface and prevents regression of recent path/token fixes. |
| 4 | Pooling Allocator + Density Truth | High | Medium | Medium | Connects existing pool/density harness to Wasmtime's allocator model and replaces anecdotes with scaling data. |
| 5 | Snapshot Sync Two-Daemon MVP | High | Medium | Medium | Turns local protocol tests into visible distributed behavior while staying inside a trusted local network model. |
| 6 | WASI Preview 2 Component Lane | High | Medium | Medium | Aligns Nexus with the current WASI/component ecosystem and opens language-neutral tool packaging. |
| 7 | Wizer-Style Preinitialized Tool Cache | Medium-High | Medium | Medium | Could improve startup for initialization-heavy tools while staying benchmark-gated and reversible. |
| 8 | Diagnostic Call-Stack Symbolication | Medium | Low-Med | Low | Improves developer/debug UX without changing security boundaries or snapshot semantics. |
| 9 | Least-Disclosure Attenuation Playbook | Medium | Low | Low | Captures the near-term alternative to ZK capability attestation with less cost and lower implementation risk. |
| 10 | Policy-Driven Denial Explainers | Medium | Medium | Medium | Better operator feedback for denied tool runs, but must avoid leaking sensitive paths or suggesting unsafe grants. |

## 1. Capability Profile Manifests

**Description:** Add a reviewed manifest format that declares allowed tools, required capabilities, allowed module directories, token validity ceilings, and whether daemon auth is required for a deployment profile.

**Benefit:** Security posture becomes diffable and testable. Reviewers can reason about "what an agent may do" without reading all call sites.

**Evidence:** Current logic already has the primitives: `Capability`, `CapabilityToken::attenuate`, MCP capability request sanitization, `NEXUS_MCP_MODULE_DIR`, `NEXUS_AGENTD_AUTH_TOKEN`, and `NEXUS_AGENTD_MODULE_DIR`.

**Implementation sketch:**

- Add `nexus profile validate <profile.toml>` as a read-only validator first.
- Define fields for tool names, capability scopes, max token validity, module dirs, and daemon auth mode.
- Generate negative tests from the manifest: missing token, too-broad token, outside module dir, expired token.

**Risk:** Overfitting a config format before deployment needs are known.

**Validation:** Unit tests for parser and generated cases; integration tests that run MCP/daemon denial paths from a sample profile.

**First safe PR slice:** Add `docs/rfcs/0004-capability-profiles.md` plus one sample profile and a no-runtime validator prototype behind a CLI subcommand.

## 2. Runtime Proof Capsule

**Description:** Emit a portable proof bundle for an execution: Wasm hash, input hash, capability-token fingerprints, trace digest, snapshot digest, runtime config, benchmark/environment metadata, and validation status.

**Benefit:** Converts "Nexus ran this safely" into an inspectable artifact for PRs, audits, demos, and downstream agent orchestration.

**Evidence:** The repo already has snapshot digests, trace hashes, capability tokens, benchmark evidence, and Sigstore-style benchmark provenance.

**Implementation sketch:**

- Define a `ProofCapsule` struct and JSON schema.
- Add opt-in `--proof-out <path>` for CLI/daemon/MCP paths without changing default behavior.
- Hash sensitive values; do not serialize full token secrets or file contents.

**Risk:** Accidentally leaking file paths, prompts, or sensitive token material.

**Validation:** Snapshot tests for redaction; integration test that runs a trivial module and verifies capsule hashes match recomputation.

**First safe PR slice:** Docs + struct + pure unit tests only; no CLI write path until redaction review passes.

## 3. MCP Security Conformance Suite

**Description:** Add a dedicated MCP security test suite that validates module-dir allowlisting, token validity clamping, `Capability::All` rejection, malformed JSON-RPC behavior, and stdio/tool-surface invariants.

**Benefit:** MCP is a live agent boundary. A conformance suite makes recent security fixes permanent and reviewable.

**Evidence:** `tests/mcp_server.rs` already covers initialization, tool listing, issue-token, WASI grant regression, and module-dir allowlist cases.

**Implementation sketch:**

- Group MCP tests under a named module or new `tests/mcp_security.rs`.
- Add table-driven negative cases for path traversal, symlink escape, invalid capability strings, excessive validity, and missing module dir.
- Add a small protocol transcript fixture for review.

**Risk:** Tests that depend on local filesystem quirks may be flaky across Windows/WSL/Linux.

**Validation:** Run on Linux and Windows CI; include symlink-specific tests behind `#[cfg(unix)]` where needed.

**First safe PR slice:** Move existing tests into clearer security sections and add one negative transcript test. No runtime change.

## 4. Pooling Allocator + Density Truth

**Description:** Connect the manual density harness to explicit Wasmtime pooling-allocator configuration and metrics, then publish pooled vs non-pooled measurements without claiming superiority until data supports it.

**Benefit:** Stronger performance story with fewer overclaims. Pooling is a natural fit for Nexus, but the proof needs measured density data.

**Evidence:** `SandboxPool`, `PoolConfig`, `tests/sandbox_pool.rs`, and `benches/density_validation.rs` exist. Wasmtime docs state pooling preallocates resources to improve instantiation speed and scalability.

**Implementation sketch:**

- Add an opt-in `pooling-allocator` feature profile if not already active.
- Extend `density_validation` output with allocator settings and host memory envelope.
- Keep it manual/nightly; do not put 1000+ concurrency in normal PR gates.

**Risk:** Pooling reserves virtual memory and may behave differently across hosts.

**Validation:** Manual run matrix at 16/64/256/1000 concurrency; compare RSS/virtual memory, p50/p95/p99 latency, failures, and permit-return correctness.

**First safe PR slice:** Add a density runbook and machine-readable output schema. No performance claim changes.

## 5. Snapshot Sync Two-Daemon MVP

**Description:** Turn the local snapshot sync protocol into a two-process local MVP: daemon A advertises, daemon B wants, framed transport transfers, lineage detects forks.

**Benefit:** Makes distributed snapshot sync demonstrable instead of only module-tested.

**Evidence:** `src/snapshot/sync/*` and `tests/snapshot_sync_*` already cover digest, framed transport, lineage, protocol, and in-memory pair behavior.

**Implementation sketch:**

- Add a local-only `nexus snapshot-sync serve` / `replicate` experiment or example.
- Use authenticated framing and a temp dir store.
- Keep trust-domain assumptions explicit; no WAN/Byzantine claims.

**Risk:** Network/demo scope could grow into transport architecture prematurely.

**Validation:** Two-daemon integration test on localhost, tamper rejection, duplicate idempotency, fork detection, and replay determinism after replicated restore.

**First safe PR slice:** Example + test harness that uses in-memory or localhost transport, marked experimental.

## 6. WASI Preview 2 Component Lane

**Description:** Add an experimental path for running WASI Preview 2 components in addition to current Preview 1 module execution.

**Benefit:** Component Model support would make Nexus more language-neutral and align it with current WASI direction.

**Evidence:** Wasmtime is the reference Component Model implementation and supports `wasi:cli/command` and `wasi:http/proxy`; WASI capability design matches Nexus' explicit-grant model.

**Implementation sketch:**

- Start with read-only research spike and an example component.
- Map Nexus capability tokens to component-world imports deliberately.
- Keep Preview 1 API stable; add Preview 2 behind feature flag.

**Risk:** New ABI/model complexity and dependency churn.

**Validation:** Compile and run one Rust component and one non-Rust component if available; test no ambient filesystem/network access without explicit grants.

**First safe PR slice:** RFC + dependency/API spike behind `wasi-components` feature, no default-on path.

## 7. Wizer-Style Preinitialized Tool Cache

**Description:** Explore preinitializing expensive Wasm tools into cached modules/snapshots, using Wizer-style build-time initialization or a Nexus-native equivalent.

**Benefit:** Could reduce first-use latency for initialization-heavy agent tools without weakening sandbox isolation.

**Evidence:** Wizer pre-initializes modules by running an init function and snapshotting the initialized state into a new module. Nexus already has module cache and snapshot primitives.

**Implementation sketch:**

- Benchmark a real initialization-heavy module before changing code.
- Compare plain module cache vs preinitialized artifact vs Nexus snapshot restore.
- Store provenance: original Wasm hash, init function, toolchain, and generated artifact hash.

**Risk:** Preinitialization can freeze environment assumptions or accidentally bake sensitive state.

**Validation:** Determinism tests, secret-scanning generated artifacts, benchmark comparison, and bit-for-bit rebuild checks.

**First safe PR slice:** Research doc plus benchmark harness with one synthetic expensive-init module.

## 8. Diagnostic Call-Stack Symbolication

**Description:** Enrich captured call stacks with optional function names/source metadata when debug information exists, while preserving current digest semantics.

**Benefit:** Better debugging and AI recovery context without changing execution authority.

**Evidence:** `CapturedCallStack`, `ErrorLog.call_stack`, and tests already prove trap stack metadata reaches error context without changing snapshot digests.

**Implementation sketch:**

- Add optional symbolication only when names/debug info are present.
- Keep raw frame indices as fallback.
- Add redaction controls for paths in debug info.

**Risk:** Source path leakage through debug names.

**Validation:** Wasm fixture with names section; test redaction and fallback behavior.

**First safe PR slice:** Add a fixture and tests for existing raw frames; defer symbolication code until privacy rules are explicit.

## 9. Least-Disclosure Attenuation Playbook

**Description:** Document and test a practical alternative to ZK capability attestation: issue a narrowly attenuated token for the exact action instead of proving possession of a broader hidden grant.

**Benefit:** Captures most near-term privacy value at much lower complexity than ZK circuits.

**Evidence:** Existing attenuation chains already narrow capabilities and reject widening; RFC 0003 says ZK is poor cost/benefit in a single trust domain.

**Implementation sketch:**

- Add examples showing broad parent -> narrow child token -> action-specific verification.
- Add docs for when not to reveal a parent token to a third-party tool.
- Add tests for path narrowing, expiry clamping, and depth limits.

**Risk:** Documentation may imply privacy guarantees beyond what bearer tokens provide.

**Validation:** Claim audit against `CapabilityToken::attenuate` tests and RFC 0003.

**First safe PR slice:** Docs-only guide with exact caveats and test references.

## 10. Policy-Driven Denial Explainers

**Description:** Return structured denial reasons for capability failures: missing token, expired token, too broad request, outside module dir, all-capability rejected, or auth required.

**Benefit:** Operators and agents can fix requests faster without weakening deny-by-default behavior.

**Evidence:** Current denial paths exist across capability manager, MCP token sanitizer, and daemon auth/module-dir checks.

**Implementation sketch:**

- Define a non-sensitive `DenialReason` enum.
- Convert internal errors to safe external messages.
- Never include full host paths or token contents by default.

**Risk:** Helpful errors can leak policy topology.

**Validation:** Golden-message tests that assert redaction, no token material, and stable remediation hints.

**First safe PR slice:** Internal enum + unit tests only; no public message changes until security review.

## Recommended Execution Order

1. Capability Profile Manifests.
2. Runtime Proof Capsule.
3. MCP Security Conformance Suite.
4. Pooling/Density truth runbook and schema.
5. Snapshot Sync two-daemon MVP.

This order keeps early PRs small, audit-friendly, and defensible. It also avoids speculative implementation until Nexus has better proof artifacts and conformance gates around the surfaces that already exist.
