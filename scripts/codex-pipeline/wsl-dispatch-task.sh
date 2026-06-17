#!/usr/bin/env bash
# scripts/codex-pipeline/wsl-dispatch-task.sh
# Usage: wsl-dispatch-task.sh <state-file> <prompt-file>
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"
. "$HOME/.cargo/env" 2>/dev/null || true
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"

STATE_FILE="${1:?Usage: wsl-dispatch-task.sh <state-file> <prompt-file>}"
PROMPT_FILE="${2:?Usage: wsl-dispatch-task.sh <state-file> <prompt-file>}"
[[ -f "$STATE_FILE" ]]  || { log_error "State file not found: $STATE_FILE"; exit 1; }
[[ -f "$PROMPT_FILE" ]] || { log_error "Prompt file not found: $PROMPT_FILE"; exit 1; }

REPO=$(state_get repo "$STATE_FILE")
TASK_BRANCH=$(state_get task_branch "$STATE_FILE")
WORKTREE=$(state_get worktree "$STATE_FILE")
LOG=$(state_get log "$STATE_FILE")
INT_BRANCH=$(state_get integration_branch "$STATE_FILE")
WAVE_ID=$(state_get wave_id "$STATE_FILE")

mkdir -p "$(dirname "$LOG")"
: > "$LOG"
echo "### DISPATCH $(date -u) task=$TASK_BRANCH worktree=$WORKTREE" >> "$LOG"

exec 9>"$CODEX_LOCK"
flock -n 9 || { log_error "Another dispatch is running (lock: $CODEX_LOCK)"; exit 1; }

git -C "$REPO" fetch origin --quiet
worktree_create "$REPO" "$WORKTREE" "$TASK_BRANCH" "origin/$INT_BRANCH"

umask 0177
RUNNER=$(mktemp "/tmp/codex-run-${WAVE_ID}-XXXXXX.sh")
RUNNER_TEMPLATE="$SCRIPT_DIR/wsl-runner-template.sh"
[[ -f "$RUNNER_TEMPLATE" ]] || { log_error "Runner template missing: $RUNNER_TEMPLATE"; exit 1; }
sed \
  -e "s|__CARGO_ENV__|$HOME/.cargo/env|g" \
  -e "s|__CODEX_RUNS_DIR__|$CODEX_RUNS_DIR|g" \
  -e "s|__WORKTREE__|$WORKTREE|g" \
  -e "s|__PROMPT_FILE__|$PROMPT_FILE|g" \
  -e "s|__LOG__|$LOG|g" \
  "$RUNNER_TEMPLATE" > "$RUNNER"
chmod 0700 "$RUNNER"

setsid bash "$RUNNER" 9>&- &
DISPATCH_PID=$!
echo "DISPATCH_PID=$DISPATCH_PID" >> "$LOG"
state_set "dispatch_pid" "$DISPATCH_PID" "$STATE_FILE"
log_ok "Dispatched $TASK_BRANCH (pid=$DISPATCH_PID) -- monitor: tail -f $LOG"
