# Nexus RFCs

Design documents for research-track (P3) and larger features. RFCs are
**design-only** — they propose and evaluate approaches before any production code
is written. Each is grounded in the current codebase.

| # | Title | Status | Headline |
|---|-------|--------|----------|
| [0001](0001-distributed-snapshot-sync.md) | Distributed Snapshot Synchronization | Draft | Snapshots are already serializable + content-addressed; ship immutable objects, surface lineage forks rather than auto-merging. Recommends a transport-agnostic core (daemon-framing first, gRPC later). |
| [0002](0002-wasm-callstack-capture.md) | WASM Call-Stack Capture | Draft | wasmtime 45 supports diagnostic backtrace *capture* but not stack *serialize/restore*. Recommends diagnostics-only; use fuel-indexed deterministic replay for time-travel. |
| [0003](0003-zk-capability-attestation.md) | Zero-Knowledge Capability Attestation | Draft | Feasible but poor cost/benefit in a single trust domain (Ed25519-in-circuit is the cost driver). Recommends deferring; try attenuation-based minimal disclosure first. |

## Status values

- **Draft** — under discussion, not accepted.
- **Accepted** — agreed direction; implementation may proceed.
- **Rejected** — evaluated and declined (rationale retained for the record).
- **Superseded** — replaced by a later RFC (linked).
