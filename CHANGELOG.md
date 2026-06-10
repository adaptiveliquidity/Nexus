# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-06-10

### Added

- **WASM Sandbox**: WebAssembly execution using wasmtime 45.0
  - Cold start ~23 µs (sandbox struct init; end-to-end first-call is higher)
  - Fuel metering for resource control (integrated, enforced per-call)
  - WASI support is in development (not yet integrated)

- **Snapshot Engine**: Native snapshot/rollback system
  - Snapshot creation: ~2.92 ms @ 1 MiB incompressible memory (~56 µs for empty/zero memory)
  - Zstd compression for efficient storage
  - SHA-256 checksum verification for state integrity
  - Ring buffer for snapshot history management
  - `restore_memory` writes snapshot bytes back into a live wasmtime Store

- **Health Validator**: Real-time system monitoring
  - CPU usage monitoring with configurable thresholds
  - Memory pressure detection
  - Execution timeout enforcement

- **AI Telemetry**: Built-in learning and feedback system (default-on)
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
  - Unix socket transport (Windows named-pipe deferred)
  - Length-prefixed JSON framing
  - Module cache integration

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
  - Signed artifacts via Sigstore, published to Bencher.dev and CodSpeed.io

### Performance Metrics

| Metric | Value | Category |
|--------|-------|----------|
| Cold Start (sandbox init) | ~23 µs | benchmarked-primitive |
| Snapshot Creation (1 MiB) | ~2.92 ms | integrated-live |
| Rollback (1 MiB) | <1 ms | benchmarked-primitive |
| Rollback (100 MiB) | ~53.6 ms | benchmarked-primitive |
| Execute trivial WASM e2e | measured | integrated-live |

### Known Limitations

- WASI filesystem/network access is not yet implemented (orphan code removed)
- Snapshot captures linear memory only (globals, tables, stack not captured)
- Concurrent sandbox density not yet benchmarked
- Windows named-pipe transport not yet implemented

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
