#!/usr/bin/env bash
# Phase 1: Criterion internal benchmarks (real Nexus APIs).
#
# Runs `cargo bench` (Criterion 0.5) which writes per-bench JSON to
# $CARGO_TARGET_DIR/criterion/<group>/<bench>/new/{estimates.json,sample.json}.
# We copy those into artifacts/raw/criterion/ so the analyzer can parse them
# from a stable location.
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$THIS_DIR/bench_env.sh"

cd "$NEXUS_ROOT"

echo "[phase1] cargo bench --bench nexus_validation"
cargo bench --bench nexus_validation 2>&1 | tee "$RAW_DIR/phase1_criterion.log"

SRC="$CARGO_TARGET_DIR/criterion"
DEST="$RAW_DIR/criterion"
rm -rf "$DEST"
mkdir -p "$DEST"

# Copy only the JSON / metadata Criterion produces; skip HTML reports.
if [ -d "$SRC" ]; then
    (cd "$SRC" && find . -type f \( -name 'estimates.json' -o -name 'sample.json' -o -name 'benchmark.json' \) -print) \
        | while read -r rel; do
            rel="${rel#./}"
            mkdir -p "$DEST/$(dirname "$rel")"
            cp "$SRC/$rel" "$DEST/$rel"
          done
    echo "[phase1] copied criterion JSON to $DEST"
else
    echo "[phase1] WARNING: no criterion output found at $SRC"
fi

echo "[phase1] done."
