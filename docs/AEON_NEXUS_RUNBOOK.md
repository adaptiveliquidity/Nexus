# AEON-IQ x Nexus Runbook

**Status:** Phase 10 release hardening
**Feature gate:** `aeon-memory` is off by default.

This runbook describes the service-boundary integration. Nexus executes WASM and emits proof/timeline evidence. AEON-IQ serves memory recall, stores timeline rows in Postgres, and exposes time-travel lookup endpoints.

## Build Nexus With AEON-IQ Memory

```bash
cargo build --features aeon-memory
cargo test --features aeon-memory
```

The default build intentionally omits AEON-IQ code paths. Use `--features aeon-memory` for the Nexus library, `nexus-agentd`, `nexus-mcp`, tests, and the E2E demo.

## Nexus Environment

`src/aeon.rs` owns the `NEXUS_AEON_*` configuration contract:

| Variable | Required? | Purpose |
| --- | --- | --- |
| `NEXUS_AEON_ENABLED` | Required to enable memory calls | Boolean: `true/false`, `yes/no`, `on/off`, or `1/0`. Default is disabled. |
| `NEXUS_AEON_BASE_URL` | Required when enabled | Base URL for the AEON-IQ proxy. Default is `http://localhost:8080`. |
| `NEXUS_AEON_AGENT_ID` | Required when enabled | AEON-IQ memory tenant agent id. Default is `nexus`. |
| `NEXUS_AEON_SESSION_ID` | Optional | Session id used for proof/timeline correlation. |
| `NEXUS_AEON_TIMEOUT_MS` | Optional | AEON-IQ HTTP timeout in milliseconds. Default is `30000`. |
| `NEXUS_AEON_MANAGEMENT_KEY` | Required for memory API calls | Management API key sent to AEON-IQ as `X-Management-Key`. Missing key disables memory API calls. |
| `NEXUS_AEON_HMAC_KEY` | Required for attested proof evidence | Hex-encoded HMAC key. Missing or empty key records `Absent` attestation mode. |

Example local shell setup:

```bash
export NEXUS_AEON_ENABLED=true
export NEXUS_AEON_BASE_URL=http://127.0.0.1:8080
export NEXUS_AEON_AGENT_ID=nexus-local
export NEXUS_AEON_SESSION_ID=phase10-smoke
export NEXUS_AEON_TIMEOUT_MS=30000
export NEXUS_AEON_MANAGEMENT_KEY="$AEON_MANAGEMENT_API_KEY"
export NEXUS_AEON_HMAC_KEY="$AEON_NEXUS_HMAC_KEY_HEX"
```

## AEON-IQ Side

AEON-IQ must be configured so its `MANAGEMENT_API_KEY` matches Nexus `NEXUS_AEON_MANAGEMENT_KEY`. Nexus uses the management key when calling memory search/store endpoints in `src/aeon.rs`.

The timeline integration expects AEON-IQ to expose the `cognitive_hypervisor_timeline` API backed by AEON-IQ migration `0023`:

| Endpoint | Purpose |
| --- | --- |
| `POST /agents/:id/timeline` | Persist Nexus execution events such as capability denial, snapshot creation, and proof capsule emission. |
| `GET /agents/:id/timeline/at` | Resolve a point-in-time timeline view to the relevant memory/proof/snapshot identifiers. |

Nexus does not write AEON-IQ Postgres directly. A caller or AEON-IQ bridge service must forward the events returned by Nexus to `POST /agents/:id/timeline`.

## MCP Timeline Tool

With `--features aeon-memory`, `src/bin/nexus_mcp.rs` exposes:

```text
nexus_aeon_execute_timeline
```

The tool executes a WASM module, applies AEON-aware capability denial negotiation when configured, returns a proof capsule, and surfaces timeline events for forwarding to `POST /agents/:id/timeline`. The response includes `aeon_timeline_path` when `aeon_agent_id` is provided.

The tool is absent from default builds.

## Minimal End-To-End Walkthrough

1. Provision shared keys:

   ```bash
   export AEON_NEXUS_HMAC_KEY_HEX="$(openssl rand -hex 32)"
   export AEON_MANAGEMENT_API_KEY="$(openssl rand -hex 32)"
   export NEXUS_AEON_MANAGEMENT_KEY="$AEON_MANAGEMENT_API_KEY"
   export NEXUS_AEON_HMAC_KEY="$AEON_NEXUS_HMAC_KEY_HEX"
   ```

2. Start AEON-IQ with the matching `MANAGEMENT_API_KEY` and the same HMAC key under its deployment-specific variable name.

3. Run the Nexus demo:

   ```bash
   cargo run --example aeon_e2e_demo --features aeon-memory
   ```

   `examples/aeon_e2e_demo.rs` starts a mock AEON-IQ memory endpoint, executes a WASM module with AEON context, verifies an attested proof capsule, and prints the timeline path and event JSON. Use it as a local shape check before pointing at a real AEON-IQ deployment.

4. For MCP, run the server from a feature-enabled build:

   ```bash
   cargo run --bin nexus-mcp --features aeon-memory
   ```

   Invoke `nexus_aeon_execute_timeline` with `wasm_path`, optional `entry`, optional `input`, optional `capabilities`, and AEON correlation fields `aeon_agent_id` and `aeon_session_id`. Forward the returned `events` array to AEON-IQ `POST /agents/:id/timeline`.

## Performance Validation

The audit targets from section 9 are targets, not claimed local measurements:

| Target | Scope |
| --- | --- |
| Boot `< 2 ms` | Nexus startup/init path for the relevant benchmark group. |
| Rollback `< 1 ms` | Snapshot rollback at the benchmarked state size. |
| Combined roundtrip `< 12 ms` | Nexus execution plus AEON-IQ integration path under the benchmark harness. |

Do not copy these thresholds into release notes as measured results unless the CI benchmark artifacts support them. Live numbers are tracked by the Criterion benchmark jobs in `.github/workflows/benchmarks.yml`, uploaded as CI artifacts, and published through Bencher.dev and CodSpeed when those services are configured. The dashboard reflects the latest successful benchmark workflow publish, not necessarily the current docs-only commit; benchmark PR runs intentionally ignore docs and Markdown changes. The benchmark definitions live in `benches/`, with methodology summarized in `BENCHMARKS.md`.

Concurrent sandbox density remains an opt-in/manual `bench-density` harness, not a default release metric.

For local investigation only, run:

```bash
cargo bench --bench nexus_validation
```

Local numbers are machine-specific and should be labeled with hardware, OS, rustc, commit SHA, feature flags, and whether AEON-IQ was a mock or a real deployment.
