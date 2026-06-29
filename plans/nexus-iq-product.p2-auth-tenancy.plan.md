# Plan: P2 — Auth + multi-tenancy for remote MCP HTTP

**Source PRD**: plans/nexus-iq-product.prd.md
**Selected Milestone**: P2 — Multi-tenant HTTP auth and controls
**Complexity**: Medium

## Summary
Add per-tenant authentication and request-rate limiting to the existing `mcp-http` transport on `nexus-mcp`, while preserving default stdio behavior. `NEXUS_MCP_HTTP_TENANTS` becomes the primary auth source (JSON file of tenant IDs, SHA-256 API-key hashes, optional RPM limit), with a backward-compatible fallback to `NEXUS_MCP_HTTP_TOKEN` as a single implicit tenant when tenants are not configured.

## Ground-truth findings (verified this session)
- `src/bin/nexus_mcp.rs` already serves `nexus-mcp` over RMCP streamable HTTP using `StreamableHttpService` under `NEXUS_MCP_TRANSPORT=http`.
- Existing auth in P1 is a single-token middleware that compared the raw header token directly.
- Read-only HTTP tool allowlist logic is already in place via `NEXUS_MCP_HTTP_READ_ONLY_TOOL_ALLOWLIST` and `NexusMcpServer::new_with_forced_tool_allowlist`.
- HTTP transport is feature-gated by `#[cfg(feature = "mcp-http")]`, so all changes in this task must remain under that gate.

## Files to Change
| File | Action | Why |
|---|---|---|
| `src/bin/nexus_mcp.rs` | UPDATE | Add tenant config loading/validation, tenant-aware auth middleware, fixed-window rate limiting, tenant context extension, and auth/HTTP startup validation and logging. |
| `docs/mcp-setup.md` | UPDATE | Document `NEXUS_MCP_HTTP_TENANTS`, hash workflow, rate limit behavior, audit logging, and backward compatibility details.
| `plans/nexus-iq-product.p2-auth-tenancy.plan.md` | CREATE | Capture decisions and acceptance criteria for this P2 slice.
| `.codex-p2-auth-result.md` | CREATE | Record final validation outcomes for this milestone.

## Tasks
### Task 1: Tenant auth config model and startup loading
- Add `NEXUS_MCP_HTTP_TENANTS` env parser under `#[cfg(feature = "mcp-http")]`.
- Load JSON file at startup when set and fail closed on missing/malformed files.
- Validate schema entries (`tenant_id`, `api_key_sha256`, optional `rate_limit_rpm`).
- Use `api_key_sha256` as hex SHA-256 only; never store/use plaintext keys.
- Keep fallback: if no tenants file and `NEXUS_MCP_HTTP_TOKEN` is set, treat as implicit tenant `default` with rate limit default (`60`).
- Fail startup on no auth and non-loopback bind.

### Task 2: Tenant auth middleware and request context
- Replace/extend `require_bearer_token` with tenant lookup by SHA-256 hash lookup.
- Use constant-time byte comparison and do not short-circuit mismatch checks.
- Insert `TenantContext { tenant_id }` into request extensions on success.
- Return `401` for missing/unknown keys.
- Enforce token-less loopback warning path unchanged for developer mode.

### Task 3: Per-tenant rate limiting and audit logging
- Add per-tenant in-memory fixed-window limiter keyed by tenant ID and `rate_limit_rpm`.
- Return `429` when limit exceeded.
- Emit structured `tracing::info!` per authenticated request including `tenant_id`, method, path, and status class.

### Task 4: Tests
- Add tests without network dependency where possible:
  - tenant config loads from temp JSON file
  - missing / unknown bearer returns `401`
  - valid bearer sets tenant context
  - rate limit exceeded returns `429`
  - constant-time compare validates equal/mismatch behavior.

### Task 5: Docs + completion summary
- Add P2 section to `docs/mcp-setup.md`.
- Generate `.codex-p2-auth-result.md` with validation commands and results.

## Validation
```bash
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features "mcp-http aeon-memory" -- -D warnings
cargo test
cargo test --features "mcp-http aeon-memory"
cargo fmt --all
```

## Acceptance
- [ ] `NEXUS_MCP_HTTP_TENANTS` is loaded and validated at startup; malformed/missing file produces startup failure.
- [ ] Missing tenant config and token leaves loopback development behavior only; non-loopback rejects on startup.
- [ ] `NEXUS_MCP_HTTP_TOKEN` still works as backward-compatible implicit single-tenant mode.
- [ ] Bearer token hashes are compared in constant time and auth is tenant-aware.
- [ ] Each authenticated request logs tenant+method+path+status class; denied auth returns `401`, rate limit returns `429`.
- [ ] Test suite + clippy pass with requested feature sets and formatting is clean.
