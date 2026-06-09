//! Fuel Metering for Infinite Loop Prevention
//!
//! Provides configurable fuel limits and monitoring for WASM execution.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Fuel consumption statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuelStats {
    /// Total fuel available
    pub total_fuel: u64,
    /// Fuel consumed in current execution
    pub consumed: u64,
    /// Fuel remaining
    pub remaining: u64,
    /// Percentage consumed
    pub percentage: f64,
    /// Timestamp of last update
    pub last_update: DateTime<Utc>,
}

impl FuelStats {
    pub fn new(total: u64, consumed: u64) -> Self {
        let remaining = total.saturating_sub(consumed);
        let percentage = if total > 0 {
            (consumed as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        FuelStats {
            total_fuel: total,
            consumed,
            remaining,
            percentage,
            last_update: Utc::now(),
        }
    }
}

/// Fuel meter that tracks consumption and enforces limits
pub struct FuelMeter {
    /// Total fuel allocation
    total: u64,
    /// Remaining fuel (atomic for concurrent access)
    remaining: Arc<AtomicU64>,
    /// Fuel consumption rate (for prediction)
    consumption_rate: AtomicU64,
    /// Execution start time
    start_time: std::time::Instant,
    /// Operations count
    operations: AtomicU64,
}

impl FuelMeter {
    /// Create a new fuel meter
    pub fn new(total_fuel: u64) -> Self {
        FuelMeter {
            total: total_fuel,
            remaining: Arc::new(AtomicU64::new(total_fuel)),
            consumption_rate: AtomicU64::new(0),
            start_time: std::time::Instant::now(),
            operations: AtomicU64::new(0),
        }
    }

    /// Consume fuel (returns false if exhausted)
    pub fn consume(&self, amount: u64) -> bool {
        let mut remaining = self.remaining.load(Ordering::Acquire);

        loop {
            if remaining < amount {
                return false;
            }

            match self.remaining.compare_exchange(
                remaining,
                remaining - amount,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => remaining = current,
            }
        }

        self.operations.fetch_add(1, Ordering::SeqCst);

        // Update consumption rate
        let elapsed = self.start_time.elapsed().as_secs();
        if let Some(rate) = self.operations.load(Ordering::SeqCst).checked_div(elapsed) {
            self.consumption_rate.store(rate, Ordering::SeqCst);
        }

        true
    }

    /// Check if fuel is available
    pub fn has_fuel(&self, amount: u64) -> bool {
        self.remaining.load(Ordering::SeqCst) >= amount
    }

    /// Get current stats
    pub fn stats(&self) -> FuelStats {
        let consumed = self.total - self.remaining.load(Ordering::SeqCst);
        FuelStats::new(self.total, consumed)
    }

    /// Reset to full fuel
    pub fn reset(&mut self) {
        self.remaining.store(self.total, Ordering::SeqCst);
        self.start_time = std::time::Instant::now();
        self.operations.store(0, Ordering::SeqCst);
    }

    /// Predict remaining execution time
    pub fn predict_remaining_time(&self) -> Option<std::time::Duration> {
        let rate = self.consumption_rate.load(Ordering::SeqCst);
        if rate == 0 {
            return None;
        }

        let remaining = self.remaining.load(Ordering::SeqCst);
        let seconds_remaining = remaining / rate;
        Some(std::time::Duration::from_secs(seconds_remaining))
    }

    /// Get warning level (0-100)
    pub fn warning_level(&self) -> u8 {
        let stats = self.stats();
        match stats.percentage {
            p if p >= 90.0 => 100,
            p if p >= 75.0 => 75,
            p if p >= 50.0 => 50,
            p if p >= 25.0 => 25,
            _ => 0,
        }
    }
}

/// Predefined fuel configurations for common scenarios
pub mod presets {
    /// Quick execution (file read, simple calc)
    pub fn quick() -> u64 {
        100_000 // 100k instructions
    }

    /// Normal execution (moderate computation)
    pub fn normal() -> u64 {
        10_000_000 // 10M instructions
    }

    /// Heavy computation (complex algorithms)
    pub fn heavy() -> u64 {
        100_000_000 // 100M instructions
    }

    /// Code generation (LLM-style)
    pub fn code_gen() -> u64 {
        50_000_000 // 50M instructions
    }

    /// Infinite loop prevention (very low)
    pub fn strict() -> u64 {
        10_000 // 10k instructions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuel_consumption() {
        let meter = FuelMeter::new(1000);

        assert!(meter.consume(100));
        assert!(meter.has_fuel(100));
        assert!(!meter.has_fuel(1000));

        let stats = meter.stats();
        assert_eq!(stats.consumed, 100);
        assert_eq!(stats.remaining, 900);
    }

    #[test]
    fn test_fuel_exhaustion() {
        let meter = FuelMeter::new(100);

        assert!(meter.consume(50));
        assert!(!meter.consume(100)); // Should fail

        let stats = meter.stats();
        assert_eq!(stats.consumed, 50);
        assert_eq!(stats.remaining, 50);
        assert_eq!(stats.percentage, 50.0);
    }

    #[test]
    fn test_reset() {
        let mut meter = FuelMeter::new(1000);
        meter.consume(500);
        meter.reset();

        let stats = meter.stats();
        assert_eq!(stats.remaining, 1000);
    }

    #[test]
    fn test_warning_levels() {
        let meter = FuelMeter::new(100);
        meter.consume(90);
        assert_eq!(meter.warning_level(), 100);
    }
}
