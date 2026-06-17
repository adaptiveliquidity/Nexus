#!/usr/bin/env bash
# scripts/codex-pipeline/lib.sh -- shared functions for the Nexus WSL Codex pipeline
# Source this file; do not execute directly.
set -euo pipefail

CODEX_STATE_DIR="${CODEX_STATE_DIR:-/home/ahpsi/nexus/.codex/state}"
CODEX_RUNS_DIR="${CODEX_RUNS_DIR:-/home/ahpsi/codex-runs}"
CODEX_LOCK="${CODEX_RUNS_DIR}/.dispatch.lock"
APPROVED_REPO="/home/ahpsi/nexus"

# -- logging ------------------------------------------------------------------
log_info()  { echo "[INFO  $(date -u +%H:%M:%S)] $*"; }
log_warn()  { echo "[WARN  $(date -u +%H:%M:%S)] $*" >&2; }
log_error() { echo "[ERROR $(date -u +%H:%M:%S)] $*" >&2; }
log_ok()    { echo "[OK    $(date -u +%H:%M:%S)] $*"; }

# -- state file ---------------------------------------------------------------
state_file() { echo "${CODEX_STATE_DIR}/${1:-current-task}.json"; }

# Fix: use sys.argv to avoid Python code injection via key/file/val interpolation
state_get() {
  local key="$1" file="${2:-$(state_file)}"
  python3 - "$file" "$key" <<'PYEOF'
import json, sys
d = json.load(open(sys.argv[1]))
print(d.get(sys.argv[2], ''))
PYEOF
}

state_set() {
  local key="$1" val="$2" file="${3:-$(state_file)}"
  python3 - "$file" "$key" "$val" <<'PYEOF'
import json, sys
with open(sys.argv[1]) as f: d = json.load(f)
d[sys.argv[2]] = sys.argv[3]
with open(sys.argv[1], 'w') as f: json.dump(d, f, indent=2)
PYEOF
}

state_get_array() {
  local key="$1" file="${2:-$(state_file)}"
  python3 - "$file" "$key" <<'PYEOF'
import json, sys
d = json.load(open(sys.argv[1]))
for x in d.get(sys.argv[2], []): print(x)
PYEOF
}

# -- validation ---------------------------------------------------------------
require_tool() {
  for t in "$@"; do
    command -v "$t" &>/dev/null || { log_error "Required tool missing: $t"; return 1; }
  done
}

validate_refname() {
  local name="$1"
  [[ "$name" =~ ^[a-zA-Z0-9._/-]+$ ]] || {
    log_error "Invalid ref name (must match [a-zA-Z0-9._/-]+): $name"; return 1
  }
}

validate_repo() {
  local raw="${1:-$APPROVED_REPO}"
  local repo
  repo=$(realpath -e "$raw" 2>/dev/null) || {
    log_error "Cannot resolve repo path: $raw"; return 1
  }
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

validate_allowed_file() {
  local f="$1" worktree="$2"
  [[ "$f" != /* && "$f" != *../* && "$f" != */../* && "$f" != "../"* ]] || {
    log_error "Forbidden allowed_file path (absolute or traversal): $f"; return 1
  }
  local abs
  abs=$(realpath --no-symlinks "$worktree/$f" 2>/dev/null) || {
    log_error "Cannot resolve allowed_file: $f"; return 1
  }
  [[ "$abs" == "$worktree/"* ]] || {
    log_error "allowed_file escapes worktree: $f -> $abs"; return 1
  }
}

no_active_processes() {
  local repo="$1"
  local repo_esc
  repo_esc=$(printf '%s' "$repo" | sed 's/[.[\*^$()|+?{]/\\&/g')
  if pgrep -f "codex e .*${repo_esc}" &>/dev/null; then
    log_error "Active Codex process found for $repo"; return 1
  fi
  if pgrep -f "cargo.*${repo_esc}" &>/dev/null; then
    log_warn "Active cargo process found for $repo -- may cause lock contention"
  fi
}

# -- gates --------------------------------------------------------------------
run_gates() {
  local repo="$1"; shift
  local gates=("$@")
  local failed=()
  . "$HOME/.cargo/env" 2>/dev/null || true
  export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
  for gate in "${gates[@]}"; do
    local first_word="${gate%%[[:space:]]*}"
    if ! [[ "$first_word" =~ ^(cargo|rustfmt)$ ]]; then
      log_error "Forbidden gate command (only cargo/rustfmt allowed): $gate"
      failed+=("$gate")
      continue
    fi
    log_info "Gate: $gate"
    read -ra _gate_argv <<< "$gate"
    if (cd "$repo" && "${_gate_argv[@]}") 2>&1; then
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

# -- worktree helpers ---------------------------------------------------------
worktree_create() {
  local base_repo="$1" worktree_path="$2" branch="$3" base_ref="$4"
  validate_refname "$branch" || return 1
  if [[ -d "$worktree_path" ]]; then
    log_warn "Worktree already exists: $worktree_path"
    return 0
  fi
  git -C "$base_repo" worktree add -b "$branch" -- "$worktree_path" "$base_ref"
  log_ok "Worktree created: $worktree_path (branch: $branch)"
}

worktree_remove() {
  local base_repo="$1" worktree_path="$2"
  [[ "$worktree_path" != "$base_repo" ]] || {
    log_error "Refusing worktree_remove: path equals base repo ($base_repo)"; return 1
  }
  local canonical
  canonical=$(realpath -e "$worktree_path" 2>/dev/null) || {
    log_warn "Worktree path does not exist (already removed?): $worktree_path"; return 0
  }
  [[ "$canonical" == /home/ahpsi/* ]] || {
    log_error "Refusing worktree_remove: unexpected path: $canonical"; return 1
  }
  git -C "$base_repo" worktree remove --force "$worktree_path"
  log_info "Worktree removed: $worktree_path"
}
