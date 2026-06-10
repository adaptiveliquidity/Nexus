//! Phase A regression test (Claude-recommended in the Phase 3 AI verdict):
//! run each of the five Phase 3 failing-WASM scenarios through the real
//! `NexusHypervisor` and assert that the resulting
//! `(FailureMode, HealthStatus, recovery_actions[0].description)` tuple is
//! distinct across scenarios.
//!
//! The pre-Phase-A code path failed this test by construction: every
//! scenario produced `HealthStatus::Corrupted` and the same two hardcoded
//! recovery strings. Phase A's `FailureMode` + `RecoveryPolicy` rewrite is
//! what makes this test pass.

use std::collections::HashSet;

use nexus::hypervisor::failure_mode::FailureMode;
use nexus::hypervisor::recovery::RecoverySource;
use nexus::hypervisor::validator::health::HealthStatus;
use nexus::{HypervisorConfig, NexusHypervisor, ToolDefinition};

fn wat_for(scenario: &str) -> &'static str {
    // Mirror of examples/capture_error.rs. Memory export is included so
    // the snapshot path actually has bytes to capture.
    match scenario {
        "infinite_loop" => {
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start") (loop $l (br $l))))"#
        }
        "trap_unreachable" => {
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start") unreachable))"#
        }
        "div_by_zero" => {
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start")
                    i32.const 1 i32.const 0 i32.div_s drop))"#
        }
        "stack_overflow" => {
            r#"(module
                (memory (export "memory") 1)
                (func $rec (call $rec))
                (func (export "_start") (call $rec)))"#
        }
        "missing_start" => {
            r#"(module
                (memory (export "memory") 1)
                (func $noop))"#
        }
        _ => panic!("unknown scenario {scenario}"),
    }
}

fn run_scenario(name: &str) -> nexus::ToolOutput {
    let wasm = wat::parse_str(wat_for(name)).expect("wat parses");
    let hv = NexusHypervisor::new(HypervisorConfig::default()).expect("hypervisor builds");
    let tool = ToolDefinition::new(name.to_string(), wasm);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(hv.execute_tool(tool, serde_json::json!({})))
        .expect("execute_tool returns")
}

#[test]
fn all_five_scenarios_produce_distinct_outputs() {
    let scenarios = [
        "infinite_loop",
        "trap_unreachable",
        "div_by_zero",
        "stack_overflow",
        "missing_start",
    ];

    let mut modes: HashSet<String> = HashSet::new();
    let mut first_actions: HashSet<String> = HashSet::new();
    let mut categories: HashSet<&'static str> = HashSet::new();

    for name in scenarios {
        let out = run_scenario(name);
        assert!(!out.success, "{name} should not succeed");
        let log = out.error_log.expect("error_log present");

        let mode_str = format!("{:?}", log.failure_mode);
        assert!(
            modes.insert(mode_str.clone()),
            "duplicate FailureMode `{mode_str}` for scenario `{name}`"
        );
        assert!(
            categories.insert(log.failure_mode.category()),
            "duplicate FailureMode category `{}` for scenario `{name}`",
            log.failure_mode.category()
        );

        assert!(
            !log.recovery_actions.is_empty(),
            "{name} produced no recovery actions"
        );
        let first = log.recovery_actions[0].description.clone();
        assert!(
            first_actions.insert(first.clone()),
            "duplicate first recovery action `{first}` for scenario `{name}`"
        );

        // Every recovery action emitted by Phase A's static policy must
        // come from the Static source until Phase B layers Instinct/LLM.
        for a in &log.recovery_actions {
            assert_eq!(
                a.source,
                RecoverySource::Static,
                "{name}: unexpected recovery source {:?}",
                a.source
            );
        }
    }
}

#[test]
fn classification_matches_phase3_expectations() {
    // Anchor the failure classification for each scenario.
    //
    // `infinite_loop` is accepted as either `FuelExhausted` (when fuel
    // metering is the active limiter, the Phase A default) OR `Timeout`
    // (when wall-clock fires first). Both AI scorers explicitly named
    // these as the two correct classifications.
    let cases: &[(&str, &[&str], &[HealthStatus])] = &[
        (
            "infinite_loop",
            &["FuelExhausted", "Timeout"],
            &[HealthStatus::FuelExhausted, HealthStatus::Timeout],
        ),
        (
            "trap_unreachable",
            &["TrapUnreachable"],
            &[HealthStatus::Trapped],
        ),
        ("div_by_zero", &["TrapDivByZero"], &[HealthStatus::Trapped]),
        (
            "stack_overflow",
            &["TrapStackOverflow"],
            &[HealthStatus::ResourceExhausted],
        ),
        (
            "missing_start",
            &["MissingEntrypoint"],
            &[HealthStatus::InvalidModule],
        ),
    ];

    for (name, allowed_mode_prefixes, allowed_healths) in cases {
        let out = run_scenario(name);
        let log = out.error_log.expect("error_log");
        let dbg = format!("{:?}", log.failure_mode);
        assert!(
            allowed_mode_prefixes.iter().any(|p| dbg.starts_with(p)),
            "{name}: expected FailureMode in {allowed_mode_prefixes:?}, got `{dbg}`"
        );
        assert!(
            allowed_healths.contains(&log.trigger_status),
            "{name}: expected HealthStatus in {allowed_healths:?}, got {:?} (mode={dbg})",
            log.trigger_status
        );
    }
}

#[test]
fn load_time_failures_dont_trigger_rollback() {
    // missing_start (and any future InvalidModule scenario) must not flip
    // `rollback_performed` to true — nothing executed, nothing to roll back.
    let out = run_scenario("missing_start");
    assert!(
        !out.rollback_performed,
        "missing_start should not perform rollback; got rollback_performed=true"
    );
}

#[test]
fn runtime_failures_do_trigger_rollback() {
    // Every runtime failure with an exported memory should land in the
    // rollback path. This is the inverse assertion of
    // `load_time_failures_dont_trigger_rollback` and pins the Phase A
    // semantics: rollback iff `requires_rollback() && pre_call_memory.is_some()`.
    for name in [
        "infinite_loop",
        "trap_unreachable",
        "div_by_zero",
        "stack_overflow",
    ] {
        let out = run_scenario(name);
        assert!(
            out.rollback_performed,
            "{name}: expected rollback_performed=true (runtime failure with memory export)"
        );
    }
}

#[test]
fn deterministic_traps_are_marked_non_retryable() {
    for name in [
        "trap_unreachable",
        "div_by_zero",
        "stack_overflow",
        "missing_start",
    ] {
        let out = run_scenario(name);
        let log = out.error_log.expect("error_log");
        assert!(
            log.recovery_actions.iter().any(|a| a.non_retryable),
            "{name}: expected at least one non-retryable recovery action"
        );
        match &log.failure_mode {
            FailureMode::TrapUnreachable
            | FailureMode::TrapDivByZero
            | FailureMode::TrapStackOverflow
            | FailureMode::MissingEntrypoint { .. }
            | FailureMode::InvalidModule(_) => {}
            other => panic!("{name}: unexpected failure mode {other:?}"),
        }
    }
}
