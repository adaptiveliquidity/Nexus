//! Warm sandbox pool backed by wasmtime's pooling instance allocator.
//!
//! The pool owns a single `Engine` configured with
//! `InstanceAllocationStrategy::Pooling`, which pre-maps memory and instance
//! slots so that `Store` + `Instance` creation reuses existing regions instead
//! of mapping new ones on every call. A SHA-256-keyed [`ModuleCache`] (compiled
//! with that same engine) avoids recompilation, and a `tokio::sync::Semaphore`
//! bounds the number of in-flight executions to the pool's configured capacity.
//!
//! ## Isolation
//!
//! The pool does **not** reuse `Store` or `Instance` across executions — each
//! call gets a fresh instance. The "pool" is the allocator's slot pool plus the
//! compiled-module cache, not a pool of live guest state. This keeps every
//! execution isolated while still avoiding the per-call mmap and compile costs.
//!
//! ## Conservative limits
//!
//! wasmtime's pooling allocator reserves fixed virtual-memory regions up front
//! (it can reserve several GiB of virtual memory per linear-memory slot). The
//! defaults here are deliberately modest (100 slots, 32 MiB max linear memory)
//! and Linux-first; Windows compiles but is not performance-tuned.

use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use wasmtime::{Config, Engine, InstanceAllocationStrategy, Module, PoolingAllocationConfig};

use crate::error::{NexusError, Result};
use crate::sandbox::module_cache::ModuleCache;
use crate::sandbox::wasm_runtime::{
    ExecutionResult, RestoredExecutionState, SandboxConfig, WasmSandbox,
};

/// Configuration for a [`SandboxPool`].
///
/// `total_instances` sizes the wasmtime pooling allocator (a fixed up-front
/// reservation). `max_concurrency` bounds in-flight executions via a semaphore
/// and must not exceed `total_instances`, since each in-flight execution holds
/// one instance slot.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of concurrent in-flight executions (semaphore permits).
    pub max_concurrency: usize,
    /// LRU capacity of the compiled-module cache.
    pub max_cached_modules: usize,
    /// Number of instance/memory/table slots the pooling allocator reserves.
    /// Must be >= `max_concurrency`.
    pub total_instances: u32,
    /// Maximum linear-memory size per instance, in bytes.
    pub max_memory_bytes: usize,
    /// Maximum table elements per instance.
    pub table_elements: usize,
    /// Per-execution sandbox config (fuel, time limit) reused for every call.
    pub sandbox_config: SandboxConfig,
}

impl Default for PoolConfig {
    fn default() -> Self {
        PoolConfig {
            max_concurrency: 16,
            max_cached_modules: 256,
            // Conservative up-front reservation. Scale only after density
            // benchmark data justifies it.
            total_instances: 100,
            // 32 MiB matches SandboxConfig::default (512 pages * 64 KiB).
            max_memory_bytes: 32 * 1024 * 1024,
            table_elements: 10_000,
            sandbox_config: SandboxConfig::default(),
        }
    }
}

/// A held execution slot: an owned semaphore permit plus the precompiled
/// module to run. Dropping the permit returns the slot to the pool.
///
/// This is **not** a reusable `Store`/`Instance` — it carries only the
/// concurrency permit and an `Arc<Module>`. Execution still builds a fresh
/// instance via [`SandboxPool::execute`].
pub struct PooledModulePermit {
    _permit: OwnedSemaphorePermit,
    module: Arc<Module>,
}

impl PooledModulePermit {
    /// The precompiled module associated with this permit.
    pub fn module(&self) -> &Arc<Module> {
        &self.module
    }
}

/// A pool of warm sandbox execution slots.
pub struct SandboxPool {
    engine: Arc<Engine>,
    cache: ModuleCache,
    semaphore: Arc<Semaphore>,
    /// A sandbox built on the pooled engine; used to run cached modules.
    sandbox: WasmSandbox,
    config: PoolConfig,
}

impl SandboxPool {
    /// Build a pool with a pooling-allocator engine sized from `config`.
    pub fn new(config: PoolConfig) -> Result<Self> {
        if config.max_concurrency == 0 {
            return Err(NexusError::ConfigError(
                "pool max_concurrency must be > 0".into(),
            ));
        }
        if config.max_cached_modules == 0 {
            return Err(NexusError::ConfigError(
                "pool max_cached_modules must be > 0".into(),
            ));
        }
        if (config.max_concurrency as u32) > config.total_instances {
            return Err(NexusError::ConfigError(format!(
                "pool max_concurrency ({}) must not exceed total_instances ({})",
                config.max_concurrency, config.total_instances
            )));
        }

        let engine = Arc::new(Self::build_pooled_engine(&config)?);
        let sandbox = WasmSandbox::from_engine(engine.clone(), config.sandbox_config.clone());
        let cache = ModuleCache::with_capacity(config.max_cached_modules);
        let semaphore = Arc::new(Semaphore::new(config.max_concurrency));

        Ok(SandboxPool {
            engine,
            cache,
            semaphore,
            sandbox,
            config,
        })
    }

    fn build_pooled_engine(config: &PoolConfig) -> Result<Engine> {
        let mut pool = PoolingAllocationConfig::default();
        pool.total_core_instances(config.total_instances);
        pool.total_memories(config.total_instances);
        pool.total_tables(config.total_instances);
        pool.max_memory_size(config.max_memory_bytes);
        pool.table_elements(config.table_elements);

        let mut cfg = Config::new();
        cfg.consume_fuel(true);
        cfg.epoch_interruption(true);
        cfg.allocation_strategy(InstanceAllocationStrategy::Pooling(pool));

        Engine::new(&cfg)
            .map_err(|e| NexusError::ConfigError(format!("failed to create pooled engine: {e}")))
    }

    /// Acquire an execution slot and the precompiled module for `wasm_bytes`.
    ///
    /// Awaits a free semaphore permit (backpressure when the pool is saturated),
    /// then compiles the module if not already cached. The returned permit holds
    /// the slot until dropped.
    pub async fn acquire(&self, wasm_bytes: &[u8]) -> Result<PooledModulePermit> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| NexusError::ConfigError(format!("pool semaphore closed: {e}")))?;

        let module = self
            .cache
            .get_or_compile(&self.engine, wasm_bytes)
            .map_err(|e| NexusError::WasmError(format!("module compile failed: {e}")))?;

        Ok(PooledModulePermit {
            _permit: permit,
            module,
        })
    }

    /// Execute a precompiled module held by `permit` on the pooled engine.
    ///
    /// Builds a fresh `Store` + `Instance` for isolation; the pooling allocator
    /// reuses pre-mapped slots so this is cheap relative to a non-pooled engine.
    ///
    /// This is a **blocking** call: `WasmSandbox::execute_module` spawns a
    /// worker thread and blocks on it up to the configured time limit. Async
    /// callers should prefer [`Self::execute_pooled`], which offloads this to
    /// tokio's blocking pool instead of stalling a runtime worker.
    pub fn execute(
        &self,
        permit: &PooledModulePermit,
        args: &[Vec<u8>],
    ) -> Result<ExecutionResult> {
        self.sandbox.execute_module(permit.module.clone(), args)
    }

    /// Convenience: acquire a slot, run the module, release the slot.
    ///
    /// The synchronous, thread-blocking execution runs on `spawn_blocking` so
    /// it does not monopolize a tokio worker thread — important under the high
    /// concurrency the pool is built for. The permit is held inside the
    /// blocking task and released when it returns.
    pub async fn execute_pooled(
        &self,
        wasm_bytes: &[u8],
        args: &[Vec<u8>],
    ) -> Result<ExecutionResult> {
        self.execute_pooled_with_entry(wasm_bytes, args, "_start")
            .await
    }

    /// Convenience: acquire a slot, run the module at an explicit entrypoint,
    /// and release the slot.
    pub async fn execute_pooled_with_entry(
        &self,
        wasm_bytes: &[u8],
        args: &[Vec<u8>],
        entry_point: &str,
    ) -> Result<ExecutionResult> {
        let permit = self.acquire(wasm_bytes).await?;
        // Clone the cheap handles needed by the blocking closure. The sandbox
        // is Arc<Engine> + small config; the module is an Arc. The permit moves
        // in so the slot stays reserved for the whole execution.
        let sandbox = self.sandbox.clone();
        let module = permit.module.clone();
        let args = args.to_vec();
        let entry_point = entry_point.to_string();
        tokio::task::spawn_blocking(move || {
            let _permit = permit; // released when the task completes
            sandbox.execute_module_with_entry(module, &args, &entry_point)
        })
        .await
        .map_err(|e| NexusError::WasmError(format!("pooled execution task panicked: {e}")))?
    }

    /// Convenience: acquire a slot, restore captured runtime state into the
    /// fresh instance, run the module, and release the slot.
    pub async fn execute_pooled_from_restored_state(
        &self,
        wasm_bytes: &[u8],
        args: &[Vec<u8>],
        restored_state: RestoredExecutionState,
    ) -> Result<ExecutionResult> {
        self.execute_pooled_from_restored_state_with_entry(
            wasm_bytes,
            args,
            restored_state,
            "_start",
        )
        .await
    }

    /// Convenience: acquire a slot, restore captured runtime state into the
    /// fresh instance, run an explicit entrypoint, and release the slot.
    pub async fn execute_pooled_from_restored_state_with_entry(
        &self,
        wasm_bytes: &[u8],
        args: &[Vec<u8>],
        restored_state: RestoredExecutionState,
        entry_point: &str,
    ) -> Result<ExecutionResult> {
        let permit = self.acquire(wasm_bytes).await?;
        let sandbox = self.sandbox.clone();
        let module = permit.module.clone();
        let args = args.to_vec();
        let entry_point = entry_point.to_string();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            sandbox.execute_precompiled_from_restored_state_with_entry(
                module,
                &args,
                restored_state,
                &entry_point,
            )
        })
        .await
        .map_err(|e| NexusError::WasmError(format!("pooled execution task panicked: {e}")))?
    }

    /// Number of currently available execution slots.
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// `(hits, misses)` for the compiled-module cache.
    pub fn cache_stats(&self) -> (u64, u64) {
        (self.cache.hits(), self.cache.misses())
    }

    /// Number of distinct modules currently cached.
    pub fn cached_modules(&self) -> usize {
        self.cache.len()
    }

    /// The pool's configuration.
    pub fn config(&self) -> &PoolConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trivial_wasm() -> Vec<u8> {
        wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#).unwrap()
    }

    #[tokio::test]
    async fn pool_creates_and_executes() {
        let pool = SandboxPool::new(PoolConfig::default()).unwrap();
        let wasm = trivial_wasm();
        let result = pool.execute_pooled(&wasm, &[]).await.unwrap();
        assert!(
            result.success,
            "execution should succeed: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn acquire_release_frees_permit() {
        let cfg = PoolConfig {
            max_concurrency: 2,
            ..Default::default()
        };
        let pool = SandboxPool::new(cfg).unwrap();
        let wasm = trivial_wasm();

        assert_eq!(pool.available_permits(), 2);
        {
            let _p = pool.acquire(&wasm).await.unwrap();
            assert_eq!(pool.available_permits(), 1);
        }
        // Permit dropped — slot returned.
        assert_eq!(pool.available_permits(), 2);
    }

    #[tokio::test]
    async fn cache_hit_on_second_acquire() {
        let pool = SandboxPool::new(PoolConfig::default()).unwrap();
        let wasm = trivial_wasm();

        let p1 = pool.acquire(&wasm).await.unwrap();
        drop(p1);
        let _p2 = pool.acquire(&wasm).await.unwrap();

        let (hits, misses) = pool.cache_stats();
        assert_eq!(misses, 1, "first acquire compiles");
        assert_eq!(hits, 1, "second acquire reuses");
        assert_eq!(pool.cached_modules(), 1);
    }

    #[tokio::test]
    async fn invalid_module_returns_clean_error() {
        let pool = SandboxPool::new(PoolConfig::default()).unwrap();
        let garbage = vec![0u8, 1, 2, 3, 4];
        let err = pool.acquire(&garbage).await;
        assert!(err.is_err(), "garbage bytes should fail to compile");
    }

    #[tokio::test]
    async fn rejects_concurrency_over_total_instances() {
        let cfg = PoolConfig {
            max_concurrency: 200,
            total_instances: 100,
            ..Default::default()
        };
        assert!(SandboxPool::new(cfg).is_err());
    }

    #[tokio::test]
    async fn sixteen_concurrent_tasks_complete() {
        let cfg = PoolConfig {
            max_concurrency: 16,
            total_instances: 100,
            ..Default::default()
        };
        let pool = Arc::new(SandboxPool::new(cfg).unwrap());
        let wasm = Arc::new(trivial_wasm());

        let mut handles = Vec::new();
        for _ in 0..16 {
            let pool = pool.clone();
            let wasm = wasm.clone();
            handles.push(tokio::spawn(async move {
                pool.execute_pooled(&wasm, &[]).await
            }));
        }

        for h in handles {
            let result = h.await.unwrap().unwrap();
            assert!(result.success);
        }
        // All permits returned.
        assert_eq!(pool.available_permits(), 16);
    }
}
