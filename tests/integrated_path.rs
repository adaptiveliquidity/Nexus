//! PR-7: Integrated path tests.
//!
//! Verifies the combined production path: capability-checked +
//! input-fed + precompiled module execution, all in a single call.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nexus::daemon::module_cache::ModuleCache;
use nexus::security::Capability;
use nexus::{HypervisorConfig, NexusHypervisor, ToolDefinition};

fn trivial_wasm() -> Vec<u8> {
    wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#).unwrap()
}

#[tokio::test]
async fn full_integrated_path_succeeds() {
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    let token = hv.issue_token(
        Capability::ReadFile(PathBuf::from("/data")),
        "test",
        Duration::from_secs(60),
    );

    let cache = ModuleCache::new();
    let engine = hv.sandbox_engine();
    let wasm = trivial_wasm();
    let module = cache.get_or_compile(&engine, &wasm).unwrap();

    let tool = ToolDefinition::new("integrated_test".into(), wasm)
        .with_capabilities(vec![Capability::ReadFile(PathBuf::from("/data"))]);

    let input = serde_json::json!({"key": "value"});

    let out = hv
        .execute_tool_precompiled_with_tokens(tool, input, std::slice::from_ref(&token), module)
        .await
        .unwrap();

    assert!(out.success, "integrated path should succeed");
}

#[tokio::test]
async fn full_integrated_path_denies_missing_token() {
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();

    let cache = ModuleCache::new();
    let engine = hv.sandbox_engine();
    let wasm = trivial_wasm();
    let module = cache.get_or_compile(&engine, &wasm).unwrap();

    let tool = ToolDefinition::new("denied_test".into(), wasm)
        .with_capabilities(vec![Capability::ReadFile(PathBuf::from("/secret"))]);

    let result = hv
        .execute_tool_precompiled_with_tokens(tool, serde_json::json!({}), &[], module)
        .await;

    assert!(result.is_err(), "missing token should be denied");
}

#[tokio::test]
async fn precompiled_reuses_cached_module() {
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    let cache = ModuleCache::new();
    let engine = hv.sandbox_engine();
    let wasm = trivial_wasm();

    let m1 = cache.get_or_compile(&engine, &wasm).unwrap();
    let m2 = cache.get_or_compile(&engine, &wasm).unwrap();
    assert!(Arc::ptr_eq(&m1, &m2), "cache should return same Arc");

    let tool = ToolDefinition::new("cached".into(), wasm);
    let out = hv
        .execute_tool_precompiled(tool, serde_json::json!({}), m1)
        .await
        .unwrap();
    assert!(out.success);
}
