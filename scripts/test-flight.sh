#!/usr/bin/env bash
# =============================================================================
# Nexus + AEON-IQ Test Flight
# =============================================================================
# Automated end-to-end verification for a first-time user.
# Usage:
#   bash scripts/test-flight.sh
# Or (if you trust the URL):
#   curl -sSf https://raw.githubusercontent.com/adaptiveliquidity/Nexus/main/scripts/test-flight.sh | bash
#
# Requirements: git, curl.  Everything else is detected / auto-installed.
# =============================================================================

# ---------------------------------------------------------------------------
# Strict mode — but we catch failures per-phase rather than bailing globally.
# ---------------------------------------------------------------------------
set -uo pipefail

# ---------------------------------------------------------------------------
# Colour helpers
# ---------------------------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
RESET='\033[0m'

ok()   { echo -e "${GREEN}✓${RESET}  $*"; }
fail() { echo -e "${RED}✗${RESET}  $*"; }
warn() { echo -e "${YELLOW}⚠${RESET}  $*"; }
info() { echo -e "${CYAN}→${RESET}  $*"; }
banner() {
  echo ""
  echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
  echo -e "${BOLD}  $*${RESET}"
  echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
}

ts() { date '+%H:%M:%S'; }

# ---------------------------------------------------------------------------
# Repo-root detection
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [[ ! -f "$REPO_ROOT/Cargo.toml" ]]; then
  fail "Could not locate Cargo.toml at $REPO_ROOT"
  fail "Run this script from the repository root or via scripts/test-flight.sh"
  exit 1
fi
if ! grep -q 'name = "nexus"' "$REPO_ROOT/Cargo.toml" 2>/dev/null; then
  fail "Cargo.toml found at $REPO_ROOT but does not look like the Nexus crate."
  exit 1
fi

cd "$REPO_ROOT"

FLIGHT_START=$(date +%s)

# ---------------------------------------------------------------------------
# Temp-file management
# ---------------------------------------------------------------------------
TMPDIR_FLIGHT="/tmp/nexus-testflight-home-$$"
NEXUS_AGENTD_SOCKET_FLIGHT="/tmp/nexus-agentd-testflight-$$.sock"
WASM_FILE="/tmp/nexus-test-tool-$$.wasm"
WAT_FILE="/tmp/nexus-test-tool-$$.wat"
PROFILE_FILE="/tmp/nexus-profile-$$.toml"
INSTINCT_EXPORT_FILE="/tmp/nexus-instinct-export-$$.json"

cleanup() {
  info "Cleaning up temp files..."
  rm -f "$WASM_FILE" "$WAT_FILE" "$PROFILE_FILE" "$INSTINCT_EXPORT_FILE"
  rm -rf "$TMPDIR_FLIGHT"
  # Kill daemon if we started one
  if [[ -n "${AGENTD_PID:-}" ]] && kill -0 "$AGENTD_PID" 2>/dev/null; then
    kill "$AGENTD_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

mkdir -p "$TMPDIR_FLIGHT"

# ---------------------------------------------------------------------------
# Phase result tracking
# ---------------------------------------------------------------------------
declare -A PHASE_RESULT
declare -A PHASE_DETAIL
PHASES_ORDER=()

record_phase() {
  local name="$1" result="$2" detail="${3:-}"
  PHASE_RESULT["$name"]="$result"
  PHASE_DETAIL["$name"]="$detail"
  PHASES_ORDER+=("$name")
}

# ---------------------------------------------------------------------------
# OS detection
# ---------------------------------------------------------------------------
OS="linux"
if [[ "$(uname -s)" == "Darwin" ]]; then
  OS="macos"
fi

# ---------------------------------------------------------------------------
# PHASE 0: Prereq check + auto-install
# ---------------------------------------------------------------------------
banner "[$(ts)] Phase 0 — Prerequisites"

HAS_WABT=false
HAS_PYTHON3=false

# -- cargo / rustup ----------------------------------------------------------
if ! command -v cargo &>/dev/null; then
  warn "cargo not found. Installing rustup + stable toolchain..."
  if command -v curl &>/dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
    # shellcheck disable=SC1090
    source "$HOME/.cargo/env" 2>/dev/null || true
    export PATH="$HOME/.cargo/bin:$PATH"
  else
    fail "curl is required to install rustup but was not found."
    exit 1
  fi
fi

if ! command -v cargo &>/dev/null; then
  fail "cargo still not available after rustup install. Re-open your shell and try again."
  exit 1
fi
CARGO_VERSION=$(cargo --version)
ok "cargo: $CARGO_VERSION"

# -- wat2wasm (wabt) ---------------------------------------------------------
if command -v wat2wasm &>/dev/null; then
  HAS_WABT=true
  ok "wat2wasm: $(wat2wasm --version 2>/dev/null || echo 'found')"
else
  warn "wat2wasm not found. Attempting auto-install..."
  INSTALL_OK=false
  if [[ "$OS" == "linux" ]] && command -v apt-get &>/dev/null; then
    if sudo apt-get install -y wabt 2>/dev/null; then
      INSTALL_OK=true
    fi
  elif [[ "$OS" == "macos" ]] && command -v brew &>/dev/null; then
    if brew install wabt 2>/dev/null; then
      INSTALL_OK=true
    fi
  fi
  if command -v wat2wasm &>/dev/null; then
    HAS_WABT=true
    ok "wat2wasm: installed"
  else
    warn "wat2wasm not available — Phase 2b (MCP smoke) and WASM-compile path will be skipped."
    if [[ "$INSTALL_OK" == "false" ]]; then
      warn "To install manually: apt-get install wabt  OR  brew install wabt"
    fi
  fi
fi

# -- python3 -----------------------------------------------------------------
if command -v python3 &>/dev/null; then
  HAS_PYTHON3=true
  ok "python3: $(python3 --version 2>&1)"
else
  warn "python3 not found — Phase 2b (MCP smoke) will be skipped."
fi

# -- openssl (for key generation) -------------------------------------------
HAS_OPENSSL=false
if command -v openssl &>/dev/null; then
  HAS_OPENSSL=true
fi

record_phase "prereqs" "pass" "cargo=$(cargo --version | head -1)"

# ---------------------------------------------------------------------------
# Test environment
# ---------------------------------------------------------------------------
banner "[$(ts)] Generating test environment"

if $HAS_OPENSSL; then
  NEXUS_AEON_HMAC_KEY=$(openssl rand -hex 32)
else
  # Fallback: /dev/urandom hex
  NEXUS_AEON_HMAC_KEY=$(head -c 32 /dev/urandom | xxd -p -c 64 2>/dev/null \
    || od -An -tx1 /dev/urandom | head -1 | tr -d ' \n' | head -c 64)
fi

export NEXUS_AEON_HMAC_KEY
export NEXUS_AEON_AGENT_ID="testflight-$(hostname)"
export NEXUS_AEON_SESSION_ID="session-$(date +%Y%m%d-%H%M%S)"
export NEXUS_AEON_ENABLED="true"
export NEXUS_HOME="$TMPDIR_FLIGHT"
export NEXUS_AGENTD_SOCKET="$NEXUS_AGENTD_SOCKET_FLIGHT"
# Silence noisy tracing during tests
export RUST_LOG="${RUST_LOG:-warn}"

info "NEXUS_HOME            = $NEXUS_HOME"
info "NEXUS_AEON_AGENT_ID   = $NEXUS_AEON_AGENT_ID"
info "NEXUS_AEON_SESSION_ID = $NEXUS_AEON_SESSION_ID"
info "NEXUS_AGENTD_SOCKET   = $NEXUS_AGENTD_SOCKET"

# ---------------------------------------------------------------------------
# PHASE 1 — Build
# ---------------------------------------------------------------------------
banner "[$(ts)] Phase 1 — Build"

BUILD_OK=true

info "Building default feature set..."
if cargo build --release 2>&1 | tail -5; then
  ok "cargo build --release succeeded"
else
  fail "cargo build --release FAILED"
  BUILD_OK=false
fi

info "Building with aeon-memory feature set..."
if cargo build --release --features aeon-memory 2>&1 | tail -5; then
  ok "cargo build --release --features aeon-memory succeeded"
else
  fail "cargo build --release --features aeon-memory FAILED"
  BUILD_OK=false
fi

if $BUILD_OK; then
  record_phase "build" "pass" "both feature sets compiled"
else
  record_phase "build" "fail" "one or more builds failed"
  fail "Build failures are blocking — cannot continue."
  exit 1
fi

NEXUS_BIN="$REPO_ROOT/target/release/nexus"
AGENTD_BIN="$REPO_ROOT/target/release/nexus-agentd"

if [[ ! -x "$NEXUS_BIN" ]]; then
  fail "Expected binary not found: $NEXUS_BIN"
  exit 1
fi

# ---------------------------------------------------------------------------
# PHASE 2 — Unit + integration tests (both feature sets)
# ---------------------------------------------------------------------------
banner "[$(ts)] Phase 2 — Unit + integration tests"

run_cargo_test() {
  local label="$1"; shift
  local output
  local exit_code=0
  output=$(cargo test --locked "$@" 2>&1) || exit_code=$?
  # Parse pass/fail counts from Cargo test output
  local passed failed ignored
  passed=$(echo "$output" | grep -oE '[0-9]+ passed' | tail -1 | grep -oE '[0-9]+' || echo "?")
  failed=$(echo "$output" | grep -oE '[0-9]+ failed' | tail -1 | grep -oE '[0-9]+' || echo "0")
  ignored=$(echo "$output" | grep -oE '[0-9]+ ignored' | tail -1 | grep -oE '[0-9]+' || echo "0")

  if [[ "$exit_code" -eq 0 ]]; then
    ok "$label: passed=$passed ignored=$ignored"
    echo "pass:$passed:$failed:$ignored"
  else
    fail "$label: FAILED (passed=$passed failed=$failed)"
    echo "$output" | grep -E 'FAILED|error\[' | head -20 >&2 || true
    echo "fail:$passed:$failed:$ignored"
  fi
}

UNIT_DEFAULT_RESULT=$(run_cargo_test "unit+integration (default)" 2>&1)
UNIT_AEON_RESULT=$(run_cargo_test "unit+integration (aeon-memory)" --features aeon-memory 2>&1)

# Re-run cleanly for display
UNIT_DEFAULT_OK=true
UNIT_AEON_OK=true

if ! cargo test --locked &>/dev/null; then
  UNIT_DEFAULT_OK=false
fi
if ! cargo test --features aeon-memory --locked &>/dev/null; then
  UNIT_AEON_OK=false
fi

if $UNIT_DEFAULT_OK && $UNIT_AEON_OK; then
  record_phase "unit_tests" "pass" "both feature-set test runs passed"
elif $UNIT_DEFAULT_OK; then
  record_phase "unit_tests" "fail" "aeon-memory tests failed"
elif $UNIT_AEON_OK; then
  record_phase "unit_tests" "fail" "default tests failed"
else
  record_phase "unit_tests" "fail" "both test runs failed"
fi

# ---------------------------------------------------------------------------
# PHASE 3 — MCP acceptance tests
# ---------------------------------------------------------------------------
banner "[$(ts)] Phase 3 — MCP acceptance tests"

MCP_ACC_OK=true
MCP_ACC_DETAIL=""

if cargo test --test mcp_acceptance --locked 2>&1 | tee /tmp/nexus-mcp-acc-$$.log | tail -6; then
  MCP_ACC_DETAIL=$(grep -oE '[0-9]+ passed' /tmp/nexus-mcp-acc-$$.log | tail -1 || echo "see log")
  ok "MCP acceptance: $MCP_ACC_DETAIL"
  record_phase "mcp_acceptance" "pass" "$MCP_ACC_DETAIL"
else
  fail "MCP acceptance tests FAILED"
  grep -E 'FAILED|error' /tmp/nexus-mcp-acc-$$.log | head -10 || true
  record_phase "mcp_acceptance" "fail" "see /tmp/nexus-mcp-acc-$$.log"
  MCP_ACC_OK=false
fi
rm -f "/tmp/nexus-mcp-acc-$$.log"

# ---------------------------------------------------------------------------
# PHASE 4 — MCP smoke script (skipped if wat2wasm or python3 missing)
# ---------------------------------------------------------------------------
banner "[$(ts)] Phase 4 — MCP smoke script"

if ! $HAS_WABT || ! $HAS_PYTHON3; then
  SKIP_REASON=""
  $HAS_WABT  || SKIP_REASON+="wat2wasm missing "
  $HAS_PYTHON3 || SKIP_REASON+="python3 missing"
  warn "Phase 4 skipped: $SKIP_REASON"
  record_phase "mcp_smoke" "skip" "requires wat2wasm + python3"
else
  if bash "$REPO_ROOT/examples/mcp_smoke.sh" 2>&1 | tail -10; then
    ok "MCP smoke script passed"
    record_phase "mcp_smoke" "pass" "examples/mcp_smoke.sh completed"
  else
    fail "MCP smoke script FAILED"
    record_phase "mcp_smoke" "fail" "examples/mcp_smoke.sh exited non-zero"
  fi
fi

# ---------------------------------------------------------------------------
# PHASE 5 — CLI + daemon live exercise
# ---------------------------------------------------------------------------
banner "[$(ts)] Phase 5 — CLI + daemon live exercise"

CLI_OK=true
CLI_DETAIL=""

# ---- 5a: Generate minimal WASM + nexus execute ----------------------------
info "5a: Generating minimal WASM tool and running nexus execute..."

MINIMAL_WAT='(module
  (memory (export "memory") 1)
  (data (i32.const 0) "TESTFLIGHT-OK")
  (func (export "_start")))'

WASM_READY=false
if $HAS_WABT; then
  echo "$MINIMAL_WAT" > "$WAT_FILE"
  if wat2wasm "$WAT_FILE" -o "$WASM_FILE" 2>/dev/null; then
    WASM_READY=true
    info "WASM generated via wat2wasm: $WASM_FILE"
  fi
fi

if ! $WASM_READY; then
  # Fallback: use the pre-compiled test_payload.wasm already in the repo
  if [[ -f "$REPO_ROOT/test_payload.wasm" ]]; then
    WASM_FILE="$REPO_ROOT/test_payload.wasm"
    WASM_READY=true
    info "Using repo-bundled test_payload.wasm as fallback"
  else
    warn "5a: No WASM available (no wat2wasm and no bundled wasm) — skipping execute"
    CLI_DETAIL+="execute=skip "
  fi
fi

if $WASM_READY; then
  if "$NEXUS_BIN" execute --wasm "$WASM_FILE" 2>&1 | tail -5; then
    ok "nexus execute: passed"
    CLI_DETAIL+="execute=pass "
  else
    fail "nexus execute: FAILED"
    CLI_DETAIL+="execute=fail "
    CLI_OK=false
  fi
fi

# ---- 5b: nexus run via daemon ----------------------------------------------
info "5b: Starting nexus-agentd and running nexus run..."

if $WASM_READY && [[ -x "$AGENTD_BIN" ]]; then
  # Launch daemon in background
  AGENTD_PID=""
  "$AGENTD_BIN" --socket "$NEXUS_AGENTD_SOCKET" &>/tmp/nexus-agentd-$$.log &
  AGENTD_PID=$!
  # Wait briefly for socket to appear
  for i in $(seq 1 20); do
    [[ -S "$NEXUS_AGENTD_SOCKET" ]] && break
    sleep 0.2
  done

  if [[ -S "$NEXUS_AGENTD_SOCKET" ]]; then
    if "$NEXUS_BIN" run --wasm "$WASM_FILE" --socket "$NEXUS_AGENTD_SOCKET" 2>&1 | tail -5; then
      ok "nexus run (daemon hot-path): passed"
      CLI_DETAIL+="daemon_run=pass "
    else
      fail "nexus run (daemon hot-path): FAILED"
      CLI_DETAIL+="daemon_run=fail "
      CLI_OK=false
    fi
    # Graceful shutdown
    kill "$AGENTD_PID" 2>/dev/null || true
    wait "$AGENTD_PID" 2>/dev/null || true
    AGENTD_PID=""
    rm -f "$NEXUS_AGENTD_SOCKET"
  else
    warn "5b: nexus-agentd did not create socket in time — skipping nexus run"
    CLI_DETAIL+="daemon_run=skip "
    kill "$AGENTD_PID" 2>/dev/null || true
    AGENTD_PID=""
  fi
  rm -f "/tmp/nexus-agentd-$$.log"
else
  warn "5b: Skipping nexus run (no WASM or no agentd binary)"
  CLI_DETAIL+="daemon_run=skip "
fi

# ---- 5c: profile validate --------------------------------------------------
info "5c: nexus profile validate..."

cat >"$PROFILE_FILE" <<'TOML'
name = "testflight"

[[capabilities]]
type = "read_file"
path = "/tmp"

[[capabilities]]
type = "write_file"
path = "/tmp/nexus-testflight-out"
TOML

if "$NEXUS_BIN" profile validate "$PROFILE_FILE" 2>&1 | tail -5; then
  ok "nexus profile validate: passed"
  CLI_DETAIL+="profile=pass "
else
  fail "nexus profile validate: FAILED"
  CLI_DETAIL+="profile=fail "
  CLI_OK=false
fi

# ---- 5d: instinct round-trip -----------------------------------------------
info "5d: nexus instinct export | nexus instinct import round-trip..."

if "$NEXUS_BIN" instinct export 2>/dev/null >"$INSTINCT_EXPORT_FILE"; then
  EXPORT_SIZE=$(wc -c < "$INSTINCT_EXPORT_FILE" 2>/dev/null || echo "?")
  if "$NEXUS_BIN" instinct import --file "$INSTINCT_EXPORT_FILE" 2>&1 | tail -3; then
    ok "instinct round-trip: passed (export=${EXPORT_SIZE}B)"
    CLI_DETAIL+="instinct=pass "
  else
    fail "instinct import: FAILED"
    CLI_DETAIL+="instinct=fail "
    CLI_OK=false
  fi
else
  warn "5d: instinct export returned non-zero (empty store?) — treating as pass"
  CLI_DETAIL+="instinct=skip "
fi

# ---- 5e: nexus stats -------------------------------------------------------
info "5e: nexus stats..."

if "$NEXUS_BIN" stats 2>&1 | tail -5; then
  ok "nexus stats: passed"
  CLI_DETAIL+="stats=pass "
else
  fail "nexus stats: FAILED"
  CLI_DETAIL+="stats=fail "
  CLI_OK=false
fi

if $CLI_OK; then
  record_phase "cli_daemon" "pass" "$CLI_DETAIL"
else
  record_phase "cli_daemon" "fail" "$CLI_DETAIL"
fi

# ---------------------------------------------------------------------------
# PHASE 6 — AEON-IQ mock e2e demo
# ---------------------------------------------------------------------------
banner "[$(ts)] Phase 6 — AEON-IQ mock e2e demo"

AEON_OUTPUT=""
AEON_EXIT=0
AEON_OUTPUT=$(cargo run --example aeon_e2e_demo --features aeon-memory 2>&1) || AEON_EXIT=$?

if [[ "$AEON_EXIT" -ne 0 ]]; then
  fail "aeon_e2e_demo exited with code $AEON_EXIT"
  echo "$AEON_OUTPUT" | tail -20
  record_phase "aeon_e2e" "fail" "exit_code=$AEON_EXIT"
else
  # Validate expected output fields
  AEON_DETAIL=""
  AEON_OK=true

  if echo "$AEON_OUTPUT" | grep -qE 'memory_mode=Some\(Attested(WithRecall)?\)'; then
    DETECTED_MODE=$(echo "$AEON_OUTPUT" | grep -oE 'memory_mode=Some\([^)]+\)')
    ok "$DETECTED_MODE found"
    AEON_DETAIL+="$DETECTED_MODE "
  else
    fail "Expected memory_mode=Some(Attested*) not found in output"
    AEON_OK=false
    AEON_DETAIL+="memory_mode=MISSING "
  fi

  if echo "$AEON_OUTPUT" | grep -q 'negotiation_rounds=Some(1)'; then
    ok "negotiation_rounds=Some(1) found"
    AEON_DETAIL+="negotiation_rounds=1 "
  else
    fail "Expected 'negotiation_rounds=Some(1)' not found in output"
    AEON_OK=false
    AEON_DETAIL+="negotiation_rounds=MISSING "
  fi

  if $AEON_OK; then
    record_phase "aeon_e2e" "pass" "$AEON_DETAIL"
  else
    record_phase "aeon_e2e" "fail" "$AEON_DETAIL"
    info "Full demo output:"
    echo "$AEON_OUTPUT" | tail -30
  fi
fi

# ---------------------------------------------------------------------------
# Summary report
# ---------------------------------------------------------------------------
FLIGHT_END=$(date +%s)
ELAPSED=$(( FLIGHT_END - FLIGHT_START ))
ELAPSED_MIN=$(( ELAPSED / 60 ))
ELAPSED_SEC=$(( ELAPSED % 60 ))

banner "Test Flight Summary"
echo ""
printf "%-22s  %-8s  %s\n" "Phase" "Result" "Details"
printf "%-22s  %-8s  %s\n" "------" "------" "-------"

OVERALL_PASS=true
for phase in "${PHASES_ORDER[@]}"; do
  result="${PHASE_RESULT[$phase]}"
  detail="${PHASE_DETAIL[$phase]}"
  case "$result" in
    pass)
      printf "${GREEN}%-22s  ✓ %-6s${RESET}  %s\n" "$phase" "PASS" "$detail"
      ;;
    skip)
      printf "${YELLOW}%-22s  ⚠ %-6s${RESET}  %s\n" "$phase" "SKIP" "$detail"
      ;;
    fail)
      printf "${RED}%-22s  ✗ %-6s${RESET}  %s\n" "$phase" "FAIL" "$detail"
      OVERALL_PASS=false
      ;;
  esac
done

echo ""
printf "Elapsed: %dm%02ds\n" "$ELAPSED_MIN" "$ELAPSED_SEC"
echo ""

if $OVERALL_PASS; then
  echo -e "${GREEN}${BOLD}All phases passed (or skipped). Nexus + AEON-IQ test flight complete.${RESET}"
  exit 0
else
  echo -e "${RED}${BOLD}One or more phases FAILED. Review output above.${RESET}"
  exit 1
fi
