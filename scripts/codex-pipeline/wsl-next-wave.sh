#!/usr/bin/env bash
# .codex/scripts/wsl-next-wave.sh — swaps in next-wave state and re-dispatches
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"
STATE_FILE="${1:?wsl-next-wave.sh <state-file>}"
NEXT_STATE="${2:?wsl-next-wave.sh <state-file> <next-state-file>}"
PROMPT_FILE="${3:?wsl-next-wave.sh <state-file> <next-state-file> <prompt-file>}"
[[ -f "$STATE_FILE" ]]  || { log_error "State file not found: $STATE_FILE"; exit 1; }
[[ -f "$NEXT_STATE" ]]  || { log_error "Next state file not found: $NEXT_STATE"; exit 1; }
[[ -f "$PROMPT_FILE" ]] || { log_error "Prompt file not found: $PROMPT_FILE"; exit 1; }
INTEGRATED=$(state_get integrated "$STATE_FILE")
[[ "$INTEGRATED" == "true" ]] || { log_error "Current wave not integrated — cannot advance"; exit 1; }
NEXT_WAVE=$(state_get wave_id "$NEXT_STATE")
log_info "Advancing from $(state_get wave_id "$STATE_FILE") to $NEXT_WAVE"
cp "$STATE_FILE" "${STATE_FILE}.prev"
"$SCRIPT_DIR/wsl-task-preflight.sh" "$NEXT_STATE"
"$SCRIPT_DIR/wsl-dispatch-task.sh"  "$NEXT_STATE" "$PROMPT_FILE"
log_ok "Next wave dispatched: $NEXT_WAVE"