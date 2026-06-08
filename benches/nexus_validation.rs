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

use std::time::Duration;

#[cfg(codspeed)]
use codspeed_criterion_compat::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
};
#[cfg(not(codspeed))]
use criterion::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
};

use nexus::hypervisor::{HypervisorConfig, NexusHypervisor, ToolDefinition};
use nexus::sandbox::{SandboxConfig, WasmSandbox};
use nexus::snapshot::{
    ExecutionState, FilesystemDiff, Snapshot, SnapshotManager, SnapshotMetadata,
};

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
fn bench_cold_start(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold_start");
    group.warm_up_time(Duration::from_secs(3));
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100);

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
/// memory, parameterized by linear memory size (1, 10, 100 MiB).
fn bench_snapshot_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_create");
    group.warm_up_time(Duration::from_secs(3));
    group.measurement_time(Duration::from_secs(15));
    // Sample size scaled down for the 100 MiB case so wall time stays bounded.
    group.sample_size(50);

    let sizes_mb: [usize; 3] = [1, 10, 100];
    for &mb in &sizes_mb {
        let bytes = mb * 1024 * 1024;
        let mem = pseudo_random_buffer(bytes);
        group.throughput(Throughput::Bytes(bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("MiB", mb),
            &mem,
            |b, mem| {
                let mgr = SnapshotManager::new(8);
                b.iter(|| {
                    let snap = make_snapshot(&mgr, mem.clone());
                    black_box(snap);
                })
            },
        );
    }

    group.finish();
}

/// Phase 1C — Rollback: pre-create a snapshot of pseudo-random memory of the
/// given size, then measure `rollback_to` (decompress + integrity revert).
fn bench_rollback(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_rollback");
    group.warm_up_time(Duration::from_secs(3));
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(50);

    let sizes_mb: [usize; 3] = [1, 10, 100];
    for &mb in &sizes_mb {
        let bytes = mb * 1024 * 1024;
        let mem = pseudo_random_buffer(bytes);
        let mgr = SnapshotManager::new(8);
        let snap = make_snapshot(&mgr, mem);
        let id = snap.id;
        group.throughput(Throughput::Bytes(bytes as u64));
        group.bench_with_input(BenchmarkId::new("MiB", mb), &id, |b, id| {
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
fn bench_execute_end_to_end(c: &mut Criterion) {
    let mut group = c.benchmark_group("execute_tool");
    group.warm_up_time(Duration::from_secs(3));
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(60);

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
fn bench_execute_with_real_memory(c: &mut Criterion) {
    let mut group = c.benchmark_group("execute_tool_real_memory");
    group.warm_up_time(Duration::from_secs(3));
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(30);

    let sizes_mib: [usize; 3] = [1, 10, 100];
    for &mib in &sizes_mib {
        let pages = (mib * 1024 * 1024) / 65536;
        // WAT that grows to `pages` pages and touches one byte in each
        // page so wasmtime actually allocates the underlying memory.
        // `memory.grow` from the start size (1 page) by (pages - 1).
        let wat = format!(
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start")
                    (local $i i32)
                    ;; grow to {pages} pages total
                    i32.const {grow}
                    memory.grow
                    drop
                    ;; touch one byte per page so the kernel commits them
                    (local.set $i (i32.const 0))
                    (loop $touch
                        (i32.store8 (local.get $i) (i32.const 42))
                        (local.set $i (i32.add (local.get $i) (i32.const 65536)))
                        (br_if $touch (i32.lt_s (local.get $i) (i32.const {total})))
                    )
                ))"#,
            pages = pages,
            grow = pages.saturating_sub(1),
            total = mib * 1024 * 1024,
        );
        let wasm = wat::parse_str(&wat).expect("wat compiles");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        // Fuel cap large enough for the page-touch loop on 100 MiB
        // (`100 * 1024 = 102_400` iterations * ~5 ops = 512 K instructions).
        let mut cfg = HypervisorConfig::default();
        cfg.sandbox_config.max_fuel = 50_000_000;
        let hv = NexusHypervisor::new(cfg).expect("hv");

        group.throughput(Throughput::Bytes((mib * 1024 * 1024) as u64));
        group.bench_with_input(
            BenchmarkId::new("MiB", mib),
            &wasm,
            |b, wasm| {
                b.iter(|| {
                    let tool = ToolDefinition::new(format!("realmem_{mib}"), wasm.clone());
                    let out = rt
                        .block_on(hv.execute_tool(tool, serde_json::json!({})))
                        .expect("execute_tool");
                    black_box(out);
                })
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_cold_start,
    bench_snapshot_creation,
    bench_rollback,
    bench_execute_end_to_end,
    bench_execute_with_real_memory,
);
criterion_main!(benches);
