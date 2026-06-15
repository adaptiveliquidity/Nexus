# Agent Coordination

This repository is built collaboratively by more than one AI coding agent
(e.g. Claude Code and Codex) working **in parallel**. Agents do not share
memory — this document plus the committed RFCs (`docs/rfcs/`) are the shared
source of truth. Read this before starting any work.

## 1. Golden rule: avoid collisions

Two agents editing the same files or branch is the main failure mode.

Before starting a task:

1. Run `gh pr list --state open` and `git branch -a` to see what's in flight.
2. Check the **Build status** table at the bottom of this file.
3. Pick work that does **not** overlap the files/modules another agent owns.
4. Claim it: create your feature branch and add a row to the Build status
   table in your first commit, naming the agent, branch, and the files/modules
   you'll touch.
5. One owner per branch and per module at a time. If two tasks genuinely need
   the same file, serialize them: land one PR first, then rebase the other.

Never commit directly to `main`. Never force-push a branch another agent owns.

## 2. Workflow (both agents follow this)

A gated pipeline keeps changes reviewable and safe:

1. **Research** the existing code and conventions.
2. **Plan** a task list and get it approved (Gate 1) before writing code.
3. **TDD** — write tests first/alongside; implementation makes them pass.
4. **Review** — self-review or a reviewer agent; security-sensitive changes
   (auth, crypto, capability/token logic, daemon/MCP surface) get a security
   review.
5. **Commit** only after the plan and tests are green (Gate 2).

Conventions:

- One feature = one branch (`feat/…`, `fix/…`, `docs/…`, `security/…`,
  `chore/…`) = one PR to `main`.
- Conventional commit messages; end commits with a `Co-Authored-By:` line for
  the authoring agent.
- The full suite and clippy must pass before opening a PR (see §4).
- Keep PRs focused; defer out-of-scope findings as tracked follow-ups rather
  than expanding the diff.
- After a PR merges, delete its branch.

## 3. Locked technical decisions

Do not silently diverge from these — they are load-bearing and tested. If a
change is needed, update the RFC and bump the relevant version constant.

- **Snapshot digest** (`src/snapshot/sync/digest.rs`, RFC 0001 §4.1):
  SHA-256 over a domain-separated, length-prefixed canonical encoding;
  **excludes** `compressed_size`, compressed bytes, `id`, and `timestamp` so
  the digest is compression- and identity-invariant. Pinned test vectors guard
  the byte layout — changing it is a breaking change (bump `SCHEMA_VERSION` +
  `DOMAIN`).
- **Sync protocol** (`src/snapshot/sync/protocol.rs`, RFC 0001 §5):
  verify-before-store (`verify_snapshot_digest` before insert), storage keyed
  by the node's own computed digest, duplicate valid snapshot = idempotent
  `Ack` (never `Nack`), `replicate()` bounded by `max_steps`.
- **Security invariants**: capability attenuation narrows only (never widens);
  MCP `issue_token` clamps validity and rejects `Capability::All`; replicated
  `fs_changes` are **never auto-applied**; `max_memory_pages` is enforced via
  wasmtime store limits. See `SECURITY.md`.
- **Scope discipline**: RFC 0001 phases land incrementally. Do not implement
  later phases (gRPC, lineage heads/HLC, restore authorization, `fs_changes`
  application) ahead of their turn.

## 4. Validation (narrowest first, then broaden)

```bash
cargo test --lib <filter>          # fast inner loop
cargo test --test <test_name>      # one integration suite
cargo test --all-targets           # full suite before a PR
cargo clippy --all-targets -- -D warnings
```

Format only the files you changed (avoid repo-wide `cargo fmt` churn). For
benchmarks, claims, or release readiness, record the exact commands run and
separate runnable local gates from external/hardware gates.

## 5. Safety in a shared checkout

- Prefer read-only audits or isolated `git worktree`s for write-heavy fan-out;
  do not run parallel write-capable agents in the same dirty checkout.
- Local, machine-specific agent configuration (e.g. Codex specialist-agent
  routing) lives outside the committed tree (under `.codex/`); it is not part
  of repo history and must not be staged unless the user asks.

## 6. Build status (keep current)

Update this table as part of your branch's first and final commits.

| Agent | Branch | Scope / files | Status |
|-------|--------|---------------|--------|
| Claude Code | `feat/snapshot-sync-daemon-transport` | RFC 0001 daemon-framing transport: `src/snapshot/sync/{auth,framed_transport}.rs` + serde on `protocol.rs`/`digest.rs` | In progress |

**Merged to `main`:** snapshot-sync Phase 1 digest core (#39), P3 research RFCs (#40), snapshot-sync Phase 2 protocol (#41), P1 security hardening (#42).

**Deferred (not yet claimed):** density benchmark run (release-time, once the
execute path is stable); the lower-priority security audit follow-ups
(non-functional `nexus_execute_wasi`, WASI `WasiToolConfig` TOCTOU, etc.).

See `docs/rfcs/` for design and `CONTRIBUTING.md` for the human contribution
process.
