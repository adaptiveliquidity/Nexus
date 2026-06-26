# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2026-06-21

### Security
- **C2**: Downgrade self-issued memory evidence from `Attested` to `Advisory` in
  `attach_memory_evidence_from_receipt` — the digest-only daemon path cannot verify hit count
  or a counter-signed AEON-IQ receipt; `Attested` will be reinstated when HMAC counter-signature
  verification is added.
- **M1**: Gate memory recall behind `Capability::MemoryRecall` in `nexus_iq_execute` —
  recall now requires an explicit `nexus:memory_recall` capability token.
- **M3**: Rate-limit memory recall per agent in `nexus_iq_execute` via a sliding-window
  counter; prevents unbounded AEON-IQ load from a single misbehaving agent.

### Fixed
- **C1**: `nexus_iq_execute` now correctly extracts `memory_digest` for all attestation
  modes (`AttestedNoHit`, `AttestedWithRecall`) — previously only `Attested` was covered.
- **H1**: `NexusIqExecuteResponse` serialises `MemoryEvidenceForMcp` instead of raw
  `MemoryEvidenceV1` — prevents leaking the search query via MCP responses.
- Add `query: String` field to `MemoryEvidenceForMcp` for caller correlation.

### Added
- **L2**: `nexus aeon verify-capsule` CLI subcommand validates `ProofCapsule` JSON for
  consistent `memory_mode` / `memory_evidence` fields.
- `nexus aeon incident` CLI subcommand generates a structured incident report from a
  `ProofCapsule` JSON file.


### Security
- **C2**: Downgrade self-issued memory evidence from `Attested` to `Advisory` in
  `attach_memory_evidence_from_receipt` — the digest-only daemon path cannot verify hit count
  or a counter-signed AEON-IQ receipt; `Attested` will be reinstated in P11 when
  counter-signature verification is added.
- **M1**: Add `MemoryRecall` capability variant with gate before `recall_memory_evidence_v1`
  in the `nexus_iq_execute` MCP path — memory recall now requires an explicit
  `nexus:memory_recall` capability token.

### Fixed
- **C1**: `nexus_iq_execute` MCP handler now correctly recognises
  `AttestedNoHit` and `AttestedWithRecall` modes when extracting the memory digest
  (match arm previously only covered `Attested`; `memory_digest` was always `None`
  for the other two modes).
- **H1**: `NexusIqExecuteResponse` now serialises `MemoryEvidenceForMcp` (omits raw query
  text) instead of the full `MemoryEvidenceV1` — prevents caller-visible leakage of the
  search query via MCP responses.

### Added
- **L2**: `nexus aeon verify-capsule` CLI subcommand validates that a `ProofCapsule` JSON
  has consistent `memory_mode` and `memory_evidence` fields.

## [0.2.0] - 2026-06-20

### Added

- **AEON-IQ memory integration (`aeon-memory` feature)**: opt-in service-boundary integration from Phases 4-10.
  - Adds HMAC-bound memory evidence references, explicit `Advisory` / `Attested` / `Degraded` / `Absent` proof modes, and an env-only `NEXUS_AEON_HMAC_KEY` path.
  - Threads AEON agent/session correlation through proof, daemon, and MCP surfaces without changing the default build shape.
  - Surfaces Nexus execution events for AEON-IQ timeline persistence while keeping timeline outages fail-open.
  - Adds bounded capability-denial negotiation with a hard two-round cap, strict-subset narrowing, and no escalation beyond caller-held tokens.
  - Exposes the `nexus_aeon_execute_timeline` MCP tool, `examples/aeon_e2e_demo.rs`, and `tests/aeon_conformance.rs` for integration validation.
  - Adds Phase 10 release hardening docs: threat model, runbook, key-provisioning guide, and closed merge audit annotations.

## [0.1.0] - 2026-06-10

### Added

- **WASM Sandbox**: WebAssembly execution using wasmtime 45.0
  - Sandbox and hypervisor init are benchmarked in the live Criterion/Bencher/CodSpeed pipeline; do not treat struct-init numbers as end-to-end cold-start latency
  - Fuel metering for resource control (integrated, enforced per-call)
  - WASI Preview 1 execution is integrated through `execute_tool_wasi` and `execute_tool_wasi_with_config`

- **Snapshot Engine**: Native snapshot/rollback system
  - Snapshot creation is tracked in the live benchmark dashboard; earlier empty-buffer timings are not used for current public claims
  - Zstd compression for efficient storage
  - SHA-256 checksum verification for state integrity
  - Ring buffer for snapshot history management
  - `restore_memory` writes snapshot bytes back into a live wasmtime Store

- **Health Validator**: Real-time system monitoring
  - CPU usage monitoring with configurable thresholds
  - Memory pressure detection
  - Execution timeout enforcement

- **AI Telemetry**: Built-in execution telemetry with opt-in self-correction
  - Error pattern detection and classification
  - Recovery action suggestions
  - LLM-compatible feedback generation
  - Successful pattern recognition

- **Self-Correction**: Opt-in instinct-based outcome feedback
  - Enabled via `with_self_correction(instinct_store)` — off by default
  - Credits/debits instincts after retry outcomes

- **Capability Enforcement**: Cryptographic access control (integrated-live)
  - Ed25519-signed capability tokens with expiration
  - `execute_tool_with_tokens` validates before guest runs
  - `CapabilityDenied` error for missing/expired/invalid tokens

- **Tool Input Plumbing**: JSON input delivered to guest memory
  - `[len: u32 LE][data]` written at offset 0 before `_start`

- **Module Cache**: SHA-256-keyed precompiled module reuse (Phase C daemon)
  - `ModuleCache::get_or_compile` skips `Module::from_binary` on cache hit
  - `execute_tool_precompiled` on NexusHypervisor
  - Wired into `nexus-agentd` daemon hot path

- **Daemon**: `nexus-agentd` long-lived daemon with hypervisor pool
  - Unix socket and Windows named-pipe transport
  - Length-prefixed JSON framing
  - Module cache integration
  - Optional per-request auth with `NEXUS_AGENTD_AUTH_TOKEN`
  - `wasm_path` reads restricted to `NEXUS_AGENTD_MODULE_DIR`; clients should prefer `wasm_bytes`

- **MCP Server**: `nexus-mcp` stdio tool surface
  - Exposes execute, WASI execute, issue-token, snapshot create/rollback, and fork-and-race tools
  - Restricts MCP `wasm_path` reads to `NEXUS_MCP_MODULE_DIR`
  - MCP WASI execution issues caller tokens for requested capabilities

- **Sandbox Pool**: Opt-in warm pool and manual density harness
  - `SandboxPool` / `PoolConfig` provide semaphore-bounded concurrent execution
  - `cargo bench --bench density_validation --features bench-density` emits density measurements outside normal PR gates

- **Snapshot Sync and Diagnostics**
  - Snapshot sync digest, framed transport, lineage, and protocol components are implemented and tested locally
  - WASM trap call-stack capture flows into `ErrorLog` as diagnostic telemetry

- **CLI Interface**: Command-line tool for sandbox management

- **Comprehensive Tests**: Integration test suite
  - Capability enforcement (8 tests)
  - Rollback restore byte-exact roundtrip (4 tests)
  - Tool input plumbing (5 tests)
  - Self-correction opt-in semantics (3 tests)
  - Integrated path: capability + input + precompiled (3 tests)

- **Benchmarks**: Criterion + CodSpeed + Bencher pipeline
  - Primitive benchmarks: cold_start, snapshot_create, snapshot_rollback, execute_tool
  - Integrated benchmarks: capability_checked, input_fed, precompiled, full_stack
  - Conditional publishing to Bencher.dev and CodSpeed.io, with best-effort Sigstore artifact signing
  - PR benchmark runs intentionally ignore `.github/**` changes, so workflow-only edits do not trigger benchmark PR runs

### Performance Metrics

| Metric | Value | Category |
|--------|-------|----------|
| Cold start / init paths | See live dashboard | benchmarked-primitive + integrated-live |
| Snapshot creation | See live dashboard | integrated-live |
| Rollback | See live dashboard | benchmarked-primitive |
| Execute trivial WASM e2e | See live dashboard | integrated-live |
| Density benchmarks | Manual `bench-density` harness | opt-in/manual |

### Known Limitations

- WASI filesystem access is capability-gated through host preopens; general WASI networking is not exposed as a default capability.
- Snapshot/rollback captures linear memory plus exported globals and tables; call stacks are diagnostic metadata, not full register/stack restoration.
- Concurrent sandbox density benchmarking exists as a manual `bench-density` harness and is intentionally excluded from normal PR gates.
- Daemon auth is backward-compatible: local-dev mode stays tokenless unless `NEXUS_AGENTD_AUTH_TOKEN` is configured.

### Dependencies

- wasmtime 45.0 (Cranelift JIT, async, fuel metering)
- tokio (async runtime)
- zstd (snapshot compression)
- ed25519-dalek (capability token signing)
- serde / serde_json / bincode (serialization)
- sha2 (content hashing)
- chrono, uuid, clap, tracing

### Roadmap

See [README.md](README.md) for planned features and research directions.
