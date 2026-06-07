//! Validator Module
//! 
//! Health validation and error logging.

pub mod health;
pub mod error_log;

pub use health::{HealthValidator, HealthConfig, HealthStatus, ResourceSnapshot};
pub use error_log::{ErrorLog, SystemState};