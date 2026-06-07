# shellcheck shell=bash
# Common environment for Nexus benchmark scripts. Source this from bash.
# Keeps PATH, CARGO_TARGET_DIR, and venv activation consistent across phases.

export PATH="$HOME/.cargo/bin:$HOME/.wasmtime/bin:$HOME/.local/bin:$PATH"

# Build artifacts on the Linux FS to avoid /mnt/c slowness during cargo builds.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/nexus-target}"
mkdir -p "$CARGO_TARGET_DIR"

# Project root resolution: the directory containing this scripts/ folder.
NEXUS_BENCH_ENV_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
export NEXUS_ROOT="$(cd "$NEXUS_BENCH_ENV_DIR/.." && pwd)"
export ARTIFACTS_DIR="$NEXUS_ROOT/artifacts"
export RAW_DIR="$ARTIFACTS_DIR/raw"
export PLOTS_DIR="$ARTIFACTS_DIR/plots"
mkdir -p "$RAW_DIR" "$PLOTS_DIR"

# Activate python venv if present (for the analyzer).
# `uv venv` does not always create a bin/activate script; prepend the venv
# bin/ to PATH so `python3` / `pip` resolve to the venv's executables.
VENV_DIR="$HOME/.venvs/nexus-bench"
if [ -x "$VENV_DIR/bin/python" ]; then
    export VIRTUAL_ENV="$VENV_DIR"
    export PATH="$VENV_DIR/bin:$PATH"
    if [ -f "$VENV_DIR/bin/activate" ]; then
        # shellcheck disable=SC1091
        . "$VENV_DIR/bin/activate"
    fi
fi
