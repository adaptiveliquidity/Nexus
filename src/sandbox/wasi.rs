//! WASI-aware sandbox execution gated by capability tokens.
//!
//! Provides `WasiSandboxConfig` to map [`Capability`] tokens into a WASI
//! context (pre-opened directories, env vars, args) and `execute_wasi` to
//! run a WASM module with those host imports available.
//!
//! The pure-compute path (`execute` / `execute_module`) is deliberately
//! unchanged — it keeps the empty `Linker` that makes deterministic replay
//! sound. This module is the opt-in I/O path.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use wasmtime::{Linker, Module, Store};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

use crate::error::Result;
use crate::hypervisor::failure_mode::FailureMode;
use crate::sandbox::wasm_runtime::{ExecutionResult, WasmSandbox};
use crate::security::Capability;

/// Pre-open entry derived from a validated capability token.
#[derive(Debug, Clone)]
pub struct PreOpen {
    pub host_path: PathBuf,
    pub guest_path: String,
    pub writable: bool,
}

/// WASI sandbox configuration built from capability tokens.
#[derive(Debug, Clone, Default)]
pub struct WasiSandboxConfig {
    pub preopens: Vec<PreOpen>,
    pub inherit_stdout: bool,
    pub inherit_stderr: bool,
    pub env_vars: Vec<(String, String)>,
    pub args: Vec<String>,
}

impl WasiSandboxConfig {
    /// Build a WASI config from a set of validated capabilities.
    /// `ReadFile`/`ListDirectory` map to read-only pre-opens;
    /// `WriteFile` maps to read-write pre-opens.
    pub fn from_capabilities(capabilities: &[Capability]) -> Self {
        let mut config = WasiSandboxConfig::default();

        for cap in capabilities {
            match cap {
                Capability::ReadFile(path) | Capability::ListDirectory(path) => {
                    if !config.preopens.iter().any(|p| &p.host_path == path) {
                        config.preopens.push(PreOpen {
                            host_path: path.clone(),
                            guest_path: path.to_string_lossy().to_string(),
                            writable: false,
                        });
                    }
                }
                Capability::WriteFile(path) => {
                    if let Some(existing) =
                        config.preopens.iter_mut().find(|p| &p.host_path == path)
                    {
                        existing.writable = true;
                    } else {
                        config.preopens.push(PreOpen {
                            host_path: path.clone(),
                            guest_path: path.to_string_lossy().to_string(),
                            writable: true,
                        });
                    }
                }
                Capability::All => {
                    config.inherit_stdout = true;
                    config.inherit_stderr = true;
                }
                _ => {}
            }
        }

        config
    }
}

enum WasiReply {
    Ok {
        fuel_consumed: u64,
        pre_call_memory: Option<Vec<u8>>,
        globals: Vec<crate::snapshot::GlobalSnapshot>,
        tables: Vec<crate::snapshot::TableSnapshot>,
    },
    Failed {
        mode: FailureMode,
        fuel_consumed: u64,
        pre_call_memory: Option<Vec<u8>>,
        globals: Vec<crate::snapshot::GlobalSnapshot>,
        tables: Vec<crate::snapshot::TableSnapshot>,
    },
}

impl WasmSandbox {
    /// Execute a WASM module with WASI host imports, gated by the
    /// capabilities expressed in `wasi_config`.
    pub fn execute_wasi(
        &self,
        wasm_bytes: &[u8],
        args: &[Vec<u8>],
        wasi_config: &WasiSandboxConfig,
    ) -> Result<ExecutionResult> {
        let start = Instant::now();

        let module = match Module::from_binary(self.engine(), wasm_bytes) {
            Ok(m) => Arc::new(m),
            Err(e) => {
                let mode = FailureMode::InvalidModule(e.to_string());
                return Ok(ExecutionResult::failure_from_mode(
                    mode,
                    0,
                    start.elapsed().as_millis() as u64,
                ));
            }
        };

        self.execute_wasi_module(module, args, wasi_config)
    }

    fn execute_wasi_module(
        &self,
        module: Arc<Module>,
        args: &[Vec<u8>],
        wasi_config: &WasiSandboxConfig,
    ) -> Result<ExecutionResult> {
        let start = Instant::now();
        let max_fuel = self.config.max_fuel;
        let time_limit = self.config.time_limit;
        let engine = self.engine.clone();
        let input_data: Vec<u8> = args.first().cloned().unwrap_or_default();
        let wasi_config = wasi_config.clone();

        let (tx, rx) = std::sync::mpsc::channel::<WasiReply>();

        std::thread::spawn(move || {
            let mut ctx_builder = WasiCtxBuilder::new();

            if wasi_config.inherit_stdout {
                ctx_builder.inherit_stdout();
            }
            if wasi_config.inherit_stderr {
                ctx_builder.inherit_stderr();
            }
            for (k, v) in &wasi_config.env_vars {
                ctx_builder.env(k, v);
            }
            for arg in &wasi_config.args {
                ctx_builder.arg(arg);
            }

            for preopen in &wasi_config.preopens {
                if let Err(e) = ctx_builder.preopened_dir(
                    &preopen.host_path,
                    &preopen.guest_path,
                    wasmtime_wasi::DirPerms::all(),
                    if preopen.writable {
                        wasmtime_wasi::FilePerms::all()
                    } else {
                        wasmtime_wasi::FilePerms::READ
                    },
                ) {
                    let _ = tx.send(WasiReply::Failed {
                        mode: FailureMode::HostError(format!(
                            "preopened_dir {:?}: {e}",
                            preopen.host_path
                        )),
                        fuel_consumed: 0,
                        pre_call_memory: None,
                        globals: Vec::new(),
                        tables: Vec::new(),
                    });
                    return;
                }
            }

            let wasi_ctx = ctx_builder.build_p1();
            let mut store = Store::new(&engine, wasi_ctx);

            if let Err(e) = store.set_fuel(max_fuel) {
                let _ = tx.send(WasiReply::Failed {
                    mode: FailureMode::HostError(format!("set_fuel: {e}")),
                    fuel_consumed: 0,
                    pre_call_memory: None,
                    globals: Vec::new(),
                    tables: Vec::new(),
                });
                return;
            }

            let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
            if let Err(e) = wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |ctx| ctx) {
                let _ = tx.send(WasiReply::Failed {
                    mode: FailureMode::HostError(format!("WASI linker: {e}")),
                    fuel_consumed: 0,
                    pre_call_memory: None,
                    globals: Vec::new(),
                    tables: Vec::new(),
                });
                return;
            }

            let instance = match linker.instantiate(&mut store, &module) {
                Ok(i) => i,
                Err(e) => {
                    let _ = tx.send(WasiReply::Failed {
                        mode: FailureMode::InvalidModule(format!("instantiate: {e}")),
                        fuel_consumed: 0,
                        pre_call_memory: None,
                        globals: Vec::new(),
                        tables: Vec::new(),
                    });
                    return;
                }
            };

            let pre_call_memory: Option<Vec<u8>> = instance
                .get_memory(&mut store, "memory")
                .map(|m| m.data(&store).to_vec());

            if !input_data.is_empty() {
                if let Some(mem) = instance.get_memory(&mut store, "memory") {
                    let needed = 4 + input_data.len();
                    let data = mem.data_mut(&mut store);
                    if needed <= data.len() {
                        let len_bytes = (input_data.len() as u32).to_le_bytes();
                        data[..4].copy_from_slice(&len_bytes);
                        data[4..4 + input_data.len()].copy_from_slice(&input_data);
                    }
                }
            }

            let start_func = match instance.get_typed_func::<(), ()>(&mut store, "_start") {
                Ok(f) => f,
                Err(_) => match instance.get_typed_func::<(), ()>(&mut store, "main") {
                    Ok(f) => f,
                    Err(_) => {
                        let _ = tx.send(WasiReply::Failed {
                            mode: FailureMode::MissingEntrypoint {
                                expected: "_start".into(),
                            },
                            fuel_consumed: 0,
                            pre_call_memory,
                            globals: Vec::new(),
                            tables: Vec::new(),
                        });
                        return;
                    }
                },
            };

            let call_result = start_func.call(&mut store, ());
            let fuel_remaining = store.get_fuel().unwrap_or(0);
            let fuel_consumed = max_fuel.saturating_sub(fuel_remaining);

            let globals = capture_globals_wasi(&instance, &mut store);
            let tables = capture_tables_wasi(&instance, &mut store);

            match call_result {
                Ok(_) => {
                    let _ = tx.send(WasiReply::Ok {
                        fuel_consumed,
                        pre_call_memory,
                        globals,
                        tables,
                    });
                }
                Err(e) => {
                    // WASI proc_exit(0) raises a trap with exit status 0
                    // which is a clean exit, not a failure.
                    let msg = format!("{e:#}");
                    if msg.contains("exit status 0") {
                        let _ = tx.send(WasiReply::Ok {
                            fuel_consumed,
                            pre_call_memory,
                            globals,
                            tables,
                        });
                        return;
                    }

                    let mode = FailureMode::from_anyhow_error(&e)
                        .unwrap_or_else(|| FailureMode::HostError(format!("wasm: {e:#}")));
                    let mode = match mode {
                        FailureMode::FuelExhausted { .. } => {
                            FailureMode::FuelExhausted { limit: max_fuel }
                        }
                        other => other,
                    };
                    let _ = tx.send(WasiReply::Failed {
                        mode,
                        fuel_consumed,
                        pre_call_memory,
                        globals,
                        tables,
                    });
                }
            }
        });

        let recv_result = rx.recv_timeout(time_limit);
        let duration_ms = start.elapsed().as_millis() as u64;

        match recv_result {
            Ok(WasiReply::Ok {
                fuel_consumed,
                pre_call_memory,
                globals,
                tables,
            }) => Ok(
                ExecutionResult::success(Vec::new(), fuel_consumed, duration_ms)
                    .with_pre_call_memory(pre_call_memory)
                    .with_post_call_state(globals, tables),
            ),
            Ok(WasiReply::Failed {
                mode,
                fuel_consumed,
                pre_call_memory,
                globals,
                tables,
            }) => Ok(
                ExecutionResult::failure_from_mode(mode, fuel_consumed, duration_ms)
                    .with_pre_call_memory(pre_call_memory)
                    .with_post_call_state(globals, tables),
            ),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let limit_ms = time_limit.as_millis() as u64;
                let mode = FailureMode::Timeout {
                    limit_ms,
                    observed_ms: duration_ms,
                };
                Ok(ExecutionResult::failure_from_mode(mode, 0, duration_ms))
            }
            Err(_) => {
                let mode = FailureMode::HostError(
                    "WASI worker disconnected before sending result".to_string(),
                );
                Ok(ExecutionResult::failure_from_mode(mode, 0, duration_ms))
            }
        }
    }
}

fn capture_globals_wasi(
    instance: &wasmtime::Instance,
    store: &mut Store<WasiP1Ctx>,
) -> Vec<crate::snapshot::GlobalSnapshot> {
    let names: Vec<String> = instance
        .exports(&mut *store)
        .filter(|e| e.clone().into_global().is_some())
        .map(|e| e.name().to_string())
        .collect();

    let mut globals = Vec::new();
    for name in names {
        if let Some(global) = instance.get_global(&mut *store, &name) {
            let mutable = global.ty(&*store).mutability() == wasmtime::Mutability::Var;
            let val = global.get(&mut *store);
            let value = match val {
                wasmtime::Val::I32(v) => crate::snapshot::GlobalValue::I32(v),
                wasmtime::Val::I64(v) => crate::snapshot::GlobalValue::I64(v),
                wasmtime::Val::F32(v) => crate::snapshot::GlobalValue::F32(f32::from_bits(v)),
                wasmtime::Val::F64(v) => crate::snapshot::GlobalValue::F64(f64::from_bits(v)),
                _ => continue,
            };
            globals.push(crate::snapshot::GlobalSnapshot {
                name,
                value,
                mutable,
            });
        }
    }
    globals
}

fn capture_tables_wasi(
    instance: &wasmtime::Instance,
    store: &mut Store<WasiP1Ctx>,
) -> Vec<crate::snapshot::TableSnapshot> {
    let names: Vec<String> = instance
        .exports(&mut *store)
        .filter(|e| e.clone().into_table().is_some())
        .map(|e| e.name().to_string())
        .collect();

    let mut tables = Vec::new();
    for name in names {
        if let Some(table) = instance.get_table(&mut *store, &name) {
            let size = table.size(&*store) as u32;
            tables.push(crate::snapshot::TableSnapshot { name, size });
        }
    }
    tables
}
