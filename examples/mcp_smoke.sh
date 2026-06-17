#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/release/nexus-mcp"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/nexus-mcp-smoke.XXXXXX")"

cleanup() {
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

need_cmd cargo
need_cmd wat2wasm
need_cmd python3

if [[ ! -x "$BIN" || "$ROOT/src/bin/nexus_mcp.rs" -nt "$BIN" || "$ROOT/Cargo.toml" -nt "$BIN" ]]; then
  echo "[mcp-smoke] building nexus-mcp"
  cargo build --release --bin nexus-mcp
fi

cat >"$WORKDIR/test_payload.wat" <<'WAT'
(module
  (memory (export "memory") 1)
  (data (i32.const 0) "BASE-MCP-SMOKE")
  (global $marker (export "marker") (mut i32) (i32.const 7))
  (func (export "_start")
    i32.const 32
    i32.const 82
    i32.store8))
WAT

cat >"$WORKDIR/mutated_payload.wat" <<'WAT'
(module
  (memory (export "memory") 1)
  (data (i32.const 0) "MUTATED-MCP-SMOKE")
  (global $marker (export "marker") (mut i32) (i32.const 11))
  (func (export "_start")
    i32.const 32
    i32.const 77
    i32.store8))
WAT

wat2wasm "$WORKDIR/test_payload.wat" -o "$WORKDIR/test_payload.wasm"
wat2wasm "$WORKDIR/mutated_payload.wat" -o "$WORKDIR/mutated_payload.wasm"

NEXUS_MCP_BIN="$BIN" \
NEXUS_MCP_SMOKE_DIR="$WORKDIR" \
python3 <<'PY'
import hashlib
import json
import os
import subprocess

WASM_PAGE_SIZE = 65_536


def expected_sha(prefix: bytes) -> str:
    memory = bytearray(WASM_PAGE_SIZE)
    memory[: len(prefix)] = prefix
    return hashlib.sha256(memory).hexdigest()


def fail(message: str) -> None:
    raise RuntimeError(message)


bin_path = os.environ["NEXUS_MCP_BIN"]
workdir = os.environ["NEXUS_MCP_SMOKE_DIR"]
base_path = os.path.join(workdir, "test_payload.wasm")
mutated_path = os.path.join(workdir, "mutated_payload.wasm")
base_sha = expected_sha(b"BASE-MCP-SMOKE")
mutated_sha = expected_sha(b"MUTATED-MCP-SMOKE")

env = os.environ.copy()
env["NEXUS_MCP_MODULE_DIR"] = workdir
env.pop("NEXUS_MCP_CAPABILITY_ALLOWLIST", None)

proc = subprocess.Popen(
    [bin_path],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
    text=True,
    cwd=workdir,
    env=env,
)


def send(message: dict) -> None:
    assert proc.stdin is not None
    proc.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
    proc.stdin.flush()


def recv() -> dict:
    assert proc.stdout is not None
    line = proc.stdout.readline()
    if not line:
        code = proc.poll()
        fail(f"nexus-mcp closed stdout before response, exit={code}")
    return json.loads(line)


def request(request_id: int, method: str, params: dict) -> dict:
    send(
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        }
    )
    response = recv()
    if response.get("id") != request_id:
        fail(f"unexpected response id for {method}: {response}")
    if "error" in response:
        fail(f"{method} failed: {response['error']}")
    return response


def tool_call(request_id: int, name: str, arguments: dict) -> dict:
    response = request(
        request_id,
        "tools/call",
        {"name": name, "arguments": arguments},
    )
    try:
        text = response["result"]["content"][0]["text"]
    except (KeyError, IndexError, TypeError) as exc:
        fail(f"tool response did not contain text content: {response!r} ({exc})")
    payload = json.loads(text)
    if payload.get("error") is not None:
        fail(f"{name} returned error: {payload['error']}")
    return payload


try:
    init = request(
        1,
        "initialize",
        {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "nexus-mcp-smoke", "version": "0.1.0"},
        },
    )
    if not isinstance(init.get("result"), dict):
        fail(f"initialize returned unexpected payload: {init}")

    send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    tools_response = request(2, "tools/list", {})
    tools = tools_response["result"]["tools"]
    tool_names = {tool["name"] for tool in tools}
    required = {
        "nexus_execute",
        "nexus_execute_wasi",
        "nexus_snapshot_create",
        "nexus_snapshot_rollback",
        "nexus_issue_token",
        "nexus_fork_and_race",
    }
    missing = sorted(required - tool_names)
    if missing:
        fail(f"tools/list missing expected tools: {missing}")

    base_exec = tool_call(
        3,
        "nexus_execute",
        {"wasm_path": base_path, "input": {"phase": "base"}},
    )
    if not base_exec.get("success"):
        fail(f"base execute failed: {base_exec}")

    created = tool_call(
        4,
        "nexus_snapshot_create",
        {"label": "mcp-smoke-base", "source": "latest_runtime"},
    )
    base_snapshot_id = created.get("snapshot_id")
    if not base_snapshot_id or base_snapshot_id != base_exec.get("snapshot_id"):
        fail(f"latest_runtime snapshot did not match base execute: {created}")

    mutated_exec = tool_call(
        5,
        "nexus_execute",
        {"wasm_path": mutated_path, "input": {"phase": "mutate"}},
    )
    if not mutated_exec.get("success"):
        fail(f"mutated execute failed: {mutated_exec}")
    if mutated_exec.get("snapshot_id") == base_snapshot_id:
        fail("mutated execute reused the base snapshot id")

    rollback = tool_call(
        6,
        "nexus_snapshot_rollback",
        {"snapshot_id": base_snapshot_id, "include_restored_state": True},
    )
    restored_memory = rollback.get("restored_state", {}).get("memory", {})
    restored_sha = restored_memory.get("sha256", "")
    if restored_sha != base_sha:
        fail(f"rollback restored unexpected sha256: {restored_sha}, want {base_sha}")
    if restored_sha == mutated_sha:
        fail("rollback restored mutated memory instead of base memory")

    recovered_exec = tool_call(
        7,
        "nexus_execute",
        {"wasm_path": base_path, "input": {"phase": "recover"}},
    )
    if not recovered_exec.get("success") or not recovered_exec.get("snapshot_id"):
        fail(f"recover execute failed: {recovered_exec}")

    print("[mcp-smoke] initialize/list_tools ok")
    print(f"[mcp-smoke] base_snapshot_id={base_snapshot_id}")
    print(f"[mcp-smoke] mutated_snapshot_id={mutated_exec.get('snapshot_id')}")
    print(f"[mcp-smoke] rollback_restored_sha256={restored_sha}")
    print(f"[mcp-smoke] recovered_snapshot_id={recovered_exec.get('snapshot_id')}")
    print("[mcp-smoke] rollback restored base memory after mutated execution")
finally:
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
PY
