# External Audit & Third-Party Benchmark Reproduction

## Status

No formal external security audit has been completed for Nexus v1.0.0.
The internal security documentation and threat modelling is in:
- [`docs/AEON_NEXUS_THREAT_MODEL.md`](docs/AEON_NEXUS_THREAT_MODEL.md)
- [`docs/NEXUSIQ_SECURITY_REVIEW.md`](docs/NEXUSIQ_SECURITY_REVIEW.md)
- [`docs/security_threat_model_phase_b.md`](docs/security_threat_model_phase_b.md)

An external audit engagement is planned before the v1.1.0 milestone.

## Third-Party Benchmark Reproduction

Anyone can reproduce the Nexus benchmark results independently.

### Prerequisites

- Linux x86_64 or aarch64 (Ubuntu 22.04+ recommended)
- Rust stable toolchain (`rustup toolchain install stable`)
- `hyperfine` >= 1.18 ([releases](https://github.com/sharkdp/hyperfine/releases))
- `wabt` >= 1.0.36 (for `wat2wasm`)

### Steps

```bash
# 1. Clone the repo at the tagged release
git clone https://github.com/adaptiveliquidity/Nexus.git
cd Nexus
git checkout v1.0.0

# 2. Install toolchain
bash scripts/install_toolchain.sh

# 3. Run full benchmark suite (Criterion + hyperfine + phase capture)
bash scripts/setup_benchmark_env.sh
bash scripts/run_phase1_criterion.sh   # Criterion micro-benchmarks → target/criterion/
bash scripts/run_phase2_hyperfine.sh   # Hyperfine wall-time benchmarks
bash scripts/run_phase3_capture.sh     # Phase 0/3 validation with AI verdict gate

# 4. Compare against published results
# Published results are in docs/baremetal_baselines.md and the live Bencher.dev dashboard:
# https://bencher.dev/perf/nexus
```

### Automated Reproduction via GitHub Actions

Fork the repo, enable Actions, and push any commit to `main`. The
`benchmarks.yml` workflow runs the full suite and uploads to Bencher.dev.

### Discrepancies

If your reproduction yields results that differ substantially from the published
numbers, please open an issue with:
- Hardware spec (CPU model, RAM, OS)
- Rust toolchain version (`rustc --version`)
- The `artifacts/raw/phase3_index.json` output from your run

## Requesting an External Audit

To commission an audit of the Nexus security model, contact:
**contact@adaptiveliquidity.com**

Relevant scope documents to share with auditors:
- This repo at a specific tag
- `docs/AEON_NEXUS_THREAT_MODEL.md`
- `crates/aeon_nexus_bridge/src/lib.rs` (the wire-type crate)
- `src/aeon.rs` (memory evidence & timeline logic)
- `src/security/` (capability model)