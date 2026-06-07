#!/usr/bin/env bash
# Phase 3: Resilience / AI telemetry capture.
#
# Runs each failing-WASM scenario through the real NexusHypervisor via the
# examples/capture_error.rs binary and saves the resulting ErrorLog JSON to
# artifacts/raw/phase3_<scenario>.json. These files are the *input* to the
# AI validator step that follows (scripts/score_phase3_with_ai.md describes
# the prompt; the scoring itself is recorded in artifacts/raw/phase3_ai_validation.md).
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$THIS_DIR/bench_env.sh"

cd "$NEXUS_ROOT"

EXAMPLE_BIN="$CARGO_TARGET_DIR/release/examples/capture_error"
# Always invoke cargo so stale binaries get rebuilt when the lib changes.
# Cargo's incremental build skips work when nothing changed.
echo "[phase3] ensuring capture_error example is up to date"
cargo build --release --example capture_error

SCENARIOS=(infinite_loop trap_unreachable div_by_zero stack_overflow missing_start \
           memory_out_of_bounds indirect_call_null integer_overflow \
           bad_float_to_int invalid_module)
for s in "${SCENARIOS[@]}"; do
    out="$RAW_DIR/phase3_${s}.json"
    echo "[phase3] capturing $s -> $out"
    "$EXAMPLE_BIN" "$s" "$out"
done

# Produce an index that the AI validator (and analyzer) can use directly.
# Strings that overflow MAX_FIELD_CHARS (e.g. wasmtime stack-overflow
# backtraces hit ~1 MiB) are trimmed; the raw files retain the full text.
python3 "$THIS_DIR/_rebuild_phase3_index.py"

echo
echo "[phase3] capture complete. Run the AI validator step next:"
echo "         see scripts/score_phase3_with_ai.md"
