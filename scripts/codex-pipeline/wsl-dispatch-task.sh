#!/usr/bin/env bash
# .codex/scripts/wsl-dispatch-task.sh
# Usage: wsl-dispatch-task.sh <state-file> <prompt-file>
# Creates a git worktree for the task, fires Codex, returns immediately.
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
BASE_REF=$(state_get base_ref "$STATE_FILE")
LOG=$(state_get log "$STATE_FILE")
INT_BRANCH=$(state_get integration_branch "$STATE_FILE")

mkdir -p "$(dirname "$LOG")"
: > "$LOG"
echo "### DISPATCH $(date -u) task=$TASK_BRANCH worktree=$WORKTREE" >> "$LOG"

# Ensure base is up to date
git -C "$REPO" fetch origin --quiet

# Create isolated worktree (each task gets its own — no checkout conflicts)
worktree_create "$REPO" "$WORKTREE" "$TASK_BRANCH" "origin/$INT_BRANCH"

# Write Codex invocation to a temp script so we can background it cleanly
RUNNER="/tmp/codex-run-$(basename "$TASK_BRANCH").sh"
cat > "$RUNNER" <<RUNNER_EOF
#!/usr/bin/env bash
. "$HOME/.cargo/env" 2>/dev/null || true
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:\$PATH"
codex e \\
  -m gpt-5.5 \\
  -c model_reasoning_effort="xhigh" \\
  -c model_reasoning_summary="auto" \\
  -c approval_policy="never" \\
  -s workspace-write \\
  --add-dir "$CODEX_RUNS_DIR" \\
  --add-dir /tmp \\
  --skip-git-repo-check \\
  -C "$WORKTREE" \\
  - < "$PROMPT_FILE" >> "$LOG" 2>&1
echo "=== CODEX_DONE exit=\$? $(date -u) ===" >> "$LOG"
git -C "$WORKTREE" status --short >> "$LOG" 2>&1
RUNNER_EOF
chmod +x "$RUNNER"
nohup bash "$RUNNER" &
echo "DISPATCH_PID=$!" >> "$LOG"
log_ok "Dispatched $TASK_BRANCH (pid=$!) — monitor: tail -f $LOG"