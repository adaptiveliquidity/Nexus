# Blueprint: Nexus Third-Party Verifiable Benchmark Pipeline

**Objective:** Ship a fully automated, cryptographically attestable, publicly viewable benchmark pipeline that runs Nexus's Criterion suite on neutral GitHub-hosted hardware, publishes to Bencher.dev + Codspeed.io, and renders a public comparison dashboard.

**Branch:** `claude/third-party-benchmarks`
**Repo:** `Adaptive-Liquidity/Nexus` (local: `C:\Users\Benna\Documents\Nexus\Nexus`)
**Base:** `main`
**PR target:** `main`

---

## Dependency Graph

```
Step 0 (load reference skills)
  |
  +---> Step 1 (branch setup)
          |
          +---> Step 2 (fix cargo config)  ---> Step 3 (bench profile + codspeed dep)
          |       |                                   |
          |       |                                   +---> Step 4 (dual-mode bench harness)
          |       |                                   |        |
          |       |                                   |        +---> Step 5 (GH Actions workflow)
          |       |                                   |        |
          |       |                                   |        +---> Step 7 (correct marketing) [parallel w/ 5]
          |       |                                   |
          |       |                                   +---> Step 8 (.mcp.json + SETUP.md) [parallel w/ 4]
          |       |
          |       +---> Step 6 (dashboard)  [parallel with 2-5, no Rust dep]
          |
          +---> (all above) ---> Step 9 (validation checklist)
                                   |
                                   +---> Step 10 (security scan + PR)
```

**Parallelism:** Step 6 (dashboard) has no Rust dependency -- it can start as soon as the branch exists (after Step 1). Steps 5 and 7 are independent once Step 4 is done. Step 8 is independent once Step 3 is done. Steps 9-10 are serial gates at the end.

---

## Step 0: Load Reference Skills (pre-requisite)

**Why:** Three external references materially improve execution quality. Loading them before implementation prevents stale-knowledge mistakes in Steps 4, 6, and 10.

**Model tier:** default

### Tasks
1. **CodSpeed Criterion integration pattern** (for Step 4): Read https://codspeed.io/docs/benchmarks/rust/criterion -- confirms `codspeed-criterion-compat` usage, `#[cfg(codspeed)]` pattern, `cargo codspeed build && cargo codspeed run` workflow
2. **Next.js static export constraints** (for Step 6): Two pitfalls:
   - `basePath` must match the GitHub Pages subpath (`/Nexus`)
   - `output: "export"` is incompatible with `getServerSideProps` and ISR; everything must be SSG or client-fetched
   - `getStaticProps` IS supported with `output: "export"` in Pages Router -- but the page component itself cannot be a client component simultaneously
3. **Security scan tool** (for Step 10): Will use `npx ecc-agentshield scan` (AgentShield from ECC) -- scans `.github/workflows/`, `.mcp.json`, `Cargo.toml` for secret leakage and injection risks

### Exit Criteria
- Agent has loaded the three reference patterns before starting implementation

---

## Step 1: Branch Setup & Pre-flight

**Why:** Establish a clean working branch from `main` so all changes are isolated and PR-able.

**Model tier:** default

### Context Brief
The repo is at `C:\Users\Benna\Documents\Nexus\Nexus`. Current branch is `feat/phase-abc-complete-26-todos`. Remote is `origin -> https://github.com/Adaptive-Liquidity/Nexus.git`. Need to create branch `claude/third-party-benchmarks` from `main`.

### Tasks
1. `git stash` if there are uncommitted changes on current branch
2. `git checkout main && git pull --ff-only`
3. `git checkout -b claude/third-party-benchmarks`
4. Verify: `cargo --version` (expect 1.75+), `rustc --version`, `cargo bench --bench nexus_validation --no-run`

### Verification
```bash
git branch --show-current  # -> claude/third-party-benchmarks
cargo bench --bench nexus_validation --no-run  # exit 0
```

### Exit Criteria
- On branch `claude/third-party-benchmarks` based on latest `main`
- `cargo bench --bench nexus_validation --no-run` succeeds

### Rollback
```bash
git checkout feat/phase-abc-complete-26-todos
git branch -D claude/third-party-benchmarks
```

---

## Step 2: Fix Cargo Config (alias shadow + malformed target table)

**Why:** Two pre-existing issues in `.cargo/config.toml` will break CI: (1) `bench` alias shadows the real `cargo bench` subcommand, (2) `[target.'cfg(...)'.build]` is a malformed table that emits warnings.

**Model tier:** default
**Depends on:** Step 1

### Context Brief
File: `.cargo/config.toml` (20 lines). Currently has:
- Line 5: `[target.'cfg(not(target_os = "windows"))'.build]` -- the `.build` suffix is wrong; `rustflags` belongs directly under `[target.'cfg(...)']`
- Line 16: `bench = "build --release --benches"` -- shadows `cargo bench`
- Lines 8-13: `[profile.release]` -- this does NOT belong in `.cargo/config.toml` (cargo profiles go in `Cargo.toml`); remove it

### Tasks
1. Replace the `[target.'cfg(not(target_os = "windows"))'.build]` table header with `[target.'cfg(not(target_os = "windows"))']` (remove `.build`)
2. In `[alias]`, rename `bench` to `build-benches`
3. Remove the `[profile.release]` block entirely (profiles belong in `Cargo.toml`, not config.toml)

### Target State
```toml
[build]
jobs = 4

[target.'cfg(not(target_os = "windows"))']
rustflags = ["-C", "link-arg=-Wl,-S"]

[alias]
build-benches = "build --release --benches"
test-all = "test --all-features"
lint = "clippy -- -D warnings"
```

### Verification
```bash
cargo bench --bench nexus_validation --no-run 2>&1 | grep -c "unused key"  # -> 0
cargo build-benches --no-run  # exit 0 (alias works)
```

### Exit Criteria
- No `unused key` warnings from cargo
- No alias shadows a built-in subcommand
- `cargo bench --bench nexus_validation --no-run` still succeeds

### Rollback
```bash
git checkout -- .cargo/config.toml
```

---

## Step 3: Add Bench Profile + CodSpeed Dependency

**Why:** (1) A `[profile.bench]` with `debug = 1` is required for CodSpeed symbol resolution and speeds up bench compilation ~10x vs release defaults. (2) `codspeed-criterion-compat` is the CodSpeed-official dual-mode crate.

**Model tier:** default
**Depends on:** Step 2

### Context Brief
File: `Cargo.toml`. Currently has `criterion = "0.5"`, `tempfile = "3.14"`, `proptest = "1.5"` in `[dev-dependencies]`. No `[profile.bench]` section exists.

### Tasks
1. Add to `[dev-dependencies]`:
   ```toml
   codspeed-criterion-compat = "2.7"
   ```
2. Add `[profile.release]` section to `Cargo.toml` (preserves the size-optimized settings that Step 2 removed from `.cargo/config.toml`):
   ```toml
   [profile.release]
   opt-level = "z"
   lto = true
   codegen-units = 1
   strip = true
   ```
3. Add `[profile.bench]` section at end of `Cargo.toml`:
   ```toml
   [profile.bench]
   inherits = "release"
   opt-level = 3
   lto = "thin"
   codegen-units = 16
   strip = false
   debug = 1
   ```

### Verification
```bash
cargo bench --bench nexus_validation --no-run  # succeeds, compiles with new profile
cargo build --release --bin nexus 2>&1 | head -5  # uses the profile.release settings
```

### Exit Criteria
- `codspeed-criterion-compat` resolves without errors
- `[profile.release]` preserves size-optimized settings (opt-level "z", lto true, strip true)
- `[profile.bench]` is present and `debug = 1` is set
- Bench compilation succeeds

### Rollback
```bash
git checkout -- Cargo.toml
```

---

## Step 4: Dual-Mode Criterion Harness (CodSpeed instrumentation)

**Why:** Same bench code must produce wall-clock data for Bencher (via `criterion`) AND instruction-count data for CodSpeed (via `codspeed-criterion-compat`). The `#[cfg(codspeed)]` conditional import is the CodSpeed-official pattern.

**Model tier:** default
**Depends on:** Step 3

### Context Brief
File: `benches/nexus_validation.rs` (262 lines). Lines 17-19 import from `criterion` directly. CodSpeed's `cargo codspeed bench` sets the `codspeed` cfg flag automatically, so a `#[cfg(codspeed)]` / `#[cfg(not(codspeed))]` conditional import switches between the two crates at compile time with zero runtime cost.

### Tasks
1. Replace lines 17-19 (the `use criterion::{...}` block) with:
   ```rust
   #[cfg(codspeed)]
   use codspeed_criterion_compat::{
       black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
   };
   #[cfg(not(codspeed))]
   use criterion::{
       black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
   };
   ```
2. No other changes needed -- all bench functions use the same API.

### Verification
```bash
cargo bench --bench nexus_validation --no-run          # wall-clock mode (criterion path)
RUSTFLAGS="--cfg codspeed" cargo check --bench nexus_validation  # codspeed path compiles
```

### Exit Criteria
- `cargo bench --bench nexus_validation --no-run` succeeds (non-codspeed path)
- CodSpeed path compiles when `--cfg codspeed` is set
- The `#[cfg(codspeed)]` import block is present

### Rollback
```bash
git checkout -- benches/nexus_validation.rs
```

---

## Step 5: GitHub Actions Workflow

**Why:** The CI workflow is the core of the pipeline -- it runs benchmarks on neutral hardware (GitHub-hosted ubuntu-24.04), publishes to two third-party services, signs artifacts with Sigstore, and deploys the dashboard.

**Model tier:** strongest (complex YAML, security-sensitive)
**Depends on:** Step 4
**Parallel with:** Steps 6, 7

### Context Brief
Create `.github/workflows/benchmarks.yml`. Existing workflows are `ci.yml` and `ai-rescore.yml` -- no conflicts. The workflow has three jobs:
1. **bencher** -- runs `cargo bench`, uploads to Bencher.dev, signs with cosign
2. **codspeed** -- runs `cargo codspeed bench` via CodSpeedHQ/action@v3
3. **publish-dashboard** -- builds Next.js static site, deploys to GitHub Pages

Triggers: push to main, PRs to main, weekly cron (Sunday 03:00 UTC), manual dispatch.

### Tasks
1. Create `.github/workflows/benchmarks.yml` with the exact YAML below:

```yaml
name: Benchmarks (Nexus vs. competitors)

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  schedule:
    - cron: "0 3 * * 0"
  workflow_dispatch:

permissions:
  contents: read
  pull-requests: write
  id-token: write

jobs:
  bencher:
    name: "Criterion -> Bencher.dev"
    runs-on: ubuntu-24.04
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
        with:
          shared-key: bench-${{ runner.os }}
      - uses: bencherdev/bencher@main

      - name: Record runner provenance
        run: |
          mkdir -p benchmark_evidence
          {
            echo "## Runner Provenance"
            echo "- Date (UTC): $(date -u --iso-8601=seconds)"
            echo "- Commit: ${{ github.sha }}"
            echo "- Runner: ubuntu-24.04 (GitHub-hosted)"
            echo "- CPU: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs)"
            echo "- Cores: $(nproc)"
            echo "- Memory: $(free -h | awk '/^Mem:/ {print $2}')"
            echo "- Kernel: $(uname -r)"
            echo "- rustc: $(rustc --version)"
          } > benchmark_evidence/runner.md

      - name: Run Criterion
        run: |
          cargo bench --bench nexus_validation -- --output-format bencher \
            | tee benchmark_evidence/criterion_raw.log

      - name: Upload to Bencher.dev
        run: |
          bencher run \
            --project "${{ vars.BENCHER_PROJECT }}" \
            --token "${{ secrets.BENCHER_API_TOKEN }}" \
            --branch "${{ github.ref_name }}" \
            --testbed "ubuntu-24.04-github" \
            --adapter rust_criterion \
            --err \
            --github-actions "${{ secrets.GITHUB_TOKEN }}" \
            --file benchmark_evidence/criterion_raw.log

      - uses: sigstore/cosign-installer@v3
      - name: Sign artifacts with Sigstore
        run: |
          tar czf benchmark_evidence/criterion_artifacts.tar.gz \
            target/criterion benchmark_evidence/runner.md \
            benchmark_evidence/criterion_raw.log
          cosign sign-blob --yes \
            --bundle benchmark_evidence/criterion_artifacts.sigstore \
            benchmark_evidence/criterion_artifacts.tar.gz

      - uses: actions/upload-artifact@v4
        with:
          name: criterion-evidence-${{ github.sha }}
          path: benchmark_evidence/
          retention-days: 90

  codspeed:
    name: "Criterion -> Codspeed.io (deterministic)"
    runs-on: ubuntu-24.04
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
        with:
          shared-key: codspeed-${{ runner.os }}
      - uses: CodSpeedHQ/action@v3
        with:
          run: cargo codspeed bench --bench nexus_validation
          token: ${{ secrets.CODSPEED_TOKEN }}

  publish-dashboard:
    name: Publish public dashboard
    needs: [bencher, codspeed]
    if: github.ref == 'refs/heads/main'
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: "20"
      - run: |
          cd dashboard
          npm ci
          npm run build
      - uses: actions/upload-pages-artifact@v3
        with:
          path: dashboard/out
      - uses: actions/deploy-pages@v4
```

### Security Notes
- No user-controlled strings in `run:` blocks (no `${{ github.event.*.title }}` injection)
- Secrets accessed only via `${{ secrets.* }}` and `${{ vars.* }}`
- `id-token: write` scoped to minimum needed for Sigstore

### Verification
```bash
# If actionlint is available:
actionlint .github/workflows/benchmarks.yml

# Manual checks:
grep "id-token: write" .github/workflows/benchmarks.yml
grep "cosign sign-blob" .github/workflows/benchmarks.yml
grep "CodSpeedHQ/action@v3" .github/workflows/benchmarks.yml
grep "deploy-pages@v4" .github/workflows/benchmarks.yml
```

### Exit Criteria
- Workflow file exists and is valid YAML
- Three jobs defined: `bencher`, `codspeed`, `publish-dashboard`
- Sigstore signing step present
- Dashboard deploy conditional on `refs/heads/main`
- No injection risks in `run:` blocks

### Rollback
```bash
rm .github/workflows/benchmarks.yml
```

---

## Step 6: Public Comparison Dashboard

**Why:** A static site pulling live Nexus data from the Bencher API and overlaying documented competitor numbers provides a single URL anyone can visit to verify claims.

**Model tier:** strongest (full Next.js app from scratch)
**Depends on:** Step 1 (branch only -- no Rust dependency)
**Parallel with:** Steps 2-5, 7, 8

### Context Brief
Create `dashboard/` directory with a Next.js 14 static export site. Critical constraints:
- `output: "export"` means NO `getServerSideProps`, NO ISR -- client-side fetch only
- `basePath: "/Nexus"` matches GitHub Pages subpath
- Brand colors: `#9cff3b` (liquidity green), `#020404` (void), `#00d8ff` (signal cyan), `#f4f7f2` (bone)
- Competitor data checked into `competitors.yml` with source URLs for every number

### Tasks
1. Create `dashboard/competitors.yml` with competitor data (8 competitors, all with source citations)
2. Create `dashboard/package.json`:
   - next@14, react@18, react-dom@18, js-yaml, recharts (chart library)
   - Scripts: `dev`, `build` (next build), `start`
3. Create `dashboard/next.config.js`:
   ```js
   module.exports = {
     output: "export",
     basePath: "/Nexus",
     images: { unoptimized: true },
     transpilePackages: ["recharts"]
   }
   ```
4. Create `dashboard/pages/index.jsx` using Pages Router pattern:
   - `getStaticProps` loads `competitors.yml` at build time and passes data as props (SSG-compatible with `output: "export"`)
   - The page component receives competitor data via props (server-side at build time)
   - A client-side `useEffect` hook fetches live Nexus data from the Bencher API (runs in browser only)
   - NOTE: `getStaticProps` and `useEffect` CAN coexist in Pages Router -- `getStaticProps` runs at build time, `useEffect` runs client-side. Do NOT add `"use client"` directive (that's App Router syntax)
   - Bar chart (recharts) comparing cold start times
   - Table with all metrics and source links
   - Header with Nexus branding using AEON colors
5. Create `dashboard/pages/_app.jsx` with global styles (AEON brand colors)
6. Verify build produces `dashboard/out/`

### Verification
```bash
cd dashboard && npm ci && npm run build
ls out/           # non-empty
ls out/index.html # exists
```

### Exit Criteria
- `npm run build` produces `dashboard/out/` with `index.html`
- `next.config.js` has `output: "export"` and `basePath: "/Nexus"`
- No `getServerSideProps` in any page file
- Competitor data rendered from `competitors.yml`

### Rollback
```bash
rm -rf dashboard/
```

---

## Step 7: Correct Marketing Claims in README + BENCHMARKS

**Why:** Four headline claims in `README.md` and `BENCHMARKS.md` do not survive technical due diligence. Replacing them with honest, measured numbers is a hard requirement of this PR.

**Model tier:** strongest (user-facing copy with legal implications)
**Depends on:** Step 4
**Parallel with:** Steps 5, 6

### Context Brief
The four claims to retire:
1. "23 us cold start, 217x faster than CF Workers" -- apples-to-oranges comparison
2. "56 us snapshot creation" -- only true for empty memory; 1 MiB = 2.92 ms
3. "<1 ms rollback" -- false at 10 MiB (1.62 ms) and 100 MiB (53.6 ms)
4. "10,000+ concurrent sandboxes" -- never measured

Files:
- `README.md` lines 11-19: "Key Performance Metrics" table
- `README.md` lines 29-34: competitor comparison claims ("Docker: 30-second cold start" etc.)
- `BENCHMARKS.md`: entire file is built around the inflated claims

### Tasks
1. In `README.md`, replace the "Key Performance Metrics" table with honest numbers:
   - Cold start: "23 us (sandbox struct init)" with note about end-to-end being higher
   - Snapshot: size-dependent range (empty -> 1 MiB -> 100 MiB)
   - Rollback: size-dependent range with actual measured values
   - Concurrent sandboxes: remove or mark "not yet measured"
   - Add "Live benchmarks" link to dashboard
2. In `README.md`, update competitor comparisons to use only cited, defensible numbers
3. In `BENCHMARKS.md`:
   - Replace static tables with link to live dashboard
   - Keep the methodology section
   - Add a "Retired Claims" section explaining what changed and why
4. Ensure no verbatim retired claims remain in either file

### Verification
```bash
grep -c "217x faster" README.md        # -> 0
grep -c "56 microseconds" README.md    # -> 0
grep -c "10,000+" README.md            # -> 0 (or clearly marked as unmeasured)
grep "adaptive-liquidity.github.io" README.md  # -> present
```

### Exit Criteria
- None of the four retired claims appear verbatim
- BENCHMARKS.md links to the live dashboard
- Methodology section preserved
- Retired claims section explains what changed

### Rollback
```bash
git checkout -- README.md BENCHMARKS.md
```

---

## Step 8: MCP Config + SETUP.md

**Why:** (1) `.mcp.json` gives Claude Code users persistent CodSpeed regression analysis via MCP tools. (2) `SETUP.md` documents the one-time manual steps the repo admin must complete.

**Model tier:** default
**Depends on:** Step 3
**Parallel with:** Steps 4-7

### Context Brief
`.mcp.json` uses `${CODSPEED_TOKEN}` env var interpolation. Adding to `.gitignore` prevents accidental plaintext token commits. `SETUP.md` covers five items: Bencher.dev, CodSpeed.io, GitHub Pages, CodSpeed MCP, Sigstore (no-op).

### Tasks
1. Create `.mcp.json`:
   ```json
   {
     "mcpServers": {
       "codspeed": {
         "type": "http",
         "url": "https://mcp.codspeed.io",
         "headers": { "Authorization": "Bearer ${CODSPEED_TOKEN}" }
       }
     }
   }
   ```
2. Add `.mcp.json` to `.gitignore`
3. Create `SETUP.md` with five setup steps:
   - Bencher.dev: sign in with `Adaptive-Liquidity` org, create project `nexus-ai`, add `BENCHER_API_TOKEN` secret + `BENCHER_PROJECT` variable
   - CodSpeed.io: sign in with GitHub, install CodSpeed GitHub app, add `CODSPEED_TOKEN` secret
   - GitHub Pages: Settings -> Pages -> Source = "GitHub Actions"
   - CodSpeed MCP (optional): paste token into Claude Code MCP config
   - Sigstore: no setup needed (uses workflow OIDC)
   - Note: both Bencher and CodSpeed are free for public OSS

### Verification
```bash
grep ".mcp.json" .gitignore   # present
python3 -m json.tool .mcp.json  # valid JSON (or equivalent)
test -f SETUP.md               # exists
```

### Exit Criteria
- `.mcp.json` exists and is valid JSON
- `.mcp.json` is in `.gitignore`
- `SETUP.md` exists with all five setup steps

### Rollback
```bash
rm .mcp.json SETUP.md
git checkout -- .gitignore
```

---

## Step 9: Validation Checklist

**Why:** Every item in the mission brief's validation checklist must pass before the PR is opened. This is the quality gate.

**Model tier:** default
**Depends on:** Steps 2-8 (all implementation steps)

### Tasks
Run each check and record pass/fail:

1. [ ] `cargo build --release --bin nexus` succeeds
2. [ ] `cargo bench --bench nexus_validation --no-run` succeeds
3. [ ] `cargo bench --bench nexus_validation -- --quick` produces non-empty output
4. [ ] `cd dashboard && npm ci && npm run build` produces non-empty `dashboard/out/`
5. [ ] `.github/workflows/benchmarks.yml` passes `actionlint` (if available)
6. [ ] No `unused key` warnings from cargo
7. [ ] No cargo alias shadows a builtin
8. [ ] `README.md` no longer contains any of the four retired claims verbatim
9. [ ] `BENCHMARKS.md` links to the live dashboard

### Exit Criteria
- All checks pass (or failures documented with explanations)
- Ready to open PR

### Rollback
N/A -- read-only step.

---

## Step 10: Security Scan + Open PR

**Why:** Pre-merge security scan catches secret leakage or injection risks in the new workflow files. Then open the PR.

**Model tier:** default
**Depends on:** Step 9

### Tasks
1. Run `npx ecc-agentshield scan` (or equivalent) on new files
2. Fix any critical findings
3. Stage all changes and commit:
   ```
   ci: third-party verifiable benchmark pipeline

   - Add Bencher.dev + Codspeed.io GitHub Actions workflows
   - Add CodSpeed MCP server config (.mcp.json) for ongoing regression analysis
   - Add dual-mode Criterion harness (wall-clock + instruction-count)
   - Add public comparison dashboard with documented competitor citations
   - Add Sigstore keyless signing of benchmark artifacts
   - Fix cargo config: remove bench alias shadow, fix malformed target table
   - Add fast [profile.bench] (~10x faster bench compile)
   - Correct four marketing claims in README + BENCHMARKS
   ```
4. Push branch and open PR:
   ```bash
   git push -u origin claude/third-party-benchmarks
   gh pr create --title "Third-party verifiable benchmark pipeline" --body "..."
   ```

### PR Body Template
```markdown
## Summary
- Third-party verifiable benchmark pipeline using Bencher.dev + Codspeed.io
- Public comparison dashboard at https://adaptive-liquidity.github.io/Nexus/
- Sigstore-signed benchmark artifacts for cryptographic attestation
- Corrected four marketing claims that did not survive technical due diligence

## What Changed
- `.cargo/config.toml`: fixed alias shadow + malformed target table
- `Cargo.toml`: added codspeed-criterion-compat, [profile.bench]
- `benches/nexus_validation.rs`: dual-mode (criterion / codspeed) imports
- `.github/workflows/benchmarks.yml`: three-job pipeline (Bencher + CodSpeed + dashboard)
- `dashboard/`: Next.js 14 static export with competitor comparison chart
- `README.md` + `BENCHMARKS.md`: honest numbers, live dashboard link
- `.mcp.json`: CodSpeed MCP server config
- `SETUP.md`: one-time admin setup checklist

## What Cael Must Do (one-time)
See SETUP.md -- five items, no paid plans required.

## Out of Scope
- Density benchmarking (10,000+ concurrent sandboxes) -- separate PR
- Cross-architecture benchmarks (arm64, riscv)
- Running on competitor platforms -- separate effort
```

### Exit Criteria
- Security scan passes (no critical findings)
- PR is open on GitHub targeting `main`

### Rollback
```bash
gh pr close claude/third-party-benchmarks --delete-branch
git checkout main
git branch -D claude/third-party-benchmarks
```

---

## Execution Summary

| Step | Description | Depends On | Parallel With | Model |
|------|-------------|------------|---------------|-------|
| 0 | Load reference skills | -- | -- | default |
| 1 | Branch setup & pre-flight | 0 | -- | default |
| 2 | Fix cargo config | 1 | 6 | default |
| 3 | Bench profile + codspeed dep | 2 | 6 | default |
| 4 | Dual-mode criterion harness | 3 | 6, 8 | default |
| 5 | GitHub Actions workflow | 4 | 6, 7 | strongest |
| 6 | Public comparison dashboard | 1 | 2-5, 7, 8 | strongest |
| 7 | Correct marketing claims | 4 | 5, 6 | strongest |
| 8 | .mcp.json + SETUP.md | 3 | 4-7 | default |
| 9 | Validation checklist | 2-8 | -- | default |
| 10 | Security scan + PR | 9 | -- | default |

**Total steps:** 11 (0-10)
**Critical path:** 0 -> 1 -> 2 -> 3 -> 4 -> 5 -> 9 -> 10 (8 serial)
**Max parallelism:** 5 steps (5, 6, 7, 8, and 6 starts early alongside 2-4)
**Estimated total effort:** ~2-3 hours of agent execution time

## Invariants (verify after EVERY step)

1. `cargo bench --bench nexus_validation --no-run` succeeds
2. No `unused key` warnings from cargo
3. Working tree is on branch `claude/third-party-benchmarks`

## Out of Scope (parking lot)

- Density benchmarking ("10,000+ concurrent sandboxes") -- separate PR, separate harness
- Cross-architecture benchmarks (arm64, riscv)
- Running on competitor platforms (E2B/Modal) for apples-to-apples -- separate effort
- ACM artifact submission -- months-long academic process
- Full ECC plugin suite installation -- separate strategic decision
