# Plan: P0 — SSRF / Egress Guard (Nexus runtime)

**Source PRD**: plans/nexus-iq-product.prd.md
**Selected Milestone**: P0 — Close SSRF/egress HIGH
**Complexity**: Medium

## Summary
Build a single reusable egress guard in the Nexus runtime that resolves a target URL's host and
rejects SSRF-class destinations (loopback, link-local/cloud-metadata `169.254.169.254`, private
ranges, IPv6 ULA/link-local) **unless the host is on an explicit allowlist**. Wire it into the only
live egress sink today ([`src/aeon.rs`](../src/aeon.rs)) and expose it as the chokepoint the future
WASM `http_get`/`http_post` sink (P5) must call. This closes the HIGH *before* remote exposure (P1).

## Ground-truth findings (verified this session)
- `Capability::HttpGet/HttpPost` are **declarative-only** — no WASI HTTP host fn is wired
  ([`src/security/capability.rs:25`](../src/security/capability.rs)); WASM cannot egress yet.
- Only live egress: AEON client reqwest calls in [`src/aeon.rs`](../src/aeon.rs)
  (`get`/`post_json`/`url`), `base_url` from `NEXUS_AEON_BASE_URL` (default `http://localhost:8080`).
- No SSRF guard exists (`src/security/` = `capability.rs`, `mod.rs`, `negotiator.rs` only).
- **Trap**: default base_url is loopback by design — guard must allow the configured base host.

## Patterns to Mirror
| Category | Source | Pattern |
|---|---|---|
| Module layout | `src/security/capability.rs` | new sibling `src/security/egress.rs`, re-export in `src/security/mod.rs` |
| Errors | `src/error.rs` (`NexusError`) | add `NexusError::EgressDenied(String)`; return `Result<>` |
| Config-from-env | `src/aeon.rs:19-26` | `NEXUS_EGRESS_ALLOWLIST` (comma host list), `NEXUS_EGRESS_ALLOW_PRIVATE` (bool) |
| Tests | `src/security/capability.rs` `#[cfg(test)] mod tests` | unit tests in-module, table-style |

## Files to Change
| File | Action | Why |
|---|---|---|
| `src/security/egress.rs` | CREATE | the guard: `EgressPolicy` + `check_url(&Url) -> Result<()>` |
| `src/security/mod.rs` | UPDATE | `pub mod egress; pub use egress::EgressPolicy;` |
| `src/error.rs` | UPDATE | add `EgressDenied` variant |
| `src/aeon.rs` | UPDATE | construct `EgressPolicy` once; call `check_url` before each `send()` |
| `docs/mcp-setup.md` | UPDATE | document `NEXUS_EGRESS_*` env knobs |

## Tasks
### Task 1: `EgressPolicy` guard
- **Action**: `EgressPolicy { allow_hosts: HashSet<String>, allow_private: bool }`.
  `from_env()` reads `NEXUS_EGRESS_ALLOWLIST` + `NEXUS_EGRESS_ALLOW_PRIVATE`. `check_url(url)`:
  1. host on `allow_hosts` → allow (covers configured base_url, e.g. `localhost`).
  2. resolve host to IPs (`to_socket_addrs`); **deny on resolution failure** (fail-closed).
  3. for every resolved IP: deny if loopback, link-local (incl. `169.254.169.254`), private,
     unspecified, ULA/IPv6 link-local — unless `allow_private`.
  4. scheme must be `http`/`https`.
- **Mirror**: `Capability::allows` shape; `NexusError` returns.
- **Validate**: `cargo test -p nexus egress`

### Task 2: Wire into AEON client
- **Action**: build `EgressPolicy::from_env()` in `AeonMemoryClient::from_config`, **auto-add the
  configured base_url host to `allow_hosts`** so default localhost self-host keeps working. Call
  `policy.check_url(&url)` in `get`/`post_json` before `send()`; on `EgressDenied` log + fail
  (consistent with existing fail-open *observability* but the request must not fire).
- **Mirror**: existing `#[cfg(test)] test_responder` short-circuit stays before the guard.
- **Validate**: existing `src/aeon.rs` tests still pass; localhost base_url still allowed.

### Task 3: Docs + env knobs
- **Action**: document `NEXUS_EGRESS_ALLOWLIST`, `NEXUS_EGRESS_ALLOW_PRIVATE` in docs/mcp-setup.md.

## Validation
```bash
cargo test -p nexus egress
cargo test -p nexus aeon
cargo clippy --all-targets -- -D warnings
```

## Risks
| Risk | Likelihood | Mitigation |
|---|---|---|
| Guard breaks default localhost self-host | High if naive | Auto-allow configured base_url host (Task 2) |
| DNS-rebinding (resolve-then-connect TOCTOU) | Medium | Note for P5 (pin resolved IP at connect); P0 documents the gap |
| Over-broad allowlist via env | Low | Default deny-private; allowlist is explicit opt-in |

## Acceptance
- [ ] `EgressPolicy::check_url` blocks `169.254.169.254`, loopback, private (tests prove it)
- [ ] Default `http://localhost:8080` AEON config still works (configured host auto-allowed)
- [ ] `cargo test` + `cargo clippy -D warnings` green
- [ ] Single chokepoint reusable by the P5 WASM http sink

## Execution note (routing)
Per standing policy (heavy Rust → Codex; Opus = final security judgment), implementation should be
dispatched to Codex against the canonical checkout, with Claude reviewing the guard logic
(SSRF correctness is exactly the security-judgment task) and committing. Open item below.
