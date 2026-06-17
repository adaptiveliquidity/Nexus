#!/usr/bin/env bash
# .codex/scripts/wsl-watch-task.sh — polls log, runs gates, writes result to state
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"
STATE_FILE="${1:?wsl-watch-task.sh <state-file>}"
[[ -f "$STATE_FILE" ]] || { log_error "State file not found: $STATE_FILE"; exit 1; }
LOG=$(state_get log "$STATE_FILE")
WORKTREE=$(state_get worktree "$STATE_FILE")
TASK_BRANCH=$(state_get task_branch "$STATE_FILE")
TIMEOUT="${WATCH_TIMEOUT_SECS:-3600}"
POLL="${WATCH_POLL_SECS:-15}"
log_info "Watching $TASK_BRANCH | log: $LOG | timeout: ${TIMEOUT}s"
elapsed=0
while [[ $elapsed -lt $TIMEOUT ]]; do
  if grep -q "CODEX_DONE" "$LOG" 2>/dev/null; then
    log_ok "Codex completion detected after ${elapsed}s"; break
  fi
  sleep "$POLL"; elapsed=$((elapsed + POLL))
done
if [[ $elapsed -ge $TIMEOUT ]]; then
  log_error "Timed out waiting for Codex after ${TIMEOUT}s"
  state_set "gate_result" "TIMEOUT" "$STATE_FILE"; exit 1
fi
mapfile -t gates < <(state_get_array required_gates "$STATE_FILE")
if run_gates "$WORKTREE" "${gates[@]}"; then
  state_set "gate_result" "PASS" "$STATE_FILE"
  log_ok "GATE PASS — $TASK_BRANCH ready to commit"
else
  FAILURES=$(state_get gate_failures "$STATE_FILE")
  FAILURES=$(( ${FAILURES:-0} + 1 ))
  state_set "gate_failures" "$FAILURES" "$STATE_FILE"
  state_set "gate_result" "FAIL" "$STATE_FILE"
  MAX=$(state_get max_gate_failures "$STATE_FILE")
  if [[ $FAILURES -ge ${MAX:-2} ]]; then
    log_error "Gate failed $FAILURES/${MAX:-2} times — STOP: escalate to Claude"; exit 2
  fi
  log_warn "Gate failed ($FAILURES/${MAX:-2}) — kickback for retry"; exit 1
fi