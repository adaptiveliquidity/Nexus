//! Validator Module
//!
//! Health validation and error logging.

pub mod error_log;
pub mod health;

pub use error_log::{ErrorLog, SystemState};
pub use health::{HealthConfig, HealthStatus, HealthValidator, ResourceSnapshot};
