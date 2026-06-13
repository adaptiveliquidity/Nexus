//! WASI-aware sandbox execution gated by capability tokens.
//!
//! Provides `WasiSandboxConfig` to map [`Capability`] tokens into a WASI
//! context (pre-opened directories, env vars, args) and `execute_wasi` to
//! run a WASM module with those host imports available.
//!
//! The pure-compute path (`execute` / `execute_module`) is deliberately
//! unchanged — it keeps the empty `Linker` that makes deterministic replay
//! sound. This module is the opt-in I/O path.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use wasmtime::{Linker, Module, Store};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

use crate::error::{NexusError, Result};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasiAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone)]
pub struct WasiMount {
    pub host_path: PathBuf,
    pub guest_path: String,
    pub access: WasiAccess,
}

impl WasiMount {
    pub fn new(
        host_path: impl Into<PathBuf>,
        guest_path: impl Into<String>,
        access: WasiAccess,
    ) -> Self {
        Self {
            host_path: host_path.into(),
            guest_path: guest_path.into(),
            access,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WasiToolConfig {
    pub mounts: Vec<WasiMount>,
    pub inherit_stdout: bool,
    pub inherit_stderr: bool,
    pub env_vars: Vec<(String, String)>,
    pub args: Vec<String>,
}

impl WasiToolConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_mount(
        mut self,
        host_path: impl Into<PathBuf>,
        guest_path: impl Into<String>,
        access: WasiAccess,
    ) -> Self {
        self.mounts
            .push(WasiMount::new(host_path, guest_path, access));
        self
    }

    pub fn inherit_stderr(mut self) -> Self {
        self.inherit_stderr = true;
        self
    }

    pub fn inherit_stdout(mut self) -> Self {
        self.inherit_stdout = true;
        self
    }

    pub fn validate(&self) -> Result<ValidatedWasiToolConfig> {
        let mut preopens = Vec::with_capacity(self.mounts.len());
        let mut required_capabilities = Vec::with_capacity(self.mounts.len() * 2);
        let mut guest_paths = HashSet::new();
        let mut normalized_guests: Vec<String> = Vec::new();

        for mount in &self.mounts {
            let guest_path = normalize_guest_path(&mount.guest_path)?;
            if !guest_paths.insert(guest_path.clone()) {
                return Err(NexusError::ConfigError(format!(
                    "duplicate WASI guest path: {guest_path}"
                )));
            }
            for existing in &normalized_guests {
                if guest_path_overlaps(existing, &guest_path) {
                    return Err(NexusError::ConfigError(format!(
                        "overlapping WASI guest paths: {existing} and {guest_path}"
                    )));
                }
            }
            normalized_guests.push(guest_path.clone());

            let host_path = canonicalize_mount_dir(&mount.host_path)?;
            let writable = matches!(mount.access, WasiAccess::ReadWrite);
            required_capabilities.push(Capability::ReadFile(host_path.clone()));
            if writable {
                required_capabilities.push(Capability::WriteFile(host_path.clone()));
            }
            preopens.push(PreOpen {
                host_path,
                guest_path,
                writable,
            });
        }

        Ok(ValidatedWasiToolConfig {
            sandbox_config: WasiSandboxConfig {
                preopens,
                inherit_stdout: self.inherit_stdout,
                inherit_stderr: self.inherit_stderr,
                env_vars: self.env_vars.clone(),
                args: self.args.clone(),
            },
            required_capabilities,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ValidatedWasiToolConfig {
    pub sandbox_config: WasiSandboxConfig,
    pub required_capabilities: Vec<Capability>,
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

fn canonicalize_mount_dir(path: &Path) -> Result<PathBuf> {
    if path.exists() && !path.is_dir() {
        return Err(NexusError::ConfigError(format!(
            "WASI mount host path is not a directory: {}",
            path.display()
        )));
    }
    if !path.exists() {
        std::fs::create_dir_all(path).map_err(|e| {
            NexusError::FilesystemError(format!(
                "failed to create WASI mount directory {}: {e}",
                path.display()
            ))
        })?;
    }
    let canonical = std::fs::canonicalize(path).map_err(|e| {
        NexusError::FilesystemError(format!(
            "failed to canonicalize WASI mount directory {}: {e}",
            path.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(NexusError::ConfigError(format!(
            "WASI mount canonical host path is not a directory: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn normalize_guest_path(path: &str) -> Result<String> {
    if path.is_empty() || !path.starts_with('/') {
        return Err(NexusError::ConfigError(format!(
            "WASI guest path must be absolute: {path}"
        )));
    }
    if path.as_bytes().contains(&0) || path.contains('\\') || path.contains("//") {
        return Err(NexusError::ConfigError(format!(
            "WASI guest path contains invalid characters: {path}"
        )));
    }

    let raw = Path::new(path);
    let mut parts = Vec::new();
    for component in raw.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => {
                let part = part.to_string_lossy();
                if part.is_empty() || part == "." || part == ".." {
                    return Err(NexusError::ConfigError(format!(
                        "WASI guest path contains invalid component: {path}"
                    )));
                }
                parts.push(part.to_string());
            }
            _ => {
                return Err(NexusError::ConfigError(format!(
                    "WASI guest path contains traversal or prefix component: {path}"
                )));
            }
        }
    }

    if parts.is_empty() {
        return Err(NexusError::ConfigError(
            "WASI guest path cannot be root".to_string(),
        ));
    }

    Ok(format!("/{}", parts.join("/")))
}

fn guest_path_overlaps(left: &str, right: &str) -> bool {
    left == right
        || right
            .strip_prefix(left)
            .map(|suffix| suffix.starts_with('/'))
            .unwrap_or(false)
        || left
            .strip_prefix(right)
            .map(|suffix| suffix.starts_with('/'))
            .unwrap_or(false)
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
                let dir_perms = if preopen.writable {
                    wasmtime_wasi::DirPerms::all()
                } else {
                    wasmtime_wasi::DirPerms::READ
                };
                let file_perms = if preopen.writable {
                    wasmtime_wasi::FilePerms::all()
                } else {
                    wasmtime_wasi::FilePerms::READ
                };
                if let Err(e) = ctx_builder.preopened_dir(
                    &preopen.host_path,
                    &preopen.guest_path,
                    dir_perms,
                    file_perms,
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
