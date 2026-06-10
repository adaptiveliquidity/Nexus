//! Adaptive Fuel Budgeting
//!
//! Auto-tunes per-tool fuel limits from historical telemetry instead of
//! using a single global `max_fuel`. Each tool accumulates a `FuelProfile`
//! of recent fuel-consumption samples; the budget handed to the sandbox is
//! derived from the P95 of that profile plus a configurable headroom factor.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// Headroom multiplier applied to P95 when computing the budget.
const DEFAULT_HEADROOM_FACTOR: f64 = 1.5;

/// Minimum number of samples before the policy switches from fallback
/// to the adaptive budget.
const MIN_SAMPLES: usize = 5;

/// Maximum number of samples retained per tool (ring buffer).
const MAX_SAMPLES: usize = 100;

/// Absolute floor: the policy never returns a budget below this value,
/// even when P95 * headroom would be smaller.
const MIN_FUEL_FLOOR: u64 = 100_000;

// Re-export the floor constant so tests can reference it without
// hard-coding a magic number.
pub use self::consts::MIN_FUEL_FLOOR as FUEL_FLOOR;

mod consts {
    pub const MIN_FUEL_FLOOR: u64 = super::MIN_FUEL_FLOOR;
}

/// Per-tool fuel consumption profile built from recent execution samples.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuelProfile {
    samples: VecDeque<u64>,
    /// Median fuel consumption.
    pub p50: u64,
    /// 95th-percentile fuel consumption.
    pub p95: u64,
    /// 99th-percentile fuel consumption.
    pub p99: u64,
    /// Maximum observed fuel consumption across all retained samples.
    pub max_observed: u64,
}

impl FuelProfile {
    fn new() -> Self {
        FuelProfile {
            samples: VecDeque::new(),
            p50: 0,
            p95: 0,
            p99: 0,
            max_observed: 0,
        }
    }

    /// Add a sample and keep the ring buffer bounded.
    fn push(&mut self, fuel_consumed: u64) {
        if self.samples.len() >= MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(fuel_consumed);
        self.recompute();
    }

    /// Sort retained samples and recalculate percentile statistics.
    pub fn recompute(&mut self) {
        if self.samples.is_empty() {
            self.p50 = 0;
            self.p95 = 0;
            self.p99 = 0;
            self.max_observed = 0;
            return;
        }

        let mut sorted: Vec<u64> = self.samples.iter().copied().collect();
        sorted.sort_unstable();

        let len = sorted.len();
        self.p50 = percentile(&sorted, 50);
        self.p95 = percentile(&sorted, 95);
        self.p99 = percentile(&sorted, 99);
        self.max_observed = sorted[len - 1];
    }

    /// Number of samples currently retained.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }
}

/// Nearest-rank percentile on a **sorted** slice.
fn percentile(sorted: &[u64], pct: u32) -> u64 {
    debug_assert!(!sorted.is_empty());
    debug_assert!(pct <= 100);
    if sorted.len() == 1 {
        return sorted[0];
    }
    // Nearest-rank: index = ceil(pct/100 * N) - 1, clamped.
    let rank = ((pct as f64 / 100.0) * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

/// Policy that computes per-tool fuel budgets from historical profiles.
#[derive(Debug, Clone)]
pub struct FuelBudgetPolicy {
    profiles: HashMap<String, FuelProfile>,
    headroom_factor: f64,
    fallback_budget: u64,
}

impl FuelBudgetPolicy {
    /// Create a new policy with the given global fallback budget.
    ///
    /// The fallback is returned for any tool that does not yet have
    /// enough samples (`< MIN_SAMPLES`).
    pub fn new(fallback_budget: u64) -> Self {
        FuelBudgetPolicy {
            profiles: HashMap::new(),
            headroom_factor: DEFAULT_HEADROOM_FACTOR,
            fallback_budget,
        }
    }

    /// Builder: override the headroom factor (default 1.5).
    pub fn with_headroom(mut self, factor: f64) -> Self {
        self.headroom_factor = factor;
        self
    }

    /// Compute the fuel budget for `tool_name`.
    ///
    /// * Known tool with >= `MIN_SAMPLES`: `max(MIN_FUEL_FLOOR, p95 * headroom)`
    /// * Otherwise: `fallback_budget`
    pub fn budget_for(&self, tool_name: &str) -> u64 {
        if let Some(profile) = self.profiles.get(tool_name) {
            if profile.sample_count() >= MIN_SAMPLES {
                let raw = (profile.p95 as f64 * self.headroom_factor) as u64;
                return raw.max(MIN_FUEL_FLOOR);
            }
        }
        self.fallback_budget
    }

    /// Record a fuel consumption sample for `tool_name`.
    pub fn record(&mut self, tool_name: &str, fuel_consumed: u64) {
        self.profiles
            .entry(tool_name.to_string())
            .or_insert_with(FuelProfile::new)
            .push(fuel_consumed);
    }

    /// Returns `true` when `fuel_consumed` is a spike: more than 3x the
    /// tool's P95. Returns `false` for unknown tools or tools with
    /// insufficient data.
    pub fn is_anomalous(&self, tool_name: &str, fuel_consumed: u64) -> bool {
        if let Some(profile) = self.profiles.get(tool_name) {
            if profile.sample_count() >= MIN_SAMPLES && profile.p95 > 0 {
                return fuel_consumed > 3 * profile.p95;
            }
        }
        false
    }

    /// Inspect a tool's profile, if one exists.
    pub fn profile_for(&self, tool_name: &str) -> Option<&FuelProfile> {
        self.profiles.get(tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_single_element() {
        assert_eq!(percentile(&[42], 50), 42);
        assert_eq!(percentile(&[42], 95), 42);
    }

    #[test]
    fn percentile_known_distribution() {
        // 1..=100
        let sorted: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&sorted, 50), 50);
        assert_eq!(percentile(&sorted, 95), 95);
        assert_eq!(percentile(&sorted, 99), 99);
    }

    #[test]
    fn profile_recompute() {
        let mut p = FuelProfile::new();
        for v in 1..=20 {
            p.push(v * 1000);
        }
        assert_eq!(p.max_observed, 20_000);
        assert!(p.p50 > 0);
        assert!(p.p95 >= p.p50);
        assert!(p.p99 >= p.p95);
    }

    #[test]
    fn policy_fallback_for_unknown_tool() {
        let policy = FuelBudgetPolicy::new(5_000_000);
        assert_eq!(policy.budget_for("never_seen"), 5_000_000);
    }

    #[test]
    fn policy_fallback_until_min_samples() {
        let mut policy = FuelBudgetPolicy::new(5_000_000);
        for _ in 0..(MIN_SAMPLES - 1) {
            policy.record("tool_a", 50_000);
        }
        // Still not enough samples
        assert_eq!(policy.budget_for("tool_a"), 5_000_000);

        // One more pushes it over
        policy.record("tool_a", 50_000);
        assert_ne!(policy.budget_for("tool_a"), 5_000_000);
    }

    #[test]
    fn anomaly_detection_basic() {
        let mut policy = FuelBudgetPolicy::new(10_000_000);
        for _ in 0..10 {
            policy.record("stable", 50_000);
        }
        // 200_000 > 3 * 50_000 (=150_000)
        assert!(policy.is_anomalous("stable", 200_000));
        // 100_000 < 150_000 -> not anomalous
        assert!(!policy.is_anomalous("stable", 100_000));
    }
}
