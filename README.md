# Nexus: AI-Native WASM Snap-Rollback Sandbox

**Game save-states for AI agents.**

Nexus provides microsecond-class cold starts, native snapshot/rollback, capability-gated WASI execution, and opt-in AI telemetry for self-correcting agents.

[![Benchmarks](https://img.shields.io/badge/benchmarks-live-brightgreen)](https://adaptive-liquidity.github.io/Nexus/)

[![Crates.io](https://img.shields.io/crates/v/nexus-ai)](https://crates.io/crates/nexus-ai)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

## Key Performance Metrics

> **Live benchmarks:** [adaptive-liquidity.github.io/Nexus](https://adaptive-liquidity.github.io/Nexus/)
> All numbers below are measured on GitHub-hosted runners (ubuntu-24.04) and published to [Bencher.dev](https://bencher.dev/perf/nexus-ai) + [CodSpeed.io](https://codspeed.io/Adaptive-Liquidity/Nexus). Artifacts are signed with Sigstore.

| Metric | Nexus (measured) | Category | Notes |
|--------|-----------------|----------|-------|
| Cold Start (sandbox init) | ~23 µs | benchmarked-primitive | `WasmSandbox::new` only; end-to-end first-call latency is higher |
| Snapshot Creation (1 MiB) | ~2.92 ms | integrated-live | Pseudo-random (incompressible) memory; empty memory is ~56 µs |
| Snapshot Creation (100 MiB) | ~290 ms | integrated-live | Scales with memory size and compressibility |
| Rollback (1 MiB) | <1 ms | benchmarked-primitive | Decompress + integrity restore |
| Rollback (10 MiB) | ~1.62 ms | benchmarked-primitive | |
| Rollback (100 MiB) | ~53.6 ms | benchmarked-primitive | |
| AI Telemetry | Default-on | integrated-live | Self-correction is opt-in via `with_self_correction` |

<details>
<summary>Retired claims (click to expand)</summary>

The following claims appeared in earlier versions of this README and have been corrected:

1. **"217x faster than CF Workers"** — compared sandbox struct init (23 µs) to full request latency (5 ms). Apples-to-oranges.
2. **"56 µs snapshot creation"** — true only for empty/zero memory. Realistic workloads with 1 MiB incompressible memory measure ~2.92 ms.
3. **"<1 ms rollback"** — true at 1 MiB, but 1.62 ms at 10 MiB and 53.6 ms at 100 MiB.
4. **"10,000+ concurrent sandboxes"** — never measured by the shipped benchmark harness. Density benchmarking is planned as a separate effort.
</details>

## What Problem Does Nexus Solve?

AI agents that write and execute code face critical failure modes:

1. **Infinite Loops** - Agents that enter infinite loops crash systems and lose all progress
2. **State Corruption** - Memory corruption creates unrecoverable errors requiring full restarts
3. **Context Loss** - Every failure costs the agent weeks of accumulated context and learning

Traditional solutions (Docker, Firecracker, gVisor) were not designed for these problems:

- Docker: ~15 s cold start (image-dependent), no native snapshot/rollback, no AI telemetry
- Firecracker: ~125 ms raw boot, ~1.5 s snapshot create, ~4 ms rollback ([source](https://github.com/firecracker-microvm/firecracker/blob/main/SPECIFICATION.md))
- E2B: ~150 ms cold start ([source](https://www.startuphub.ai/ai-news/artificial-intelligence/2026/daytona-vs-e2b-vs-modal-vs-vercel-sandbox-2026)), no native snapshots, no AI telemetry

Nexus is purpose-built for AI agent execution with native support for the failure modes that matter.

## Architecture Overview

```
+--------------------------------------------------------------+
|                      NexusHypervisor                         |
|  execute_tool · execute_tool_wasi · execute_tool_with_tokens |
+--------------------------------------------------------------+
     |                  |                  |              |
     v                  v                  v              v
+--------------+ +--------------+ +--------------+ +----------+
| HealthValid. | | SnapshotMgr  | | TelemetrySink| | Specul.  |
| CPU/mem/time | | Zstd + ring  | | Error learn  | | fork_and |
|              | | globals/table| | Pattern rec. | | _race    |
+--------------+ +--------------+ +--------------+ +----------+
     |                  |                  |
     v                  v                  v
+--------------+ +--------------+ +--------------+
| WasmSandbox  | | Snapshot Eng | | CapabilityMgr|
| pure-compute | | sub-ms @1MiB | | Ed25519 sign |
| + WASI path  | |              | | attenuation  |
| (wasmtime 45)| |              | | chain tokens |
+--------------+ +--------------+ +--------------+
```

**Two execution paths:**
- **Pure-compute** (`execute_tool`) — empty `Linker`, fully deterministic, supports execution replay
- **WASI** (`execute_tool_wasi`) — WASI Preview 1 host imports, capability tokens mapped to pre-opened directories

## Quick Start

### Installation

```bash
git clone https://github.com/Adaptive-Liquidity/Nexus.git
cd Nexus
cargo build --release
```

### Pure-Compute Execution

```rust
use nexus::{NexusHypervisor, HypervisorConfig, ToolDefinition};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let hypervisor = NexusHypervisor::new(HypervisorConfig::default())?;

    let tool = ToolDefinition::new("code_runner".into(), wasm_bytes);
    let result = hypervisor.execute_tool(tool, serde_json::json!({})).await?;

    if result.success {
        println!("OK ({}ms, fuel={})", result.execution_time_ms, result.fuel_consumed);
    } else if let Some(log) = result.error_log {
        println!("Failed: {}", log.to_llm_context());
    }
    Ok(())
}
```

### WASI Execution with Capability Tokens

```rust
use std::time::Duration;
use nexus::security::Capability;

// Issue a ReadFile token signed by the hypervisor's Ed25519 key
let token = hypervisor.issue_token(
    Capability::ReadFile("/data".into()),
    "agent-session",
    Duration::from_secs(300),
)?;

// Tool requires ReadFile — WASI linker maps it to a read-only pre-open
let tool = ToolDefinition::new("reader".into(), wasm_bytes)
    .with_capabilities(vec![Capability::ReadFile("/data".into())]);

// Validates token, builds WASI context, executes with fuel metering
let result = hypervisor.execute_tool_wasi(tool, input, &[token]).await?;
```

Run the end-to-end example:

```bash
cargo run --example wasi_file_read
```

### CLI

```bash
# Execute a WASM module (cold path)
nexus execute --wasm tool.wasm

# Hot path via long-lived daemon (Unix only)
nexus run --wasm tool.wasm

# Run demos
nexus demo --demo all
```

## Features

### Shipped (integrated-live)

| Feature | Status | Description |
|---------|--------|-------------|
| Snap-rollback | Shipped | Linear memory + globals + tables captured; sub-ms rollback at 1 MiB |
| WASI execution | Shipped | WASI Preview 1 via `execute_tool_wasi`; capability tokens gate pre-opens |
| Capability enforcement | Shipped | Ed25519-signed tokens with attenuation chains (`attenuate()`) |
| Speculative execution | Shipped | `fork_and_race` runs N sandbox branches, picks the winner |
| Execution replay | Shipped | Deterministic checkpoint trace with time-travel cursor |
| Failure taxonomy | Shipped | 15+ typed `FailureMode` variants with `requires_rollback()` |
| Adaptive fuel budgeting | Shipped | Per-tool fuel profiles adjust from execution history |
| Recovery policies | Shipped | Static + instinct-based + optional LLM-backed (`ai-recovery` feature) |
| Module cache | Shipped | SHA-256-keyed `Arc<Module>` reuse avoids recompilation |
| Daemon mode | Shipped | `nexus-agentd` with Unix socket, hypervisor pool (Unix only) |

### Roadmap

| Priority | Item | Status |
|----------|------|--------|
| P1 | Cross-platform daemon (named pipes on Windows) | Planned |
| P1 | MCP server integration | Planned |
| P1 | Security review / audit | Planned |
| P2 | Sandbox pool with warm instances | Planned |
| P2 | Concurrent sandbox density benchmarking | Planned |
| P2 | Live benchmark dashboard (fix GitHub Pages deploy) | Planned |
| P3 | Distributed snapshot synchronization | Research |
| P3 | WASM call-stack capture | Research |
| P3 | Zero-knowledge capability attestation | Research |

## Technical Design

### Snapshot and Rollback

Nexus snapshots capture **linear memory, exported globals, and exported tables**. On failure, `rollback_to` decompresses the snapshot and `restore_memory` writes bytes back into a live `Store`.

1. **Pre-execution capture** — WASM memory bytes saved after instantiation, before entrypoint runs
2. **Health monitoring** — CPU, memory, and execution time monitored during execution
3. **Failure classification** — typed `FailureMode` with full context (15+ variants)
4. **Rollback** — only when `FailureMode::requires_rollback()` is true (load-time failures skip rollback)

### WASI + Capability Enforcement

The WASI execution path maps validated capability tokens to WASI contexts:

| Capability | WASI mapping |
|-----------|-------------|
| `ReadFile(path)` | Read-only pre-opened directory |
| `WriteFile(path)` | Read-write pre-opened directory |
| `ListDirectory(path)` | Read-only pre-opened directory |
| `All` | Inherit stdout + stderr |

Tokens support **attenuation chains** — a parent token can be narrowed (never widened) and re-issued to downstream agents, with Ed25519 signatures at each level.

### Speculative Execution

`fork_and_race` runs N independent sandbox branches with the same module but different inputs or fuel budgets. The first successful branch wins; failed branches are discarded. Selection strategies: `FirstSuccess`, `LowestFuel`, `HighestFuel`.

### Execution Replay

The trace engine records deterministic checkpoints (memory hash, globals, fuel counter) at configurable intervals. A replay cursor can step forward/backward through the trace for time-travel debugging.

### Health Validator

Monitors three dimensions during execution:

- **CPU Usage** — detects spikes indicating infinite loops
- **Memory Pressure** — detects exhaustion before OOM
- **Execution Time** — fuel-metered + wall-clock timeout

## Security Model

1. **Ed25519-signed tokens** — every capability token is cryptographically signed
2. **Validation before execution** — `execute_tool_with_tokens` / `execute_tool_wasi` validate all required capabilities before the guest runs
3. **WASM isolation** — each sandbox operates in complete memory isolation
4. **Attenuation chains** — tokens can be narrowed and re-delegated, never widened
5. **Denial on failure** — missing, expired, revoked, or incorrectly-signed tokens produce `CapabilityDenied`

## Benchmark Results

> See the [live dashboard](https://adaptive-liquidity.github.io/Nexus/) for the latest numbers and head-to-head competitor comparison with cited sources.

### Competitor Feature Matrix

| Feature | Nexus | Docker | E2B | Firecracker |
|---------|-------|--------|-----|-------------|
| Cold Start < 1ms | Yes (sandbox init) | No | No | No |
| Native Snapshots | Yes (mem+globals+tables) | No | No | External tooling |
| Sub-ms Rollback (small state) | Yes | No | No | ~4 ms |
| WASI + Capability Gating | Yes | No | No | No |
| AI Telemetry | Default-on | No | No | No |
| Self-Correction | Opt-in | No | No | No |
| Speculative Execution | Yes | No | No | No |

See [BENCHMARKS.md](BENCHMARKS.md) for methodology.

## Project Structure

```
nexus/
├── src/
│   ├── main.rs              # CLI (execute, run, demo, benchmark, instinct)
│   ├── lib.rs               # Public API re-exports
│   ├── bin/
│   │   └── nexus_agentd.rs  # Long-lived daemon (Unix socket)
│   ├── hypervisor/
│   │   ├── mod.rs           # NexusHypervisor, execute_tool, execute_tool_wasi
│   │   ├── failure_mode.rs  # 15+ typed FailureMode variants
│   │   ├── recovery.rs      # StaticPolicy, LayeredPolicy, InstinctPolicy
│   │   ├── llm_policy.rs    # Optional LLM-backed recovery (ai-recovery feature)
│   │   ├── speculative.rs   # fork_and_race, SelectionStrategy
│   │   └── validator/       # HealthValidator, ErrorLog
│   ├── sandbox/
│   │   ├── wasm_runtime.rs  # WasmSandbox (pure-compute path)
│   │   ├── wasi.rs          # WASI execution + WasiSandboxConfig
│   │   ├── fuel_meter.rs    # Adaptive fuel budgeting
│   │   └── wasm_memory.rs
│   ├── snapshot/
│   │   ├── manager.rs       # SnapshotManager, restore_memory, globals/tables
│   │   └── compression.rs   # Zstd + diff snapshots
│   ├── security/
│   │   └── capability.rs    # Ed25519 tokens, attenuation chains, authorize()
│   ├── telemetry/
│   │   ├── mod.rs           # TelemetrySink, patterns
│   │   ├── trace.rs         # Execution replay / time-travel debugging
│   │   └── learning.rs
│   ├── daemon/              # nexus-agentd support
│   │   ├── pool.rs          # HypervisorPool
│   │   ├── protocol.rs      # Length-prefixed JSON framing
│   │   └── module_cache.rs  # SHA-256-keyed Module cache
│   ├── instinct/            # Self-correction (opt-in)
│   └── error.rs
├── examples/
│   ├── wasi_file_read.rs    # End-to-end WASI + capability demo
│   ├── capture_error.rs     # Failure-mode capture
│   └── instinct_ab.rs       # Instinct A/B testing
├── tests/                   # 176+ integration tests
├── benches/
│   └── nexus_validation.rs  # Primitive + integrated benchmarks
├── Cargo.toml
├── BENCHMARKS.md
└── LICENSE
```

## Dependencies

- **wasmtime 45.0** — WASM runtime (Cranelift JIT, fuel metering)
- **wasmtime-wasi 45.0** — WASI Preview 1 host imports
- **tokio** — async runtime
- **zstd** — snapshot compression
- **ed25519-dalek** — capability token signing
- **serde** / **bincode** — serialization
- **sha2** — content hashing for snapshots and module cache

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, testing guidelines, and pull request process.

## License

MIT License. See [LICENSE](LICENSE) for details.

## References

- [wasmtime Documentation](https://docs.rs/wasmtime/latest/wasmtime/)
- [WebAssembly Specification](https://webassembly.org/)

---

*Last updated: June 2026*
