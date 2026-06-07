//! Error Log for AI Feedback
//! 
//! Structured error information for AI model self-correction.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::health::{HealthStatus, ResourceSnapshot};

/// Structured error log for AI feedback
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorLog {
    pub id: String,
    pub error_type: String,
    pub timestamp: DateTime<Utc>,
    pub operation: String,
    pub description: String,
    pub system_state: SystemState,
    pub recovery_actions: Vec<String>,
    pub successful_patterns: Vec<String>,
    pub trigger_status: HealthStatus,
    pub resources: ResourceSnapshot,
}

impl ErrorLog {
    pub fn new(
        error_type: String,
        operation: String,
        description: String,
        trigger_status: HealthStatus,
        resources: ResourceSnapshot,
    ) -> Self {
        ErrorLog {
            id: uuid::Uuid::new_v4().to_string(),
            error_type,
            timestamp: Utc::now(),
            operation,
            description,
            system_state: SystemState::default(),
            recovery_actions: Vec::new(),
            successful_patterns: Vec::new(),
            trigger_status,
            resources,
        }
    }
    
    pub fn with_recovery(mut self, actions: Vec<String>) -> Self {
        self.recovery_actions = actions;
        self
    }
    
    pub fn with_patterns(mut self, patterns: Vec<String>) -> Self {
        self.successful_patterns = patterns;
        self
    }
    
    pub fn to_llm_context(&self) -> String {
        let mut ctx = String::new();
        
        ctx.push_str(&format!("## Error Detected\n"));
        ctx.push_str(&format!("Type: {}\n", self.error_type));
        ctx.push_str(&format!("Operation: {}\n", self.operation));
        ctx.push_str(&format!("Description: {}\n\n", self.description));
        
        ctx.push_str("## System State\n");
        ctx.push_str(&format!("CPU: {:.1}%\n", self.resources.cpu_usage));
        ctx.push_str(&format!("Memory: {} MB / {} MB\n\n", 
            self.resources.memory_used_mb, 
            self.resources.memory_limit_mb));
        
        if !self.recovery_actions.is_empty() {
            ctx.push_str("## Suggested Recovery Actions\n");
            for (i, action) in self.recovery_actions.iter().enumerate() {
                ctx.push_str(&format!("{}. {}\n", i + 1, action));
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