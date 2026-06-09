//! Phase B test for the outcome-feedback loop. After three retries where
//! each attempt fails with a deterministic `TrapDivByZero`, a recovery
//! action that came from the instinct store should accumulate three
//! `failure_count` increments (each attempt re-emits the same instinct
//! and the next attempt's outcome credits/debits it).
//!
//! Because `execute_with_retry` will never actually succeed on a
//! deterministic guest trap, we only assert the debit path here; the
//! credit path is exercised at unit-test level in
//! `src/instinct/mod.rs::tests::confidence_increases_with_support`.

use std::sync::Arc;

use nexus::hypervisor::recovery::{LayeredPolicy, RecoveryPolicy, StaticPolicy};
use nexus::instinct::{InstinctPolicy, InstinctStore};
use nexus::{FailureMode, HypervisorConfig, NexusHypervisor, ToolDefinition};
use tempfile::tempdir;

fn divzero_wat() -> &'static str {
    r#"(module
        (memory (export "memory") 1)
        (func (export "_start")
            i32.const 1 i32.const 0 i32.div_s drop))"#
}

#[test]
fn instinct_is_debited_after_failed_retries() {
    let tmp = tempdir().unwrap();
    let store = Arc::new(InstinctStore::open(tmp.path().to_path_buf()).unwrap());

    // Seed an instinct that StaticPolicy would not emit, so we can tell
    // it apart in the recovery_actions list.
    let instinct_id = store
        .register(
            &FailureMode::TrapDivByZero,
            "*",
            "instinct: validate divisor at API boundary",
        )
        .unwrap();

    // Layered policy: static first, then instinct. Instinct is what
    // carries the `instinct_id` that the outcome-feedback loop needs.
    let policy: Arc<dyn RecoveryPolicy> = Arc::new(LayeredPolicy::new(vec![
        Box::new(StaticPolicy::new()),
        Box::new(InstinctPolicy::new(store.clone())),
    ]));

    let cfg = HypervisorConfig {
        max_retries: 3, // execute_with_retry will call execute_tool 4 times
        ..HypervisorConfig::default()
    };
    let hv = NexusHypervisor::new_with_policy(cfg, policy)
        .unwrap()
        .with_instinct_store(store.clone());

    let wasm = wat::parse_str(divzero_wat()).unwrap();
    let tool = ToolDefinition::new("debit_test".into(), wasm);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let out = rt
        .block_on(hv.execute_with_retry(tool, serde_json::json!({})))
        .unwrap();

    // Sanity: every retry failed with the same deterministic trap.
    assert!(!out.success, "div-by-zero should never succeed");

    // The outcome loop debits the instinct once per "previous attempt"
    // that produced it and was followed by another attempt. With 4 attempts
    // (0..=3), attempts 0,1,2 each leave a pending instinct that attempts
    // 1,2,3 then debit -> exactly 3 debits.
    let stats = store.stats();
    assert_eq!(stats.total_failures, 3, "expected 3 debits, got {stats:?}");
    assert_eq!(stats.total_support, 0, "no successes expected");

    // The instinct itself should reflect the debits and have eroded
    // confidence (Bayes-smoothed below the initial 0.5).
    let queried = store.query(&FailureMode::TrapDivByZero, "debit_test");
    let learned = queried
        .iter()
        .find(|i| i.id == instinct_id)
        .expect("instinct still present");
    assert_eq!(learned.failure_count, 3);
    assert!(
        learned.confidence < 0.5,
        "confidence should have eroded; got {}",
        learned.confidence
    );
}
