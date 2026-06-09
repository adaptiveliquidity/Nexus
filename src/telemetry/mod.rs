//! Telemetry Module
//! 
//! Execution tracking and AI feedback for self-correction.

use std::sync::RwLock;
use std::collections::VecDeque;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::hypervisor::validator::health::{HealthStatus, ResourceSnapshot};
use crate::hypervisor::validator::error_log::ErrorLog;

/// An execution record for history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub operation: String,
    pub success: bool,
    pub duration_ms: u64,
    pub fuel_consumed: u64,
    pub health_status: HealthStatus,
    pub error: Option<ErrorLog>,
    pub resources: ResourceSnapshot,
}

impl ExecutionRecord {
    /// Build a success record. The caller is expected to pass a real
    /// `ResourceSnapshot` from `HealthValidator::current_resources()`;
    /// passing a zero-filled placeholder is what Phase A explicitly
    /// removed, since it made the report's per-execution resource numbers
    /// useless.
    pub fn success(
        operation: String,
        duration_ms: u64,
        fuel_consumed: u64,
        resources: ResourceSnapshot,
    ) -> Self {
        ExecutionRecord {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            operation,
            success: true,
            duration_ms,
            fuel_consumed,
            health_status: HealthStatus::Healthy,
            error: None,
            resources,
        }
    }

    /// Build a failure record. The `ResourceSnapshot` on the embedded
    /// `ErrorLog` is reused so the record and the error log agree.
    pub fn failure(
        operation: String,
        error: ErrorLog,
        duration_ms: u64,
        fuel_consumed: u64,
    ) -> Self {
        let resources = error.resources.clone();
        ExecutionRecord {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            operation,
            success: false,
            duration_ms,
            fuel_consumed,
            health_status: error.trigger_status.clone(),
            error: Some(error),
            resources,
        }
    }
}

/// Telemetry sink for AI agent feedback
pub struct TelemetrySink {
    /// Execution history (ring buffer)
    history: RwLock<VecDeque<ExecutionRecord>>,
    /// Maximum history size
    max_history: usize,
    /// Successful patterns learned
    patterns: RwLock<Vec<LearnedPattern>>,
    /// Statistics
    stats: RwLock<TelemetryStats>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelemetryStats {
    pub total_executions: u64,
    pub successful_executions: u64,
    pub failed_executions: u64,
    pub total_rollbacks: u64,
    pub avg_duration_ms: f64,
    pub avg_fuel_per_execution: f64,
    pub success_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedPattern {
    pub operation: String,
    pub pattern: String,
    pub success_count: u64,
    pub last_used: DateTime<Utc>,
}

impl TelemetrySink {
    pub fn new(max_history: usize) -> Self {
        TelemetrySink {
            history: RwLock::new(VecDeque::with_capacity(max_history)),
            max_history,
            patterns: RwLock::new(Vec::new()),
            stats: RwLock::new(TelemetryStats::default()),
        }
    }
    
    /// Record a successful execution
    pub fn record_success(&self, record: ExecutionRecord) {
        self.add_to_history(record.clone());
        self.update_pattern(&record.operation, true);
        self.update_stats(|s| {
            s.total_executions += 1;
            s.successful_executions += 1;
            s.avg_duration_ms = (s.avg_duration_ms * (s.total_executions - 1) as f64 
                + record.duration_ms as f64) / s.total_executions as f64;
            s.avg_fuel_per_execution = (s.avg_fuel_per_execution * (s.total_executions - 1) as f64 
                + record.fuel_consumed as f64) / s.total_executions as f64;
            s.success_rate = s.successful_executions as f64 / s.total_executions as f64;
        });
    }
    
    /// Record a failed execution
    pub fn record_failure(&self, record: ExecutionRecord) {
        self.add_to_history(record.clone());
        self.update_pattern(&record.operation, false);
        self.update_stats(|s| {
            s.total_executions += 1;
            s.failed_executions += 1;
            if record.error.is_some() {
                s.total_rollbacks += 1;
            }
            s.avg_duration_ms = (s.avg_duration_ms * (s.total_executions - 1) as f64 
                + record.duration_ms as f64) / s.total_executions as f64;
            s.success_rate = s.successful_executions as f64 / s.total_executions as f64;
        });
    }
    
    fn add_to_history(&self, record: ExecutionRecord) {
        let mut history = self.history.write().unwrap();
        if history.len() >= self.max_history {
            history.pop_front();
        }
        history.push_back(record);
    }
    
    fn update_pattern(&self, operation: &str, success: bool) {
        let mut patterns = self.patterns.write().unwrap();

        if let Some(existing) = patterns.iter_mut().find(|p| p.operation == operation) {
            if success {
                existing.success_count = existing.success_count.saturating_add(1);
            } else {
                // Phase A: erode instinct on failure instead of resetting to
                // zero. A single bad run shouldn't wipe out the entire
                // learned history of an operation.
                existing.success_count = existing.success_count.saturating_sub(1);
            }
            existing.last_used = Utc::now();
        } else if success {
            patterns.push(LearnedPattern {
                operation: operation.to_string(),
                pattern: "initial_success".to_string(),
                success_count: 1,
                last_used: Utc::now(),
            });
        }
    }
    
    fn update_stats<F>(&self, f: F)
    where
        F: FnOnce(&mut TelemetryStats),
    {
        let mut stats = self.stats.write().unwrap();
        f(&mut stats);
    }
    
    /// Get execution history
    pub fn get_history(&self, limit: Option<usize>) -> Vec<ExecutionRecord> {
        let history = self.history.read().unwrap();
        let limit = limit.unwrap_or(history.len());
        history.iter().rev().take(limit).cloned().collect()
    }
    
    /// Get successful patterns for an operation
    pub fn get_patterns(&self, operation: &str) -> Vec<String> {
        let patterns = self.patterns.read().unwrap();
        patterns
            .iter()
            .filter(|p| p.operation == operation && p.success_count > 0)
            .map(|p| p.pattern.clone())
            .collect()
    }
    
    /// Get telemetry statistics
    pub fn stats(&self) -> TelemetryStats {
        self.stats.read().unwrap().clone()
    }
    
    /// Generate AI feedback context for an error
    pub fn generate_feedback(&self, operation: &str) -> String {
        let patterns = self.get_patterns(operation);
        let recent = self.get_history(Some(5));
        
        let mut feedback = String::new();
        
        if !patterns.is_empty() {
            feedback.push_str("## Previously Successful Approaches\n");
            for pattern in patterns.iter().take(3) {
                feedback.push_str(&format!("- {}\n", pattern));
            }
            feedback.push('\n');
        }
        
        feedback.push_str("## Recent Executions\n");
        for record in recent.iter().take(3) {
            let status = if record.success { "✓" } else { "✗" };
            feedback.push_str(&format!(
                "{} {} ({}ms)\n", 
                status, 
                record.operation, 
                record.duration_ms
            ));
        }
        
        feedback
    }
    
    /// Get all learned patterns
    pub fn all_patterns(&self) -> Vec<LearnedPattern> {
        self.patterns.read().unwrap().clone()
    }
    
    /// Clear history (for privacy)
    pub fn clear_history(&self) {
        self.history.write().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_resources() -> ResourceSnapshot {
        ResourceSnapshot {
            cpu_usage: 1.0,
            memory_used_mb: 10,
            memory_limit_mb: 1024,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_telemetry_recording() {
        let sink = TelemetrySink::new(10);

        let success = ExecutionRecord::success("read_file".into(), 50, 1000, fake_resources());
        sink.record_success(success);

        let stats = sink.stats();
        assert_eq!(stats.total_executions, 1);
        assert_eq!(stats.successful_executions, 1);
        assert_eq!(stats.success_rate, 1.0);
    }

    #[test]
    fn test_pattern_learning() {
        let sink = TelemetrySink::new(10);
        for _ in 0..3 {
            let record =
                ExecutionRecord::success("execute_command".into(), 100, 5000, fake_resources());
            sink.record_success(record);
        }
        let patterns = sink.get_patterns("execute_command");
        assert!(!patterns.is_empty());
    }

    #[test]
    fn test_feedback_generation() {
        let sink = TelemetrySink::new(10);
        let record = ExecutionRecord::success("test_op".into(), 100, 1000, fake_resources());
        sink.record_success(record);
        let feedback = sink.generate_feedback("test_op");
        assert!(
            feedback.contains("Previously Successful Approaches")
                || feedback.contains("Recent Executions"),
            "got: {feedback}"
        );
    }

    #[test]
    fn pattern_decrement_does_not_wipe_history() {
        // Phase A: failure should erode, not erase, the learned count.
        let sink = TelemetrySink::new(10);
        for _ in 0..5 {
            sink.record_success(ExecutionRecord::success(
                "op".into(),
                10,
                100,
                fake_resources(),
            ));
        }
        // Build a failure record so we can call record_failure with the new
        // 4-arg signature. We only care about the side effect on patterns.
        let mode = crate::hypervisor::FailureMode::TrapDivByZero;
        let err = ErrorLog::new("op".into(), mode, fake_resources());
        sink.record_failure(ExecutionRecord::failure("op".into(), err, 10, 0));

        let stored = sink.all_patterns();
        let op = stored.iter().find(|p| p.operation == "op").unwrap();
        // Was 5, one failure decrements by 1 -> 4 (not 0 as before).
        assert_eq!(op.success_count, 4);
    }
}