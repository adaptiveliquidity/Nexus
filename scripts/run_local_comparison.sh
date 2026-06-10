#!/usr/bin/env bash
# Reproducible local benchmark comparison: Nexus vs wasmtime vs Docker.
#
# Anyone can clone the repo and run this script to independently verify
# Nexus's performance claims. The script:
#   1. Records full hardware/OS/toolchain provenance
#   2. Builds the Nexus release binary
#   3. Compiles a deterministic WASM payload (fib(30)*10)
#   4. Runs hyperfine comparing: nexus cold-start, wasmtime raw, Docker
#   5. Outputs results to artifacts/local_comparison/
#
# Prerequisites:
#   - Rust toolchain (rustup.rs)
#   - wasmtime CLI: curl https://wasmtime.dev/install.sh -sSf | bash
#   - hyperfine: cargo install hyperfine (or apt install hyperfine)
#   - wat2wasm: apt install wabt (or from github.com/WebAssembly/wabt)
#   - Docker (optional — skipped if unavailable)
#
# Usage:
#   bash scripts/run_local_comparison.sh
#   bash scripts/run_local_comparison.sh --runs 50   # fewer runs (faster)
#   bash scripts/run_local_comparison.sh --no-docker  # skip Docker test
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NEXUS_ROOT="$(cd "$THIS_DIR/.." && pwd)"
OUTDIR="$NEXUS_ROOT/artifacts/local_comparison"
mkdir -p "$OUTDIR"

# ── Parse arguments ──────────────────────────────────────────────────
MIN_RUNS=100
MAX_RUNS=200
WARMUP=10
SKIP_DOCKER=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --runs)     MIN_RUNS="$2"; MAX_RUNS="$2"; shift 2 ;;
    --no-docker) SKIP_DOCKER=true; shift ;;
    --help|-h)
      echo "Usage: $0 [--runs N] [--no-docker]"
      exit 0 ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

# ── Check prerequisites ─────────────────────────────────────────────
missing=()
for cmd in cargo rustc wasmtime hyperfine wat2wasm; do
  command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
done
if [[ ${#missing[@]} -gt 0 ]]; then
  echo "ERROR: missing required tools: ${missing[*]}"
  echo "See the Prerequisites section in this script for install instructions."
  exit 1
fi

if ! $SKIP_DOCKER && ! command -v docker >/dev/null 2>&1; then
  echo "WARN: docker not found — skipping Docker benchmark. Use --no-docker to suppress."
  SKIP_DOCKER=true
fi

# ── Record provenance ───────────────────────────────────────────────
PROVENANCE="$OUTDIR/provenance.json"
timestamp="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
cpu_model="$(lscpu 2>/dev/null | awk -F: '/Model name/{print $2; exit}' | sed 's/^ *//' || echo unknown)"
cpu_cores="$(nproc 2>/dev/null || echo unknown)"
ram_gb="$(awk '/MemTotal/ {printf "%.1f", $2/1024/1024}' /proc/meminfo 2>/dev/null || echo unknown)"
git_sha="$(git -C "$NEXUS_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
git_dirty="$(git -C "$NEXUS_ROOT" diff --quiet 2>/dev/null && echo false || echo true)"

cat > "$PROVENANCE" <<EOF
{
  "timestamp_utc": "$timestamp",
  "cpu": "$cpu_model",
  "cores": "$cpu_cores",
  "ram_gb": "$ram_gb",
  "kernel": "$(uname -r)",
  "rustc": "$(rustc --version)",
  "wasmtime": "$(wasmtime --version)",
  "hyperfine": "$(hyperfine --version)",
  "git_sha": "$git_sha",
  "git_dirty": $git_dirty
}
EOF
echo "[provenance] $PROVENANCE"

# ── Build WASM payload ──────────────────────────────────────────────
PAYLOAD_WAT="$NEXUS_ROOT/test_payload.wat"
PAYLOAD_WASM="$OUTDIR/test_payload.wasm"
if [ ! -f "$PAYLOAD_WAT" ]; then
  echo "ERROR: test_payload.wat not found at $PAYLOAD_WAT"
  exit 1
fi
wat2wasm "$PAYLOAD_WAT" -o "$PAYLOAD_WASM"
echo "[build] compiled test_payload.wasm"

# ── Build Nexus release binary ──────────────────────────────────────
echo "[build] cargo build --release --bin nexus (this may take a few minutes)..."
cargo build --release --bin nexus --manifest-path "$NEXUS_ROOT/Cargo.toml" 2>&1 | tail -3
NEXUS_BIN="$NEXUS_ROOT/target/release/nexus"
if [ ! -x "$NEXUS_BIN" ]; then
  echo "ERROR: nexus binary not found at $NEXUS_BIN"
  exit 1
fi

# ── Build Docker image (optional) ──────────────────────────────────
DOCKER_IMAGE="nexus-bench-wasmtime:local"
DOCKERFILE="$NEXUS_ROOT/scripts/docker/Dockerfile.wasmtime"
if ! $SKIP_DOCKER; then
  echo "[build] building Docker image for wasmtime baseline..."
  docker build -f "$DOCKERFILE" -t "$DOCKER_IMAGE" "$NEXUS_ROOT" 2>&1 | tail -3
fi

# ── Run hyperfine ───────────────────────────────────────────────────
echo
echo "════════════════════════════════════════════════════════════════"
echo "  Benchmark: $WARMUP warmups, $MIN_RUNS-$MAX_RUNS runs each"
echo "════════════════════════════════════════════════════════════════"
echo

CMDS=(
  --command-name "nexus (cold)"
  "$NEXUS_BIN execute --wasm $PAYLOAD_WASM"
  --command-name "wasmtime (raw)"
  "wasmtime $PAYLOAD_WASM"
)

if ! $SKIP_DOCKER; then
  CMDS+=(
    --command-name "docker + wasmtime"
    "docker run --rm -v $OUTDIR:/data $DOCKER_IMAGE /data/test_payload.wasm"
  )
fi

hyperfine \
  --warmup "$WARMUP" \
  --min-runs "$MIN_RUNS" \
  --max-runs "$MAX_RUNS" \
  --export-json "$OUTDIR/results.json" \
  --export-markdown "$OUTDIR/results.md" \
  "${CMDS[@]}"

# ── Summary ─────────────────────────────────────────────────────────
echo
echo "════════════════════════════════════════════════════════════════"
echo "  Results saved to: $OUTDIR/"
echo "════════════════════════════════════════════════════════════════"
echo
echo "Files:"
echo "  provenance.json  — hardware/toolchain fingerprint"
echo "  results.json     — hyperfine raw data"
echo "  results.md       — human-readable comparison table"
echo
echo "To share your results, attach all three files."
echo "To compare with CI numbers, see: https://bencher.dev/perf/nexus-ai"
