#!/usr/bin/env bash
# Phase 2: Cross-platform CLI comparison via hyperfine.
#
# Compares three things that all execute the SAME deterministic WASM payload:
#   1. nexus  execute --wasm test_payload.wasm        (release binary)
#   2. wasmtime test_payload.wasm                     (raw runtime baseline)
#   3. docker run ... nexus-bench-wasmtime wasmtime   (container overhead)
#
# Firecracker and Cloudflare Workers are intentionally NOT included; see
# specs.json -> deviations[] for the rationale. Including fabricated
# competitor numbers would violate the mission's anti-fabrication guardrail.
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$THIS_DIR/bench_env.sh"

cd "$NEXUS_ROOT"

PAYLOAD_WAT="$NEXUS_ROOT/test_payload.wat"
PAYLOAD_WASM="$NEXUS_ROOT/test_payload.wasm"
NEXUS_BIN="$CARGO_TARGET_DIR/release/nexus"
NEXUS_AGENTD="$CARGO_TARGET_DIR/release/nexus-agentd"
DAEMON_SOCKET="/tmp/nexus-agentd-bench.sock"
DOCKER_IMAGE="nexus-bench-wasmtime:latest"
DOCKERFILE="$THIS_DIR/docker/Dockerfile.wasmtime"

# 1. Make sure the payload exists and is up-to-date relative to the .wat.
if [ ! -f "$PAYLOAD_WASM" ] || [ "$PAYLOAD_WAT" -nt "$PAYLOAD_WASM" ]; then
    echo "[phase2] compiling test_payload.wat -> test_payload.wasm"
    wat2wasm "$PAYLOAD_WAT" -o "$PAYLOAD_WASM"
fi

# 2. Make sure the nexus + nexus-agentd release binaries exist.
if [ ! -x "$NEXUS_BIN" ] || [ ! -x "$NEXUS_AGENTD" ]; then
    echo "[phase2] building nexus + nexus-agentd release binaries"
    cargo build --release --bin nexus --bin nexus-agentd
fi

# 3. Make sure the docker image exists.
if ! docker image inspect "$DOCKER_IMAGE" >/dev/null 2>&1; then
    echo "[phase2] building docker image $DOCKER_IMAGE"
    docker build -f "$DOCKERFILE" -t "$DOCKER_IMAGE" "$NEXUS_ROOT"
fi

# 4. Start the daemon for the hot-path measurement.
# Kill any stale daemon first so the bench is reproducible.
if [ -e "$DAEMON_SOCKET" ]; then
    pkill -f "nexus-agentd --socket $DAEMON_SOCKET" 2>/dev/null || true
    rm -f "$DAEMON_SOCKET"
    sleep 0.3
fi

echo "[phase2] starting nexus-agentd in background"
"$NEXUS_AGENTD" --socket "$DAEMON_SOCKET" --pool 4 --fuel 1000000000 --timeout-ms 5000 \
    >"$RAW_DIR/phase2_agentd.log" 2>&1 &
DAEMON_PID=$!
trap 'kill $DAEMON_PID 2>/dev/null; rm -f "$DAEMON_SOCKET"' EXIT

# Wait for the daemon to become reachable.
for i in $(seq 1 40); do
    [ -S "$DAEMON_SOCKET" ] && break
    sleep 0.05
done
if [ ! -S "$DAEMON_SOCKET" ]; then
    echo "[phase2] ERROR: daemon failed to bind $DAEMON_SOCKET"
    cat "$RAW_DIR/phase2_agentd.log"
    exit 1
fi
echo "[phase2] daemon ready"

# 5. Run hyperfine. 30 warmups + at least 100 runs satisfies the mission's
# statistical-power requirement (Hyperfine handles outlier detection itself
# and reports mean/median/stddev/min/max in --export-json).
echo
echo "[phase2] hyperfine: 30 warmups, 100-200 runs per command"

NEXUS_EXEC_CMD="$NEXUS_BIN execute --wasm $PAYLOAD_WASM"
NEXUS_RUN_CMD="$NEXUS_BIN run --wasm $PAYLOAD_WASM --socket $DAEMON_SOCKET"
WASMTIME_CMD="wasmtime $PAYLOAD_WASM"
DOCKER_CMD="docker run --rm -v $NEXUS_ROOT:/app $DOCKER_IMAGE /app/test_payload.wasm"

hyperfine \
    --warmup 30 \
    --min-runs 100 \
    --max-runs 200 \
    --export-json "$RAW_DIR/phase2_hyperfine.json" \
    --export-markdown "$RAW_DIR/phase2_hyperfine.md" \
    --command-name "nexus_cold" "$NEXUS_EXEC_CMD" \
    --command-name "nexus_warm" "$NEXUS_RUN_CMD" \
    --command-name "wasmtime"   "$WASMTIME_CMD" \
    --command-name "docker_wasmtime" "$DOCKER_CMD"

echo
echo "[phase2] hyperfine JSON: $RAW_DIR/phase2_hyperfine.json"
echo "[phase2] hyperfine MD  : $RAW_DIR/phase2_hyperfine.md"
echo
echo "Reminder: Firecracker and Cloudflare Workers are NOT measured (see specs.json -> deviations)."
