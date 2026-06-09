# Benchmark Pipeline Setup

One-time setup steps for the third-party verifiable benchmark pipeline. All services are free for public OSS repositories.

## 1. Bencher.dev

1. Go to [bencher.dev](https://bencher.dev) and sign in with the `Adaptive-Liquidity` GitHub org
2. Create a new project with slug `nexus-ai`
3. Generate an API token
4. In GitHub repo Settings > Secrets and variables > Actions:
   - Add secret: `BENCHER_API_TOKEN` = the generated token
   - Add variable: `BENCHER_PROJECT` = `nexus-ai`

## 2. CodSpeed.io

1. Go to [codspeed.io](https://codspeed.io) and sign in with GitHub
2. Install the CodSpeed GitHub App on `Adaptive-Liquidity/Nexus`
3. Copy the project token
4. In GitHub repo Settings > Secrets and variables > Actions:
   - Add secret: `CODSPEED_TOKEN` = the project token

## 3. GitHub Pages

1. Go to repo Settings > Pages
2. Set Source = **GitHub Actions**

## 4. CodSpeed MCP Server (optional)

For post-merge regression analysis directly from Claude Code:

1. Set the `CODSPEED_TOKEN` environment variable in your shell
2. The `.mcp.json` file in the repo root configures the MCP server connection
3. Once configured, you can use `list_runs`, `compare_runs`, and `query_flamegraph` as Claude Code tools

## 5. Sigstore

No setup required. The workflow uses GitHub's OIDC token for keyless signing automatically.

## Verification

After completing steps 1-3, trigger the workflow manually:

```bash
gh workflow run benchmarks.yml
```

Check the Actions tab for successful completion of all three jobs (Bencher, CodSpeed, Dashboard).
