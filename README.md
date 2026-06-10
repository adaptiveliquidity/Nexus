# Nexus: AI-Native WASM Snap-Rollback Sandbox

**Game save-states for AI agents.**

Nexus provides microsecond-class cold starts, native snapshot/rollback capabilities, and opt-in AI telemetry for self-correcting agents.

[![Benchmarks](https://img.shields.io/badge/benchmarks-live-brightgreen)](https://adaptive-liquidity.github.io/Nexus/)

[![Crates.io](https://img.shields.io/crates/v/nexus-ai)](https://crates.io/crates/nexus-ai)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

## Key Performance Metrics

> **Live benchmarks:** [adaptive-liquidity.github.io/Nexus](https://adaptive-liquidity.github.io/Nexus/)
> All numbers below are measured on GitHub-hosted runners (ubuntu-24.04) and published to [Bencher.dev](https://bencher.dev/perf/nexus-ai) + [CodSpeed.io](https://codspeed.io/Adaptive-Liquidity/Nexus). Artifacts are signed with Sigstore.

| Metric | Nexus (measured) | Notes |
|--------|-----------------|-------|
| Cold Start (sandbox init) | ~23 µs | `WasmSandbox::new` only; end-to-end first-call latency is higher |
| Snapshot Creation (1 MiB) | ~2.92 ms | Pseudo-random (incompressible) memory; empty memory is ~56 µs |
| Snapshot Creation (100 MiB) | ~290 ms | Scales with memory size and compressibility |
| Rollback (1 MiB) | <1 ms | Decompress + integrity restore |
| Rollback (10 MiB) | ~1.62 ms | |
| Rollback (100 MiB) | ~53.6 ms | |
| AI Telemetry | Native | Only solution with built-in error learning |

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

Nexus is built on three foundational principles:

1. **WebAssembly Runtime** - Pre-compiled WASM modules execute in microseconds, not seconds
2. **Native State Management** - Snapshots and rollback are built into the core, not layered on top
3. **AI-Native Telemetry** - Every execution is instrumented; self-correction is opt-in via `with_self_correction`

```
+------------------+------------------+------------------+
|   NexusHypervisor                    |
+------------------+------------------+------------------+
     |                  |                  |
     v                  v                  v
+------------------+------------------+------------------+
|  HealthValidator | SnapshotManager  | TelemetrySink   |
|  - CPU monitoring |  - Zstd compression| - Error learning|
|  - Memory monitoring| - Checksum verification| - Pattern detection|
|  - Timeout detection| - Ring buffer    | - Feedback generation|
+------------------+------------------+------------------+
     |                  |                  |
     v                  v                  v
+------------------+------------------+------------------+
|  WasmSandbox     |  Snapshot Engine |  CapabilityMgr  |
|  (wasmtime 37)    |  (sub-ms @ 1MiB) |  (Token-based)  |
+------------------+------------------+------------------+
```

## Quick Start

### Installation

```bash
# From source
git clone https://github.com/Adaptive-Liquidity/Nexus.git
cd Nexus/nexus
cargo build --release

# Or install via cargo
cargo install nexus-ai
```

### Run the Demo

```bash
# Infinite loop prevention demo
cargo run --demo -d infinite-loop

# Benchmark suite
cargo run --benchmark --iterations 100

# Execute WASM file
cargo run --execute --wasm your_file.wasm
```

### Rust API

```rust
use nexus::{NexusHypervisor, HypervisorConfig, ToolDefinition};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = HypervisorConfig::default();
    let hypervisor = NexusHypervisor::new(config)?;
    
    let tool = ToolDefinition::new(
        "code_runner".to_string(),
        wasm_bytes,
    );
    
    let result = hypervisor.execute_tool(tool, serde_json::json!({})).await?;
    
    match result.success {
        true => println!("Execution succeeded"),
        false => {
            println!("Execution failed: {:?}", result.error);
            if let Some(log) = result.error_log {
                println!("AI Feedback: {}", log.to_llm_context());
            }
        }
    }
    
    Ok(())
}
```

## Benchmark Results

> See the [live dashboard](https://adaptive-liquidity.github.io/Nexus/) for the latest numbers and head-to-head competitor comparison with cited sources.

### Competitor Feature Matrix

| Feature | Nexus | Docker | E2B | Firecracker |
|---------|-------|--------|-----|-------------|
| Cold Start < 1ms | Yes (sandbox init) | No | No | No |
| Native Snapshots | Yes | No | No | External tooling |
| Sub-ms Rollback (small state) | Yes | No | No | ~4 ms |
| AI Telemetry | Yes | No | No | No |
| Self-Correction | Opt-in | No | No | No |

### Detailed Benchmark Data

See [BENCHMARKS.md](BENCHMARKS.md) for methodology and the [live dashboard](https://adaptive-liquidity.github.io/Nexus/) for current numbers.

## Technical Design

### Snapshot and Rollback

Nexus implements a microsecond snapshot and rollback system:

1. **Pre-Execution Snapshot** - Before every tool execution, the complete state is captured
2. **Health Monitoring** - CPU, memory, and execution time are monitored in real-time
3. **Error Detection** - Failures are classified and logged with full context
4. **Instant Rollback** - On error, state is restored to the pre-execution snapshot

```rust
// Simplified rollback flow
async fn execute_with_safety(&self, tool: ToolDefinition) -> ToolOutput {
    // 1. Capture snapshot (~2.92 ms @ 1 MiB)
    let snapshot = self.snapshot_manager.capture().await;
    
    // 2. Execute with health monitoring
    let result = self.execute_with_health_check(tool).await;
    
    // 3. Check for failure
    if !result.success {
        // 4. Rollback (<1 ms @ 1 MiB)
        self.snapshot_manager.restore(&snapshot).await;
        
        // 5. Generate AI feedback
        let feedback = self.telemetry.learn_from_error(&result);
        return ToolOutput { error_log: Some(feedback), ..result };
    }
    
    result
}
```

### Health Validator

The health validator monitors three critical dimensions:

- **CPU Usage** - Detects CPU spikes that indicate infinite loops
- **Memory Pressure** - Detects memory exhaustion before OOM events
- **Execution Time** - Timeout detection with configurable thresholds

### AI Telemetry

Every execution generates structured telemetry that enables:

1. **Error Pattern Learning** - The system learns which error types occur most frequently
2. **Recovery Action Suggestions** - Generates actionable feedback for AI agents
3. **Successful Pattern Recognition** - Identifies patterns that lead to success

## Security Model

Nexus implements capability-based security:

1. **Cryptographic Tokens** - Every system call requires a valid capability token
2. **No Host Kernel Access** - WASM sandbox cannot access host resources without explicit grants
3. **Memory Isolation** - Each sandbox operates in complete isolation

```rust
// Capability-based access control
let capability = CapabilityManager::create_token(
    "filesystem:read:/tmp/nexus".into(),
    Duration::from_secs(300),
)?;
    
sandbox.grant_capability(capability)?;
```

## Performance Characteristics

### Latency Budget

For a typical AI agent tool execution:

| Phase | Time (1 MiB state) | Notes |
|-------|-------------------|-------|
| Snapshot Creation | ~2.92 ms | Scales with memory size |
| WASM Execution | Variable | Dominates total latency |
| Rollback (on error) | <1 ms | At 1 MiB; ~53.6 ms at 100 MiB |
| Telemetry Recording | <100 µs | |

### Scalability

Nexus is designed for high-density agent deployments:

- **Concurrent Sandboxes**: Not yet benchmarked (density testing planned)
- **Memory per Sandbox**: <1MB overhead
- **Snapshot Storage**: Zstd compressed (typically 60-80% reduction)

## Project Structure

```
nexus/
├── src/
│   ├── main.rs           # CLI entry point
│   ├── lib.rs            # Public API
│   ├── hypervisor/       # Core orchestration
│   │   ├── mod.rs
│   │   ├── executor.rs
│   │   ├── validator.rs
│   │   └── health.rs
│   ├── sandbox/          # WASM execution
│   │   ├── mod.rs
│   │   ├── wasm_runtime.rs
│   │   ├── fuel_meter.rs
│   │   └── wasm_memory.rs
│   ├── snapshot/         # State management
│   │   ├── mod.rs
│   │   ├── manager.rs
│   │   └── compression.rs
│   ├── telemetry/         # AI learning
│   │   ├── mod.rs
│   │   └── learning.rs
│   ├── security/          # Access control
│   │   └── capability.rs
│   └── error.rs           # Error types
├── tests/
│   └── integration_tests.rs
├── Cargo.toml
├── README.md
├── BENCHMARKS.md
├── LICENSE
└── CONTRIBUTING.md
```

## Dependencies

- **wasmtime 37.0** - High-performance WASM runtime
- **tokio** - Async runtime for concurrent execution
- **zstd** - Fast compression for snapshots
- **serde** - Serialization for state management
- **uuid** - Unique identifiers for snapshots and logs
- **sha2** - Checksum verification for state integrity

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, testing guidelines, and pull request process.

## Roadmap

### In Development
- Full WASI filesystem integration
- Real WASM memory state capture
- Distributed snapshot synchronization

### Planned
- Predictive ML-based rollback triggers
- Graph-based memory navigation
- Cross-sandbox state sharing

### Research
- Hardware-enforced state isolation
- Formal verification of rollback correctness
- Zero-knowledge capability attestation

## License

MIT License. See [LICENSE](LICENSE) for details.

## References

- [wasmtime Documentation](https://docs.rs/wasmtime/latest/wasmtime/)
- [WebAssembly Specification](https://webassembly.org/)
- [Rust WebAssembly Book](https://rustwasm.github.io/docs/book/)

---

*Last updated: June 2026*