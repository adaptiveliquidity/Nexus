# Nexus RFCs

Design documents for research-track (P3) and larger features. RFCs are
**design-only** — they propose and evaluate approaches before any production code
is written. Each is grounded in the current codebase.

| # | Title | Status | Headline |
|---|-------|--------|----------|
| [0001](0001-distributed-snapshot-sync.md) | Distributed Snapshot Synchronization | Draft | Snapshots are already serializable + content-addressed; ship immutable objects, surface lineage forks rather than auto-merging. Recommends a transport-agnostic core (daemon-framing first, gRPC later). |
| [0002](0002-wasm-callstack-capture.md) | WASM Call-Stack Capture | Draft | wasmtime 45 supports diagnostic backtrace *capture* but not stack *serialize/restore*. Recommends diagnostics-only; use fuel-indexed deterministic replay for time-travel. |
| [0003](0003-zk-capability-attestation.md) | Zero-Knowledge Capability Attestation | Draft | Feasible but poor cost/benefit in a single trust domain (Ed25519-in-circuit is the cost driver). Recommends deferring; try attenuation-based minimal disclosure first. |
| [0004](0004-capability-profiles.md) | Capability Profile Manifests | Accepted (Slice 1 shipped) | TOML manifest that captures full deployment security posture: allowed module dirs, daemon auth, token lifetime, MCP tool exposure, and capability scopes. Slice 1 (validator + `[mcp]` enforcement in nexus-mcp) is shipped; Slice 2 (profile-driven `module_dirs`) and Slice 3 (daemon auth) remain. |

## Status values

- **Draft** — under discussion, not accepted.
- **Accepted** — agreed direction; implementation may or may not be complete (see RFC for slice status).
- **Rejected** — evaluated and declined (rationale retained for the record).
- **Superseded** — replaced by a later RFC (linked).
