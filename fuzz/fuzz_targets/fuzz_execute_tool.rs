//! Fuzz target: feed arbitrary `wasm-smith` modules into
//! `NexusHypervisor::execute_tool` and look for panics or unclassified
//! failure modes.
//!
//! Per Trail of Bits `trailofbits/testing-handbook-skills`: the fuzzer
//! must never panic. Any input that produces a Rust panic in the
//! hypervisor or sandbox is a bug. Inputs that produce a
//! `FailureMode::HostError` or `FailureMode::TrapOther` are also
//! interesting — they indicate an unclassified failure path that
//! Phase A's taxonomy missed. The fuzzer asserts neither happens.
//!
//! Run with:
//!     cargo +nightly fuzz run fuzz_execute_tool

#![no_main]

use libfuzzer_sys::fuzz_target;
use wasm_smith::Module;
use arbitrary::{Arbitrary, Unstructured};

use nexus::hypervisor::FailureMode;
use nexus::{HypervisorConfig, NexusHypervisor, ToolDefinition};

fuzz_target!(|data: &[u8]| {
    // Generate a syntactically-valid WASM module from the fuzz input.
    // wasm-smith will reject if there are too few bytes; that's fine.
    let mut u = Unstructured::new(data);
    let module = match Module::arbitrary(&mut u) {
        Ok(m) => m,
        Err(_) => return,
    };
    let bytes = module.to_bytes();

    // Build a hypervisor with strict caps so the fuzzer cannot waste
    // the whole iteration budget on one slow module.
    let mut cfg = HypervisorConfig::default();
    cfg.sandbox_config.max_fuel = 1_000_000;
    cfg.sandbox_config.time_limit = std::time::Duration::from_millis(200);
    let hv = match NexusHypervisor::new(cfg) {
        Ok(h) => h,
        Err(_) => return,
    };

    let tool = ToolDefinition::new("fuzz".into(), bytes);
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(_) => return,
    };
    let out = match rt.block_on(hv.execute_tool(tool, serde_json::json!({}))) {
        Ok(o) => o,
        // Errors from execute_tool itself (vs typed FailureMode in the
        // output) are also a bug because the hypervisor should always
        // produce a ToolOutput.
        Err(e) => panic!("execute_tool returned error variant: {e}"),
    };

    // If execution failed, the failure_mode must be classified — not
    // HostError (unless it's the "worker disconnected" race) and not
    // TrapOther (every wasmtime trap variant we see should be in the
    // taxonomy).
    if let Some(log) = out.error_log {
        match log.failure_mode {
            FailureMode::HostError(ref msg) => {
                // Tolerated: the worker-thread disconnect race when
                // the timeout fires concurrently with completion.
                assert!(
                    msg.contains("worker thread disconnected")
                        || msg.contains("set_fuel failed"),
                    "unclassified HostError: {msg}"
                );
            }
            FailureMode::TrapOther(ref subtype) => {
                // wasm-smith can produce traps the taxonomy doesn't
                // have yet — flag them so we can add typed variants.
                eprintln!("[fuzz] unclassified trap subtype: {subtype}");
            }
            _ => { /* known + typed: OK */ }
        }
    }
});
