//! Regression tests for `SandboxConfig::max_memory_pages` enforcement
//! (P1 security audit finding #4). The configured ceiling must actually be
//! enforced by wasmtime store limits, not just declared.

use nexus::sandbox::{SandboxConfig, WasmSandbox};

/// A module whose *initial* linear memory exceeds the configured limit must be
/// rejected (instantiation fails under the store limiter).
#[test]
fn memory_limit_rejects_oversized_module() {
    let config = SandboxConfig {
        max_memory_pages: 2, // 128 KiB
        ..SandboxConfig::default()
    };
    let sandbox = WasmSandbox::new(config).unwrap();
    // 10 pages = 640 KiB initial, well over the 2-page (128 KiB) ceiling.
    let wasm = wat::parse_str(r#"(module (memory (export "memory") 10) (func (export "_start")))"#)
        .unwrap();

    let result = sandbox.execute(&wasm, &[]).unwrap();
    assert!(
        !result.success,
        "a module requesting 10 pages must be rejected under a 2-page limit"
    );
}

/// A module within the configured budget runs normally.
#[test]
fn memory_limit_allows_within_budget() {
    let config = SandboxConfig {
        max_memory_pages: 16, // 1 MiB
        ..SandboxConfig::default()
    };
    let sandbox = WasmSandbox::new(config).unwrap();
    let wasm = wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#)
        .unwrap();

    let result = sandbox.execute(&wasm, &[]).unwrap();
    assert!(
        result.success,
        "a 1-page module must run under a 16-page limit: {:?}",
        result.error
    );
}
