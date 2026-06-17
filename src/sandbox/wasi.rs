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
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

use wasmtime::{Linker, Module, Store};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

use crate::error::{NexusError, Result};
use crate::hypervisor::failure_mode::FailureMode;
use crate::sandbox::wasm_runtime::{
    configure_epoch_deadline, is_epoch_interrupt, join_with_timeout, timeout_mode, ExecutionResult,
    WasmSandbox, TIMEOUT_JOIN_GRACE,
};
use crate::security::capability::normalize_lexical_path;
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

    /// Derive required capabilities without creating missing host mount
    /// directories.
    pub fn required_capabilities(&self) -> Result<Vec<Capability>> {
        let mounts = self.normalized_mounts()?;
        let mut caps = Vec::with_capacity(self.mounts.len() * 2);

        for mount in mounts {
            let host_path = required_capability_mount_dir(mount.host_path)?;
            caps.push(Capability::ReadFile(host_path.clone()));
            if matches!(mount.access, WasiAccess::ReadWrite) {
                caps.push(Capability::WriteFile(host_path));
            }
        }

        Ok(caps)
    }

    /// Validate and prepare WASI mounts after caller authorization.
    ///
    /// This may create missing host mount directories before canonicalizing them
    /// into the preopen configuration.
    pub fn prepare_mounts(&self) -> Result<ValidatedWasiToolConfig> {
        let mounts = self.normalized_mounts()?;
        let mut preopens = Vec::with_capacity(self.mounts.len());
        let mut required_capabilities = Vec::with_capacity(self.mounts.len() * 2);

        for mount in mounts {
            let host_path = prepare_mount_dir(mount.host_path)?;
            let writable = matches!(mount.access, WasiAccess::ReadWrite);
            required_capabilities.push(Capability::ReadFile(host_path.clone()));
            if writable {
                required_capabilities.push(Capability::WriteFile(host_path.clone()));
            }
            preopens.push(PreOpen {
                host_path,
                guest_path: mount.guest_path,
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

    pub fn validate(&self) -> Result<ValidatedWasiToolConfig> {
        self.prepare_mounts()
    }

    fn normalized_mounts(&self) -> Result<Vec<NormalizedWasiMount<'_>>> {
        let mut mounts = Vec::with_capacity(self.mounts.len());
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

            mounts.push(NormalizedWasiMount {
                host_path: mount.host_path.as_path(),
                guest_path,
                access: mount.access,
            });
        }

        Ok(mounts)
    }
}

struct NormalizedWasiMount<'a> {
    host_path: &'a Path,
    guest_path: String,
    access: WasiAccess,
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

fn required_capability_mount_dir(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return canonicalize_existing_mount_dir(path);
    }

    let absolute = absolute_mount_path(path)?;
    let mut ancestor = absolute.as_path();
    let mut missing_components = Vec::new();
    while !ancestor.exists() {
        let component = ancestor.file_name().ok_or_else(|| {
            NexusError::FilesystemError(format!(
                "failed to resolve WASI mount directory {} without creation",
                path.display()
            ))
        })?;
        missing_components.push(component.to_os_string());
        ancestor = ancestor.parent().ok_or_else(|| {
            NexusError::FilesystemError(format!(
                "failed to resolve WASI mount directory {} without creation",
                path.display()
            ))
        })?;
    }

    let mut resolved = canonicalize_existing_mount_dir(ancestor)?;
    for component in missing_components.iter().rev() {
        resolved.push(component);
    }

    Ok(normalize_lexical_path(&resolved))
}

fn prepare_mount_dir(path: &Path) -> Result<PathBuf> {
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
    canonicalize_existing_mount_dir(path)
}

fn canonicalize_existing_mount_dir(path: &Path) -> Result<PathBuf> {
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

fn absolute_mount_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|e| NexusError::FilesystemError(format!("failed to resolve current dir: {e}")))
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
                    let path = normalize_lexical_path(path);
                    if !config.preopens.iter().any(|p| p.host_path == path) {
                        config.preopens.push(PreOpen {
                            host_path: path.clone(),
                            guest_path: path.to_string_lossy().to_string(),
                            writable: false,
                        });
                    }
                }
                Capability::WriteFile(path) => {
                    let path = normalize_lexical_path(path);
                    if let Some(existing) = config.preopens.iter_mut().find(|p| p.host_path == path)
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
        let epoch = self.epoch.clone();
        let epoch_deadline = epoch.reserve_deadline(time_limit);
        let max_memory_bytes = self.config.max_memory_pages as usize * 65536;
        let input_data: Vec<u8> = args.first().cloned().unwrap_or_default();
        let wasi_config = wasi_config.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();

        let (tx, rx) = std::sync::mpsc::channel::<WasiReply>();

        let handle = std::thread::spawn(move || {
            let worker_start = Instant::now();
            if worker_cancel.load(Ordering::Acquire) {
                return;
            }

            let mut ctx_builder = WasiCtxBuilder::new();

            if wasi_config.inherit_stdout {
                ctx_builder.inherit_stdout();
            }
            if wasi_config.inherit_stderr {
                ctx_builder.inherit_stderr();
            }
            for (k, v) in &wasi_config.env_vars {
                if worker_cancel.load(Ordering::Acquire) {
                    return;
                }
                ctx_builder.env(k, v);
            }
            for arg in &wasi_config.args {
                if worker_cancel.load(Ordering::Acquire) {
                    return;
                }
                ctx_builder.arg(arg);
            }

            if worker_cancel.load(Ordering::Acquire) {
                return;
            }
            for preopen in &wasi_config.preopens {
                if worker_cancel.load(Ordering::Acquire) {
                    return;
                }
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
            if worker_cancel.load(Ordering::Acquire) {
                return;
            }

            let wasi_ctx = ctx_builder.build_p1();
            let mut store = Store::new(&engine, WasiState::new(wasi_ctx, max_memory_bytes));
            configure_epoch_deadline(&mut store, epoch.relative_ticks_until(epoch_deadline));
            store.limiter(|s| &mut s.limits);

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
            if worker_cancel.load(Ordering::Acquire) {
                return;
            }

            let mut linker: Linker<WasiState> = Linker::new(&engine);
            if worker_cancel.load(Ordering::Acquire) {
                return;
            }
            if let Err(e) = wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |s| &mut s.wasi) {
                let _ = tx.send(WasiReply::Failed {
                    mode: FailureMode::HostError(format!("WASI linker: {e}")),
                    fuel_consumed: 0,
                    pre_call_memory: None,
                    globals: Vec::new(),
                    tables: Vec::new(),
                });
                return;
            }
            if worker_cancel.load(Ordering::Acquire) {
                return;
            }

            if worker_cancel.load(Ordering::Acquire) {
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
            if worker_cancel.load(Ordering::Acquire) {
                return;
            }

            let pre_call_memory: Option<Vec<u8>> = instance
                .get_memory(&mut store, "memory")
                .map(|m| m.data(&store).to_vec());

            if worker_cancel.load(Ordering::Acquire) {
                return;
            }
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
            if worker_cancel.load(Ordering::Acquire) {
                return;
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
            if worker_cancel.load(Ordering::Acquire) {
                return;
            }

            let call_result = start_func.call(&mut store, ());
            if worker_cancel.load(Ordering::Acquire) {
                return;
            }
            let fuel_remaining = store.get_fuel().unwrap_or(0);
            let fuel_consumed = max_fuel.saturating_sub(fuel_remaining);

            if worker_cancel.load(Ordering::Acquire) {
                return;
            }
            let globals = capture_globals_wasi(&instance, &mut store);
            if worker_cancel.load(Ordering::Acquire) {
                return;
            }
            let tables = capture_tables_wasi(&instance, &mut store);
            if worker_cancel.load(Ordering::Acquire) {
                return;
            }

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

                    let mode = if is_epoch_interrupt(&e) {
                        timeout_mode(time_limit, worker_start.elapsed())
                    } else {
                        let mode = FailureMode::from_anyhow_error(&e)
                            .unwrap_or_else(|| FailureMode::HostError(format!("wasm: {e:#}")));
                        match mode {
                            FailureMode::FuelExhausted { .. } => {
                                FailureMode::FuelExhausted { limit: max_fuel }
                            }
                            other => other,
                        }
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
            }) => {
                let _ = handle.join();
                Ok(
                    ExecutionResult::success(Vec::new(), fuel_consumed, duration_ms)
                        .with_pre_call_memory(pre_call_memory)
                        .with_post_call_state(globals, tables),
                )
            }
            Ok(WasiReply::Failed {
                mode,
                fuel_consumed,
                pre_call_memory,
                globals,
                tables,
            }) => {
                let _ = handle.join();
                Ok(
                    ExecutionResult::failure_from_mode(mode, fuel_consumed, duration_ms)
                        .with_pre_call_memory(pre_call_memory)
                        .with_post_call_state(globals, tables),
                )
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                cancel.store(true, Ordering::Release);
                self.epoch.advance_to(&self.engine, epoch_deadline);
                if !join_with_timeout(handle, TIMEOUT_JOIN_GRACE) {
                    tracing::warn!(
                        "WASI worker did not stop within {:?} after timeout; sync WASI host calls may continue until the host syscall returns",
                        TIMEOUT_JOIN_GRACE
                    );
                }
                let mode = timeout_mode(time_limit, start.elapsed());
                Ok(ExecutionResult::failure_from_mode(mode, 0, duration_ms))
            }
            Err(_) => {
                let _ = handle.join();
                let mode = FailureMode::HostError(
                    "WASI worker disconnected before sending result".to_string(),
                );
                Ok(ExecutionResult::failure_from_mode(mode, 0, duration_ms))
            }
        }
    }
}

/// Per-store state for the WASI path: the WASI Preview 1 context plus a
/// `StoreLimits` so `SandboxConfig::max_memory_pages` is enforced (matching the
/// pure-compute path's `WasmState`).
struct WasiState {
    wasi: WasiP1Ctx,
    limits: wasmtime::StoreLimits,
}

impl WasiState {
    fn new(wasi: WasiP1Ctx, max_memory_bytes: usize) -> Self {
        WasiState {
            wasi,
            limits: wasmtime::StoreLimitsBuilder::new()
                .memory_size(max_memory_bytes)
                .build(),
        }
    }
}

fn capture_globals_wasi(
    instance: &wasmtime::Instance,
    store: &mut Store<WasiState>,
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
    store: &mut Store<WasiState>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_capabilities_normalizes_lexical_parent_segments() {
        let caps = vec![Capability::ReadFile(PathBuf::from("/safe/../outside"))];
        let config = WasiSandboxConfig::from_capabilities(&caps);

        assert_eq!(config.preopens.len(), 1);
        assert_eq!(config.preopens[0].host_path, PathBuf::from("/outside"));
        assert_eq!(config.preopens[0].guest_path, "/outside");
    }

    #[test]
    fn from_capabilities_dedupes_after_lexical_normalization() {
        let caps = vec![
            Capability::ReadFile(PathBuf::from("/safe/./data")),
            Capability::WriteFile(PathBuf::from("/safe/data/nested/..")),
        ];
        let config = WasiSandboxConfig::from_capabilities(&caps);

        assert_eq!(config.preopens.len(), 1);
        assert_eq!(config.preopens[0].host_path, PathBuf::from("/safe/data"));
        assert!(config.preopens[0].writable);
    }

    #[test]
    fn required_capabilities_must_not_create_host_directories_before_authorization() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("not-yet-authorized");
        let config = WasiToolConfig::new().with_mount(&mount, "/data", WasiAccess::ReadWrite);

        let result = config.required_capabilities();

        assert!(
            result.is_ok(),
            "deriving requirements should remain fallible"
        );
        assert!(
            !mount.exists(),
            "required_capabilities must not create host directories before token authorization"
        );
    }

    #[cfg(unix)]
    #[test]
    fn required_capabilities_and_validate_use_same_canonical_host_path() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed");
        let outside = tmp.path().join("outside");
        let target = outside.join("target");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        symlink(&target, allowed.join("link")).unwrap();

        let raw_mount = allowed.join("link").join("..");
        let expected = std::fs::canonicalize(&outside).unwrap();
        let config = WasiToolConfig::new().with_mount(&raw_mount, "/data", WasiAccess::ReadWrite);

        let required = config.required_capabilities().unwrap();
        let validated = config.validate().unwrap();

        assert_eq!(validated.sandbox_config.preopens[0].host_path, expected);
        assert_eq!(required, validated.required_capabilities);
        assert!(required.contains(&Capability::ReadFile(expected.clone())));
        assert!(required.contains(&Capability::WriteFile(expected)));
        assert!(
            !required.contains(&Capability::ReadFile(raw_mount)),
            "raw symlink traversal path must not be authorized"
        );
    }
}
