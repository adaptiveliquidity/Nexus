//! Health Validator
//!
//! Validates execution health and detects issues.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use std::time::{Duration, Instant};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

/// Health status of an execution.
///
/// Phase A introduced `Trapped` and `InvalidModule` so the same enum can
/// classify guest-side WASM traps (deterministic, instance-recoverable) and
/// load/linking failures (no execution occurred, no rollback needed)
/// separately from genuine host-state `Corrupted` events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthStatus {
    Healthy,
    /// Genuine host-state corruption. Reserve for cases the rollback path
    /// is actually fixing real damage; not the default for guest traps.
    Corrupted,
    /// Wall-clock timeout (currently the 500 ms thread-based watchdog).
    Timeout,
    /// CPU/memory/stack budget exhausted (excluding fuel).
    ResourceExhausted,
    /// Fuel budget consumed (only emitted when fuel metering is enabled).
    FuelExhausted,
    /// Deterministic WASM trap (`unreachable`, divide-by-zero, etc.). The
    /// sandbox guard fired correctly and isolation held.
    Trapped,
    /// Module failed to load or link (missing entrypoint, validation error).
    /// No execution occurred, no state mutation could have happened.
    InvalidModule,
}

impl HealthStatus {
    pub fn is_healthy(&self) -> bool {
        matches!(self, HealthStatus::Healthy)
    }

    /// Whether this status requires rolling back captured state.
    /// `InvalidModule` returns false because nothing executed.
    pub fn requires_rollback(&self) -> bool {
        !matches!(self, HealthStatus::Healthy | HealthStatus::InvalidModule)
    }

    pub fn category(&self) -> &'static str {
        match self {
            HealthStatus::Healthy => "SUCCESS",
            HealthStatus::Corrupted => "STATE_CORRUPTION",
            HealthStatus::Timeout => "TIMEOUT",
            HealthStatus::ResourceExhausted => "RESOURCE_EXHAUSTED",
            HealthStatus::FuelExhausted => "INFINITE_LOOP_PREVENTED",
            HealthStatus::Trapped => "WASM_TRAP",
            HealthStatus::InvalidModule => "INVALID_MODULE",
        }
    }
}

/// Health validation configuration
#[derive(Debug, Clone)]
pub struct HealthConfig {
    pub max_cpu_percent: f32,
    pub max_memory_growth_ratio: f64,
    pub timeout: Duration,
    pub cpu_spike_threshold: f32,
    /// Whether a high *host* CPU reading may flip a guest execution into a
    /// failure. Off by default: a sandbox guest's success must not depend on
    /// ambient host load, and host-global CPU cannot be attributed to the
    /// guest. Memory-critical corruption is always checked regardless of this.
    pub fail_on_host_cpu: bool,
}

impl Default for HealthConfig {
    fn default() -> Self {
        HealthConfig {
            max_cpu_percent: 95.0,
            max_memory_growth_ratio: 3.0,
            timeout: Duration::from_secs(30),
            cpu_spike_threshold: 50.0,
            fail_on_host_cpu: false,
        }
    }
}

/// System resource snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceSnapshot {
    pub cpu_usage: f32,
    pub memory_used_mb: u64,
    pub memory_limit_mb: u64,
    pub timestamp: DateTime<Utc>,
}

impl ResourceSnapshot {
    pub fn memory_usage_percent(&self) -> f64 {
        if self.memory_limit_mb == 0 {
            return 0.0;
        }
        (self.memory_used_mb as f64 / self.memory_limit_mb as f64) * 100.0
    }
}

/// Health validator
pub struct HealthValidator {
    config: HealthConfig,
    system: RwLock<System>,
    baseline: RwLock<Option<ResourceSnapshot>>,
    start: RwLock<Option<Instant>>,
    /// Timestamp of the last CPU refresh. sysinfo computes CPU usage as the
    /// delta between two refreshes spaced >= `MINIMUM_CPU_UPDATE_INTERVAL`;
    /// we only refresh CPU once per interval so the reading is meaningful.
    last_cpu_refresh: RwLock<Option<Instant>>,
    /// Last valid CPU reading. Initialised to 0.0 so that before the first
    /// valid interval elapses we report healthy rather than a spurious value.
    last_cpu_usage: RwLock<f32>,
}

impl HealthValidator {
    pub fn new(config: HealthConfig) -> Self {
        HealthValidator {
            config,
            system: RwLock::new(System::new_all()),
            baseline: RwLock::new(None),
            start: RwLock::new(None),
            last_cpu_refresh: RwLock::new(None),
            last_cpu_usage: RwLock::new(0.0),
        }
    }

    pub fn start_execution(&self) {
        let resources = self.capture();
        *self.baseline.write().unwrap() = Some(resources);
        *self.start.write().unwrap() = Some(Instant::now());
    }

    fn capture(&self) -> ResourceSnapshot {
        let mut sys = self.system.write().unwrap();

        // Memory is accurate from a single refresh.
        sys.refresh_specifics(RefreshKind::new().with_memory(MemoryRefreshKind::everything()));

        // CPU usage is the *delta* between two refreshes spaced at least
        // `sysinfo::MINIMUM_CPU_UPDATE_INTERVAL` apart. Refreshing more often
        // yields a meaningless value (it can read ~100% on the first shot), so
        // we refresh CPU at most once per interval and reuse the last good
        // reading otherwise. Before the first valid interval elapses the cached
        // value is 0.0, so host load is never misread as a maxed-out CPU.
        let now = Instant::now();
        let interval_elapsed = self
            .last_cpu_refresh
            .read()
            .unwrap()
            .map(|t| now.duration_since(t) >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL)
            .unwrap_or(false);

        let cpu_usage = if interval_elapsed {
            sys.refresh_specifics(RefreshKind::new().with_cpu(CpuRefreshKind::everything()));
            let usage = sys.global_cpu_usage();
            *self.last_cpu_usage.write().unwrap() = usage;
            *self.last_cpu_refresh.write().unwrap() = Some(now);
            usage
        } else {
            // Anchor the first refresh so the interval can begin to elapse, but
            // do not trust this (invalid) sub-interval reading yet.
            if self.last_cpu_refresh.read().unwrap().is_none() {
                sys.refresh_specifics(RefreshKind::new().with_cpu(CpuRefreshKind::everything()));
                *self.last_cpu_refresh.write().unwrap() = Some(now);
            }
            *self.last_cpu_usage.read().unwrap()
        };

        ResourceSnapshot {
            cpu_usage,
            memory_used_mb: sys.used_memory() / 1024,
            memory_limit_mb: sys.total_memory() / 1024,
            timestamp: Utc::now(),
        }
    }

    pub fn validate(&self) -> HealthStatus {
        if let Some(start) = *self.start.read().unwrap() {
            if start.elapsed() > self.config.timeout {
                return HealthStatus::Timeout;
            }
        }

        let current = self.capture();

        if let Some(baseline) = self.baseline.read().unwrap().as_ref() {
            // Host CPU is only allowed to flip a guest when explicitly opted in
            // (see `HealthConfig::fail_on_host_cpu`). Host-global CPU cannot be
            // attributed to the guest, so it is off by default.
            if self.config.fail_on_host_cpu {
                if current.cpu_usage > self.config.max_cpu_percent {
                    return HealthStatus::ResourceExhausted;
                }

                let spike = current.cpu_usage - baseline.cpu_usage;
                if spike > self.config.cpu_spike_threshold {
                    return HealthStatus::ResourceExhausted;
                }
            }

            let growth = if baseline.memory_used_mb > 0 {
                current.memory_used_mb as f64 / baseline.memory_used_mb as f64
            } else {
                1.0
            };

            if growth > self.config.max_memory_growth_ratio {
                return HealthStatus::ResourceExhausted;
            }
        }

        HealthStatus::Healthy
    }

    pub fn check_corruption(&self) -> Option<String> {
        let resources = self.capture();

        if resources.memory_usage_percent() > 99.0 {
            return Some("Memory usage critical".to_string());
        }

        // Host CPU load is not guest corruption; only flag it when explicitly
        // opted in (default off). With a valid CPU reading (see `capture`) this
        // would catch a genuinely pegged host, but it never flips a guest's
        // success unless `fail_on_host_cpu` is set.
        if self.config.fail_on_host_cpu && resources.cpu_usage > 99.0 {
            return Some("CPU usage stuck at maximum".to_string());
        }

        None
    }

    pub fn reset(&self) {
        *self.baseline.write().unwrap() = None;
        *self.start.write().unwrap() = None;
    }

    pub fn current_resources(&self) -> ResourceSnapshot {
        self.capture()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_status() {
        assert!(HealthStatus::Healthy.is_healthy());
        assert!(HealthStatus::Corrupted.requires_rollback());
    }

    /// Part B: with the default config a high host CPU reading must never be
    /// reported as guest corruption or ill health. Regression for the false
    /// `HostError` that flipped successful executions on loaded hosts.
    #[test]
    fn cold_host_cpu_is_not_corruption_by_default() {
        let v = HealthValidator::new(HealthConfig::default());
        v.start_execution();
        assert_eq!(
            v.check_corruption(),
            None,
            "host CPU must not be reported as corruption by default"
        );
        assert!(
            v.validate().is_healthy(),
            "host CPU must not flip health by default"
        );
    }

    /// Part A: even when host-CPU failure is explicitly enabled, a freshly
    /// constructed validator must not false-positive on the first (invalid,
    /// sub-interval) sysinfo sample — the cached reading is 0.0 until a valid
    /// CPU delta is available.
    #[test]
    fn cold_cpu_sample_is_not_trusted_even_when_enabled() {
        let cfg = HealthConfig {
            fail_on_host_cpu: true,
            ..HealthConfig::default()
        };
        let v = HealthValidator::new(cfg);
        v.start_execution();
        // First reading is the cold cache (0.0), so no spurious "stuck at
        // maximum" regardless of real host load.
        assert_eq!(v.check_corruption(), None);
        assert!(v.current_resources().cpu_usage < 99.0);
    }
}
