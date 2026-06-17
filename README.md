# Nexus: AI-Native WASM Snap-Rollback Sandbox

**Game save-states for AI agents.**

Nexus provides microsecond-class sandbox initialization, native snapshot/rollback, capability-gated WASI execution, and opt-in self-correction telemetry for agents.

[![Benchmarks](https://img.shields.io/badge/benchmarks-live-brightgreen)](https://adaptiveliquidity.github.io/Nexus/)

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

## Benchmark Results

<p align="center">
  <img src="docs/benchmark-chart.svg" alt="Nexus benchmark results — log-scale horizontal bar chart showing cold start, rollback, execute, snapshot, and integrated benchmarks across 12 workloads" width="850"/>
</p>

> Measured with [Criterion.rs](https://github.com/bheisler/criterion.rs) on ubuntu-24.04 CI runners. The always-on pipeline tracks wall-clock latency and binary size ([Bencher.dev](https://bencher.dev/perf/nexus-ai)) plus CPU simulation and heap memory ([CodSpeed.io](https://codspeed.io/adaptiveliquidity/Nexus)); bare-metal walltime is opt-in. Benchmark PRs are gated on configured regression checks. [Live dashboard →](https://adaptiveliquidity.github.io/Nexus/)

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
|  execute_tool · execute_tool_wasi[_with_config] · with_tokens|
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
git clone https://github.com/adaptiveliquidity/Nexus.git
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

Or use the `WasiToolConfig` builder for multi-mount WASI execution:

```rust
use nexus::{WasiToolConfig, WasiMount, WasiAccess};

let config = WasiToolConfig::new()
    .with_mount(WasiMount {
        guest_path: "/input".into(),
        host_path: "/data/input".into(),
        access: WasiAccess::ReadOnly,
    })
    .with_mount(WasiMount {
        guest_path: "/output".into(),
        host_path: "/data/output".into(),
        access: WasiAccess::ReadWrite,
    });

let result = hypervisor
    .execute_tool_wasi_with_config(tool, input, &tokens, config)
    .await?;
```

Run the end-to-end examples:

```bash
cargo run --example wasi_file_read
cargo run --example wasi_capability_demo
```

### CLI

```bash
# Execute a WASM module (cold path)
nexus execute --wasm tool.wasm

# Hot path via long-lived daemon (Unix socket / Windows named pipe)
nexus run --wasm tool.wasm

# Run demos
nexus demo --demo all
```

## Features

### Shipped (integrated-live)

| Feature | Status | Description |
|---------|--------|-------------|
| Snap-rollback | Shipped | Linear memory + globals + tables captured; sub-ms rollback at 1 MiB |
| WASI execution | Shipped | WASI Preview 1 via `execute_tool_wasi` / `execute_tool_wasi_with_config`; capability tokens gate pre-opens |
| WASI mount builder | Shipped | `WasiToolConfig` builder API with `with_mount()`, `validate()`, `required_capabilities()` |
| Capability enforcement | Shipped | Ed25519-signed tokens with attenuation chains (`attenuate()`); authorization before filesystem side effects |
| Speculative execution | Shipped | `fork_and_race` runs N sandbox branches, picks the winner |
| Execution replay | Shipped | Deterministic checkpoint trace with time-travel cursor (`TraceReplay`) |
| Failure taxonomy | Shipped | 15+ typed `FailureMode` variants with `requires_rollback()` |
| Adaptive fuel budgeting | Shipped | Per-tool fuel profiles adjust from execution history (`FuelBudgetPolicy`) |
| Recovery policies | Shipped | Static + instinct-based + optional LLM-backed (`ai-recovery` feature) |
| Module cache | Shipped | SHA-256-keyed `Arc<Module>` reuse avoids recompilation |
| Daemon mode | Shipped | `nexus-agentd` with Unix socket + Windows named pipes, hypervisor pool |
| MCP server | Shipped | `nexus-mcp` exposes execute, WASI execute, issue-token, snapshot, and fork-and-race tools over stdio |
| Warm sandbox pool | Shipped | Opt-in `SandboxPool` / `PoolConfig` with semaphore backpressure and module-cache reuse |
| Density benchmark harness | Shipped (manual) | `cargo bench --bench density_validation --features bench-density`; intentionally excluded from normal PR gates |
| WASM call-stack capture | Shipped (diagnostic) | Trap call stacks flow into `ErrorLog` as telemetry metadata without changing snapshot digests |
| Snapshot sync protocol | Shipped (local/tested) | Digest, framed transport, lineage, and protocol tests are in-tree; distributed deployment remains RFC work |
| Live benchmarks | Shipped | Always-on wall-clock, binary size, CPU-simulation, and heap-memory checks; bare-metal walltime is opt-in; dashboard auto-updates from main |

### Roadmap

| Priority | Item | Status |
|----------|------|--------|
| P1 | Cross-platform daemon (named pipes on Windows) | **Shipped** |
| P1 | MCP server integration | **Shipped** |
| P1 | Security review / audit | Ongoing; CI and dependency gates are active, deeper capability-model design is tracked separately |
| P2 | Sandbox pool with warm instances | **Shipped** |
| P2 | Concurrent sandbox density benchmarking | Manual harness shipped behind `bench-density`; not part of normal PR gates |
| P3 | Distributed snapshot synchronization | RFC + local protocol/test harness shipped; networked multi-node fabric remains research |
| P3 | WASM call-stack capture | Opt-in diagnostic capture shipped; richer stack/register recovery remains research |
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
2. **Authorization before side effects** — capability tokens are validated before any filesystem operations (directory creation, path resolution)
3. **WASM isolation** — each sandbox operates in complete memory isolation
4. **Attenuation chains** — tokens can be narrowed and re-delegated, never widened
5. **Denial on failure** — missing, expired, revoked, or incorrectly-signed tokens produce `CapabilityDenied`

## Benchmark Results

> See the [live dashboard](https://adaptiveliquidity.github.io/Nexus/) for the latest numbers and head-to-head competitor comparison with cited sources.

### Competitor Feature Matrix

| Feature | Nexus | Docker | E2B | Firecracker |
|---------|-------|--------|-----|-------------|
| Cold Start < 1ms | Yes (sandbox init) | No | No | No |
| Native Snapshots | Yes (mem+globals+tables) | No | No | External tooling |
| Sub-ms Rollback (small state) | Yes | No | No | ~4 ms |
| WASI + Capability Gating | Yes | No | No | No |
| AI Telemetry / Recovery Hints | Built-in telemetry; self-correction opt-in | No | No | No |
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
│   │   └── nexus_agentd.rs  # Long-lived daemon (Unix socket / Windows named pipe)
│   ├── hypervisor/
│   │   ├── mod.rs           # NexusHypervisor, execute_tool, execute_tool_wasi
│   │   ├── failure_mode.rs  # 15+ typed FailureMode variants
│   │   ├── recovery.rs      # StaticPolicy, LayeredPolicy, InstinctPolicy
│   │   ├── llm_policy.rs    # Optional LLM-backed recovery (ai-recovery feature)
│   │   ├── speculative.rs   # fork_and_race, SelectionStrategy
│   │   └── validator/       # HealthValidator, ErrorLog
│   ├── sandbox/
│   │   ├── wasm_runtime.rs  # WasmSandbox (pure-compute path)
│   │   ├── wasi.rs          # WASI execution + WasiToolConfig builder
│   │   ├── fuel_meter.rs    # Adaptive fuel metering
│   │   ├── fuel_budget.rs   # FuelBudgetPolicy (per-tool fuel profiles)
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
│   ├── wasi_file_read.rs        # End-to-end WASI + capability demo
│   ├── wasi_capability_demo/    # Multi-file WASI capability + denial demo
│   ├── capture_error.rs         # Failure-mode capture
│   └── instinct_ab.rs           # Instinct A/B testing
├── tests/                       # 184+ integration tests
├── benches/
│   └── nexus_validation.rs      # Primitive + integrated benchmarks
├── scripts/
│   ├── generate_benchmark_svg.py # Auto-generate docs/benchmark-chart.svg from Criterion
│   └── ai_rescore.py            # LLM-scored recovery-path validation
├── dashboard/                    # Next.js benchmark dashboard (GitHub Pages)
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
