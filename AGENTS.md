# AGENTS.md

This repo is built by multiple AI agents working in parallel. **Before starting
any work, read [`docs/AGENT-COORDINATION.md`](docs/AGENT-COORDINATION.md)** — it
defines the collision-avoidance protocol, the gated workflow, the locked
technical decisions, validation commands, and the current build status.

Design lives in [`docs/rfcs/`](docs/rfcs/); the human contribution process is in
[`CONTRIBUTING.md`](CONTRIBUTING.md); the security policy is in
[`SECURITY.md`](SECURITY.md).

## Portable ground rules

- Treat the live repository as authoritative. Start substantial work by checking
  `git status --short --branch`, the current branch, `gh pr list --state open`,
  and the relevant source files.
- One feature = one branch = one PR to `main`. Never commit to `main` directly;
  never force-push a branch another agent owns.
- Follow the gated pipeline (research → plan → TDD → review → commit) and the
  validation steps in `docs/AGENT-COORDINATION.md` §4.
- Do not run write-capable fan-out in a dirty shared checkout; use read-only
  audits or isolated `git worktree`s.
- Local, machine-specific agent configuration (e.g. Codex specialist-agent
  routing) lives under `.codex/` and is intentionally **not** committed — do not
  add or stage harness artifacts unless the user explicitly asks.
