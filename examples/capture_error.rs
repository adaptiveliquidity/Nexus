//! Phase 3 evidence capture: run a named failing WASM scenario through the
//! real `NexusHypervisor` and serialize the resulting `ErrorLog` as JSON.
//!
//! Usage:
//!     cargo run --release --example capture_error -- <scenario> <output.json>
//!
//! Scenarios:
//!   infinite_loop  — `(loop (br 0))`, caught by the 500 ms timeout path
//!   trap_unreachable — `unreachable` instruction, caught as an EXECUTION_TRAP
//!   div_by_zero    — `i32.div_s` by 0, caught as an EXECUTION_TRAP
//!   stack_overflow — recursive function with no base case, caught as a trap
//!   missing_start  — module without `_start` or `main`, caught at link time
//!
//! Each scenario is real WASM compiled in-process via `wat`; the failure mode
//! is detected by the same code path that production tools would hit.

use std::env;
use std::fs;
use std::process::ExitCode;

use nexus::hypervisor::{HypervisorConfig, NexusHypervisor, ToolDefinition};

fn wat_for(scenario: &str) -> Option<&'static str> {
    // Every runtime-trap scenario exports a memory so the hypervisor's
    // pre-call-memory capture has something to snapshot. The
    // `missing_start` and `invalid_module` scenarios deliberately
    // exercise the load-time-failure / no-rollback path.
    match scenario {
        "infinite_loop" => Some(
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start") (loop $l (br $l))))"#,
        ),
        "trap_unreachable" => Some(
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start") unreachable))"#,
        ),
        "div_by_zero" => Some(
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start")
                    i32.const 1 i32.const 0 i32.div_s drop))"#,
        ),
        "stack_overflow" => Some(
            r#"(module
                (memory (export "memory") 1)
                (func $rec (call $rec))
                (func (export "_start") (call $rec)))"#,
        ),
        "missing_start" => Some(
            r#"(module
                (memory (export "memory") 1)
                (func $noop))"#,
        ),
        // Phase B additions: cover more of the failure-mode taxonomy.
        // Reads memory beyond the single allocated page.
        "memory_out_of_bounds" => Some(
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start")
                    i32.const 1000000 i32.load drop))"#,
        ),
        // Indirect call into a table slot that was never populated.
        "indirect_call_null" => Some(
            r#"(module
                (memory (export "memory") 1)
                (table 1 funcref)
                (func (export "_start")
                    i32.const 0
                    call_indirect))"#,
        ),
        // Integer overflow on signed-min / -1 division.
        "integer_overflow" => Some(
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start")
                    i32.const -2147483648
                    i32.const -1
                    i32.div_s
                    drop))"#,
        ),
        // Bad float-to-int conversion (NaN).
        "bad_float_to_int" => Some(
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start")
                    f32.const nan
                    i32.trunc_f32_s
                    drop))"#,
        ),
        // Module bytes that fail validation — exercises InvalidModule.
        "invalid_module" => Some(
            // Truncated module header bytes (start of a valid module then garbage)
            // Wat for a syntactically invalid module: extra closing paren forces
            // a parse error at wat::parse_str time -> InvalidModule via
            // hypervisor's pre-flight (we still want a valid-as-wasm-but-rejected
            // example, so use a function with type mismatch).
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start") (result i32)
                    ;; Declares an i32 return but pushes nothing — validator rejects.
                    ))"#,
        ),
        _ => None,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!(
            "usage: {} <scenario> <output.json>\nscenarios: infinite_loop, trap_unreachable, div_by_zero, stack_overflow, missing_start",
            args.get(0).map(String::as_str).unwrap_or("capture_error")
        );
        return ExitCode::from(2);
    }
    let scenario = &args[1];
    let out_path = &args[2];

    let wat_src = match wat_for(scenario) {
        Some(s) => s,
        None => {
            eprintln!("unknown scenario: {scenario}");
            return ExitCode::from(2);
        }
    };
    let wasm = match wat::parse_str(wat_src) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("wat parse error: {e}");
            return ExitCode::from(1);
        }
    };

    let hv = match NexusHypervisor::new(HypervisorConfig::default()) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("hypervisor build failed: {e}");
            return ExitCode::from(1);
        }
    };

    let tool = ToolDefinition::new(scenario.clone(), wasm);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let output = match rt.block_on(hv.execute_tool(tool, serde_json::json!({}))) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("execute_tool returned error: {e}");
            return ExitCode::from(1);
        }
    };

    // We expect failures here; success would mean the scenario picked the
    // wrong WAT. Still write the full ToolOutput so the analyzer can see it.
    let record = serde_json::json!({
        "scenario": scenario,
        "tool_output": {
            "success": output.success,
            "error": output.error,
            "rollback_performed": output.rollback_performed,
            "execution_time_ms": output.execution_time_ms,
            "fuel_consumed": output.fuel_consumed,
        },
        "error_log": output.error_log,
    });

    let pretty = serde_json::to_string_pretty(&record).expect("serialize");
    if let Some(parent) = std::path::Path::new(out_path).parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Err(e) = fs::write(out_path, pretty) {
        eprintln!("write {out_path} failed: {e}");
        return ExitCode::from(1);
    }
    println!("wrote {out_path} (success={}, rollback={})", output.success, output.rollback_performed);
    ExitCode::SUCCESS
}
