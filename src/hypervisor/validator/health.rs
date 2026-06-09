//! Health Validator
//! 
//! Validates execution health and detects issues.

use std::sync::RwLock;
use std::time::{Duration, Instant};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
}

impl Default for HealthConfig {
    fn default() -> Self {
        HealthConfig {
            max_cpu_percent: 95.0,
            max_memory_growth_ratio: 3.0,
            timeout: Duration::from_secs(30),
            cpu_spike_threshold: 50.0,
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
}

impl HealthValidator {
    pub fn new(config: HealthConfig) -> Self {
        HealthValidator {
            config,
            system: RwLock::new(System::new_all()),
            baseline: RwLock::new(None),
            start: RwLock::new(None),
        }
    }
    
    pub fn start_execution(&self) {
        let resources = self.capture();
        *self.baseline.write().unwrap() = Some(resources);
        *self.start.write().unwrap() = Some(Instant::now());
    }
    
    fn capture(&self) -> ResourceSnapshot {
        let mut sys = self.system.write().unwrap();
        sys.refresh_specifics(
            RefreshKind::new()
                .with_cpu(CpuRefreshKind::everything())
                .with_memory(MemoryRefreshKind::everything()),
        );
        
        ResourceSnapshot {
            cpu_usage: sys.global_cpu_usage(),
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
            if current.cpu_usage > self.config.max_cpu_percent {
                return HealthStatus::ResourceExhausted;
            }
            
            let spike = current.cpu_usage - baseline.cpu_usage;
            if spike > self.config.cpu_spike_threshold {
                return HealthStatus::ResourceExhausted;
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
        
        if resources.cpu_usage > 99.0 {
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
}