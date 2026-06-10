//! WASM Micro-Sandbox Runtime
//!
//! High-performance WebAssembly sandbox with fuel metering for AI agent execution.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wasmtime::{Config, Engine, Linker, Module, Store};

use crate::error::{NexusError, Result};
use crate::hypervisor::failure_mode::FailureMode;

/// Configuration for the WASM sandbox
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Maximum fuel (instructions) before termination
    pub max_fuel: u64,
    /// Maximum memory in WASM pages (64KB each)
    pub max_memory_pages: u32,
    /// Maximum execution time
    pub time_limit: Duration,
    /// Pre-initialized WASM module bytes
    pub module_bytes: Option<Vec<u8>>,
    /// Enable WASI (files, network)
    pub enable_wasi: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        SandboxConfig {
            max_fuel: 10_000_000,                   // 10 million instructions
            max_memory_pages: 512,                  // 32MB
            time_limit: Duration::from_millis(500), // 500ms for fast demo
            module_bytes: None,
            enable_wasi: true,
        }
    }
}

/// Execution result from WASM sandbox.
///
/// On failure, `failure_mode` carries the typed taxonomy entry (introduced
/// in Phase A) so callers do not have to substring-match `error`.
///
/// `pre_call_memory` (Phase A) carries the actual bytes of the instance's
/// `"memory"` export, captured *after* instantiation but *before* the
/// entrypoint runs. The hypervisor uses these bytes to build a real
/// snapshot instead of the prior hardcoded `vec![0u8; 65536]` placeholder.
/// `None` means the module had no `"memory"` export, or instantiation
/// itself failed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Whether execution succeeded
    pub success: bool,
    /// Return value if any
    pub return_value: Option<Vec<u8>>,
    /// Fuel consumed
    pub fuel_consumed: u64,
    /// Time taken
    pub duration_ms: u64,
    /// Error message if failed (human-readable, derived from `failure_mode`)
    pub error: Option<String>,
    /// Typed failure classification (only `Some` when `success == false`)
    pub failure_mode: Option<FailureMode>,
    /// Pre-call WASM linear memory (post-instantiation snapshot input)
    pub pre_call_memory: Option<Vec<u8>>,
    /// Number of system function calls
    pub syscall_count: u32,
}

impl ExecutionResult {
    /// Create a successful result
    pub fn success(return_value: Vec<u8>, fuel_consumed: u64, duration_ms: u64) -> Self {
        ExecutionResult {
            success: true,
            return_value: Some(return_value),
            fuel_consumed,
            duration_ms,
            error: None,
            failure_mode: None,
            pre_call_memory: None,
            syscall_count: 0,
        }
    }

    pub fn with_pre_call_memory(mut self, mem: Option<Vec<u8>>) -> Self {
        self.pre_call_memory = mem;
        self
    }

    /// Create a failure result from a typed `FailureMode`. The error string
    /// is generated from the failure mode so the two stay consistent.
    pub fn failure_from_mode(mode: FailureMode, fuel_consumed: u64, duration_ms: u64) -> Self {
        let error = Some(mode.describe());
        ExecutionResult {
            success: false,
            return_value: None,
            fuel_consumed,
            duration_ms,
            error,
            failure_mode: Some(mode),
            pre_call_memory: None,
            syscall_count: 0,
        }
    }

    /// Back-compat shim. New code should call `failure_from_mode`.
    pub fn failure(error: String, fuel_consumed: u64) -> Self {
        ExecutionResult {
            success: false,
            return_value: None,
            fuel_consumed,
            duration_ms: 0,
            error: Some(error.clone()),
            failure_mode: Some(FailureMode::HostError(error)),
            pre_call_memory: None,
            syscall_count: 0,
        }
    }
}

/// WASM Micro-Sandbox with fuel metering
pub struct WasmSandbox {
    engine: Arc<Engine>,
    config: SandboxConfig,
}

/// Reply payload sent from the worker thread to the timeout-bounded receiver.
/// Carries a typed `FailureMode` so callers do not have to substring-match.
/// `pre_call_memory` is populated whenever instantiation succeeded so the
/// hypervisor can build a real snapshot from the actual WASM linear memory.
enum ExecReply {
    Ok {
        fuel_consumed: u64,
        pre_call_memory: Option<Vec<u8>>,
    },
    Failed {
        mode: FailureMode,
        fuel_consumed: u64,
        pre_call_memory: Option<Vec<u8>>,
    },
}

impl WasmSandbox {
    /// Create a new WASM sandbox.
    ///
    /// Phase A: fuel metering is now enabled in the engine config; combined
    /// with `Store::set_fuel` in `execute` this turns the prior 500 ms
    /// wall-clock-only watchdog into a fuel + wall-clock combination.
    pub fn new(config: SandboxConfig) -> Result<Self> {
        let mut cfg = Config::new();
        cfg.consume_fuel(true);

        let engine = Engine::new(&cfg)
            .map_err(|e| NexusError::ConfigError(format!("Failed to create engine: {}", e)))?;

        Ok(WasmSandbox {
            engine: Arc::new(engine),
            config,
        })
    }

    /// Execute WASM code with fuel + timeout metering.
    ///
    /// Returns a typed `FailureMode` via `ExecutionResult.failure_mode` on
    /// every failure path so the hypervisor can derive the correct
    /// `HealthStatus` and recovery actions without substring matching.
    pub fn execute(&self, wasm_bytes: &[u8], _args: &[Vec<u8>]) -> Result<ExecutionResult> {
        let start = Instant::now();
        let max_fuel = self.config.max_fuel;
        let time_limit = self.config.time_limit;

        // Module compilation failures are load-time errors with no execution.
        let module = match Module::from_binary(&self.engine, wasm_bytes) {
            Ok(m) => m,
            Err(e) => {
                let mode = FailureMode::InvalidModule(e.to_string());
                return Ok(ExecutionResult::failure_from_mode(
                    mode,
                    0,
                    start.elapsed().as_millis() as u64,
                ));
            }
        };

        let engine = self.engine.clone();

        let (tx, rx) = std::sync::mpsc::channel::<ExecReply>();

        let handle = std::thread::spawn(move || {
            let state = WasmState::new(max_fuel);
            let mut store = Store::new(&engine, state);

            // With consume_fuel(true) in Config, set_fuel is required and
            // succeeds; failures here mean the engine config drifted.
            if let Err(e) = store.set_fuel(max_fuel) {
                let _ = tx.send(ExecReply::Failed {
                    mode: FailureMode::HostError(format!("set_fuel failed: {e}")),
                    fuel_consumed: 0,
                    pre_call_memory: None,
                });
                return;
            }

            let linker = Linker::new(&engine);

            let instance = match linker.instantiate(&mut store, &module) {
                Ok(i) => i,
                Err(e) => {
                    let _ = tx.send(ExecReply::Failed {
                        mode: FailureMode::InvalidModule(format!("instantiate failed: {e}")),
                        fuel_consumed: 0,
                        pre_call_memory: None,
                    });
                    return;
                }
            };

            // Phase A: capture the *real* WASM linear memory right after
            // instantiation. This is what the hypervisor needs to build a
            // snapshot it can actually roll back to. `None` here means the
            // module has no `"memory"` export, which is legal.
            let pre_call_memory: Option<Vec<u8>> = instance
                .get_memory(&mut store, "memory")
                .map(|m| m.data(&store).to_vec());

            // Resolve entrypoint: prefer `_start`, fall back to `main`.
            let start_func = match instance.get_typed_func::<(), ()>(&mut store, "_start") {
                Ok(f) => f,
                Err(_) => match instance.get_typed_func::<(), ()>(&mut store, "main") {
                    Ok(f) => f,
                    Err(_) => {
                        let _ = tx.send(ExecReply::Failed {
                            mode: FailureMode::MissingEntrypoint {
                                expected: "_start".into(),
                            },
                            fuel_consumed: 0,
                            pre_call_memory,
                        });
                        return;
                    }
                },
            };

            let call_result = start_func.call(&mut store, ());
            // Compute fuel consumption regardless of outcome.
            let fuel_remaining = store.get_fuel().unwrap_or(0);
            let fuel_consumed = max_fuel.saturating_sub(fuel_remaining);

            match call_result {
                Ok(_) => {
                    let _ = tx.send(ExecReply::Ok {
                        fuel_consumed,
                        pre_call_memory,
                    });
                }
                Err(e) => {
                    // Prefer typed Trap classification; fall back to a
                    // HostError carrying the textual chain otherwise.
                    let mode = FailureMode::from_anyhow_error(&e)
                        .unwrap_or_else(|| FailureMode::HostError(format!("wasm error: {e:#}")));
                    // If wasmtime told us OutOfFuel, fill in the real limit.
                    let mode = match mode {
                        FailureMode::FuelExhausted { .. } => {
                            FailureMode::FuelExhausted { limit: max_fuel }
                        }
                        other => other,
                    };
                    let _ = tx.send(ExecReply::Failed {
                        mode,
                        fuel_consumed,
                        pre_call_memory,
                    });
                }
            }
        });

        let recv_result = rx.recv_timeout(time_limit);
        let duration_ms = start.elapsed().as_millis() as u64;

        match recv_result {
            Ok(ExecReply::Ok {
                fuel_consumed,
                pre_call_memory,
            }) => {
                let _ = handle.join();
                Ok(
                    ExecutionResult::success(Vec::new(), fuel_consumed, duration_ms)
                        .with_pre_call_memory(pre_call_memory),
                )
            }
            Ok(ExecReply::Failed {
                mode,
                fuel_consumed,
                pre_call_memory,
            }) => {
                let _ = handle.join();
                Ok(
                    ExecutionResult::failure_from_mode(mode, fuel_consumed, duration_ms)
                        .with_pre_call_memory(pre_call_memory),
                )
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Detach the worker — the WASM is sandboxed so the loop is
                // contained, but we want to return to the caller now.
                drop(handle);
                let limit_ms = time_limit.as_millis() as u64;
                let mode = FailureMode::Timeout {
                    limit_ms,
                    observed_ms: duration_ms,
                };
                Ok(ExecutionResult::failure_from_mode(mode, 0, duration_ms))
            }
            Err(_) => {
                let _ = handle.join();
                let mode = FailureMode::HostError(
                    "worker thread disconnected before sending a result".to_string(),
                );
                Ok(ExecutionResult::failure_from_mode(mode, 0, duration_ms))
            }
        }
    }

    /// Execute a specific function with arguments
    pub fn execute_function(
        &self,
        wasm_bytes: &[u8],
        function_name: &str,
        args: &[i32],
    ) -> Result<ExecutionResult> {
        let start = Instant::now();
        let start_fuel = self.config.max_fuel;

        let module = Module::from_binary(&self.engine, wasm_bytes)
            .map_err(|e| NexusError::WasmError(format!("Failed to compile module: {}", e)))?;

        let mut store = self.create_store()?;
        let linker = self.create_linker()?;

        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| NexusError::WasmError(format!("Failed to instantiate: {}", e)))?;

        // Try to find the function
        let func = instance.get_typed_func::<(i32,), (i32,)>(&mut store, function_name);

        match func {
            Ok(f) => {
                let mut results = Vec::new();
                for &arg in args {
                    let result = f.call(&mut store, (arg,));
                    match result {
                        Ok((ret,)) => results.push(ret),
                        Err(e) => {
                            return Ok(ExecutionResult::failure(
                                format!("WASM error: {}", e),
                                start_fuel,
                            ));
                        }
                    }
                }

                let fuel_consumed = start_fuel;
                let duration_ms = start.elapsed().as_millis() as u64;

                // Encode results as bytes
                let return_bytes = results.iter().flat_map(|&v| v.to_le_bytes()).collect();

                Ok(ExecutionResult::success(
                    return_bytes,
                    fuel_consumed,
                    duration_ms,
                ))
            }
            Err(_) => Err(NexusError::WasmError(format!(
                "Function {} not found",
                function_name
            ))),
        }
    }

    /// Create a store with fuel metering and resource limits
    fn create_store(&self) -> Result<Store<WasmState>> {
        let state = WasmState::new(self.config.max_fuel);
        let mut store = Store::new(&self.engine, state);

        // Set fuel for this execution (fuel is enabled via engine config)
        if let Err(e) = store.set_fuel(self.config.max_fuel) {
            // Fuel setting failed, but continue without it
            eprintln!("Warning: Could not set fuel: {}", e);
        }

        Ok(store)
    }

    fn create_linker(&self) -> Result<Linker<WasmState>> {
        // WASI integration is in development; for now, bare linker only.
        let linker = Linker::new(&self.engine);
        Ok(linker)
    }
}

#[derive(Debug, Clone)]
pub struct WasmState;

impl WasmState {
    pub fn new(_fuel: u64) -> Self {
        WasmState
    }
}
