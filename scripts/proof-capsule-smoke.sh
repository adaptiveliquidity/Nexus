#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_FILE="${ROOT}/artifacts/proof-capsule.json"
WASM_PATH="${ROOT}/test_payload.wasm"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --output)
      if [ "$#" -lt 2 ]; then
        echo "--output requires a file path" >&2
        exit 2
      fi
      OUTPUT_FILE="$2"
      shift 2
      ;;
    --wasm)
      if [ "$#" -lt 2 ]; then
        echo "--wasm requires a file path" >&2
        exit 2
      fi
      WASM_PATH="$2"
      shift 2
      ;;
    *)
      echo "Usage: $0 [--output path] [--wasm path]" >&2
      exit 2
      ;;
  esac
done

mkdir -p "$(dirname "$OUTPUT_FILE")"
mkdir -p "$ROOT/artifacts"

if [ ! -x "$ROOT/target/debug/nexus-mcp" ] || [ "$ROOT/src/bin/nexus_mcp.rs" -nt "$ROOT/target/debug/nexus-mcp" ] || [ "$ROOT/Cargo.toml" -nt "$ROOT/target/debug/nexus-mcp" ]; then
  echo "[proof-capsule-smoke] building nexus-mcp (features: aeon-memory)"
  cargo build --locked --features aeon-memory --bin nexus-mcp
fi

if [ ! -f "$WASM_PATH" ]; then
  echo "missing wasm module: $WASM_PATH" >&2
  exit 1
fi

echo "[proof-capsule-smoke] generating proof capsule from $WASM_PATH"

python3 - "$ROOT/target/debug/nexus-mcp" "$WASM_PATH" "$OUTPUT_FILE" <<'PY'
import json
import os
import subprocess
import sys
from typing import Any, Dict


def fail(message: str) -> None:
    print(f"[proof-capsule-smoke] {message}", file=sys.stderr)
    raise SystemExit(1)


def request(proc: subprocess.Popen, message: Dict[str, Any]) -> Dict[str, Any]:
    assert proc.stdin is not None
    proc.stdin.write(json.dumps(message) + "\n")
    proc.stdin.flush()


def response(proc: subprocess.Popen) -> Dict[str, Any]:
    assert proc.stdout is not None
    line = proc.stdout.readline()
    if not line:
        fail("nexus-mcp closed output before response")
    return json.loads(line)


def tool_call(proc: subprocess.Popen, request_id: int, name: str, arguments: Dict[str, Any]) -> Dict[str, Any]:
    request(
        proc,
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments,
            },
        },
    )
    payload = response(proc)
    if "error" in payload:
        fail(f"tools/call failed: {payload['error']}")
    result = payload.get("result", {})
    content = result.get("content", [])
    if not content:
        fail(f"{name} response missing content payload: {payload!r}")
    text = content[0].get("text", "")
    data = json.loads(text)
    if data.get("error") is not None:
        fail(f"{name} returned error: {data['error']}")
    return data


def check_tool_present(tools: Any) -> None:
    names = {tool.get("name") for tool in tools}
    if "nexus_execute_proof" not in names:
        fail(f"nexus_execute_proof missing from tool list: {tools!r}")


bin_path = sys.argv[1]
wasm_path = sys.argv[2]
output_path = sys.argv[3]
env = os.environ.copy()
env["NEXUS_MCP_RETURN_FULL_PROOF"] = "1"
env["NEXUS_MCP_MODULE_DIR"] = os.path.dirname(wasm_path)

proc = subprocess.Popen(
    [bin_path],
    cwd=os.path.dirname(wasm_path),
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
    env=env,
)

try:
    request(
        proc,
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "proof-capsule-smoke", "version": "1"},
            },
        },
    )
    init = response(proc)
    if "error" in init:
        fail(f"initialize failed: {init['error']}")

    # Standard MCP notification, required by some clients.
    request(
        proc,
        {
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {},
        },
    )

    request(
        proc,
        {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {},
        },
    )
    tools_response = response(proc)
    check_tool_present(tools_response.get("result", {}).get("tools", []))

    payload = tool_call(
        proc,
        3,
        "nexus_execute_proof",
        {
            "wasm_path": wasm_path,
            "input": {"message": "proof-capsule-smoke"},
        },
    )

    # A proof capsule is a signed record of what happened, including failures.
    # The smoke test verifies capsule generation and signature integrity, not
    # execution success — so we do not gate on output.success here.
    proof_capsule = payload.get("proof_capsule")
    if proof_capsule is None:
        fail(f"nexus_execute_proof did not return proof_capsule: {payload!r}")

    with open(output_path, "w", encoding="utf-8") as handle:
        json.dump(proof_capsule, handle, indent=2)
    print(f"[proof-capsule-smoke] wrote {output_path}")

    summary = payload.get("proof_reference", {}).get("inline_summary", {})
    if proof_capsule.get("signature") is None and not summary.get("has_signature", False):
        fail("generated proof capsule did not include signature metadata")

    print("[proof-capsule-smoke] OK")
finally:
    if proc.stdin is not None:
        try:
            proc.stdin.close()
        except OSError:
            pass
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=3)
PY

