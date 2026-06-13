# Nexus Benchmark Report

## Live Numbers

**All current benchmark data is available on the live dashboard:**

> **[adaptiveliquidity.github.io/Nexus](https://adaptiveliquidity.github.io/Nexus/)**

Numbers are measured on GitHub-hosted runners (ubuntu-24.04) and published automatically to:
- [Bencher.dev](https://bencher.dev/perf/nexus-ai) — wall-clock latency, throughput, and binary size tracking with Student's t-test regression detection
- [CodSpeed.io](https://codspeed.io/adaptiveliquidity/Nexus) — deterministic CPU simulation (instruction count), heap memory tracking, and bare-metal walltime

PRs are gated by both services — statistical regressions block merge. All benchmark artifacts are cryptographically signed with [Sigstore](https://www.sigstore.dev/) via GitHub OIDC for provenance attestation.

## Retired Claims

The following claims appeared in earlier versions of this document and have been corrected:

1. **"23 µs cold start, 217x faster than CF Workers"** — compared sandbox struct init to full request latency. The 23 µs number is real but measures only `WasmSandbox::new`; end-to-end first-call latency is higher. The comparison was apples-to-oranges.

2. **"56 µs snapshot creation"** — true only for empty/zero memory buffers. With 1 MiB of pseudo-random (incompressible) memory, the measured time is ~2.92 ms. The original benchmark used all-zero buffers, which zstd compresses to near-nothing.

3. **"<1 ms rollback"** — true at 1 MiB (sub-millisecond), but 1.62 ms at 10 MiB and 53.6 ms at 100 MiB. Rollback time scales with state size.

4. **"10,000+ concurrent sandboxes"** — never measured by the shipped benchmark harness. Density benchmarking is planned as a separate effort.

## Benchmark Methodology

### Pipeline Architecture

The benchmark pipeline uses two independent third-party services across five CI jobs:

1. **Bencher.dev** — receives native Criterion output via the `rust_criterion` adapter, tracks wall-clock latency (upper boundary) and throughput (lower boundary) with Student's t-test at 99th percentile. Also tracks release binary sizes (`nexus`, `nexus-agentd`) with percentage-based thresholds. PR branches clone thresholds from their base branch and fail on regression (`--error-on-alert`). Stale PR branches are auto-archived on close.
2. **CodSpeed.io** — three measurement modes via `codspeed-criterion-compat` v4:
   - **CPU simulation** (cachegrind): deterministic instruction counts, immune to noisy-neighbor effects
   - **Memory**: heap allocation tracking alongside CPU simulation
   - **Walltime**: real elapsed time on bare-metal ARM64 runners (16-core, 32 GB RAM), opt-in via `CODSPEED_WALLTIME_ENABLED`

### Test Environment

- **Hardware**: GitHub-hosted runner (ubuntu-24.04), hardware specs recorded per run
- **Operating System**: Ubuntu 24.04 LTS
- **Statistical rigor**: Criterion with configurable warm-up, measurement time, and sample size
- **WASM Runtime**: wasmtime 45.0 with Cranelift JIT compiler
- **Memory buffers**: Pseudo-random (LCG-generated) to prevent compression from skewing results

### Test Cases

The `nexus_validation` benchmark harness measures:

| Benchmark Group | What It Measures | Category |
|----------------|-----------------|----------|
| `cold_start/sandbox_new` | `WasmSandbox::new` — struct initialization | benchmarked-primitive |
| `cold_start/hypervisor_new` | `NexusHypervisor::new` — full hypervisor init | benchmarked-primitive |
| `snapshot_create/size/{1,10,100}MiB` | `SnapshotManager::create_snapshot` with pseudo-random memory | integrated-live |
| `snapshot_rollback/size/{1,10,100}MiB` | `SnapshotManager::rollback_to` — decompress + integrity restore | benchmarked-primitive |
| `execute_tool/trivial_wasm_start` | End-to-end `execute_tool` with a minimal WASM module | integrated-live |
| `execute_tool_real_memory/size/{1,10,100}MiB` | End-to-end with WASM modules that allocate real linear memory | integrated-live |
| `integrated_capability_checked` | `execute_tool_with_tokens` with ed25519 token validation | integrated-live |
| `integrated_input_fed` | `execute_tool` with non-trivial JSON input plumbing | integrated-live |
| `integrated_precompiled` | `execute_tool_precompiled` vs `execute_tool` (cache hit vs recompile) | integrated-live |
| `integrated_full_stack` | Combined: capability + input + precompiled in a single call | integrated-live |

### Competitor Data Sources

All competitor numbers on the dashboard are from cited third-party sources (vendor documentation, academic papers, independent benchmarks). Source URLs are provided for every data point. See `dashboard/competitors.yml` for the complete citation list.

### Provenance

Every CI benchmark run produces:
- `benchmark_evidence/runner.md` — hardware specs, date, commit SHA
- `benchmark_evidence/criterion_raw.log` — raw Criterion output
- `criterion_artifacts.sigstore` — Sigstore signature bundle

Verify any artifact:
```bash
cosign verify-blob \
  --bundle criterion_artifacts.sigstore \
  criterion_artifacts.tar.gz
```
