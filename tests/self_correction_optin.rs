//! PR-5: Self-correction API semantics tests.
//!
//! Verifies that self-correction (instinct outcome feedback) is OFF by
//! default and only activates when explicitly opted in via
//! `with_self_correction` or `with_instinct_store`.

use std::sync::Arc;

use nexus::{HypervisorConfig, InstinctStore, NexusHypervisor};

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
