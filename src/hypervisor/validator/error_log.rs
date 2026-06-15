//! Error Log for AI Feedback
//!
//! Structured error information for AI model self-correction.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::health::{HealthStatus, ResourceSnapshot};
use crate::hypervisor::failure_mode::FailureMode;
use crate::hypervisor::recovery::RecoveryAction;
use crate::telemetry::CapturedCallStack;

/// Structured error log for AI feedback.
///
/// Phase A: `failure_mode` is now a typed enum (was missing) and
/// `recovery_actions` carries structured `RecoveryAction` values (was
/// `Vec<String>` of hardcoded fallback strings).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorLog {
    pub id: String,
    pub error_type: String,
    pub timestamp: DateTime<Utc>,
    pub operation: String,
    pub description: String,
    pub system_state: SystemState,
    /// Typed failure classification produced by `FailureMode::from_*`.
    pub failure_mode: FailureMode,
    /// Structured recovery suggestions from the active `RecoveryPolicy`.
    pub recovery_actions: Vec<RecoveryAction>,
    pub successful_patterns: Vec<String>,
    /// Diagnostic-only WASM frames captured at trap/checkpoint sites.
    ///
    /// This is never rollback state and must not influence snapshot equality,
    /// memory checksums, or content-addressed snapshot digests.
    pub call_stack: Option<CapturedCallStack>,
    pub trigger_status: HealthStatus,
    pub resources: ResourceSnapshot,
}

impl ErrorLog {
    pub fn new(operation: String, failure_mode: FailureMode, resources: ResourceSnapshot) -> Self {
        let trigger_status: HealthStatus = (&failure_mode).into();
        let error_type = failure_mode.category().to_string();
        let description = failure_mode.describe();
        ErrorLog {
            id: uuid::Uuid::new_v4().to_string(),
            error_type,
            timestamp: Utc::now(),
            operation,
            description,
            system_state: SystemState::default(),
            failure_mode,
            recovery_actions: Vec::new(),
            successful_patterns: Vec::new(),
            call_stack: None,
            trigger_status,
            resources,
        }
    }

    pub fn with_recovery(mut self, actions: Vec<RecoveryAction>) -> Self {
        self.recovery_actions = actions;
        self
    }

    pub fn with_patterns(mut self, patterns: Vec<String>) -> Self {
        self.successful_patterns = patterns;
        self
    }

    pub fn with_system_state(mut self, system_state: SystemState) -> Self {
        self.system_state = system_state;
        self
    }

    pub fn with_call_stack(mut self, call_stack: Option<CapturedCallStack>) -> Self {
        self.call_stack = call_stack;
        self
    }

    pub fn to_llm_context(&self) -> String {
        let mut ctx = String::new();

        ctx.push_str("## Error Detected\n");
        ctx.push_str(&format!("Type: {}\n", self.error_type));
        ctx.push_str(&format!("Operation: {}\n", self.operation));
        ctx.push_str(&format!("Description: {}\n\n", self.description));

        if let Some(call_stack) = &self.call_stack {
            if !call_stack.is_empty() {
                ctx.push_str("## WASM Call Stack\n");
                ctx.push_str(&format!("Captured at: {:?}\n", call_stack.captured_at));
                for (i, frame) in call_stack.top_frames(8).enumerate() {
                    let func = frame.func_name.as_deref().unwrap_or("<unknown>");
                    let module = frame.module_name.as_deref().unwrap_or("<unknown>");
                    let offset = frame
                        .module_offset
                        .map(|o| format!(" module_offset=0x{o:x}"))
                        .unwrap_or_default();
                    ctx.push_str(&format!(
                        "{}. {module}!{func} func_index={}{}\n",
                        i + 1,
                        frame.func_index,
                        offset
                    ));
                }
                ctx.push('\n');
            }
        }

        ctx.push_str("## System State\n");
        ctx.push_str(&format!("CPU: {:.1}%\n", self.resources.cpu_usage));
        ctx.push_str(&format!(
            "Memory: {} MB / {} MB\n\n",
            self.resources.memory_used_mb, self.resources.memory_limit_mb
        ));

        if !self.recovery_actions.is_empty() {
            ctx.push_str("## Suggested Recovery Actions\n");
            for (i, action) in self.recovery_actions.iter().enumerate() {
                let nr = if action.non_retryable {
                    " [non-retryable]"
                } else {
                    ""
                };
                ctx.push_str(&format!(
                    "{}. ({:?}, conf={:.2}){nr} {}\n",
                    i + 1,
                    action.source,
                    action.confidence,
                    action.description
                ));
            }
            ctx.push('\n');
        }

        if !self.successful_patterns.is_empty() {
            ctx.push_str("## Previously Successful Approaches\n");
            for pattern in &self.successful_patterns {
                ctx.push_str(&format!("- {}\n", pattern));
            }
        }

        ctx
    }
}

/// System state at error time
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemState {
    pub files_affected: Vec<String>,
    pub reverted: bool,
    pub snapshot_id: Option<String>,
    pub execution_time_ms: u64,
}
