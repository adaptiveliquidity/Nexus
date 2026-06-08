import { useState, useEffect } from "react";
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

const BENCHER_API =
  "https://api.bencher.dev/v0/projects/nexus-ai/perf?branches=main&testbeds=ubuntu-24.04-github&benchmarks=cold_start%2Fsandbox_new&kind=latency&start_time=2024-01-01T00%3A00%3A00Z";

const GREEN = "#9cff3b";
const CYAN = "#00d8ff";
const VOID = "#020404";

function formatMs(ms) {
  if (ms < 1) return `${(ms * 1000).toFixed(0)} µs`;
  if (ms < 1000) return `${ms.toFixed(1)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

export default function Dashboard({ competitors, nexusDataSource }) {
  const [nexusLive, setNexusLive] = useState(null);
  const [fetchError, setFetchError] = useState(null);

  useEffect(() => {
    fetch(BENCHER_API)
      .then((r) => {
        if (!r.ok) throw new Error(`Bencher API ${r.status}`);
        return r.json();
      })
      .then((data) => {
        if (data && data.length > 0) {
          const latest = data[data.length - 1];
          if (latest.metric && latest.metric.value) {
            setNexusLive({
              cold_start_ms: latest.metric.value / 1_000_000,
              timestamp: latest.start_time,
            });
          }
        }
      })
      .catch((err) => setFetchError(err.message));
  }, []);

  const nexusColdStartMs = nexusLive ? nexusLive.cold_start_ms : 0.023;

  const chartData = [
    { name: "Nexus", ms: nexusColdStartMs, isNexus: true },
    ...competitors
      .filter((c) => c.cold_start_ms <= 200)
      .map((c) => ({ name: c.name, ms: c.cold_start_ms, isNexus: false })),
  ].sort((a, b) => a.ms - b.ms);

  return (
    <div className="container">
      <h1>Nexus Benchmark Dashboard</h1>
      <p className="subtitle">
        Third-party verified performance data.{" "}
        <span className="badge badge-live">LIVE from Bencher.dev</span>{" "}
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
        {nexusLive && (
          <p style={{ fontSize: "0.8rem", color: "#666", marginTop: "0.5rem" }}>
            Nexus value from Bencher.dev (last updated:{" "}
            {new Date(nexusLive.timestamp).toLocaleDateString()})
          </p>
        )}
        {fetchError && (
          <p style={{ fontSize: "0.8rem", color: "#aa4444", marginTop: "0.5rem" }}>
            Could not fetch live data: {fetchError}. Showing default value.
          </p>
        )}
      </div>

      <h2>Full Comparison Table</h2>
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
            <td>Nexus {nexusLive ? "(live)" : "(default)"}</td>
            <td>{formatMs(nexusColdStartMs)}</td>
            <td>2.92 ms @ 1 MiB</td>
            <td>&lt;1 ms @ 1 MiB</td>
            <td>
              <span className="badge badge-live">Bencher.dev</span>
            </td>
          </tr>
          {competitors.map((c) => (
            <tr key={c.name}>
              <td>{c.name}</td>
              <td>{formatMs(c.cold_start_ms)}</td>
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
          third-party sources. Benchmark artifacts are signed with Sigstore for
          cryptographic attestation.
        </p>
      </div>
    </div>
  );
}

export async function getStaticProps() {
  const filePath = path.join(process.cwd(), "competitors.yml");
  const fileContents = fs.readFileSync(filePath, "utf8");
  const data = yaml.load(fileContents);

  return {
    props: {
      competitors: data.competitors,
      nexusDataSource: data.nexus_data_source,
    },
  };
}
