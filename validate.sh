#!/usr/bin/env bash
# Nexus Validation & Benchmarking Protocol — top-level orchestrator.
#
# Replaces the previous demo script that wrote hardcoded JSON.
# This version dispatches the real per-phase scripts under scripts/ and
# produces only data that was actually measured on the running host.
#
# Phases:
#   0  Environment audit (scripts/setup_benchmark_env.sh)
#   1  Criterion internal benches  (cargo bench --release)
#   2  Hyperfine cross-platform    (scripts/run_phase2_hyperfine.sh)
#   3  AI telemetry capture        (scripts/run_phase3_capture.sh)
#   R  Analyzer + report           (scripts/analyze_and_report.py)
#
# Usage:
#   bash validate.sh [phase ...]
# Examples:
#   bash validate.sh           # run all phases
#   bash validate.sh 0 1       # phase 0 then 1
#   bash validate.sh report    # only re-run the analyzer/report
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS="$THIS_DIR/scripts"
# shellcheck disable=SC1091
. "$SCRIPTS/bench_env.sh"

cd "$NEXUS_ROOT"

phases=("$@")
if [ ${#phases[@]} -eq 0 ]; then
    phases=(0 1 2 3 report)
fi

for p in "${phases[@]}"; do
    echo
    echo "================================================================"
    echo " Nexus Validation: Phase $p"
    echo "================================================================"
    case "$p" in
        0)
            bash "$SCRIPTS/setup_benchmark_env.sh"
            ;;
        1)
            bash "$SCRIPTS/run_phase1_criterion.sh"
            ;;
        2)
            bash "$SCRIPTS/run_phase2_hyperfine.sh"
            ;;
        3)
            bash "$SCRIPTS/run_phase3_capture.sh"
            ;;
        report|R|r)
            python3 "$SCRIPTS/analyze_and_report.py" \
                --specs-json "$RAW_DIR/../specs.json" \
                --criterion-target "$CARGO_TARGET_DIR/criterion" \
                --hyperfine-json "$RAW_DIR/phase2_hyperfine.json" \
                --phase3-dir "$RAW_DIR" \
                --output-report "$NEXUS_ROOT/VALIDATION_REPORT.md" \
                --plots-dir "$PLOTS_DIR"
            ;;
        *)
            echo "Unknown phase: $p"
            echo "Valid: 0 1 2 3 report"
            exit 2
            ;;
    esac
done

echo
echo "All requested phases finished."
echo "Artifacts: $ARTIFACTS_DIR"
echo "Report:    $NEXUS_ROOT/VALIDATION_REPORT.md"
