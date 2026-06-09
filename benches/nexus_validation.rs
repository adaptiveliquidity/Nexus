//! Nexus Validation Protocol — Phase 1 Criterion Benchmarks
//!
//! Measures the *real* Nexus APIs under statistical rigor:
//!   - cold_start::sandbox_new          -> WasmSandbox::new
//!   - cold_start::hypervisor_new       -> NexusHypervisor::new
//!   - snapshot::create/{1,10,100}MB    -> SnapshotManager::create_snapshot
//!     (pseudo-random memory so zstd cannot "cheat" by compressing zeros)
//!   - snapshot::rollback/{1,10,100}MB  -> SnapshotManager::rollback_to
//!   - execute_tool::trivial_wasm       -> NexusHypervisor::execute_tool end-to-end
//!
//! All inputs are deterministic. Memory is filled with a linear-congruential
//! pseudo-random stream so compression ratios are realistic (~1.0), not the
//! near-zero ratio that all-zero buffers would produce.
//!
//! ## Bench surface modes
//!
//! Wall-clock mode (default `cargo bench`) runs the full {1, 10, 100} MiB
//! surface — these absolute numbers go to Bencher.dev for regression tracking.
//!
//! CodSpeed mode (`cargo codspeed run`) runs each bench under Valgrind/
//! cachegrind for deterministic instruction counts. Cachegrind adds 20-100x
//! wall-clock overhead, so the GitHub workflow shards CodSpeed across benchmark
//! feature flags. Native Criterion/Bencher runs leave those features disabled
//! and execute the complete surface; CodSpeed runs compile one selected shard at
//! a time. For size-parameterized CodSpeed shards, we still restrict the sweep
//! to the smallest size so each shard stays under timeout. Absolute throughput
//! is still measured at full surface by the wall-clock Bencher job.

use std::time::Duration;

#[cfg(codspeed)]
use codspeed_criterion_compat::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
};
#[cfg(not(codspeed))]
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use nexus::hypervisor::{HypervisorConfig, NexusHypervisor, ToolDefinition};
use nexus::sandbox::{SandboxConfig, WasmSandbox};
use nexus::snapshot::{
    ExecutionState, FilesystemDiff, Snapshot, SnapshotManager, SnapshotMetadata,
};

/// Snapshot buffer sizes as `(bytes, label)`.
///
/// CodSpeed mode uses 64 KiB — `codspeed-criterion-compat` replaces the
/// Criterion measurement backend entirely (ignoring `sample_size`,
/// `warm_up_time`, `measurement_time`), so the *only* lever we have is
/// data size.  1 MiB of pseudo-random data through zstd + SHA-256 under
/// cachegrind generates billions of tracked instructions and blows past
/// the 60-min GitHub Actions timeout.  64 KiB is large enough for the
/// algorithm's instruction profile to be representative while keeping
/// each shard well under 30 min.
///
/// Wall-clock mode runs the full {1, 10, 100} MiB sweep for Bencher's
/// absolute throughput tracking.
#[cfg(codspeed)]
const SNAPSHOT_SIZES: &[(usize, &str)] = &[(65_536, "64KiB")];
#[cfg(not(codspeed))]
const SNAPSHOT_SIZES: &[(usize, &str)] = &[
    (1_048_576, "1MiB"),
    (10_485_760, "10MiB"),
    (104_857_600, "100MiB"),
];

/// End-to-end execute_tool real-memory sizes as `(bytes, label)`.
/// Same rationale as `SNAPSHOT_SIZES`.
#[cfg(codspeed)]
const EXECUTE_REAL_MEM_SIZES: &[(usize, &str)] = &[(65_536, "64KiB")];
#[cfg(not(codspeed))]
const EXECUTE_REAL_MEM_SIZES: &[(usize, &str)] = &[
    (1_048_576, "1MiB"),
    (10_485_760, "10MiB"),
    (104_857_600, "100MiB"),
];

/// Deterministic, *non-trivially-compressible* memory buffer.
///
/// Uses a 64-bit linear congruential generator so the output passes basic
/// entropy expectations; zstd cannot meaningfully compress it. Seeded by
/// size so different sizes produce different content but each size is
/// repeatable across runs.
fn pseudo_random_buffer(size_bytes: usize) -> Vec<u8> {
    let mut buf = vec![0u8; size_bytes];
    // Knuth LCG constants.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15u64.wrapping_add(size_bytes as u64);
    for chunk in buf.chunks_mut(8) {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let bytes = state.to_le_bytes();
        for (i, b) in chunk.iter_mut().enumerate() {
            *b = bytes[i];
        }
    }
    buf
}

fn make_snapshot(mgr: &SnapshotManager, mem: Vec<u8>) -> Snapshot {
    let fs = FilesystemDiff::new();
    let st = ExecutionState::default();
    let meta = SnapshotMetadata::new("bench".into(), format!("size_{}", mem.len()));
    mgr.create_snapshot(mem, fs, st, meta)
        .expect("snapshot creation should succeed")
}

/// Phase 1A — Cold start: building the engine + sandbox is the dominant
/// cost path users hit when spawning a new agent context.
#[cfg(any(not(codspeed), feature = "bench-cold-start"))]
fn bench_cold_start(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold_start");
    #[cfg(not(codspeed))]
    {
        group.warm_up_time(Duration::from_secs(3));
        group.measurement_time(Duration::from_secs(10));
        group.sample_size(100);
    }
    #[cfg(codspeed)]
    {
        group.warm_up_time(Duration::from_millis(1));
        group.measurement_time(Duration::from_secs(1));
        group.sample_size(10);
    }

    group.bench_function("sandbox_new", |b| {
        b.iter(|| {
            let cfg = SandboxConfig::default();
            let sandbox = WasmSandbox::new(cfg).expect("sandbox new");
            black_box(sandbox);
        })
    });

    group.bench_function("hypervisor_new", |b| {
        b.iter(|| {
            let cfg = HypervisorConfig::default();
            let hv = NexusHypervisor::new(cfg).expect("hypervisor new");
            black_box(hv);
        })
    });

    group.finish();
}

/// Phase 1B — Snapshot creation: zstd compression + SHA-256 of pseudo-random
/// memory, parameterized by linear memory size (1, 10, 100 MiB in wall-clock
/// mode; 1 MiB only in codspeed mode — see SNAPSHOT_SIZES_MIB).
#[cfg(any(not(codspeed), feature = "bench-snapshot-create"))]
fn bench_snapshot_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_create");
    #[cfg(not(codspeed))]
    {
        group.warm_up_time(Duration::from_secs(3));
        group.measurement_time(Duration::from_secs(15));
        group.sample_size(50);
    }
    #[cfg(codspeed)]
    {
        group.warm_up_time(Duration::from_millis(1));
        group.measurement_time(Duration::from_secs(1));
        group.sample_size(10);
    }

    for &(bytes, label) in SNAPSHOT_SIZES {
        let mem = pseudo_random_buffer(bytes);
        group.throughput(Throughput::Bytes(bytes as u64));
        group.bench_with_input(BenchmarkId::new("size", label), &mem, |b, mem| {
            let mgr = SnapshotManager::new(8);
            b.iter(|| {
                let snap = make_snapshot(&mgr, mem.clone());
                black_box(snap);
            })
        });
    }

    group.finish();
}

/// Phase 1C — Rollback: pre-create a snapshot of pseudo-random memory of the
/// given size, then measure `rollback_to` (decompress + integrity revert).
#[cfg(any(not(codspeed), feature = "bench-snapshot-rollback"))]
fn bench_rollback(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_rollback");
    #[cfg(not(codspeed))]
    {
        group.warm_up_time(Duration::from_secs(3));
        group.measurement_time(Duration::from_secs(15));
        group.sample_size(50);
    }
    #[cfg(codspeed)]
    {
        group.warm_up_time(Duration::from_millis(1));
        group.measurement_time(Duration::from_secs(1));
        group.sample_size(10);
    }

    for &(bytes, label) in SNAPSHOT_SIZES {
        let mem = pseudo_random_buffer(bytes);
        let mgr = SnapshotManager::new(8);
        let snap = make_snapshot(&mgr, mem);
        let id = snap.id;
        group.throughput(Throughput::Bytes(bytes as u64));
        group.bench_with_input(BenchmarkId::new("size", label), &id, |b, id| {
            b.iter(|| {
                let result = mgr.rollback_to(id).expect("rollback");
                black_box(result);
            })
        });
    }

    group.finish();
}

/// Phase 1D — End-to-end execution path: a tiny but valid WASM module run
/// through the full hypervisor (snapshot + sandbox + health check).
///
/// Uses the synchronous bencher with a single shared tokio current-thread
/// runtime so we are not paying for runtime construction inside the loop.
#[cfg(any(not(codspeed), feature = "bench-execute-tool"))]
fn bench_execute_end_to_end(c: &mut Criterion) {
    let mut group = c.benchmark_group("execute_tool");
    #[cfg(not(codspeed))]
    {
        group.warm_up_time(Duration::from_secs(3));
        group.measurement_time(Duration::from_secs(15));
        group.sample_size(60);
    }
    #[cfg(codspeed)]
    {
        group.warm_up_time(Duration::from_millis(1));
        group.measurement_time(Duration::from_secs(1));
        group.sample_size(10);
    }

    let wasm = wat::parse_str(
        r#"
        (module
            (func (export "_start"))
        )
        "#,
    )
    .expect("wat compiles");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let hv = NexusHypervisor::new(HypervisorConfig::default()).expect("hv");

    group.bench_function("trivial_wasm_start", |b| {
        b.iter(|| {
            let tool = ToolDefinition::new("bench_trivial".into(), wasm.clone());
            let out = rt
                .block_on(hv.execute_tool(tool, serde_json::json!({})))
                .expect("execute_tool result");
            black_box(out);
        })
    });

    group.finish();
}

/// Phase C — End-to-end snapshot via `execute_tool` on a WASM module
/// that grows its real linear memory. This is the apples-to-apples
/// number that pairs with the Phase 1 `snapshot_create` bench (which
/// calls `SnapshotManager` directly): here the snapshot bytes come from
/// `instance.get_memory("memory").data()` captured by `WasmSandbox`,
/// not from a synthetic buffer.
///
/// The guest module exports a memory of `pages` pages (each 64 KiB),
/// writes a deterministic pattern across it, then returns. The
/// hypervisor's pre-call-memory capture (Phase A) is what feeds the
/// snapshot. Together this measures the *real* end-to-end snap cost for
/// a workload that actually owns N MiB of WASM linear memory.
#[cfg(any(not(codspeed), feature = "bench-execute-real-memory"))]
fn bench_execute_with_real_memory(c: &mut Criterion) {
    let mut group = c.benchmark_group("execute_tool_real_memory");
    #[cfg(not(codspeed))]
    {
        group.warm_up_time(Duration::from_secs(3));
        group.measurement_time(Duration::from_secs(20));
        group.sample_size(30);
    }
    #[cfg(codspeed)]
    {
        group.warm_up_time(Duration::from_millis(1));
        group.measurement_time(Duration::from_secs(1));
        group.sample_size(10);
    }

    for &(total_bytes, label) in EXECUTE_REAL_MEM_SIZES {
        let pages = total_bytes / 65536;
        let wat = format!(
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start")
                    (local $i i32)
                    i32.const {grow}
                    memory.grow
                    drop
                    (local.set $i (i32.const 0))
                    (loop $touch
                        (i32.store8 (local.get $i) (i32.const 42))
                        (local.set $i (i32.add (local.get $i) (i32.const 65536)))
                        (br_if $touch (i32.lt_s (local.get $i) (i32.const {total})))
                    )
                ))"#,
            grow = pages.saturating_sub(1),
            total = total_bytes,
        );
        let wasm = wat::parse_str(&wat).expect("wat compiles");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let mut cfg = HypervisorConfig::default();
        cfg.sandbox_config.max_fuel = 50_000_000;
        let hv = NexusHypervisor::new(cfg).expect("hv");

        group.throughput(Throughput::Bytes(total_bytes as u64));
        group.bench_with_input(BenchmarkId::new("size", label), &wasm, |b, wasm| {
            b.iter(|| {
                let tool = ToolDefinition::new(format!("realmem_{label}"), wasm.clone());
                let out = rt
                    .block_on(hv.execute_tool(tool, serde_json::json!({})))
                    .expect("execute_tool");
                black_box(out);
            })
        });
    }
    group.finish();
}

fn selected_benches(c: &mut Criterion) {
    #[cfg(any(not(codspeed), feature = "bench-cold-start"))]
    bench_cold_start(c);
    #[cfg(any(not(codspeed), feature = "bench-snapshot-create"))]
    bench_snapshot_creation(c);
    #[cfg(any(not(codspeed), feature = "bench-snapshot-rollback"))]
    bench_rollback(c);
    #[cfg(any(not(codspeed), feature = "bench-execute-tool"))]
    bench_execute_end_to_end(c);
    #[cfg(any(not(codspeed), feature = "bench-execute-real-memory"))]
    bench_execute_with_real_memory(c);
}

criterion_group!(benches, selected_benches);
criterion_main!(benches);
