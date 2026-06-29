# Plan: P1 — Remote HTTP transport (read-only)

**Source PRD**: plans/nexus-iq-product.prd.md
**Selected Milestone**: P1 — Remote MCP transport
**Complexity**: Medium

## Summary
Add a new `mcp-http` feature that exposes `nexus-mcp` over RMCP streamable HTTP on a loopback-only default bind address, while forcing a read-only tool allowlist for that transport path. Keep default `stdio` mode unchanged and unchanged by default.

## Ground-truth findings (verified this session)
- `src/bin/nexus_mcp.rs` currently always boots `stdio` via `server.serve(stdio()).await`.
- Tool exposure already checks through `ensure_tool_allowed`, and every MCP tool path calls that helper.
- Tool names are embedded in each MCP tool method (e.g., `nexus_execute_wasi`, `nexus_snapshot_create`, `nexus_instinct_export`, `nexus_aeon_execute_timeline`, etc.).
- `rmcp` streamable HTTP server API (`StreamableHttpService`, `StreamableHttpServerConfig`) is available under `transport-streamable-http-server` and can be mounted with `axum`.

## Files to Change
| File | Action | Why |
|---|---|---|
| `Cargo.toml` | UPDATE | add `mcp-http` feature and optional HTTP stack dependencies |
| `src/bin/nexus_mcp.rs` | UPDATE | transport mode switch, HTTP auth mode, forced read-only allowlist, HTTP serve path, unit test |
| `docs/mcp-setup.md` | UPDATE | document P1 remote HTTP transport settings and allowlist |
| `.codex-p1-http-result.md` | CREATE | record required validation outputs |

## Tasks
### Task 1: Gate feature + deps
- Add `mcp-http` feature and optional dependencies (`axum`, rmcp HTTP transport feature).
- Keep `default = []` unchanged.
- Validate that default build remains stdio-only.

### Task 2: Read-only allowlist enforcement
- Add `forced_tool_allowlist: Option<HashSet<String>>` to `NexusMcpServer`.
- Add pre-check in `ensure_tool_allowed` before profile allowlist check.
- Define constant read-only set for HTTP and force it in HTTP mode.
- Add unit test with `read-only set denies nexus_execute_wasi` and allows `nexus_get_stats`.

### Task 3: HTTP transport mode in main
- Add `NEXUS_MCP_TRANSPORT` selector (`stdio` default, `http` mode).
- In HTTP mode build `NexusMcpServer` with forced allowlist and serve `StreamableHttpService` via `axum`.
- Bind on `NEXUS_MCP_HTTP_ADDR` default `127.0.0.1:8765`.
- Reject `http` at runtime when feature is missing.
- Implement unauthenticated warning when token unset; enforce bearer auth when token present (401 on mismatch).

### Task 4: Docs + validation
- Add docs section describing build command and env vars.
- Add explicit execution/mutation exclusion and P2 auth/tenancy note.

## Validation
```bash
cargo build --features "mcp-http aeon-memory"
cargo test --features "mcp-http aeon-memory"
cargo clippy --all-targets --features "mcp-http aeon-memory" -- -D warnings
cargo fmt --all
```

## Acceptance
- [ ] `NEXUS_MCP_TRANSPORT` defaults to `stdio` and preserves old stdio behavior.
- [ ] `NEXUS_MCP_TRANSPORT=http` starts only with `mcp-http` feature.
- [ ] HTTP mode exposes only forced read-only tools.
- [ ] `nexus_execute_wasi` is denied by forced allowlist test.
- [ ] `cargo test` and `cargo clippy -D warnings` succeed with required flags.
