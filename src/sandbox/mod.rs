//! Sandbox Module
//!
//! WASM micro-sandbox with fuel metering for safe AI agent code execution.

pub mod fuel_budget;
pub mod fuel_meter;
pub mod wasi;
pub mod wasm_memory;
pub mod wasm_runtime;

pub use fuel_budget::{FuelBudgetPolicy, FuelProfile};
pub use fuel_meter::{presets, FuelMeter, FuelStats};
pub use wasi::{
    PreOpen, ValidatedWasiToolConfig, WasiAccess, WasiMount, WasiSandboxConfig, WasiToolConfig,
};
pub use wasm_memory::{MemoryStats, WasmExecutionSnapshot, WasmMemoryState};
pub use wasm_runtime::{ExecutionResult, SandboxConfig, StepCapture, WasmSandbox};
