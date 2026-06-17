//! WASM Micro-Sandbox Runtime
//!
//! High-performance WebAssembly sandbox with fuel metering for AI agent execution.

use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use wasmtime::{Config, Engine, Linker, Module, Store, Trap};

use crate::error::{NexusError, Result};
use crate::hypervisor::failure_mode::FailureMode;
use crate::telemetry::{CaptureSite, CapturedCallStack};

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
    /// Post-call exported globals (captured after entrypoint returns)
    pub post_call_globals: Option<Vec<crate::snapshot::GlobalSnapshot>>,
    /// Post-call exported tables (captured after entrypoint returns)
    pub post_call_tables: Option<Vec<crate::snapshot::TableSnapshot>>,
    /// Diagnostic-only WASM call stack captured at a trap/failure site.
    ///
    /// This is never serialized into [`crate::snapshot::ExecutionState`] and
    /// must not affect memory checksums or snapshot digests.
    pub call_stack: Option<CapturedCallStack>,
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
            post_call_globals: None,
            post_call_tables: None,
            call_stack: None,
        }
    }

    pub fn with_pre_call_memory(mut self, mem: Option<Vec<u8>>) -> Self {
        self.pre_call_memory = mem;
        self
    }

    pub fn with_call_stack(mut self, call_stack: Option<CapturedCallStack>) -> Self {
        self.call_stack = call_stack;
        self
    }

    pub fn with_post_call_state(
        mut self,
        globals: Vec<crate::snapshot::GlobalSnapshot>,
        tables: Vec<crate::snapshot::TableSnapshot>,
    ) -> Self {
        self.post_call_globals = Some(globals);
        self.post_call_tables = Some(tables);
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
            post_call_globals: None,
            post_call_tables: None,
            call_stack: None,
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
            post_call_globals: None,
            post_call_tables: None,
            call_stack: None,
        }
    }
}

/// State captured after running a module up to a fuel cap.
#[derive(Debug, Clone)]
pub struct StepCapture {
    /// Fuel consumed by the bounded execution attempt.
    pub fuel_consumed: u64,
    /// True when the guest finished before exhausting `fuel_cap`.
    pub completed: bool,
    /// Post-execution linear memory exported as `"memory"`, if present.
    pub memory: Option<Vec<u8>>,
    /// Post-execution exported globals.
    pub globals: Vec<crate::snapshot::GlobalSnapshot>,
}

/// WASM Micro-Sandbox with fuel metering.
///
/// Cloning is cheap: `engine` is an `Arc` and `config` is a small value type.
/// The pool clones a sandbox into a blocking task so pooled execution does not
/// block a tokio worker thread.
#[derive(Clone)]
pub struct WasmSandbox {
    pub(crate) engine: Arc<Engine>,
    pub(crate) config: SandboxConfig,
    pub(crate) epoch: Arc<SandboxEpoch>,
    #[cfg(test)]
    active_workers: Arc<std::sync::atomic::AtomicUsize>,
}

/// Reply payload sent from the worker thread to the timeout-bounded receiver.
/// Carries a typed `FailureMode` so callers do not have to substring-match.
/// `pre_call_memory` is populated whenever instantiation succeeded so the
/// hypervisor can build a real snapshot from the actual WASM linear memory.
enum ExecReply {
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
        call_stack: Option<CapturedCallStack>,
    },
}

enum StepReply {
    Captured {
        fuel_consumed: u64,
        completed: bool,
        memory: Option<Vec<u8>>,
        globals: Vec<crate::snapshot::GlobalSnapshot>,
    },
    Degenerate,
    Failed(String),
}

const EPOCH_TICK_MS: u128 = 10;
pub(crate) const TIMEOUT_JOIN_GRACE: Duration = Duration::from_millis(200);

pub(crate) struct SandboxEpoch {
    current: AtomicU64,
    next_deadline: AtomicU64,
}

impl SandboxEpoch {
    fn new() -> Self {
        Self {
            current: AtomicU64::new(0),
            next_deadline: AtomicU64::new(1),
        }
    }

    pub(crate) fn reserve_deadline(&self, time_limit: Duration) -> u64 {
        let ticks = epoch_ticks_for_time_limit(time_limit);
        let mut deadline = self.next_deadline.fetch_add(ticks, Ordering::SeqCst);
        loop {
            let current = self.current.load(Ordering::SeqCst);
            if deadline > current {
                return deadline;
            }

            let adjusted = current.saturating_add(ticks.max(1));
            match self.next_deadline.compare_exchange(
                deadline.saturating_add(ticks),
                adjusted.saturating_add(1),
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return adjusted,
                Err(next) => deadline = next,
            }
        }
    }

    pub(crate) fn relative_ticks_until(&self, deadline: u64) -> u64 {
        let current = self.current.load(Ordering::SeqCst);
        deadline.saturating_sub(current).max(1)
    }

    pub(crate) fn advance_to(&self, engine: &Engine, deadline: u64) {
        loop {
            let current = self.current.load(Ordering::SeqCst);
            if current >= deadline {
                break;
            }

            if self
                .current
                .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                increment_engine_epoch(engine);
            }
        }
    }
}

fn epoch_ticks_for_time_limit(time_limit: Duration) -> u64 {
    let millis = time_limit.as_millis().max(1);
    let ticks = millis.div_ceil(EPOCH_TICK_MS);
    ticks.min(u64::MAX as u128) as u64
}

pub(crate) fn configure_epoch_deadline<T>(store: &mut Store<T>, relative_ticks: u64) {
    #[cfg(target_has_atomic = "64")]
    {
        store.epoch_deadline_trap();
        store.set_epoch_deadline(relative_ticks.max(1));
    }

    #[cfg(not(target_has_atomic = "64"))]
    {
        let _ = (store, relative_ticks);
    }
}

fn increment_engine_epoch(engine: &Engine) {
    #[cfg(target_has_atomic = "64")]
    engine.increment_epoch();

    #[cfg(not(target_has_atomic = "64"))]
    let _ = engine;
}

pub(crate) fn join_with_timeout<T>(handle: JoinHandle<T>, grace: Duration) -> bool {
    let deadline = Instant::now()
        .checked_add(grace)
        .unwrap_or_else(Instant::now);
    loop {
        if handle.is_finished() {
            let _ = handle.join();
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(Duration::from_millis(1)));
    }
}

pub(crate) fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

pub(crate) fn timeout_mode(time_limit: Duration, observed: Duration) -> FailureMode {
    FailureMode::Timeout {
        limit_ms: duration_millis(time_limit),
        observed_ms: duration_millis(observed),
    }
}

pub(crate) fn is_epoch_interrupt(err: &wasmtime::Error) -> bool {
    if matches!(err.downcast_ref::<Trap>(), Some(Trap::Interrupt)) {
        return true;
    }
    err.chain()
        .any(|cause| matches!(cause.downcast_ref::<Trap>(), Some(Trap::Interrupt)))
}

#[cfg(test)]
struct WorkerThreadGuard {
    active_workers: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl WorkerThreadGuard {
    fn new(active_workers: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        active_workers.fetch_add(1, Ordering::SeqCst);
        Self { active_workers }
    }
}

#[cfg(test)]
impl Drop for WorkerThreadGuard {
    fn drop(&mut self) {
        self.active_workers.fetch_sub(1, Ordering::SeqCst);
    }
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
        cfg.epoch_interruption(true);

        let engine = Engine::new(&cfg)
            .map_err(|e| NexusError::ConfigError(format!("Failed to create engine: {}", e)))?;

        Ok(WasmSandbox {
            engine: Arc::new(engine),
            config,
            epoch: Arc::new(SandboxEpoch::new()),
            #[cfg(test)]
            active_workers: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    /// Build a sandbox from a pre-configured `Engine`.
    ///
    /// Used by [`crate::sandbox::pool::SandboxPool`] so cached modules are
    /// compiled with — and executed on — the pool's pooling-allocator engine.
    /// The engine must have `consume_fuel(true)` and `epoch_interruption(true)`
    /// set, matching [`Self::new`].
    pub fn from_engine(engine: Arc<Engine>, config: SandboxConfig) -> Self {
        WasmSandbox {
            engine,
            config,
            epoch: Arc::new(SandboxEpoch::new()),
            #[cfg(test)]
            active_workers: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Access the wasmtime `Engine` for use with `ModuleCache`.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Execute WASM code with fuel + timeout metering.
    ///
    /// Returns a typed `FailureMode` via `ExecutionResult.failure_mode` on
    /// every failure path so the hypervisor can derive the correct
    /// `HealthStatus` and recovery actions without substring matching.
    pub fn execute(&self, wasm_bytes: &[u8], args: &[Vec<u8>]) -> Result<ExecutionResult> {
        let start = Instant::now();

        // Module compilation failures are load-time errors with no execution.
        let module = match Module::from_binary(&self.engine, wasm_bytes) {
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

        self.execute_module(module, args)
    }

    /// Re-run a deterministic module up to `fuel_cap` and capture post-run state.
    ///
    /// This is a debugging primitive for fuel-indexed replay. It mirrors the
    /// normal worker-thread execution path, but returns the memory/global state
    /// observed at the cap. It does not mutate normal execution semantics.
    pub fn execute_to_fuel(
        &self,
        wasm_bytes: &[u8],
        args: &[Vec<u8>],
        fuel_cap: u64,
    ) -> Result<StepCapture> {
        let module = Module::from_binary(&self.engine, wasm_bytes)
            .map_err(|e| NexusError::WasmError(format!("compile failed: {e}")))?;

        let time_limit = self.config.time_limit;
        let engine = self.engine.clone();
        let epoch = self.epoch.clone();
        let epoch_deadline = epoch.reserve_deadline(time_limit);
        let max_memory_bytes = self.config.max_memory_pages as usize * 65536;
        let input_data: Vec<u8> = args.first().cloned().unwrap_or_default();
        let (tx, rx) = std::sync::mpsc::channel::<StepReply>();
        #[cfg(test)]
        let active_workers = self.active_workers.clone();

        let handle = std::thread::spawn(move || {
            #[cfg(test)]
            let _worker_guard = WorkerThreadGuard::new(active_workers);

            let mut store = Store::new(&engine, WasmState::new(max_memory_bytes));
            configure_epoch_deadline(&mut store, epoch.relative_ticks_until(epoch_deadline));
            store.limiter(|s| &mut s.limits);
            if let Err(e) = store.set_fuel(fuel_cap) {
                let _ = tx.send(StepReply::Failed(format!("set_fuel failed: {e}")));
                return;
            }

            let linker = Linker::new(&engine);
            let instance = match linker.instantiate(&mut store, &module) {
                Ok(i) => i,
                Err(_) => {
                    let _ = tx.send(StepReply::Degenerate);
                    return;
                }
            };

            if !input_data.is_empty() {
                if let Some(mem) = instance.get_memory(&mut store, "memory") {
                    let header_size = 4;
                    let needed = header_size + input_data.len();
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
                        let _ = tx.send(StepReply::Degenerate);
                        return;
                    }
                },
            };

            let call_result = start_func.call(&mut store, ());
            let completed = match &call_result {
                Ok(_) => true,
                Err(e) if is_epoch_interrupt(e) => false,
                Err(e) => !matches!(
                    FailureMode::from_anyhow_error(e),
                    Some(FailureMode::FuelExhausted { .. })
                ),
            };
            let fuel_remaining = store.get_fuel().unwrap_or(0);
            let fuel_consumed = fuel_cap.saturating_sub(fuel_remaining);
            let memory = instance
                .get_memory(&mut store, "memory")
                .map(|m| m.data(&store).to_vec());
            let globals = WasmSandbox::capture_globals(&instance, &mut store);

            let _ = tx.send(StepReply::Captured {
                fuel_consumed,
                completed,
                memory,
                globals,
            });
        });

        match rx.recv_timeout(time_limit) {
            Ok(StepReply::Captured {
                fuel_consumed,
                completed,
                memory,
                globals,
            }) => {
                let _ = handle.join();
                Ok(StepCapture {
                    fuel_consumed,
                    completed,
                    memory,
                    globals,
                })
            }
            Ok(StepReply::Degenerate) => {
                let _ = handle.join();
                Ok(StepCapture {
                    fuel_consumed: 0,
                    completed: true,
                    memory: None,
                    globals: Vec::new(),
                })
            }
            Ok(StepReply::Failed(message)) => {
                let _ = handle.join();
                Err(NexusError::WasmError(message))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.epoch.advance_to(&self.engine, epoch_deadline);
                if !join_with_timeout(handle, TIMEOUT_JOIN_GRACE) {
                    tracing::warn!(
                        "WASM replay worker did not stop within {:?} after timeout",
                        TIMEOUT_JOIN_GRACE
                    );
                }
                Err(NexusError::Timeout(time_limit.as_millis() as u64))
            }
            Err(_) => {
                let _ = handle.join();
                Err(NexusError::WasmError(
                    "worker thread disconnected before capturing step".to_string(),
                ))
            }
        }
    }

    /// Execute a precompiled `Module`. Skips `Module::from_binary`,
    /// making repeat invocations of the same WASM significantly faster
    /// when paired with `ModuleCache`.
    pub fn execute_precompiled(
        &self,
        module: Arc<Module>,
        args: &[Vec<u8>],
    ) -> Result<ExecutionResult> {
        self.execute_module(module, args)
    }

    fn capture_globals(
        instance: &wasmtime::Instance,
        store: &mut Store<WasmState>,
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

    fn capture_tables(
        instance: &wasmtime::Instance,
        store: &mut Store<WasmState>,
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

    /// Execute a pre-compiled module. Public so [`crate::sandbox::pool`] can
    /// run modules from its cache on the pooled engine.
    pub fn execute_module(&self, module: Arc<Module>, args: &[Vec<u8>]) -> Result<ExecutionResult> {
        let start = Instant::now();
        let max_fuel = self.config.max_fuel;
        let time_limit = self.config.time_limit;
        let engine = self.engine.clone();
        let epoch = self.epoch.clone();
        let epoch_deadline = epoch.reserve_deadline(time_limit);
        let max_memory_bytes = self.config.max_memory_pages as usize * 65536;
        let input_data: Vec<u8> = args.first().cloned().unwrap_or_default();

        let (tx, rx) = std::sync::mpsc::channel::<ExecReply>();
        #[cfg(test)]
        let active_workers = self.active_workers.clone();

        let handle = std::thread::spawn(move || {
            #[cfg(test)]
            let _worker_guard = WorkerThreadGuard::new(active_workers);

            let worker_start = Instant::now();
            let mut store = Store::new(&engine, WasmState::new(max_memory_bytes));
            configure_epoch_deadline(&mut store, epoch.relative_ticks_until(epoch_deadline));
            store.limiter(|s| &mut s.limits);

            // With consume_fuel(true) in Config, set_fuel is required and
            // succeeds; failures here mean the engine config drifted.
            if let Err(e) = store.set_fuel(max_fuel) {
                let _ = tx.send(ExecReply::Failed {
                    mode: FailureMode::HostError(format!("set_fuel failed: {e}")),
                    fuel_consumed: 0,
                    pre_call_memory: None,
                    globals: Vec::new(),
                    tables: Vec::new(),
                    call_stack: None,
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
                        globals: Vec::new(),
                        tables: Vec::new(),
                        call_stack: None,
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

            // Write input into guest memory: [len: u32 LE][data].
            // Skipped when input is empty or the module has no memory export.
            if !input_data.is_empty() {
                if let Some(mem) = instance.get_memory(&mut store, "memory") {
                    let header_size = 4;
                    let needed = header_size + input_data.len();
                    let data = mem.data_mut(&mut store);
                    if needed <= data.len() {
                        let len_bytes = (input_data.len() as u32).to_le_bytes();
                        data[..4].copy_from_slice(&len_bytes);
                        data[4..4 + input_data.len()].copy_from_slice(&input_data);
                    }
                }
            }

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
                            globals: Vec::new(),
                            tables: Vec::new(),
                            call_stack: None,
                        });
                        return;
                    }
                },
            };

            let call_result = start_func.call(&mut store, ());
            // Compute fuel consumption regardless of outcome.
            let fuel_remaining = store.get_fuel().unwrap_or(0);
            let fuel_consumed = max_fuel.saturating_sub(fuel_remaining);

            let globals = WasmSandbox::capture_globals(&instance, &mut store);
            let tables = WasmSandbox::capture_tables(&instance, &mut store);

            match call_result {
                Ok(_) => {
                    let _ = tx.send(ExecReply::Ok {
                        fuel_consumed,
                        pre_call_memory,
                        globals,
                        tables,
                    });
                }
                Err(e) => {
                    // RFC 0002 Option A: capture diagnostic frames only when
                    // execution has already failed. Wasmtime 45 does not expose
                    // locals, operand-stack values, or a restore/resume API, so
                    // this metadata must never become snapshot state. Function
                    // names and offsets depend on current wasmtime config and
                    // module debug/name/address-map data; do not force-enable
                    // expensive debug info globally here.
                    let call_stack = e
                        .downcast_ref::<wasmtime::WasmBacktrace>()
                        .map(|bt| CapturedCallStack::from_wasm_backtrace(bt, CaptureSite::Trap));
                    let mode = if is_epoch_interrupt(&e) {
                        timeout_mode(time_limit, worker_start.elapsed())
                    } else {
                        let mode = FailureMode::from_anyhow_error(&e).unwrap_or_else(|| {
                            FailureMode::HostError(format!("wasm error: {e:#}"))
                        });
                        match mode {
                            FailureMode::FuelExhausted { .. } => {
                                FailureMode::FuelExhausted { limit: max_fuel }
                            }
                            other => other,
                        }
                    };
                    let _ = tx.send(ExecReply::Failed {
                        mode,
                        fuel_consumed,
                        pre_call_memory,
                        globals,
                        tables,
                        call_stack,
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
            Ok(ExecReply::Failed {
                mode,
                fuel_consumed,
                pre_call_memory,
                globals,
                tables,
                call_stack,
            }) => {
                let _ = handle.join();
                Ok(
                    ExecutionResult::failure_from_mode(mode, fuel_consumed, duration_ms)
                        .with_pre_call_memory(pre_call_memory)
                        .with_post_call_state(globals, tables)
                        .with_call_stack(call_stack),
                )
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.epoch.advance_to(&self.engine, epoch_deadline);
                if !join_with_timeout(handle, TIMEOUT_JOIN_GRACE) {
                    tracing::warn!(
                        "WASM worker did not stop within {:?} after epoch cancellation",
                        TIMEOUT_JOIN_GRACE
                    );
                }
                let mode = timeout_mode(time_limit, start.elapsed());
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
}

/// Per-store state for the pure-compute path. Carries a `StoreLimits` so the
/// configured `SandboxConfig::max_memory_pages` ceiling is enforced by wasmtime
/// (a guest that grows linear memory past the limit traps instead of being
/// bounded only by the wasm32 4 GiB hard cap).
pub struct WasmState {
    limits: wasmtime::StoreLimits,
}

impl WasmState {
    /// Build store state limiting linear memory to `max_memory_bytes`.
    pub fn new(max_memory_bytes: usize) -> Self {
        WasmState {
            limits: wasmtime::StoreLimitsBuilder::new()
                .memory_size(max_memory_bytes)
                .build(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tight_loop_wasm() -> Vec<u8> {
        wat::parse_str(
            r#"(module
                (func (export "_start")
                    (loop $spin
                        br $spin)))"#,
        )
        .unwrap()
    }

    fn timeout_sandbox() -> WasmSandbox {
        WasmSandbox::new(SandboxConfig {
            max_fuel: u64::MAX / 4,
            time_limit: Duration::from_millis(20),
            ..SandboxConfig::default()
        })
        .unwrap()
    }

    #[test]
    fn tight_loop_timeout_exits_worker_thread() {
        let sandbox = timeout_sandbox();
        let wasm = tight_loop_wasm();
        let start = Instant::now();

        let result = sandbox.execute(&wasm, &[]).unwrap();

        assert!(!result.success);
        assert!(matches!(
            result.failure_mode,
            Some(FailureMode::Timeout { .. })
        ));
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "timeout cancellation should finish within the bounded join window"
        );
        assert_eq!(
            sandbox.active_workers.load(Ordering::SeqCst),
            0,
            "timeout worker should exit after epoch cancellation"
        );
    }

    #[test]
    fn sequential_timeouts_do_not_accumulate_orphaned_runtime_threads() {
        let sandbox = timeout_sandbox();
        let wasm = tight_loop_wasm();

        for attempt in 0..5 {
            let result = sandbox.execute(&wasm, &[]).unwrap();
            assert!(
                matches!(result.failure_mode, Some(FailureMode::Timeout { .. })),
                "attempt {attempt} should time out, got {:?}",
                result.failure_mode
            );
            assert_eq!(
                sandbox.active_workers.load(Ordering::SeqCst),
                0,
                "attempt {attempt} left a runtime worker running"
            );
        }
    }
}
