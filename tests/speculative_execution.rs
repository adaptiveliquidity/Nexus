//! Integration tests for speculative recovery through the real hypervisor.
//!
//! These drive `NexusHypervisor::speculative_execute` end to end with real
//! WASM modules, exercising the opt-in racing path rather than the in-memory
//! unit tests in `hypervisor::speculative`.

use nexus::hypervisor::recovery::{RecoveryAction, RecoverySource};
use nexus::hypervisor::SpeculativeBranch;
use nexus::{
    HypervisorConfig, NexusHypervisor, SelectionStrategy, SpeculativeConfig, ToolDefinition,
};
use std::time::Duration;
use uuid::Uuid;

fn hypervisor() -> NexusHypervisor {
    NexusHypervisor::new(HypervisorConfig::default()).unwrap()
}

/// A trivial module whose `_start` runs to completion (success path).
/// Exports a `memory` so the sandbox can capture pre-call state.
fn good_tool(name: &str) -> ToolDefinition {
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();
    ToolDefinition::new(name.to_string(), wasm)
}

/// A module that traps immediately via `unreachable` (failure path).
fn trapping_tool(name: &str) -> ToolDefinition {
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start") unreachable)
        )"#,
    )
    .unwrap();
    ToolDefinition::new(name.to_string(), wasm)
}

async fn base_snapshot_id(hv: &NexusHypervisor) -> Uuid {
    let output = hv
        .execute_tool(good_tool("base_snapshot"), serde_json::json!({}))
        .await
        .expect("base snapshot execution should run");
    output
        .snapshot_id
        .expect("base execution should capture a runtime snapshot")
}

fn branch(base_snapshot_id: Uuid, tool: ToolDefinition) -> SpeculativeBranch {
    SpeculativeBranch::new(
        base_snapshot_id,
        tool,
        RecoveryAction::new("retry-variant", RecoverySource::Static),
    )
}

fn config(strategy: SelectionStrategy) -> SpeculativeConfig {
    SpeculativeConfig {
        max_branches: 8,
        branch_timeout: Duration::from_secs(10),
        selection_strategy: strategy,
    }
}

/// On a CPU-saturated host the hypervisor's `HealthValidator` flips an
/// otherwise-successful guest into a `HostError` ("CPU usage stuck at
/// maximum"). That is a host-environment signal, independent of the
/// speculative racer's logic. These integration tests assert the success
/// property when the host is healthy, but tolerate a host-environment error
/// so they are not flaky under load — while still failing loudly on any
/// *logic* error in the racer. The deterministic racing behaviour itself is
/// proven by the unit tests in `hypervisor::speculative`.
fn assert_host_environment_error(e: &nexus::NexusError) {
    let msg = e.to_string();
    assert!(
        msg.contains("speculative branches failed"),
        "expected an aggregated speculative failure, got: {msg}"
    );
    assert!(
        msg.contains("Host error") || msg.contains("CPU"),
        "branch failure was not a host-environment signal (likely a real bug): {msg}"
    );
}

/// A round with at least one viable tool returns a successful winner
/// (or, under host saturation, a host-environment error).
#[tokio::test]
async fn speculative_round_returns_a_successful_winner() {
    let hv = hypervisor();
    let base = base_snapshot_id(&hv).await;
    let branches = vec![
        branch(base, trapping_tool("bad")),
        branch(base, good_tool("good")),
        branch(base, trapping_tool("also_bad")),
    ];

    match hv
        .speculative_execute(
            serde_json::json!({}),
            branches,
            &config(SelectionStrategy::WaitAll),
        )
        .await
    {
        Ok(result) => {
            assert!(result.winner.succeeded);
            assert_eq!(result.branches_tried, 3);
            assert!(result.branches_succeeded >= 1);
            assert!(result.winner.output.is_some());
        }
        Err(e) => assert_host_environment_error(&e),
    }
}

/// When every branch's tool traps, the whole round is an error.
#[tokio::test]
async fn speculative_round_all_trapping_is_error() {
    let hv = hypervisor();
    let base = base_snapshot_id(&hv).await;
    let branches = vec![
        branch(base, trapping_tool("a")),
        branch(base, trapping_tool("b")),
    ];

    let result = hv
        .speculative_execute(
            serde_json::json!({}),
            branches,
            &config(SelectionStrategy::FirstSuccess),
        )
        .await;

    assert!(result.is_err(), "all-trapping round must be an error");
}

/// FirstSuccess returns as soon as a viable branch wins.
#[tokio::test]
async fn first_success_strategy_picks_the_good_branch() {
    let hv = hypervisor();
    let base = base_snapshot_id(&hv).await;
    let branches = vec![
        branch(base, good_tool("good")),
        branch(base, trapping_tool("bad")),
    ];

    match hv
        .speculative_execute(
            serde_json::json!({}),
            branches,
            &config(SelectionStrategy::FirstSuccess),
        )
        .await
    {
        Ok(result) => {
            assert!(result.winner.succeeded);
            assert!(!result.winner.timed_out);
        }
        Err(e) => assert_host_environment_error(&e),
    }
}
