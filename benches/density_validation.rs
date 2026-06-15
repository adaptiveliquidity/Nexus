//! Concurrent sandbox density benchmark — pooled vs non-pooled.
//!
//! This is a **custom-harness** benchmark (no Criterion). It spawns N
//! concurrent `execute_tool` calls against both the default per-call sandbox
//! and the opt-in warm pool, then reports per-call latency percentiles
//! (p50/p95/p99), total wall-clock, throughput, and peak process RSS.
//!
//! It is deliberately kept out of the default `cargo bench` / PR CI surface
//! (gated behind the `bench-density` feature) because spawning hundreds to
//! thousands of concurrent executions is heavy and noisy. Run it manually or
//! on a nightly schedule:
//!
//! ```bash
//! cargo bench --bench density_validation --features bench-density
//! ```
//!
//! Results are printed as a table and written to
//! `artifacts/density-benchmark.json` for downstream tracking.
//!
//! NOTE: This harness only *measures*. It makes no claims about pooled vs
//! non-pooled superiority — interpret the emitted numbers directly.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nexus::{HypervisorConfig, NexusHypervisor, PoolConfig, ToolDefinition};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// Default density sweep. Conservative ceiling (1000) — scale only after the
/// pooling allocator's virtual-memory behavior is confirmed acceptable.
const SIZES: &[usize] = &[10, 100, 500, 1000];

/// A minimal module: exports memory + a no-op `_start`. Keeps per-call work
/// near-zero so the measurement isolates sandbox setup/teardown overhead.
fn trivial_wasm() -> Vec<u8> {
    wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#).unwrap()
}

#[derive(Debug)]
struct Scenario {
    backend: &'static str,
    n: usize,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    total_ms: f64,
    throughput_ops_s: f64,
    peak_rss_mb: f64,
    rss_per_sandbox_kb: f64,
    successes: usize,
    failures: usize,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p / 100.0) * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = rank - lo as f64;
        sorted[lo] * (1.0 - frac) + sorted[hi] * frac
    }
}

fn current_rss_bytes(sys: &mut System, pid: Pid) -> u64 {
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::new().with_memory(),
    );
    sys.process(pid).map(|p| p.memory()).unwrap_or(0)
}

/// Run N concurrent executions; return per-call latencies (ms) plus
/// success/failure counts.
async fn run_scenario(
    hv: Arc<NexusHypervisor>,
    wasm: Arc<Vec<u8>>,
    n: usize,
) -> (Vec<f64>, usize, usize) {
    let mut handles = Vec::with_capacity(n);
    for _ in 0..n {
        let hv = hv.clone();
        let wasm = wasm.clone();
        handles.push(tokio::spawn(async move {
            let tool = ToolDefinition::new("density".into(), (*wasm).clone());
            let start = Instant::now();
            let out = hv.execute_tool(tool, serde_json::json!({})).await;
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            (elapsed_ms, out.map(|o| o.success).unwrap_or(false))
        }));
    }

    let mut latencies = Vec::with_capacity(n);
    let mut successes = 0;
    let mut failures = 0;
    for h in handles {
        match h.await {
            Ok((ms, true)) => {
                latencies.push(ms);
                successes += 1;
            }
            Ok((ms, false)) => {
                latencies.push(ms);
                failures += 1;
            }
            Err(_) => failures += 1,
        }
    }
    (latencies, successes, failures)
}

fn build_hypervisor(pooled: bool, n: usize) -> NexusHypervisor {
    let mut config = HypervisorConfig::default();
    if pooled {
        // Size the pool to admit the full concurrency level for this run.
        let total = (n as u32).max(16);
        config.pool_config = Some(PoolConfig {
            max_concurrency: n.max(1),
            total_instances: total,
            ..Default::default()
        });
    }
    NexusHypervisor::new(config).expect("hypervisor construction")
}

fn measure(
    rt: &tokio::runtime::Runtime,
    backend: &'static str,
    pooled: bool,
    n: usize,
    wasm: &Arc<Vec<u8>>,
) -> Scenario {
    let hv = Arc::new(build_hypervisor(pooled, n));

    // Background RSS sampler: poll every 5ms, keep the max observed.
    let pid = sysinfo::get_current_pid().expect("current pid");
    let peak_rss = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = {
        let peak_rss = peak_rss.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut sys = System::new();
            while !stop.load(Ordering::Relaxed) {
                let rss = current_rss_bytes(&mut sys, pid);
                peak_rss.fetch_max(rss, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(5));
            }
            // Final sample after the run.
            let rss = current_rss_bytes(&mut sys, pid);
            peak_rss.fetch_max(rss, Ordering::Relaxed);
        })
    };

    let start = Instant::now();
    let (mut latencies, successes, failures) = rt.block_on(run_scenario(hv, wasm.clone(), n));
    let total = start.elapsed();

    stop.store(true, Ordering::Relaxed);
    let _ = sampler.join();

    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let total_ms = total.as_secs_f64() * 1000.0;
    let throughput = if total_ms > 0.0 {
        successes as f64 / (total_ms / 1000.0)
    } else {
        0.0
    };
    let peak_rss_bytes = peak_rss.load(Ordering::Relaxed);
    let peak_rss_mb = peak_rss_bytes as f64 / (1024.0 * 1024.0);
    let rss_per_sandbox_kb = if n > 0 {
        (peak_rss_bytes as f64 / 1024.0) / n as f64
    } else {
        0.0
    };

    Scenario {
        backend,
        n,
        p50_ms: percentile(&latencies, 50.0),
        p95_ms: percentile(&latencies, 95.0),
        p99_ms: percentile(&latencies, 99.0),
        total_ms,
        throughput_ops_s: throughput,
        peak_rss_mb,
        rss_per_sandbox_kb,
        successes,
        failures,
    }
}

fn main() {
    let sizes: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse::<usize>().ok())
        .collect();
    let sizes: &[usize] = if sizes.is_empty() { SIZES } else { &sizes };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let wasm = Arc::new(trivial_wasm());
    let mut results = Vec::new();

    for &n in sizes {
        // Non-pooled first, then pooled, so the comparison is apples-to-apples
        // for each density level.
        results.push(measure(&rt, "non_pooled", false, n, &wasm));
        results.push(measure(&rt, "pooled", true, n, &wasm));
    }

    // Print a human-readable table.
    println!();
    println!(
        "{:<12} {:>6} {:>9} {:>9} {:>9} {:>10} {:>12} {:>11} {:>9} {:>5}/{:<5}",
        "backend",
        "N",
        "p50(ms)",
        "p95(ms)",
        "p99(ms)",
        "total(ms)",
        "ops/s",
        "peakRSS(MB)",
        "RSS/sb(KB)",
        "ok",
        "fail"
    );
    for s in &results {
        println!(
            "{:<12} {:>6} {:>9.3} {:>9.3} {:>9.3} {:>10.2} {:>12.1} {:>11.1} {:>9.1} {:>5}/{:<5}",
            s.backend,
            s.n,
            s.p50_ms,
            s.p95_ms,
            s.p99_ms,
            s.total_ms,
            s.throughput_ops_s,
            s.peak_rss_mb,
            s.rss_per_sandbox_kb,
            s.successes,
            s.failures
        );
    }
    println!();

    // Write JSON sidecar artifact.
    let json: Vec<serde_json::Value> = results
        .iter()
        .map(|s| {
            serde_json::json!({
                "backend": s.backend,
                "n": s.n,
                "p50_ms": s.p50_ms,
                "p95_ms": s.p95_ms,
                "p99_ms": s.p99_ms,
                "total_ms": s.total_ms,
                "throughput_ops_s": s.throughput_ops_s,
                "peak_rss_mb": s.peak_rss_mb,
                "rss_per_sandbox_kb": s.rss_per_sandbox_kb,
                "successes": s.successes,
                "failures": s.failures,
            })
        })
        .collect();

    let out = serde_json::json!({
        "benchmark": "sandbox_density",
        "note": "Custom-harness density measurement. No pooled-vs-nonpooled claim is implied; read the numbers directly.",
        "scenarios": json,
    });

    let _ = std::fs::create_dir_all("artifacts");
    match std::fs::write(
        "artifacts/density-benchmark.json",
        serde_json::to_string_pretty(&out).unwrap(),
    ) {
        Ok(_) => println!("Wrote artifacts/density-benchmark.json"),
        Err(e) => eprintln!("Failed to write density JSON: {e}"),
    }
}
