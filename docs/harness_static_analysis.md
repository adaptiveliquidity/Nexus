# Harness — Static analysis & code-review automation

This doc closes the `harness-static` plan todo. It captures the
clippy/audit gates wired into CI (see [.github/workflows/ci.yml](../.github/workflows/ci.yml))
and the per-PR review-subagent routing the plan called for.

## Static analysis in CI

| Check | Where | Threshold |
|---|---|---|
| `cargo clippy --all-targets -- -D warnings` (with a small `-A` allowlist for pre-existing dead-code warnings in untouched files) | `.github/workflows/ci.yml::clippy` | Failure on any *new* warning |
| `cargo audit` | `.github/workflows/ci.yml::clippy` | Failure on any unresolved RUSTSEC advisory |
| `cargo test --all-targets` | `.github/workflows/ci.yml::test` | All tests pass |
| `cargo test --test phase3_distinct_outputs` | Same workflow, explicit step | The Claude-recommended regression test must stay green |
| `bash validate.sh 0 3` | Same workflow | Phase 0 specs + Phase 3 capture succeed |
| AI rescore | `.github/workflows/ai-rescore.yml` (triggered on push-to-main only) | `Aggregate accuracy rate >= 70%` |

The `-A` allowlist on the clippy step is intentional and small:
`-A dead_code -A unused_imports`. The pre-Phase-A code carries a handful
of these in files Phase A did not touch (e.g. the unused `Arc` imports
in `src/snapshot/manager.rs`). Each future PR should aim to remove one
of these and tighten the clippy line accordingly.

The `cargo-audit` advisory database is fetched at CI time; if it
flags one of our dependencies (e.g. an `openssl` CVE through `reqwest`
when the `ai-recovery` feature is on), the job fails and the PR
cannot land. Track upstream advisories in
`scripts/audit_baseline.txt` (TODO: populate when the first false
positive appears).

## Pre-merge review-subagent routing

The build plan calls for routing PRs through specific Cursor
subagents based on what changed. This is enforced by the
`.github/CODEOWNERS` file (added below) plus a Cursor-side rule that
the maintainer applies when invoking subagents locally before pushing.

| PR touches | Required review |
|---|---|
| Anything | Cursor `code-reviewer` subagent (always, fast) |
| `src/hypervisor/**` OR `src/sandbox/**` | Cursor `thermo-nuclear-review-subagent` (hypervisor/sandbox are security-sensitive) |
| Total diff > 300 lines | Cursor `thermo-nuclear-code-quality-review-subagent` |
| New public API in `src/lib.rs` re-exports | Trail of Bits `trailofbits/differential-review` over the diff |
| `src/hypervisor/llm_policy.rs` OR `docs/security_threat_model_phase_b.md` | OpenAI `openai/security-threat-model` (revisit the threat model when the LLM path changes) |
| Anything under `src/` (Rust) | ECC `agents/rust-reviewer.md` |
| `Cargo.toml` dependency additions | ECC `agents/security-reviewer.md` + `cargo audit` re-run locally |

These subagents are invoked by the maintainer locally (Cursor side)
before pushing — they are not enforced by CI today. A follow-up PR
can wire a `gh pr comment` step that posts the subagent verdict on
the PR, but the verdicts themselves should always be human-approved.

## CODEOWNERS

`.github/CODEOWNERS` (added) ensures any change to the hypervisor or
sandbox path requires the `@nexus-eng/security` team to approve, and
any change to the LLM policy requires `@nexus-eng/llm` to approve.
Placeholder team names; replace with the real org teams.

## Reproducing locally

```bash
cargo clippy --all-targets --locked -- -D warnings -A dead_code -A unused_imports
cargo audit
cargo test --all-targets --locked
bash validate.sh 0 3
```

Everything in the CI workflow runs in this order; if you want
parity with CI locally, follow the same sequence.

## What still isn't enforced

- The fuzz targets (`fuzz/`) are wired but not run in CI (they need
  nightly Rust and a time budget). Run them locally:
  `cargo +nightly fuzz run fuzz_execute_tool` and
  `cargo +nightly fuzz run fuzz_sanitize_for_prompt`.
- The Phase 1 Criterion benches are not run in CI (they take minutes
  and the results are sensitive to runner type). Run them locally:
  `bash validate.sh 1`.
- The Phase 2 hyperfine + Docker compare is not run in CI (needs the
  docker daemon and is even more runner-sensitive). Run locally:
  `bash validate.sh 2`.
