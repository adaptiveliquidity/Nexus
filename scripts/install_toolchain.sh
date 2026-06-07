#!/usr/bin/env bash
# Userspace install of the Nexus validation toolchain (no sudo required).
# Targets Ubuntu 24.04 x86_64 (WSL2). Idempotent: skips already-installed bits.
set -euo pipefail

TOOLS_DIR="$HOME/.local/bin"
DOWNLOADS="$HOME/.cache/nexus-bench-installer"
VENV_DIR="$HOME/.venvs/nexus-bench"

mkdir -p "$TOOLS_DIR" "$DOWNLOADS"
export PATH="$HOME/.cargo/bin:$TOOLS_DIR:$PATH"

log() { printf '\n[install] %s\n' "$*"; }

# 1. rustup / cargo
if ! command -v cargo >/dev/null 2>&1; then
    log "Installing rustup (default profile, stable)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile default --no-modify-path
fi
. "$HOME/.cargo/env"
rustc --version
cargo --version

# 2. hyperfine (prebuilt binary)
if ! command -v hyperfine >/dev/null 2>&1; then
    log "Installing hyperfine binary"
    HF_VER="1.18.0"
    HF_TGZ="hyperfine-v${HF_VER}-x86_64-unknown-linux-musl.tar.gz"
    curl -fsSL -o "$DOWNLOADS/$HF_TGZ" \
        "https://github.com/sharkdp/hyperfine/releases/download/v${HF_VER}/${HF_TGZ}"
    tar -xzf "$DOWNLOADS/$HF_TGZ" -C "$DOWNLOADS"
    install -m 0755 "$DOWNLOADS/hyperfine-v${HF_VER}-x86_64-unknown-linux-musl/hyperfine" "$TOOLS_DIR/hyperfine"
fi
hyperfine --version

# 3. wabt (gives wat2wasm)
if ! command -v wat2wasm >/dev/null 2>&1; then
    log "Installing wabt (wat2wasm)"
    WABT_VER="1.0.36"
    WABT_TGZ="wabt-${WABT_VER}-ubuntu-20.04.tar.gz"
    curl -fsSL -o "$DOWNLOADS/$WABT_TGZ" \
        "https://github.com/WebAssembly/wabt/releases/download/${WABT_VER}/${WABT_TGZ}"
    tar -xzf "$DOWNLOADS/$WABT_TGZ" -C "$DOWNLOADS"
    install -m 0755 "$DOWNLOADS/wabt-${WABT_VER}/bin/wat2wasm" "$TOOLS_DIR/wat2wasm"
    install -m 0755 "$DOWNLOADS/wabt-${WABT_VER}/bin/wasm2wat" "$TOOLS_DIR/wasm2wat" || true
fi
wat2wasm --version

# 4. wasmtime (official installer)
if ! command -v wasmtime >/dev/null 2>&1; then
    log "Installing wasmtime"
    curl -fsSL https://wasmtime.dev/install.sh | bash
fi
# add to PATH for this session
export PATH="$HOME/.wasmtime/bin:$PATH"
wasmtime --version

# 5. jq (binary)
if ! command -v jq >/dev/null 2>&1; then
    log "Installing jq binary"
    JQ_VER="1.7.1"
    curl -fsSL -o "$TOOLS_DIR/jq" \
        "https://github.com/jqlang/jq/releases/download/jq-${JQ_VER}/jq-linux-amd64"
    chmod +x "$TOOLS_DIR/jq"
fi
jq --version

# 6. uv (Astral) — handles venvs without needing python3-venv apt pkg
if ! command -v uv >/dev/null 2>&1; then
    log "Installing uv (Astral python package manager)"
    curl -fsSL https://astral.sh/uv/install.sh | sh
fi
export PATH="$HOME/.local/bin:$PATH"
uv --version

# 7. Python venv via uv (system python is fine, no python3-venv apt pkg needed)
if [ ! -x "$VENV_DIR/bin/python" ]; then
    log "Creating python venv at $VENV_DIR (via uv)"
    uv venv --python python3 "$VENV_DIR"
fi
uv pip install --python "$VENV_DIR/bin/python" --quiet pandas numpy scipy matplotlib seaborn tabulate
"$VENV_DIR/bin/python" -c "import pandas,numpy,scipy,matplotlib,seaborn; print('python deps OK:', pandas.__version__, numpy.__version__)"

log "All toolchain components present."
echo
echo "Add to PATH in subsequent shells (already exported in this script):"
echo "  export PATH=\"\$HOME/.cargo/bin:\$HOME/.wasmtime/bin:\$HOME/.local/bin:\$PATH\""
echo "Python venv:"
echo "  source $VENV_DIR/bin/activate"
