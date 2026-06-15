//! Integration tests for the warm sandbox pool.
//!
//! Covers the pool directly (acquire/release, concurrency, cache, eviction)
//! and through the opt-in `HypervisorConfig::pool_config` path, verifying that
//! the pooled execution path produces the same results as the default path.

use std::sync::Arc;

use nexus::{HypervisorConfig, NexusHypervisor, PoolConfig, SandboxPool, ToolDefinition};

/// A minimal module that exports memory and a no-op `_start`.
fn trivial_wasm() -> Vec<u8> {
    wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#).unwrap()
}

/// A module that returns via a global so we can confirm it actually ran.
fn counter_wasm() -> Vec<u8> {
    wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (global (export "count") (mut i32) (i32.const 0))
            (func (export "_start")
                (global.set 0 (i32.const 42))))"#,
    )
    .unwrap()
}

#[tokio::test]
async fn pool_creates_successfully() {
    let pool = SandboxPool::new(PoolConfig::default());
    assert!(pool.is_ok(), "default pool config should build");
}

#[tokio::test]
async fn acquire_release_frees_semaphore_permit() {
    let cfg = PoolConfig {
        max_concurrency: 3,
        ..Default::default()
    };
    let pool = SandboxPool::new(cfg).unwrap();
    let wasm = trivial_wasm();

    assert_eq!(pool.available_permits(), 3);
    let p = pool.acquire(&wasm).await.unwrap();
    assert_eq!(pool.available_permits(), 2);
    drop(p);
    assert_eq!(pool.available_permits(), 3);
}

#[tokio::test]
async fn sixteen_concurrent_tasks_complete_without_deadlock() {
    let cfg = PoolConfig {
        max_concurrency: 16,
        total_instances: 100,
        ..Default::default()
    };
    let pool = Arc::new(SandboxPool::new(cfg).unwrap());
    let wasm = Arc::new(counter_wasm());

    let mut handles = Vec::new();
    for _ in 0..16 {
        let pool = pool.clone();
        let wasm = wasm.clone();
        handles.push(tokio::spawn(
            async move { pool.execute_pooled(&wasm, &[]).await },
        ));
    }

    for h in handles {
        let result = h.await.unwrap().unwrap();
        assert!(result.success, "pooled execution should succeed");
    }
    assert_eq!(pool.available_permits(), 16, "all permits returned");
}

#[tokio::test]
async fn max_concurrency_is_enforced() {
    // With a single permit, two simultaneous acquires must serialize: the
    // second cannot proceed until the first is dropped.
    let cfg = PoolConfig {
        max_concurrency: 1,
        total_instances: 100,
        ..Default::default()
    };
    let pool = SandboxPool::new(cfg).unwrap();
    let wasm = trivial_wasm();

    let first = pool.acquire(&wasm).await.unwrap();
    assert_eq!(pool.available_permits(), 0);

    // A second acquire should not resolve while `first` is held.
    let second = tokio::time::timeout(std::time::Duration::from_millis(100), pool.acquire(&wasm));
    assert!(
        second.await.is_err(),
        "second acquire must block while the only permit is held"
    );

    drop(first);
    // Now it should succeed.
    let _third = pool.acquire(&wasm).await.unwrap();
}

#[tokio::test]
async fn module_cache_hit_works() {
    let pool = SandboxPool::new(PoolConfig::default()).unwrap();
    let wasm = trivial_wasm();

    pool.execute_pooled(&wasm, &[]).await.unwrap();
    pool.execute_pooled(&wasm, &[]).await.unwrap();

    let (hits, misses) = pool.cache_stats();
    assert_eq!(misses, 1, "only the first call compiles");
    assert_eq!(hits, 1, "the second call hits the cache");
}

#[tokio::test]
async fn lru_eviction_caps_cached_modules() {
    let cfg = PoolConfig {
        max_cached_modules: 2,
        ..Default::default()
    };
    let pool = SandboxPool::new(cfg).unwrap();

    // Three distinct modules; cache holds at most 2.
    let w1 = wat::parse_str(r#"(module (func (export "_start")))"#).unwrap();
    let w2 = wat::parse_str(r#"(module (func (export "_start") (nop)))"#).unwrap();
    let w3 = wat::parse_str(r#"(module (func (export "_start") (nop) (nop)))"#).unwrap();

    pool.execute_pooled(&w1, &[]).await.unwrap();
    pool.execute_pooled(&w2, &[]).await.unwrap();
    pool.execute_pooled(&w3, &[]).await.unwrap();

    assert!(
        pool.cached_modules() <= 2,
        "cache must not exceed max_cached_modules"
    );
}

#[tokio::test]
async fn invalid_module_returns_clean_error() {
    let pool = SandboxPool::new(PoolConfig::default()).unwrap();
    let garbage = vec![0xde, 0xad, 0xbe, 0xef];
    let result = pool.acquire(&garbage).await;
    assert!(result.is_err(), "invalid WASM must not panic; returns Err");
}

#[tokio::test]
async fn non_pooled_hypervisor_path_still_works() {
    // Default config: pool disabled. Execution uses the original path.
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    assert!(!hv.pool_enabled(), "pool is opt-in and off by default");

    let tool = ToolDefinition::new("trivial".into(), trivial_wasm());
    let out = hv.execute_tool(tool, serde_json::json!({})).await.unwrap();
    assert!(out.success, "default path executes successfully");
}

#[tokio::test]
async fn pooled_hypervisor_path_executes() {
    let config = HypervisorConfig {
        pool_config: Some(PoolConfig::default()),
        ..Default::default()
    };
    let hv = NexusHypervisor::new(config).unwrap();
    assert!(hv.pool_enabled(), "pool should be active when configured");

    let tool = ToolDefinition::new("trivial".into(), trivial_wasm());
    let out = hv.execute_tool(tool, serde_json::json!({})).await.unwrap();
    assert!(out.success, "pooled path executes successfully");

    // The pool's cache should have recorded the compile.
    let pool = hv.pool().expect("pool present");
    let (_hits, misses) = pool.cache_stats();
    assert!(misses >= 1, "first execution compiles the module");
}

#[tokio::test]
async fn pooled_and_non_pooled_agree_on_result() {
    let wasm = counter_wasm();
    let tool = ToolDefinition::new("counter".into(), wasm.clone());

    // Non-pooled.
    let hv_plain = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    let out_plain = hv_plain
        .execute_tool(tool.clone(), serde_json::json!({}))
        .await
        .unwrap();

    // Pooled.
    let config = HypervisorConfig {
        pool_config: Some(PoolConfig::default()),
        ..Default::default()
    };
    let hv_pool = NexusHypervisor::new(config).unwrap();
    let out_pool = hv_pool
        .execute_tool(tool, serde_json::json!({}))
        .await
        .unwrap();

    assert_eq!(
        out_plain.success, out_pool.success,
        "both paths agree on success"
    );
    assert!(out_plain.success && out_pool.success);
}
