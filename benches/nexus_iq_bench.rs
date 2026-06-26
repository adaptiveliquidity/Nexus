//! NexusIQ full-loop performance benchmarks.
//!
//! Criterion writes reports and raw measurements under `target/criterion/`.
//! The async benchmarks use a shared Tokio runtime so each measurement focuses
//! on NexusIQ work rather than runtime construction.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use nexus::proof::{ProofCapsuleBuilder, TypedDigest};
use nexus::{HypervisorConfig, NexusHypervisor, ToolDefinition};

fn noop_wasm() -> Vec<u8> {
    wat::parse_str(r#"(module (func (export "_start") nop))"#).expect("noop WAT compiles")
}

fn tokio_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn module_digest() -> TypedDigest {
    TypedDigest::sha256_public(b"bench-module")
}

fn input_digest() -> TypedDigest {
    TypedDigest {
        algorithm: "hmac-sha256".to_owned(),
        value: "bench-input-digest".to_owned(),
        public_recomputable: false,
    }
}

/// Measures the absent AEON-IQ memory recall baseline:
/// `recall_memory_evidence_v1(None, ..)` returns local `Absent` evidence with
/// no network client. Results land in `target/criterion/`.
#[cfg(feature = "aeon-memory")]
fn bench_recall_memory_evidence(c: &mut Criterion) {
    let rt = tokio_runtime();

    c.bench_function("bench_recall_memory_evidence", |b| {
        b.to_async(&rt).iter(|| async {
            let recall =
                nexus::aeon::recall_memory_evidence_v1(None, black_box("bench context"), 5).await;
            black_box(recall);
        })
    });
}

/// Measures minimal proof-capsule construction through `ProofCapsuleBuilder`
/// and `.build()` without WASM execution. Results land in `target/criterion/`.
fn bench_proof_capsule_build(c: &mut Criterion) {
    let module_digest = module_digest();
    let input_digest = input_digest();

    c.bench_function("bench_proof_capsule_build", |b| {
        b.iter(|| {
            let capsule = ProofCapsuleBuilder::new(
                black_box("bench_proof_capsule"),
                module_digest.clone(),
                input_digest.clone(),
            )
            .build();
            black_box(capsule);
        })
    });
}

/// Measures `NexusHypervisor::execute_tool_proof()` on a one-instruction WASM
/// no-op `_start`, including sandbox execution and proof-capsule creation.
/// Results land in `target/criterion/`.
fn bench_execute_tool_proof(c: &mut Criterion) {
    let rt = tokio_runtime();
    let hypervisor = NexusHypervisor::new(HypervisorConfig::default()).expect("hypervisor");
    let wasm = noop_wasm();

    c.bench_function("bench_execute_tool_proof", |b| {
        let hypervisor = &hypervisor;
        b.to_async(&rt).iter(|| {
            let tool = ToolDefinition::new("bench_execute_tool_proof".to_owned(), wasm.clone());
            async move {
                let result = hypervisor
                    .execute_tool_proof(tool, serde_json::json!({}))
                    .await
                    .expect("execute_tool_proof");
                black_box(result);
            }
        })
    });
}

/// Measures the public NexusIQ-equivalent loop: absent memory recall,
/// proof-producing WASM execution, and binding the proof capsule id back into
/// `MemoryEvidenceV1`. The MCP `do_nexus_iq_execute` method is private to the
/// binary, so this benchmark uses the same exported components with mock
/// inputs. Results land in `target/criterion/`.
#[cfg(feature = "aeon-memory")]
fn bench_full_iq_loop(c: &mut Criterion) {
    let rt = tokio_runtime();
    let hypervisor = NexusHypervisor::new(HypervisorConfig::default()).expect("hypervisor");
    let wasm = noop_wasm();
    let input = serde_json::json!({ "message": "hello" });

    c.bench_function("bench_full_iq_loop", |b| {
        let hypervisor = &hypervisor;
        b.to_async(&rt).iter(|| {
            let wasm = wasm.clone();
            let input = input.clone();
            async move {
                let recall = nexus::aeon::recall_memory_evidence_v1(None, "bench context", 5).await;
                let tool = ToolDefinition::new("bench_full_iq_loop".to_owned(), wasm);
                let (output, capsule) = hypervisor
                    .execute_tool_proof(tool, input)
                    .await
                    .expect("execute_tool_proof");
                let memory_evidence = recall
                    .evidence
                    .with_capsule_digest(Some(capsule.capsule_id.to_string()));
                black_box((output, capsule, memory_evidence));
            }
        })
    });
}

fn selected_benches(c: &mut Criterion) {
    #[cfg(feature = "aeon-memory")]
    bench_recall_memory_evidence(c);
    bench_proof_capsule_build(c);
    bench_execute_tool_proof(c);
    #[cfg(feature = "aeon-memory")]
    bench_full_iq_loop(c);
}

criterion_group!(benches, selected_benches);
criterion_main!(benches);
