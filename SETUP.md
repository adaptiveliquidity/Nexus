# Benchmark Pipeline Setup

One-time setup steps for the third-party verifiable benchmark pipeline. All services are free for public OSS repositories.

## 1. Bencher.dev

1. Go to [bencher.dev](https://bencher.dev) and sign in with the `adaptiveliquidity` GitHub org
2. Create a new project with slug `nexus-ai`
3. Generate an API token
4. In GitHub repo Settings > Secrets and variables > Actions:
   - Add secret: `BENCHER_API_TOKEN` = the generated token
   - Add variable: `BENCHER_PROJECT` = `nexus-ai`

### What's configured

- **Measures**: latency (ns, upper boundary) + throughput (bytes/s, lower boundary)
- **Threshold**: Student's t-test at 99th percentile, 2-64 sample rolling window
- **Testbed**: `ubuntu-24.04-github` (GitHub-hosted runner)
- **PR gating**: PRs compare against base branch; CI fails on statistical regression (`--error-on-alert`)
- **Branch management**: PR branches clone thresholds from their start-point (main)
- **Perf plots**: Auto-generated at [bencher.dev/perf/nexus-ai](https://bencher.dev/perf/nexus-ai)

## 2. CodSpeed.io

1. Go to [codspeed.io](https://codspeed.io) and sign in with GitHub
2. Install the CodSpeed GitHub App on `adaptiveliquidity/Nexus`
3. Authentication is handled via OIDC (no token needed for public repos)
   - Legacy: if OIDC fails, add secret `CODSPEED_TOKEN` from the CodSpeed dashboard

### What's configured

- **CPU simulation** (cachegrind): deterministic instruction-count benchmarks, runs on every push and PR
- **Memory instrument**: heap allocation tracking alongside CPU simulation
- **Walltime** (bare-metal): real elapsed time on CodSpeed macro runners (16-core ARM64, 32 GB RAM)
  - Disabled by default. Enable via GitHub variable: `CODSPEED_WALLTIME_ENABLED` = `true`
  - 600 free minutes/month; contact support@codspeed.io for open-source uplift

### Recommended CodSpeed dashboard settings

Configure these in the CodSpeed web UI:

1. **Regression threshold**: Lower from default 10% to **5%** for tighter regression detection
2. **Performance checks**: Enable "CodSpeed Performance Analysis" as a required status check
   - Repo Settings > Branches > Branch protection > Require status checks > add "CodSpeed Performance Analysis"
3. **PR comments**: Set to **On Change** (only comment when there's a meaningful delta)

## 3. GitHub Pages

1. Go to repo Settings > Pages
2. Set Source = **GitHub Actions**

## 4. CodSpeed MCP Server (optional)

For post-merge regression analysis directly from Claude Code:

1. The `.mcp.json` file in the repo root configures the MCP server connection
2. Once configured, you can use `list_runs`, `compare_runs`, and `query_flamegraph` as Claude Code tools

## 5. Sigstore

No setup required. The workflow uses GitHub's OIDC token for keyless signing automatically.

## 6. Variables reference

| Name | Type | Value | Purpose |
|------|------|-------|---------|
| `BENCHER_PROJECT` | Variable | `nexus-ai` | Bencher project slug |
| `BENCHER_API_TOKEN` | Secret | (from bencher.dev) | Bencher API auth |
| `CODSPEED_TOKEN` | Secret | (optional, OIDC preferred) | CodSpeed fallback auth |
| `CODSPEED_ENABLED` | Variable | `true` (default) | Set to `false` to skip CodSpeed |
| `CODSPEED_WALLTIME_ENABLED` | Variable | `false` (default) | Set to `true` for bare-metal walltime |

## Verification

After completing steps 1-3, trigger the workflow manually:

```bash
gh workflow run benchmarks.yml
```

Check the Actions tab for successful completion of all jobs:
- **Criterion -> Bencher.dev**: wall-clock benchmarks + Bencher upload (push: baseline, PR: regression check)
- **Criterion -> CodSpeed.io (CPU simulation + memory)**: deterministic instruction counts + heap tracking
- **Criterion -> CodSpeed.io (bare-metal walltime)**: real elapsed time (only when `CODSPEED_WALLTIME_ENABLED=true`)
- **Update benchmark SVG**: auto-commits chart to `docs/benchmark-chart.svg` (push to main only)
- **Publish dashboard**: deploys Next.js dashboard to GitHub Pages (push to main only)
