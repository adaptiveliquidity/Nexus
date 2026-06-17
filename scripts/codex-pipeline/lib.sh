#!/usr/bin/env bash
# .codex/scripts/lib.sh — shared functions for the Nexus WSL Codex pipeline
# Source this file; do not execute directly.
set -euo pipefail

CODEX_STATE_DIR="${CODEX_STATE_DIR:-/home/ahpsi/nexus/.codex/state}"
CODEX_RUNS_DIR="${CODEX_RUNS_DIR:-/home/ahpsi/codex-runs}"
CODEX_LOCK="${CODEX_RUNS_DIR}/.dispatch.lock"
APPROVED_REPO="/home/ahpsi/nexus"

# ── logging ──────────────────────────────────────────────────────────────────
log_info()  { echo "[INFO  $(date -u +%H:%M:%S)] $*"; }
log_warn()  { echo "[WARN  $(date -u +%H:%M:%S)] $*" >&2; }
log_error() { echo "[ERROR $(date -u +%H:%M:%S)] $*" >&2; }
log_ok()    { echo "[OK    $(date -u +%H:%M:%S)] $*"; }

# ── state file ───────────────────────────────────────────────────────────────
state_file() { echo "${CODEX_STATE_DIR}/${1:-current-task}.json"; }

state_get() {
  local key="$1" file="${2:-$(state_file)}"
  python3 -c "import json,sys; d=json.load(open('${file}')); print(d.get('${key}',''))" 2>/dev/null
}

state_set() {
  local key="$1" val="$2" file="${3:-$(state_file)}"
  python3 - <<PYEOF
import json, sys
with open('${file}') as f: d = json.load(f)
d['${key}'] = '${val}'
with open('${file}', 'w') as f: json.dump(d, f, indent=2)
PYEOF
}

state_get_array() {
  local key="$1" file="${2:-$(state_file)}"
  python3 -c "import json; d=json.load(open('${file}')); [print(x) for x in d.get('${key}',[])]" 2>/dev/null
}

# ── validation ───────────────────────────────────────────────────────────────
require_tool() {
  for t in "$@"; do
    command -v "$t" &>/dev/null || { log_error "Required tool missing: $t"; return 1; }
  done
}

validate_repo() {
  local repo="${1:-$APPROVED_REPO}"
  # Must be the approved WSL repo, never /mnt/c
  [[ "$repo" == /home/* ]] || { log_error "Repo must be under /home, got: $repo"; return 1; }
  [[ "$repo" != /mnt/* ]]  || { log_error "Repo must not be under /mnt/c: $repo"; return 1; }
  [[ -d "$repo/.git" ]]    || { log_error "Not a git repo: $repo"; return 1; }
  log_ok "Repo validated: $repo"
}

validate_branch() {
  local repo="$1" expected="$2"
  local actual
  actual=$(git -C "$repo" rev-parse --abbrev-ref HEAD 2>/dev/null)
  [[ "$actual" == "$expected" ]] || {
    log_error "Branch mismatch: expected '$expected', got '$actual'"
    return 1
  }
  log_ok "Branch: $actual"
}

validate_clean_except() {
  # Ensure only allowed_files are modified; nothing else dirty
  local repo="$1"; shift
  local allowed=("$@")
  local dirty
  dirty=$(git -C "$repo" status --porcelain | awk '{print $2}')
  local unexpected=()
  while IFS= read -r f; do
    [[ -z "$f" ]] && continue
    local ok=0
    for a in "${allowed[@]}"; do [[ "$f" == "$a" ]] && ok=1 && break; done
    [[ $ok -eq 0 ]] && unexpected+=("$f")
  done <<< "$dirty"
  if [[ ${#unexpected[@]} -gt 0 ]]; then
    log_error "Unexpected dirty files: ${unexpected[*]}"
    return 1
  fi
  log_ok "Working tree clean (allowed files only)"
}

no_active_processes() {
  local repo="$1"
  if pgrep -f "codex.*${repo}" &>/dev/null; then
    log_error "Active Codex process found for $repo"; return 1
  fi
  if pgrep -f "cargo.*${repo}" &>/dev/null; then
    log_warn "Active cargo process found for $repo — may cause lock contention"
  fi
}

# ── gates ─────────────────────────────────────────────────────────────────────
run_gates() {
  local repo="$1"; shift
  local gates=("$@")
  local failed=()
  . "$HOME/.cargo/env" 2>/dev/null || true
  export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
  for gate in "${gates[@]}"; do
    log_info "Gate: $gate"
    if (cd "$repo" && eval "$gate") 2>&1; then
      log_ok "PASS: $gate"
    else
      log_error "FAIL: $gate"
      failed+=("$gate")
    fi
  done
  if [[ ${#failed[@]} -gt 0 ]]; then
    log_error "Gates failed: ${failed[*]}"
    return 1
  fi
  log_ok "All gates passed"
}

# ── worktree helpers ──────────────────────────────────────────────────────────
worktree_create() {
  local base_repo="$1" worktree_path="$2" branch="$3" base_ref="$4"
  if [[ -d "$worktree_path" ]]; then
    log_warn "Worktree already exists: $worktree_path"
    return 0
  fi
  git -C "$base_repo" worktree add -b "$branch" "$worktree_path" "$base_ref"
  log_ok "Worktree created: $worktree_path (branch: $branch)"
}

worktree_remove() {
  local base_repo="$1" worktree_path="$2"
  git -C "$base_repo" worktree remove --force "$worktree_path" 2>/dev/null || true
  log_info "Worktree removed: $worktree_path"
}