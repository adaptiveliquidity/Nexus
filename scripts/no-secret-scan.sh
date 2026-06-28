#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -lt 1 ]; then
  echo "Usage: $0 <file-or-grep-target>..." >&2
  exit 2
fi

status=0
files=("$@")

check_matches() {
  local label="$1"
  local pattern="$2"
  local output=""
  local file

  for file in "${files[@]}"; do
    if [ ! -f "$file" ]; then
      continue
    fi
    output="$(rg -n -U --no-heading --hidden --pcre2 -g '*' -e "$pattern" "$file" || true)"
    if [ -n "$output" ]; then
      echo "::error::${label} pattern match in ${file}"
      echo "$output"
      status=1
    fi
  done
}

check_matches \
  "api-token-like-string" \
  '(?i)(api[_-]?key|secret[_-]?key|bearer[_-]?token|access[_-]?token)[[:space:]]*[:=][[:space:]]*[A-Za-z0-9._~+/-]{20,}'

check_matches \
  "raw-bearer-token" \
  '(?i)bearer[[:space:]]+[A-Za-z0-9._~+/-]{20,}'

check_matches \
  "absolute-host-path" \
  '(^|[[:space:]"'"'"'])/(home|Users|tmp|var|root|mnt)/[A-Za-z0-9._-](/[^[:space:]"'"'"':]+)+'

check_matches \
  "windows-absolute-path" \
  '[A-Za-z]:\\[A-Za-z0-9._-]+(\\[A-Za-z0-9._-]+)*'

check_matches \
  "raw-memory-text" \
  '(?i)raw[ -_]?(memory|text)|memory[ -_]?text'

if [ "$status" -ne 0 ]; then
  echo "no-secret-scan: blocked by policy"
  exit 1
fi

echo "no-secret-scan: clean"
