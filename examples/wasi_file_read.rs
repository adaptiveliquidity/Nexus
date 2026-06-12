//! End-to-end example: capability-gated WASI file read.
//!
//! Demonstrates the full Nexus flow:
//!   1. Create a hypervisor with default config
//!   2. Issue an Ed25519-signed capability token for ReadFile
//!   3. Define a WASI tool that imports `fd_read` from the host
//!   4. Execute through `execute_tool_wasi` with token validation
//!   5. Show result including fuel consumed and timing
//!
//! Usage:
//!     cargo run --example wasi_file_read
//!
//! This creates a temp directory, writes a test file, issues a ReadFile
//! capability token for that directory, and runs a WASI module that
//! proves the WASI linker is active (the module uses `proc_exit`).

use std::time::Duration;

use nexus::security::Capability;
use nexus::{HypervisorConfig, NexusHypervisor, ToolDefinition};

fn main() -> anyhow::Result<()> {
    println!("Nexus WASI + Capability Enforcement Demo");
    println!("=========================================\n");

    // --- Step 1: Stand up the hypervisor ---
    let config = HypervisorConfig::default();
    let hypervisor = NexusHypervisor::new(config)?;
    println!("[1] Hypervisor ready\n");

    // --- Step 2: Issue a capability token via the hypervisor's key ---
    let tmp = tempfile::tempdir()?;
    let tmp_path = tmp.path().to_path_buf();

    std::fs::write(tmp_path.join("hello.txt"), b"Hello from Nexus!")?;
    println!("[2] Test file created at {}/hello.txt", tmp_path.display());

    let token = hypervisor.issue_token(
        Capability::ReadFile(tmp_path.clone()),
        "demo-orchestrator",
        Duration::from_secs(60),
    )?;
    println!("    Issued ReadFile token (id={}, chain_depth={})\n",
        &token.id.to_string()[..8], token.chain_depth);

    // --- Step 3: Define a WASI-aware tool ---
    // This module imports proc_exit from WASI preview 1 — it will only
    // instantiate if the WASI linker is wired up (the pure-compute path
    // would fail with an unresolved import).
    let wasm = wat::parse_str(
        r#"(module
            (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
            (memory (export "memory") 1)
            (func (export "_start")
                ;; A real tool would call fd_read here.
                ;; For this demo, we prove the WASI linker resolved
                ;; the import by reaching _start and exiting cleanly.
                (call $exit (i32.const 0))
            )
        )"#,
    )?;

    let tool = ToolDefinition::new("wasi_file_reader".into(), wasm)
        .with_capabilities(vec![Capability::ReadFile(tmp_path.clone())]);
    println!("[3] Tool defined: '{}' (requires ReadFile on {})\n",
        tool.name, tmp_path.display());

    // --- Step 4: Execute through the WASI path ---
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    println!("[4] Executing with capability token...");
    let output = rt.block_on(
        hypervisor.execute_tool_wasi(tool, serde_json::json!({}), &[token])
    )?;

    // --- Step 5: Report result ---
    println!("\n[5] Result");
    println!("    success:       {}", output.success);
    println!("    time:          {}ms", output.execution_time_ms);
    println!("    fuel consumed: {}", output.fuel_consumed);
    if let Some(err) = &output.error {
        println!("    error:         {err}");
    }
    println!();

    // --- Bonus: Show what happens WITHOUT a valid token ---
    println!("[bonus] Attempting execution without required token...");
    let tool2 = ToolDefinition::new("denied_tool".into(), wat::parse_str(
        r#"(module (memory (export "memory") 1) (func (export "_start")))"#,
    )?)
    .with_capabilities(vec![Capability::ReadFile(tmp_path)]);

    let denied = rt.block_on(
        hypervisor.execute_tool_wasi(tool2, serde_json::json!({}), &[]) // no tokens
    );
    match denied {
        Err(e) => println!("    Correctly denied: {e}"),
        Ok(o) => println!("    Unexpected success: {o:?}"),
    }

    println!("\nDone.");
    Ok(())
}
