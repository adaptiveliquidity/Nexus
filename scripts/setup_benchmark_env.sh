#!/usr/bin/env bash
# Phase 0: Environment Auditor & Provisioner
# Captures REAL hardware/toolchain specs to artifacts/specs.json (no hardcoded values).
# Detects WSL2 and degrades gracefully (no /sys cpufreq, perf often unavailable).
set -euo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$THIS_DIR/bench_env.sh"

cd "$NEXUS_ROOT"

SPECS_JSON="$ARTIFACTS_DIR/specs.json"
SPECS_MD="$ARTIFACTS_DIR/specs.md"

q() { # quote-as-json-string a single line of text
    python3 -c 'import json,sys;print(json.dumps(sys.stdin.read().strip()))'
}

ver() { # return single-line --version output or "not installed"
    local cmd="$1"
    if command -v "$cmd" >/dev/null 2>&1; then
        "$cmd" --version 2>/dev/null | head -1
    else
        echo "not installed"
    fi
}

is_wsl="false"
grep -qi microsoft /proc/version 2>/dev/null && is_wsl="true"

# CPU governor: best-effort; WSL2 typically has no cpufreq sysfs
governor="unavailable"
if [ -r /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor ]; then
    governor="$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor)"
fi

cpu_model="$(lscpu | awk -F: '/Model name/{print $2; exit}' | sed 's/^ *//')"
cpu_cores="$(nproc)"
cpu_mhz_max="$(lscpu | awk -F: '/CPU max MHz/{print $2; exit}' | sed 's/^ *//')"
[ -z "$cpu_mhz_max" ] && cpu_mhz_max="unknown"

ram_gb="$(awk '/MemTotal/ {printf "%.1f", $2/1024/1024}' /proc/meminfo)"
kernel="$(uname -r)"
uname_full="$(uname -a)"

# Quick disk I/O via dd (write 1 GiB then drop caches isn't possible without sudo;
# we just measure raw write throughput). Cleanup after.
echo "[phase0] running 1 GiB dd write test (to $HOME/.cache/nexus_dd_test)..."
mkdir -p "$HOME/.cache"
dd_test_file="$HOME/.cache/nexus_dd_test"
dd_out="$(dd if=/dev/zero of="$dd_test_file" bs=1M count=1024 conv=fdatasync 2>&1 | tail -1)"
rm -f "$dd_test_file"
disk_write="$dd_out"

# Git info
if git rev-parse HEAD >/dev/null 2>&1; then
    git_commit="$(git rev-parse HEAD)"
    if git diff --quiet 2>/dev/null && git diff --quiet --cached 2>/dev/null; then
        git_dirty="false"
    else
        git_dirty="true"
    fi
else
    git_commit="not_a_git_repo"
    git_dirty="unknown"
fi

timestamp_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Tool versions
rustc_v="$(ver rustc)"
cargo_v="$(ver cargo)"
hyperfine_v="$(ver hyperfine)"
wasmtime_v="$(ver wasmtime)"
docker_v="$(ver docker)"
wat2wasm_v="$(ver wat2wasm)"
jq_v="$(ver jq)"
python_v="$(ver python3)"
perf_v="$(ver perf)"
valgrind_v="$(ver valgrind)"
cpupower_v="$(ver cpupower)"

# Docker server reachable?
docker_server="unreachable"
if command -v docker >/dev/null 2>&1; then
    docker_server="$(docker info --format '{{.ServerVersion}}' 2>/dev/null || echo unreachable)"
fi

# /dev/kvm present?
kvm_dev="absent"
[ -e /dev/kvm ] && kvm_dev="present"

cat > "$SPECS_JSON" <<EOF
{
  "timestamp_utc": "$timestamp_utc",
  "host": {
    "is_wsl2": $is_wsl,
    "uname": $(printf '%s' "$uname_full" | q),
    "kernel": $(printf '%s' "$kernel" | q),
    "cpu_model": $(printf '%s' "$cpu_model" | q),
    "cpu_cores": $cpu_cores,
    "cpu_max_mhz": $(printf '%s' "$cpu_mhz_max" | q),
    "cpu_governor": $(printf '%s' "$governor" | q),
    "ram_gb": $ram_gb,
    "disk_write_dd": $(printf '%s' "$disk_write" | q),
    "kvm_dev": $(printf '%s' "$kvm_dev" | q)
  },
  "toolchain": {
    "rustc": $(printf '%s' "$rustc_v" | q),
    "cargo": $(printf '%s' "$cargo_v" | q),
    "hyperfine": $(printf '%s' "$hyperfine_v" | q),
    "wasmtime": $(printf '%s' "$wasmtime_v" | q),
    "docker_client": $(printf '%s' "$docker_v" | q),
    "docker_server": $(printf '%s' "$docker_server" | q),
    "wat2wasm": $(printf '%s' "$wat2wasm_v" | q),
    "jq": $(printf '%s' "$jq_v" | q),
    "python3": $(printf '%s' "$python_v" | q),
    "perf": $(printf '%s' "$perf_v" | q),
    "valgrind": $(printf '%s' "$valgrind_v" | q),
    "cpupower": $(printf '%s' "$cpupower_v" | q)
  },
  "repo": {
    "git_commit": $(printf '%s' "$git_commit" | q),
    "git_dirty": $git_dirty
  },
  "deviations": [
    "WSL2: cpufreq sysfs typically unavailable; CPU governor cannot be locked to performance mode.",
    "WSL2: perf often unavailable or limited; perf-counters phase skipped if missing.",
    "WSL2: Firecracker not measured (no bare-metal KVM ownership in WSL2 environment).",
    "Cloudflare Workers not measured (requires hosted environment; out of scope)."
  ]
}
EOF

# Human-readable mirror for the report appendix
cat > "$SPECS_MD" <<EOF
# Environment Specs (captured $timestamp_utc)

- WSL2: $is_wsl
- Kernel: \`$kernel\`
- CPU: \`$cpu_model\` ($cpu_cores cores, max ${cpu_mhz_max} MHz)
- CPU governor: \`$governor\`
- RAM: ${ram_gb} GiB
- dd write (1 GiB, fdatasync): \`$disk_write\`
- /dev/kvm: $kvm_dev

## Toolchain
- rustc: \`$rustc_v\`
- cargo: \`$cargo_v\`
- hyperfine: \`$hyperfine_v\`
- wasmtime: \`$wasmtime_v\`
- docker (client/server): \`$docker_v\` / \`$docker_server\`
- wat2wasm: \`$wat2wasm_v\`
- jq: \`$jq_v\`
- python3: \`$python_v\`
- perf: \`$perf_v\`
- valgrind: \`$valgrind_v\`
- cpupower: \`$cpupower_v\`

## Repo
- git commit: \`$git_commit\`
- git dirty: $git_dirty

## Documented deviations
- WSL2: cpufreq sysfs typically unavailable; CPU governor cannot be locked to performance mode.
- WSL2: perf often unavailable or limited; perf-counters phase skipped if missing.
- WSL2: Firecracker not measured (no bare-metal KVM ownership in WSL2 environment).
- Cloudflare Workers not measured (requires hosted environment; out of scope).
EOF

echo
echo "[phase0] specs written:"
echo "  $SPECS_JSON"
echo "  $SPECS_MD"
echo
cat "$SPECS_JSON"
