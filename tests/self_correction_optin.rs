//! PR-5: Self-correction API semantics tests.
//!
//! Verifies that self-correction (instinct outcome feedback) is OFF by
//! default and only activates when explicitly opted in via
//! `with_self_correction` or `with_instinct_store`.

use std::sync::Arc;

use nexus::{
    FailureMode, HypervisorConfig, InstinctStore, NexusHypervisor, RecoveryConfig, RecoverySource,
    ToolDefinition,
};

fn temp_store() -> Arc<InstinctStore> {
    let dir = std::env::temp_dir().join(format!("nexus_test_instinct_{}", uuid::Uuid::new_v4()));
    Arc::new(InstinctStore::open(dir).unwrap())
}

#[test]
fn self_correction_off_by_default() {
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    assert!(
        !hv.self_correction_enabled(),
        "self-correction should be OFF by default"
    );
    assert!(
        hv.instinct_store().is_none(),
        "instinct store should be None by default"
    );
}

#[test]
fn with_self_correction_enables_it() {
    let store = temp_store();
    let hv = NexusHypervisor::new(HypervisorConfig::default())
        .unwrap()
        .with_self_correction(store);

    assert!(
        hv.self_correction_enabled(),
        "self-correction should be ON after with_self_correction"
    );
    assert!(
        hv.instinct_store().is_some(),
        "instinct store should be Some after opt-in"
    );
}

#[test]
fn with_instinct_store_also_enables_self_correction() {
    let store = temp_store();
    let hv = NexusHypervisor::new(HypervisorConfig::default())
        .unwrap()
        .with_instinct_store(store);

    assert!(
        hv.self_correction_enabled(),
        "with_instinct_store should also enable self_correction_enabled()"
    );
}

#[tokio::test]
async fn default_recovery_config_emits_only_static_actions() {
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    assert_eq!(hv.recovery_config(), &RecoveryConfig::Static);
    assert!(!hv.self_correction_enabled());

    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start") unreachable)
        )"#,
    )
    .unwrap();
    let tool = ToolDefinition::new("static_recovery".into(), wasm);
    let output = hv.execute_tool(tool, serde_json::json!({})).await.unwrap();

    let log = output.error_log.expect("trap should produce an error log");
    assert!(
        log.recovery_actions
            .iter()
            .all(|action| action.source == RecoverySource::Static),
        "default recovery config should use only StaticPolicy"
    );
}

#[tokio::test]
async fn layered_instinct_recovery_config_is_selectable_and_exercised() {
    let dir = std::env::temp_dir().join(format!("nexus_config_instinct_{}", uuid::Uuid::new_v4()));
    let cfg = HypervisorConfig {
        max_retries: 1,
        recovery_config: RecoveryConfig::LayeredInstinct {
            store_dir: dir,
            min_confidence: 0.0,
        },
        ..HypervisorConfig::default()
    };
    let hv = NexusHypervisor::new(cfg).unwrap();
    assert!(matches!(
        hv.recovery_config(),
        RecoveryConfig::LayeredInstinct { .. }
    ));
    assert!(hv.self_correction_enabled());

    let store = hv.instinct_store().expect("config should attach store");
    store
        .register(
            &FailureMode::TrapDivByZero,
            "*",
            "instinct: validate divisor before division",
        )
        .unwrap();

    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start")
                i32.const 1 i32.const 0 i32.div_s drop))"#,
    )
    .unwrap();
    let tool = ToolDefinition::new("configured_instinct".into(), wasm);
    let output = hv
        .execute_with_retry(tool, serde_json::json!({}))
        .await
        .unwrap();

    assert!(!output.success);
    let log = output
        .error_log
        .expect("failed retry should keep error log");
    assert!(
        log.recovery_actions
            .iter()
            .any(|action| action.source == RecoverySource::Instinct),
        "LayeredInstinct should consult InstinctPolicy"
    );
    assert_eq!(
        store.stats().total_failures,
        1,
        "execute_with_retry should exercise outcome feedback"
    );
}
