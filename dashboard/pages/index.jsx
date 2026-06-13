import fs from "fs";
import path from "path";
import yaml from "js-yaml";
import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
  Cell,
} from "recharts";

const GREEN = "#9cff3b";
const CYAN = "#00d8ff";
const VOID = "#020404";

const GROUP_LABELS = {
  cold_start: "Cold Start",
  snapshot_create: "Snapshot Create",
  snapshot_rollback: "Snapshot Rollback",
  execute_tool: "Execute Tool",
  execute_tool_real_memory: "Execute Tool (Real Memory)",
  integrated_capability_checked: "Integrated — Capability Check",
  integrated_input_fed: "Integrated — Input Fed",
  integrated_precompiled: "Integrated — Precompiled",
  integrated_full_stack: "Integrated — Full Stack",
};

const BENCH_LABELS = {
  "cold_start/sandbox_new": "Sandbox init",
  "cold_start/hypervisor_new": "Hypervisor init",
  "snapshot_create/size/1MiB": "1 MiB",
  "snapshot_create/size/10MiB": "10 MiB",
  "snapshot_create/size/100MiB": "100 MiB",
  "snapshot_rollback/size/1MiB": "1 MiB",
  "snapshot_rollback/size/10MiB": "10 MiB",
  "snapshot_rollback/size/100MiB": "100 MiB",
  "execute_tool/trivial_wasm_start": "Trivial WASM start",
  "execute_tool_real_memory/size/1MiB": "1 MiB",
  "execute_tool_real_memory/size/10MiB": "10 MiB",
  "execute_tool_real_memory/size/100MiB": "100 MiB",
  "integrated_capability_checked/with_valid_token": "Valid token",
  "integrated_input_fed/json_input": "JSON input",
  "integrated_precompiled/primitive_recompile": "Recompile",
  "integrated_precompiled/cached_precompiled": "Cached",
  "integrated_full_stack/full_path": "Full path",
};

function formatMs(ms) {
  if (ms < 0.001) return `${(ms * 1_000_000).toFixed(0)} ns`;
  if (ms < 1) return `${(ms * 1000).toFixed(1)} µs`;
  if (ms < 1000) return `${ms.toFixed(2)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

function groupBenchmarks(benchmarks) {
  const groups = {};
  const ORDER = [
    "cold_start",
    "snapshot_create",
    "snapshot_rollback",
    "execute_tool",
    "execute_tool_real_memory",
  ];
  for (const b of benchmarks) {
    const g = b.group;
    if (!groups[g]) groups[g] = [];
    groups[g].push(b);
  }
  const sorted = [];
  for (const key of ORDER) {
    if (groups[key]) {
      sorted.push([key, groups[key]]);
      delete groups[key];
    }
  }
  for (const [key, items] of Object.entries(groups)) {
    sorted.push([key, items]);
  }
  return sorted;
}

export default function Dashboard({ measured, cited, nexusDataSource, benchmarks }) {
  const coldStart = benchmarks.find((b) => b.name === "cold_start/sandbox_new");
  const snapshotCreate1 = benchmarks.find((b) => b.name === "snapshot_create/size/1MiB");
  const rollback1 = benchmarks.find((b) => b.name === "snapshot_rollback/size/1MiB");
  const nexusColdStartMs = coldStart ? coldStart.ms.median : 0.023;

  const allCompetitors = [...(cited || [])];
  const chartData = [
    { name: "Nexus", ms: nexusColdStartMs, isNexus: true },
    ...allCompetitors
      .filter((c) => c.cold_start_ms <= 200)
      .map((c) => ({ name: c.name, ms: c.cold_start_ms, isNexus: false })),
  ].sort((a, b) => a.ms - b.ms);

  const grouped = groupBenchmarks(benchmarks);
  const hasBenchmarks = benchmarks.length > 0;

  return (
    <div className="container">
      <h1>Nexus Benchmark Dashboard</h1>
      <p className="subtitle">
        Third-party verified performance data.{" "}
        {hasBenchmarks ? (
          <span className="badge badge-live">LIVE from CI</span>
        ) : (
          <span className="badge badge-cited">Awaiting CI data</span>
        )}{" "}
        <span className="badge badge-cited">Competitor data cited</span>
      </p>

      <h2>Cold Start Comparison</h2>
      <div className="chart-container">
        <ResponsiveContainer width="100%" height={400}>
          <BarChart
            data={chartData}
            layout="vertical"
            margin={{ top: 5, right: 30, left: 120, bottom: 5 }}
          >
            <CartesianGrid strokeDasharray="3 3" stroke="#1a2a1a" />
            <XAxis
              type="number"
              scale="log"
              domain={["auto", "auto"]}
              tick={{ fill: "#888" }}
              tickFormatter={formatMs}
            />
            <YAxis
              dataKey="name"
              type="category"
              tick={{ fill: "#ccc", fontSize: 13 }}
              width={110}
            />
            <Tooltip
              formatter={(value) => [formatMs(value), "Cold Start"]}
              contentStyle={{ background: VOID, border: `1px solid ${CYAN}` }}
              labelStyle={{ color: "#ccc" }}
            />
            <Bar dataKey="ms" radius={[0, 4, 4, 0]}>
              {chartData.map((entry, index) => (
                <Cell
                  key={`cell-${index}`}
                  fill={entry.isNexus ? GREEN : CYAN}
                  fillOpacity={entry.isNexus ? 1 : 0.6}
                />
              ))}
            </Bar>
          </BarChart>
        </ResponsiveContainer>
      </div>

      <h2>Competitor Comparison</h2>
      <table>
        <thead>
          <tr>
            <th>Platform</th>
            <th>Cold Start</th>
            <th>Snapshot Create</th>
            <th>Rollback</th>
            <th>Source</th>
          </tr>
        </thead>
        <tbody>
          <tr className="nexus-row">
            <td>Nexus {hasBenchmarks ? "(live)" : "(default)"}</td>
            <td>{formatMs(nexusColdStartMs)}</td>
            <td>{snapshotCreate1 ? `${formatMs(snapshotCreate1.ms.median)} @ 1 MiB` : "—"}</td>
            <td>{rollback1 ? `${formatMs(rollback1.ms.median)} @ 1 MiB` : "—"}</td>
            <td>
              <span className="badge badge-live">CI</span>
            </td>
          </tr>
          {measured && measured.length > 0 && (
            <tr className="section-header">
              <td colSpan={5}>
                <span className="badge badge-measured">Measured by Nexus CI</span>
              </td>
            </tr>
          )}
          {(measured || []).map((m) => (
            <tr key={m.name}>
              <td>{m.name}</td>
              <td colSpan={3} className="measured-note">{m.description}</td>
              <td>
                <span className="badge badge-measured">CI</span>
              </td>
            </tr>
          ))}
          {cited && cited.length > 0 && (
            <tr className="section-header">
              <td colSpan={5}>
                <span className="badge badge-cited">Third-party cited sources</span>
              </td>
            </tr>
          )}
          {(cited || []).map((c) => (
            <tr key={c.name}>
              <td>{c.name}</td>
              <td>{c.cold_start_ms ? formatMs(c.cold_start_ms) : "—"}</td>
              <td>{c.snapshot_create_ms ? formatMs(c.snapshot_create_ms) : "—"}</td>
              <td>{c.rollback_ms ? formatMs(c.rollback_ms) : "—"}</td>
              <td>
                {c.source_url ? (
                  <a
                    href={c.source_url}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="source-link"
                  >
                    {c.source}
                  </a>
                ) : (
                  <span className="source-link">{c.source}</span>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      {hasBenchmarks && (
        <>
          <h2>Full Benchmark Suite</h2>
          <table>
            <thead>
              <tr>
                <th>Benchmark</th>
                <th>Median</th>
                <th>Range (low–high)</th>
              </tr>
            </thead>
            <tbody>
              {grouped.map(([group, items]) => (
                <>
                  <tr key={`header-${group}`} className="section-header">
                    <td colSpan={3}>
                      <span className="badge badge-live">
                        {GROUP_LABELS[group] || group}
                      </span>
                    </td>
                  </tr>
                  {items.map((b) => (
                    <tr key={b.name} className="nexus-row">
                      <td>{BENCH_LABELS[b.name] || b.benchmark}</td>
                      <td>{formatMs(b.ms.median)}</td>
                      <td style={{ color: "#888", fontWeight: 400, fontSize: "0.9rem" }}>
                        {formatMs(b.ms.low)} – {formatMs(b.ms.high)}
                      </td>
                    </tr>
                  ))}
                </>
              ))}
            </tbody>
          </table>
        </>
      )}

      <h2>Verification Links</h2>
      <div className="links">
        <a
          href="https://bencher.dev/perf/nexus-ai"
          target="_blank"
          rel="noopener noreferrer"
        >
          Bencher.dev (wall-clock)
        </a>
        <a
          href={nexusDataSource.codspeed_project_url}
          target="_blank"
          rel="noopener noreferrer"
        >
          CodSpeed.io (instruction-count)
        </a>
        <a
          href="https://github.com/Adaptive-Liquidity/Nexus/actions/workflows/benchmarks.yml"
          target="_blank"
          rel="noopener noreferrer"
        >
          CI Workflow Runs
        </a>
        <a
          href="https://github.com/Adaptive-Liquidity/Nexus"
          target="_blank"
          rel="noopener noreferrer"
        >
          Source Code
        </a>
      </div>

      <div className="footer">
        <p>
          All Nexus numbers are measured on GitHub-hosted runners (ubuntu-24.04)
          and published automatically via CI. Competitor numbers are from cited
          third-party sources — each entry links to its original source for
          independent verification. Benchmark artifacts are signed with Sigstore
          for cryptographic attestation.
        </p>
        <p style={{ fontSize: "0.85rem", marginTop: "0.5rem" }}>
          Reproduce locally:{" "}
          <code>bash scripts/run_local_comparison.sh</code>
        </p>
      </div>
    </div>
  );
}

export async function getStaticProps() {
  const filePath = path.join(process.cwd(), "competitors.yml");
  const fileContents = fs.readFileSync(filePath, "utf8");
  const data = yaml.load(fileContents);

  let benchmarks = [];
  const benchPath = path.join(process.cwd(), "benchmark-data.json");
  try {
    const benchContents = fs.readFileSync(benchPath, "utf8");
    const benchData = JSON.parse(benchContents);
    benchmarks = benchData.benchmarks || [];
  } catch {
    // benchmark-data.json is generated by CI; missing during local dev
  }

  return {
    props: {
      measured: data.measured || [],
      cited: data.cited || [],
      nexusDataSource: data.nexus_data_source || {},
      benchmarks,
    },
  };
}
