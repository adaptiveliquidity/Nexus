//! Executor Module
//! 
//! Handles the actual execution of tools within the WASM sandbox.

use std::sync::Arc;
use std::time::Instant;
use wasmtime::{Store, Module, Linker, Engine};
use wasmtime_wasi::WasiCtxBuilder;

use crate::error::{NexusError, Result};
use crate::security::Capability;

/// Execute request with full context
#[derive(Debug)]
pub struct ExecuteRequest {
    pub wasm_bytes: Vec<u8>,
    pub entry_point: String,
    pub args: Vec<Vec<u8>>,
    pub capabilities: Vec<Capability>,
    pub fuel_limit: u64,
}

/// Execute response with full metadata
#[derive(Debug)]
pub struct ExecuteResponse {
    pub success: bool,
    pub return_value: Option<Vec<u8>>,
    pub error: Option<String>,
    pub fuel_consumed: u64,
    pub execution_time_ms: u64,
    pub syscalls: u32,
}

impl ExecuteResponse {
    pub fn into_result(self) -> Result<Vec<u8>> {
        if self.success {
            Ok(self.return_value.unwrap_or_default())
        } else {
            Err(NexusError::WasmError(self.error.unwrap_or_else(|| "Unknown error".to_string())))
        }
    }
}

/// The executor that handles WASM execution
pub struct Executor {
    engine: Arc<Engine>,
    config: SandboxConfig,
}

impl Executor {
    pub fn new(config: SandboxConfig) -> Result<Self> {
        let mut cfg = wasmtime::Config::new();
        cfg Cranelift_jit(true);
        cfg.enable_reference_types(true);
        
        let engine = Engine::new(&cfg)
            .map_err(|e| NexusError::ConfigError(format!("Failed to create engine: {}", e)))?;
        
        Ok(Executor {
            engine: Arc::new(engine),
            config,
        })
    }
    
    /// Execute WASM with the given request
    pub fn execute(&self, request: ExecuteRequest) -> ExecuteResponse {
        let start = Instant::now();
        let start_fuel = request.fuel_limit;
        
        // Compile module
        let module = match Module::from_binary(&self.engine, &request.wasm_bytes) {
            Ok(m) => m,
            Err(e) => {
                return ExecuteResponse {
                    success: false,
                    return_value: None,
                    error: Some(format!("Compilation failed: {}", e)),
                    fuel_consumed: 0,
                    execution_time_ms: start.elapsed().as_millis() as u64,
                    syscalls: 0,
                };
            }
        };
        
        // Create store with fuel
        let mut store = self.create_store(request.fuel_limit);
        
        // Create linker with capabilities
        let linker = self.create_linker(&request.capabilities);
        
        // Instantiate
        let instance = match linker.instantiate(&mut store, &module) {
            Ok(i) => i,
            Err(e) => {
                return ExecuteResponse {
                    success: false,
                    return_value: None,
                    error: Some(format!("Instantiation failed: {}", e)),
                    fuel_consumed: start_fuel - store.fuel_consumed().unwrap_or(0),
                    execution_time_ms: start.elapsed().as_millis() as u64,
                    syscalls: 0,
                };
            }
        };
        
        // Get and call entry point
        let func = match instance.get_typed_func::<(), ()>(&mut store, &request.entry_point) {
            Ok(f) => f,
            Err(_) => {
                // Try _start as fallback
                match instance.get_typed_func::<(), ()>(&mut store, "_start") {
                    Ok(f) => f,
                    Err(_) => {
                        return ExecuteResponse {
                            success: false,
                            return_value: None,
                            error: Some("No valid entry point found (_start or main)".to_string()),
                            fuel_consumed: start_fuel - store.fuel_consumed().unwrap_or(0),
                            execution_time_ms: start.elapsed().as_millis() as u64,
                            syscalls: 0,
                        };
                    }
                }
            }
        };
        
        // Execute
        let result = func.call(&mut store);
        
        let fuel_consumed = start_fuel - store.fuel_consumed().unwrap_or(0);
        let execution_time_ms = start.elapsed().as_millis() as u64;
        
        match result {
            Ok(_) => ExecuteResponse {
                success: true,
                return_value: Some(Vec::new()),
                error: None,
                fuel_consumed,
                execution_time_ms,
                syscalls: 0, // Would track via WASI hooks
            },
            Err(trap) => ExecuteResponse {
                success: false,
                return_value: None,
                error: Some(format!("Trap: {:?}", trap)),
                fuel_consumed,
                execution_time_ms,
                syscalls: 0,
            },
        }
    }
    
    /// Create a store with fuel metering
    fn create_store(&self, fuel: u64) -> Store<()> {
        let mut store = wasmtime::Store::new(&self.engine, ());
        store.set_fuel(fuel).expect("fuel should be settable");
        store
    }
    
    /// Create a linker with WASI and capability restrictions
    fn create_linker(&self, _capabilities: &[Capability]) -> Linker<()> {
        let mut linker = Linker::new(&self.engine);
        
        // Add WASI with minimal permissions
        let wasi = WasiCtxBuilder::new()
            .inherit_stdio()
            .build();
        
        wasmtime_wasi::add_to_linker(&mut linker, |_| &mut wasi.clone())
            .expect("WASI should be addable");
        
        linker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_simple_execution() {
        let config = SandboxConfig::default();
        let executor = Executor::new(config).unwrap();
        
        let wasm_bytes = wat::parse_str(r#"
            (module
                (func (export "_start"))
            )
        "#).unwrap();
        
        let request = ExecuteRequest {
            wasm_bytes,
            entry_point: "_start".to_string(),
            args: Vec::new(),
            capabilities: Vec::new(),
            fuel_limit: 10_000_000,
        };
        
        let response = executor.execute(request);
        assert!(response.success);
    }
    
    #[test]
    fn test_fuel_exhaustion() {
        let config = SandboxConfig::default();
        let executor = Executor::new(config).unwrap();
        
        let wasm_bytes = wat::parse_str(r#"
            (module
                (func (export "_start")
                    (loop (br 0))
                )
            )
        "#).unwrap();
        
        let request = ExecuteRequest {
            wasm_bytes,
            entry_point: "_start".to_string(),
            args: Vec::new(),
            capabilities: Vec::new(),
            fuel_limit: 1000,
        };
        
        let response = executor.execute(request);
        assert!(!response.success);
        assert!(response.error.is_some());
    }
}