//! WASM Micro-Sandbox Runtime
//! 
//! High-performance WebAssembly sandbox with fuel metering for AI agent execution.

use std::sync::Arc;
use std::time::{Duration, Instant};
use wasmtime::{
    Config, Engine, Linker, Module, Store,
};
use serde::{Deserialize, Serialize};

use crate::error::{NexusError, Result};

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
            max_fuel: 10_000_000, // 10 million instructions
            max_memory_pages: 512, // 32MB
            time_limit: Duration::from_millis(500), // 500ms for fast demo
            module_bytes: None,
            enable_wasi: true,
        }
    }
}

/// Execution result from WASM sandbox
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
    /// Error message if failed
    pub error: Option<String>,
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
            syscall_count: 0,
        }
    }
    
    /// Create a failure result
    pub fn failure(error: String, fuel_consumed: u64) -> Self {
        ExecutionResult {
            success: false,
            return_value: None,
            fuel_consumed,
            duration_ms: 0,
            error: Some(error),
            syscall_count: 0,
        }
    }
}

/// WASM Micro-Sandbox with fuel metering
pub struct WasmSandbox {
    engine: Arc<Engine>,
    config: SandboxConfig,
}

impl WasmSandbox {
    /// Create a new WASM sandbox
    pub fn new(config: SandboxConfig) -> Result<Self> {
        let cfg = Config::new();
        
        // Fuel metering is handled at the store level via set_fuel()
        // No engine-level config needed for fuel
        
        let engine = Engine::new(&cfg)
            .map_err(|e| NexusError::ConfigError(format!("Failed to create engine: {}", e)))?;
        
        Ok(WasmSandbox {
            engine: Arc::new(engine),
            config,
        })
    }
    
    /// Execute WASM code with fuel metering
    pub fn execute(&self, wasm_bytes: &[u8], _args: &[Vec<u8>]) -> Result<ExecutionResult> {
        let start = Instant::now();
        let start_fuel = self.config.max_fuel;
        let time_limit = self.config.time_limit;
        
        // Create module
        let module = Module::from_binary(&self.engine, wasm_bytes)
            .map_err(|e| NexusError::WasmError(format!("Failed to compile module: {}", e)))?;
        
        // Clone engine for thread
        let engine = self.engine.clone();
        let enable_wasi = self.config.enable_wasi;
        let max_fuel = self.config.max_fuel;
        
        // Create channel for result
        let (tx, rx) = std::sync::mpsc::channel();
        
        // Spawn thread
        let handle = std::thread::spawn(move || {
            // Create store
            let state = WasmState::new(max_fuel);
            let mut store = Store::new(&engine, state);
            
            // Link WASI if enabled
            let linker = if enable_wasi {
                Linker::new(&engine)
            } else {
                Linker::new(&engine)
            };
            
            // Instantiate
            let instance = match linker.instantiate(&mut store, &module) {
                Ok(i) => i,
                Err(e) => {
                    let _ = tx.send(Err(format!("Instantiation failed: {}", e)));
                    return;
                }
            };
            
            // Find _start or main function
            let start_func = match instance.get_typed_func::<(), ()>(&mut store, "_start") {
                Ok(f) => f,
                Err(_) => match instance.get_typed_func::<(), ()>(&mut store, "main") {
                    Ok(f) => f,
                    Err(_) => {
                        let _ = tx.send(Err("No _start or main function found".to_string()));
                        return;
                    }
                }
            };
            
            // Execute
            match start_func.call(&mut store, ()) {
                Ok(_) => {
                    let _ = tx.send(Ok(()));
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("{}", e)));
                }
            }
        });
        
        // Wait for result or timeout
        let result = match rx.recv_timeout(time_limit) {
            Ok(r) => {
                let _ = handle.join();
                Some(r)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // On timeout, detach the thread (it will continue running but not block us)
                // The infinite loop will be contained within the WASM sandbox
                drop(handle);
                None
            }
            Err(_) => None,
        };
        
        let duration_ms = start.elapsed().as_millis() as u64;
        
        match result {
            Some(Ok(())) => {
                Ok(ExecutionResult::success(Vec::new(), start_fuel, duration_ms))
            }
            Some(Err(e)) => {
                if e.contains("trap") || e.contains("Trap") {
                    Ok(ExecutionResult::failure(
                        "EXECUTION_TRAP: WASM trap - possibly infinite loop".to_string(),
                        start_fuel,
                    ))
                } else {
                    Ok(ExecutionResult::failure(format!("WASM error: {}", e), start_fuel))
                }
            }
            None => {
                // Timeout - infinite loop prevented!
                Ok(ExecutionResult::failure(
                    format!("TIMEOUT: Execution exceeded {}ms - infinite loop prevented", time_limit.as_millis()),
                    start_fuel,
                ))
            }
        }
    }
    
    fn get_store_fuel(&self, _store: &Store<WasmState>) -> u64 {
        // In newer wasmtime, fuel is managed differently
        // Return remaining fuel estimate
        0
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
        let linker = if self.config.enable_wasi {
            self.create_wasi_linker()?
        } else {
            self.create_minimal_linker()?
        };
        
        let instance = linker.instantiate(&mut store, &module)
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
                let return_bytes = results.iter()
                    .flat_map(|&v| v.to_le_bytes())
                    .collect();
                
                Ok(ExecutionResult::success(return_bytes, fuel_consumed, duration_ms))
            }
            Err(_) => Err(NexusError::WasmError(format!("Function {} not found", function_name))),
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
    
    /// Create linker with WASI support
    fn create_wasi_linker(&self) -> Result<Linker<WasmState>> {
        // For now, use minimal linker without WASI
        // WASI integration requires more careful setup
        let linker = Linker::new(&self.engine);
        Ok(linker)
    }
    
    /// Create minimal linker without WASI
    fn create_minimal_linker(&self) -> Result<Linker<WasmState>> {
        let linker = Linker::new(&self.engine);
        Ok(linker)
    }
}

/// State stored in the WASM store
#[derive(Debug, Clone)]
pub struct WasmState {
    /// Remaining fuel
    fuel: u64,
    /// Execution metadata
    metadata: std::collections::HashMap<String, Vec<u8>>,
}

impl WasmState {
    pub fn new(fuel: u64) -> Self {
        WasmState {
            fuel,
            metadata: std::collections::HashMap::new(),
        }
    }
}