#!/usr/bin/env bash
# .codex/scripts/wsl-task-preflight.sh
# Usage: wsl-task-preflight.sh [state-file]
# Validates environment before dispatching a Codex task.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"
. "$HOME/.cargo/env" 2>/dev/null || true
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"

STATE_FILE="${1:-$(state_file)}"
[[ -f "$STATE_FILE" ]] || { log_error "State file not found: $STATE_FILE"; exit 1; }
log_info "Preflight using state: $STATE_FILE"

REPO=$(state_get repo "$STATE_FILE")
MODE=$(state_get mode "$STATE_FILE")
TASK_BRANCH=$(state_get task_branch "$STATE_FILE")
WORKTREE=$(state_get worktree "$STATE_FILE")

log_info "Mode: $MODE | Repo: $REPO | Branch: $TASK_BRANCH"

# 1. Approved repo
validate_repo "$REPO"

# 2. Required tools
require_tool git cargo python3 codex

# 3. No active collisions
no_active_processes "$REPO"

# 4. Base repo HEAD matches expected
BASE_REF=$(state_get base_ref "$STATE_FILE")
ACTUAL_HEAD=$(git -C "$REPO" rev-parse --short "$BASE_REF" 2>/dev/null || echo "unknown")
log_info "Base ref $BASE_REF resolves to $ACTUAL_HEAD"

# 5. No existing worktree conflict
if [[ -n "$WORKTREE" && -d "$WORKTREE" ]]; then
  log_warn "Worktree already exists: $WORKTREE — remove it first or it will be reused"
fi

# 6. Integration branch exists on remote
INT_BRANCH=$(state_get integration_branch "$STATE_FILE")
git -C "$REPO" fetch origin --quiet
git -C "$REPO" rev-parse "origin/$INT_BRANCH" &>/dev/null || {
  log_error "Integration branch not found on remote: $INT_BRANCH"
  exit 1
}

log_ok "PREFLIGHT PASS — ready to dispatch $TASK_BRANCH"