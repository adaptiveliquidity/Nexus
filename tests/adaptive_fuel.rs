//! Adaptive Fuel Budgeting – integration tests
//!
//! Validates per-tool fuel budget computation, anomaly detection, and
//! hypervisor integration.

use nexus::{FuelBudgetPolicy, FuelProfile, HypervisorConfig, NexusHypervisor, ToolDefinition};

/// The absolute floor exported by the fuel_budget module (100 000).
const MIN_FUEL_FLOOR: u64 = 100_000;

// ---------------------------------------------------------------------------
// 1. Unknown tool → fallback budget
// ---------------------------------------------------------------------------

#[test]
fn unknown_tool_gets_fallback_budget() {
    let policy = FuelBudgetPolicy::new(7_000_000);
    assert_eq!(
        policy.budget_for("never_registered"),
        7_000_000,
        "An unknown tool must receive the fallback budget"
    );
}

// ---------------------------------------------------------------------------
// 2. Budget adapts after enough samples
// ---------------------------------------------------------------------------

#[test]
fn budget_adapts_after_samples() {
    let mut policy = FuelBudgetPolicy::new(10_000_000);

    // Record 10 identical samples of 100 000 fuel. We use 100K rather
    // than 50K so that P95 * 1.5 (= 150K) exceeds the MIN_FUEL_FLOOR
    // (100K) and the adaptive logic is actually visible.
    for _ in 0..10 {
        policy.record("compressor", 100_000);
    }

    let budget = policy.budget_for("compressor");

    // P95 of a uniform 100K distribution is 100K. With 1.5x headroom the
    // expected budget is 150 000. Allow a small tolerance for nearest-rank
    // rounding.
    assert!(
        (149_000..=151_000).contains(&budget),
        "Expected ~150 000, got {budget}"
    );
}

// ---------------------------------------------------------------------------
// 3. Anomaly detection flags spikes
// ---------------------------------------------------------------------------

#[test]
fn anomaly_detection_flags_spikes() {
    let mut policy = FuelBudgetPolicy::new(10_000_000);

    // Build a stable baseline at ~50K.
    for _ in 0..10 {
        policy.record("stable_tool", 50_000);
    }

    // A 200K sample is > 3x P95 (50K * 3 = 150K) → anomalous.
    assert!(
        policy.is_anomalous("stable_tool", 200_000),
        "200K should be flagged as anomalous (> 3x P95 of 50K)"
    );

    // A 100K sample is NOT > 150K → not anomalous.
    assert!(
        !policy.is_anomalous("stable_tool", 100_000),
        "100K should not be flagged (≤ 3x P95)"
    );
}

// ---------------------------------------------------------------------------
// 4. Budget never below floor
// ---------------------------------------------------------------------------

#[test]
fn budget_never_below_floor() {
    let mut policy = FuelBudgetPolicy::new(10_000_000);

    // Record tiny samples (10 fuel each).
    for _ in 0..10 {
        policy.record("tiny", 10);
    }

    let budget = policy.budget_for("tiny");
    assert!(
        budget >= MIN_FUEL_FLOOR,
        "Budget {budget} is below the floor {MIN_FUEL_FLOOR}"
    );
}

// ---------------------------------------------------------------------------
// 5. Headroom factor is configurable
// ---------------------------------------------------------------------------

#[test]
fn headroom_factor_is_configurable() {
    let mut policy = FuelBudgetPolicy::new(10_000_000).with_headroom(2.0);

    for _ in 0..10 {
        policy.record("doubled", 50_000);
    }

    let budget = policy.budget_for("doubled");

    // P95 = 50K, headroom 2.0 → expected budget 100 000.
    assert!(
        (99_000..=101_000).contains(&budget),
        "Expected ~100 000 with 2.0x headroom, got {budget}"
    );
}

// ---------------------------------------------------------------------------
// 6. Percentiles are correct for a known sample set
// ---------------------------------------------------------------------------

#[test]
fn percentiles_are_correct() {
    let mut policy = FuelBudgetPolicy::new(10_000_000);

    // Insert values 1 000 .. 100 000 in steps of 1 000 (100 samples).
    for i in 1..=100u64 {
        policy.record("precise", i * 1_000);
    }

    let profile: FuelProfile = policy.profile_for("precise").unwrap().clone();

    // Nearest-rank on 100 sorted samples:
    //   P50 → index ceil(0.50 * 100) - 1 = 49 → value 50 000
    //   P95 → index ceil(0.95 * 100) - 1 = 94 → value 95 000
    //   P99 → index ceil(0.99 * 100) - 1 = 98 → value 99 000
    assert_eq!(profile.p50, 50_000, "P50 mismatch");
    assert_eq!(profile.p95, 95_000, "P95 mismatch");
    assert_eq!(profile.p99, 99_000, "P99 mismatch");
    assert_eq!(profile.max_observed, 100_000, "max_observed mismatch");
}

// ---------------------------------------------------------------------------
// 7. Hypervisor records fuel after execution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hypervisor_records_fuel_after_execution() {
    // Simple no-op WASM that succeeds quickly.
    let wasm = wat::parse_str(
        r#"
        (module
            (func (export "_start")
                (nop)
            )
        )
    "#,
    )
    .unwrap();

    let config = HypervisorConfig::default();
    let hypervisor = NexusHypervisor::new(config).unwrap();

    let tool = ToolDefinition::new("profiled_tool".to_string(), wasm);

    let output = hypervisor
        .execute_tool(tool, serde_json::json!({}))
        .await
        .expect("execution should not return Err");

    // The tool ran (success or sandbox-level failure is irrelevant here);
    // either way the hypervisor must have recorded fuel telemetry.
    let profile = hypervisor.fuel_profile("profiled_tool");
    assert!(
        profile.is_some(),
        "Fuel profile should exist after one execution"
    );

    let profile = profile.unwrap();
    assert_eq!(
        profile.sample_count(),
        1,
        "Exactly one sample should be recorded"
    );

    // The consumed fuel in the profile should match the execution output.
    // With only one sample every percentile equals that sample.
    assert_eq!(profile.p50, output.fuel_consumed);
}
