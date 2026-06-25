# Nexus MCP Setup

This guide connects a local MCP client to the `nexus-mcp` stdio server and gives
you a working tool surface in about 10 minutes.

## Build

From the repository root:

```bash
cargo build --release --bin nexus-mcp
```

If your workspace has `nexus-mcp` split into its own package, use the
package-target form:

```bash
cargo build --release -p nexus-mcp
```

The binary is written to:

```text
target/release/nexus-mcp
```

`nexus-mcp` is the binary target in the current `nexus` Cargo package.

## MCP Client Config

Use this as `mcp.json` for Claude Desktop or another MCP client that accepts the
standard `mcpServers` object. Replace `/home/ahpsi/nexus` with your checkout
path if needed.

```json
{
  "mcpServers": {
    "nexus": {
      "command": "/home/ahpsi/nexus/target/release/nexus-mcp",
      "args": [],
      "env": {
        "NEXUS_MCP_MODULE_DIR": "/home/ahpsi/nexus"
      }
    }
  }
}
```

Restart the client after saving the config. The server speaks MCP over stdio; it
does not need a port.

For a generic MCP client that expects a single server object instead of
`mcpServers`, use the same command/env fields:

```json
{
  "command": "/home/ahpsi/nexus/target/release/nexus-mcp",
  "args": [],
  "env": {
    "NEXUS_MCP_MODULE_DIR": "/home/ahpsi/nexus"
  }
}
```

## Tool Surface

`nexus_execute`
: Execute a WASM tool in the Nexus sandbox. Parameters: `wasm_path`, optional
`entry`, optional JSON `input`. Returns success/error, result bytes as text or
base64, execution time, fuel consumed, rollback flag, and a runtime
`snapshot_id` when memory was captured.

`nexus_execute_wasi`
: Execute a WASM tool with WASI filesystem/env/stdio support. Parameters:
`wasm_path`, optional `entry`, optional JSON `input`, optional `capabilities`,
and optional `parent_token_id`. Capabilities use objects like
`{"type":"read_file","path":"/tmp/data"}`.

`nexus_execute_proof`
: Execute a WASM module through the proof path and return `proof_reference`
  (digest + scorecard) with output by default. Set
  `NEXUS_MCP_RETURN_FULL_PROOF=1` only for debug/development when the full
  capsule body is required.

`nexus_snapshot_create`
: Create an MCP snapshot handle. Omit `source` for an empty/stateless baseline,
or pass `{"source":"latest_runtime"}` after `nexus_execute` to return the real
runtime snapshot captured from sandbox memory/state.

`nexus_snapshot_rollback`
: Roll back to a snapshot id. Parameters: `snapshot_id` and optional
`include_restored_state`. When requested, the response includes restored memory
length, SHA-256, base64 preview, and execution-state counts.

`nexus_issue_token`
: Issue an operator-allowlisted capability token for `nexus_execute_wasi`.
Parameters: `capability`, optional `path`, optional `validity_secs`. The server
rejects `all` and clamps validity to one hour.

`nexus_fork_and_race`
: Race multiple WASM branches. Parameters: `wasm_path`, optional
`base_snapshot_id` or `source:"latest_runtime"`, `branches`, and optional
`strategy` (`first_success` or `wait_all`).

## Capability Allowlist

WASI capability requests need either a parent token or an operator allowlist.
Configure `NEXUS_MCP_CAPABILITY_ALLOWLIST` as a JSON array using the same shape
as `nexus_execute_wasi` capabilities:

For local debugging, you can enable full proof capsule payloads:

```bash
export NEXUS_MCP_RETURN_FULL_PROOF=1
```

```bash
export NEXUS_MCP_CAPABILITY_ALLOWLIST='[{"type":"read_file","path":"/tmp/nexus-demo"}]'
```

Minimal MCP config with both module-directory and read-file allowlists:

```json
{
  "mcpServers": {
    "nexus": {
      "command": "/home/ahpsi/nexus/target/release/nexus-mcp",
      "args": [],
      "env": {
        "NEXUS_MCP_MODULE_DIR": "/tmp/nexus-demo",
        "NEXUS_MCP_CAPABILITY_ALLOWLIST": "[{\"type\":\"read_file\",\"path\":\"/tmp/nexus-demo\"}]"
      }
    }
  }
}
```

Supported capability types are `read_file`, `write_file`, `list_dir`,
`http_get`, `http_post`, `execute`, and `mount_tmpfs`.

## Smoke Demo

Run the end-to-end stdio demo from the repository root:

```bash
bash examples/mcp_smoke.sh
```

The script builds `nexus-mcp` if needed, generates WASM payloads, performs the
MCP initialize handshake, lists tools, executes a payload, creates a
`latest_runtime` snapshot, executes a mutated payload, rolls back to the first
snapshot, executes again, and prints a rollback checksum summary.
