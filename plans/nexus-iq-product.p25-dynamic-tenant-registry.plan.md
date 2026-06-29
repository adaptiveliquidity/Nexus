# Plan: P2.5 — Dynamic tenant registry (Postgres) for mcp-http

**Source PRD**: plans/nexus-iq-product.prd.md
**Selected Milestone**: P2.5 — Dynamic tenant registry
**Complexity**: Medium

## Summary
Switch the MCP HTTP tenant registry from a single static file source to a runtime
`TenantRegistry` abstraction with two implementations:

- file-backed source from `NEXUS_MCP_HTTP_TENANTS` (default), preserving all
  existing behavior and fallback via `NEXUS_MCP_HTTP_TOKEN`.
- PostgreSQL-backed source behind new `tenant-registry-postgres` feature with
  short-TTL snapshot refresh.

## Ground-truth findings (verified this session)
- `src/bin/nexus_mcp.rs` already hosts all tenant auth logic used by `NEXUS_MCP_HTTP`.
- Request middleware currently hashes incoming bearer tokens, enforces per-tenant
  fixed-window rate limiting, and logs tenant/method/path/status-class.
- Static-file auth behavior for default builds and existing `NEXUS_MCP_HTTP_TOKEN`
  fallback is required to remain unchanged.

## Files to Change
| File | Action | Why |
|---|---|---|
| `Cargo.toml` | UPDATE | Add `tenant-registry-postgres` feature and optional Postgres/client deps (`sqlx`, `arc-swap`). |
| `src/bin/nexus_mcp.rs` | UPDATE | Introduce `TenantRegistry` abstraction, file + postgres implementations, startup wiring for selected source, and unit/integration tests for refresh and stale semantics. |
| `docs/mcp-setup.md` | UPDATE | Add P2.5 environment and security documentation, DB contract, and fail-closed behavior.
| `plans/nexus-iq-product.p25-dynamic-tenant-registry.plan.md` | CREATE | Record execution plan and acceptance criteria for this P2.5 slice. |
| `.codex-p25-result.md` | CREATE | Record validation commands and outcomes.

## Tasks
### Task 1: Registry abstraction and source selection
- Add `TenantSnapshot` keyed by `api_key_sha256`.
- Implement `TenantRegistry` trait with `current_snapshot()`.
- Add source selector env `NEXUS_MCP_TENANT_SOURCE` with `file` (default) and
  `postgres`.

### Task 2: PostgreSQL implementation
- Add optional `tenant-registry-postgres` feature + deps.
- Implement SQL refresh loop that reads from a configurable relation,
  prefers `active_api_keys` when relation is `api_keys`, and uses only runtime
  queries.
- Keep in-memory snapshot in `ArcSwap`, refresh asynchronously on interval.
- Track snapshot age and enforce stale bound.

### Task 3: Fail-closed semantics
- No request-time DB fallback from middleware.
- If no snapshot match, return `401`.
- On refresh failure, keep last snapshot until stale threshold then empty.
- Initial load failure results in empty snapshot and no panic.

### Task 4: Tests
- Add registry-level in-memory tests for active/unknown key behavior, revocation,
  stale refresh behavior, and empty snapshot deny.
- Add optional PostgreSQL integration test gated by feature/env, skipped when no
  DB URL is supplied.

### Task 5: Docs and execution evidence
- Add P2.5 docs section with environment variables, SQL contract, and GRANT
  guidance.
- Create `.codex-p25-result.md` with validation command results.

## Validation
```bash
export CARGO_HOME=/tmp/cargo-home CARGO_TARGET_DIR=/tmp/cargo-target
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features "mcp-http aeon-memory tenant-registry-postgres" -- -D warnings
cargo test
cargo test --features "mcp-http aeon-memory tenant-registry-postgres"
cargo fmt --all
```

## Acceptance
- [ ] `TenantRegistry` abstraction is used by auth middleware.
- [ ] File source remains default and behavior matches existing static setup.
- [ ] Postgres source refreshes active key snapshot with configured TTL/stale bounds.
- [ ] Refresh failures are fail-closed and bounded by stale window.
- [ ] Non-loopback startup guard remains in place.
- [ ] Required tests and clippy run clean with default and feature builds.
