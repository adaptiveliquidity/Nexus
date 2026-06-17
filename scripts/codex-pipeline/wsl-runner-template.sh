#!/usr/bin/env bash
# scripts/codex-pipeline/wsl-runner-template.sh
# Instantiated by wsl-dispatch-task.sh via sed. NOT executed directly.
# Placeholders: __CARGO_ENV__ __CODEX_RUNS_DIR__ __WORKTREE__ __PROMPT_FILE__ __LOG__
# Pre-create bubblewrap mount target before codex invocation
mkdir -p /tmp/.git 2>/dev/null || true
. __CARGO_ENV__ 2>/dev/null || true
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
codex e \
  -m gpt-5.5 \
  -c model_reasoning_effort="xhigh" \
  -c model_reasoning_summary="auto" \
  -c approval_policy="never" \
  -s workspace-write \
  --add-dir __CODEX_RUNS_DIR__ \
  --add-dir /tmp \
  --skip-git-repo-check \
  -C __WORKTREE__ \
  - < __PROMPT_FILE__ >> __LOG__ 2>&1
echo "=== CODEX_DONE exit=$? $(date -u) ===" >> __LOG__
git -C __WORKTREE__ status --short >> __LOG__ 2>&1
