#!/usr/bin/env bash
# .codex/scripts/wsl-integrate-wave.sh — merges task branch into integration branch, pushes
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"
STATE_FILE="${1:?wsl-integrate-wave.sh <state-file>}"
[[ -f "$STATE_FILE" ]] || { log_error "State file not found: $STATE_FILE"; exit 1; }
REPO=$(state_get repo "$STATE_FILE")
INT_BRANCH=$(state_get integration_branch "$STATE_FILE")
TASK_BRANCH=$(state_get task_branch "$STATE_FILE")
WORKTREE=$(state_get worktree "$STATE_FILE")
WAVE_ID=$(state_get wave_id "$STATE_FILE")
COMMITTED=$(state_get committed "$STATE_FILE")
[[ "$COMMITTED" == "true" ]] || { log_error "Task not committed yet — run wsl-commit-wave.sh first"; exit 1; }
git -C "$REPO" fetch origin --quiet
git -C "$REPO" checkout "$INT_BRANCH"
git -C "$REPO" pull origin "$INT_BRANCH" --ff-only
MSG_FILE="/tmp/codex-merge-msg-${WAVE_ID}.txt"
printf 'merge(proof): %s from %s\n\nWave %s gates passed; merging into %s.\n' \
  "$TASK_BRANCH" "$TASK_BRANCH" "$WAVE_ID" "$INT_BRANCH" > "$MSG_FILE"
git -C "$REPO" merge --no-ff "$TASK_BRANCH" -F "$MSG_FILE"
git -C "$REPO" push origin "$INT_BRANCH"
state_set "integrated" "true" "$STATE_FILE"
log_ok "Integrated $TASK_BRANCH into $INT_BRANCH and pushed"
worktree_remove "$REPO" "$WORKTREE"