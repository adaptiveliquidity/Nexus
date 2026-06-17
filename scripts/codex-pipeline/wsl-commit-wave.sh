#!/usr/bin/env bash
# scripts/codex-pipeline/wsl-commit-wave.sh -- stages ONLY allowed_files, commits, pushes
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib.sh"
STATE_FILE="${1:?wsl-commit-wave.sh <state-file>}"
[[ -f "$STATE_FILE" ]] || { log_error "State file not found: $STATE_FILE"; exit 1; }
WORKTREE=$(state_get worktree "$STATE_FILE")
TASK_BRANCH=$(state_get task_branch "$STATE_FILE")
WAVE_ID=$(state_get wave_id "$STATE_FILE")
GATE_RESULT=$(state_get gate_result "$STATE_FILE")
[[ "$GATE_RESULT" == "PASS" ]] || { log_error "Gate result is '$GATE_RESULT' -- must be PASS before commit"; exit 1; }
validate_branch "$WORKTREE" "$TASK_BRANCH"
mapfile -t allowed < <(state_get_array allowed_files "$STATE_FILE")
[[ ${#allowed[@]} -gt 0 ]] || { log_error "No allowed_files declared in state"; exit 1; }
for f in "${allowed[@]}"; do
  validate_allowed_file "$f" "$WORKTREE" || exit 1
done
validate_clean_except "$WORKTREE" "${allowed[@]}"
staged=()
for f in "${allowed[@]}"; do
  if git -C "$WORKTREE" diff --name-only HEAD -- "$f" | grep -q . 2>/dev/null \
     || git -C "$WORKTREE" ls-files --others --exclude-standard -- "$f" | grep -q . 2>/dev/null; then
    git -C "$WORKTREE" add -- "$f"; staged+=("$f"); log_info "Staged: $f"
  else
    log_info "Unchanged (skip): $f"
  fi
done
[[ ${#staged[@]} -gt 0 ]] || { log_warn "Nothing to stage -- already committed?"; exit 0; }
MSG_FILE="/tmp/codex-commit-msg-${WAVE_ID}.txt"
printf 'feat(proof): %s implementation\n\nWave ID: %s | Branch: %s\nFiles: %s\nGate: PASS\n\nCo-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>\n' \
  "$WAVE_ID" "$WAVE_ID" "$TASK_BRANCH" "${staged[*]}" > "$MSG_FILE"
git -C "$WORKTREE" commit -F "$MSG_FILE"
git -C "$WORKTREE" push origin "$TASK_BRANCH"
state_set "committed" "true" "$STATE_FILE"
log_ok "Committed and pushed $TASK_BRANCH"
