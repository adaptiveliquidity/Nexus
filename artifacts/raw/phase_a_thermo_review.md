# Phase A Thermo-Nuclear Review — Nexus WASM Sandbox Defect Cleanup

- **Branch / base:** working tree vs `HEAD` (`7e70cab Add validation protocol and benchmark artifacts`)
- **Scope:** files enumerated in the Phase A plan (`failure_mode.rs`, `recovery.rs`, `hypervisor/mod.rs`, `validator/health.rs`, `validator/error_log.rs`, `sandbox/wasm_runtime.rs`, `telemetry/mod.rs`, `examples/capture_error.rs`, `tests/phase3_distinct_outputs.rs`, `lib.rs`). The Phase B `src/instinct/mod.rs` is **out of scope** for review but is implicated by one Phase A finding (M7).
- **PR / BugBot discussion consulted:** none — `gh pr list` against `Adaptive-Liquidity/Nexus` returns zero open PRs for this branch.
- **Reviewer:** Thermo-nuclear-review subagent, diff-scoped per skill rubric.

## Verdict: **CONDITIONAL FAIL — fix H1, H2, M7 before merge; M1-M6 strongly recommended**

The Phase A refactor delivers what it claims at the conceptual level: a typed `FailureMode`, a pluggable `RecoveryPolicy` with per-variant advice, and an `execute_tool` that no longer substring-matches `wasmtime` error text. The new tests in `tests/phase3_distinct_outputs.rs` correctly anchor the regression. **However**, two high-severity regressions sneak in under the cover of the rewrite — an unbounded host-memory copy on the new snapshot-input path (H1) and a mandatory per-call snapshot cost on the success hot path (H2) — and one Phase B file leak (M7) breaks the build. The "rollback" path is, and remains, theatrical (H3): `RollbackResult` is discarded immediately. None of these blockers are visible from the AI-distinct-outputs test alone, which is why this audit calls them out.

---

## Findings — by severity

### HIGH

#### H1 — Unbounded host-memory copy via `pre_call_memory`
- **File:line:** `src/sandbox/wasm_runtime.rs:220-222`
- **Code under review:**

```220:222:src/sandbox/wasm_runtime.rs
let pre_call_memory: Option<Vec<u8>> =
    instance.get_memory(&mut store, "memory")
        .map(|m| m.data(&store).to_vec());
```

- **What changed:** before Phase A, `NexusHypervisor::execute_tool` snapshotted a hardcoded `vec![0u8; 65536]` placeholder (was 64 KiB regardless of module). Phase A replaces that with `m.data(&store).to_vec()`, which allocates **exactly the WASM linear memory size declared by the guest**.
- **Why it is high:** the sandbox's `SandboxConfig::max_memory_pages` field is **not wired to wasmtime** — `WasmSandbox::new` only calls `cfg.consume_fuel(true)`; there is no `Store::limiter`, no `Config::max_memory_size`, no `ResourceLimiter` impl anywhere in `src/sandbox/wasm_runtime.rs`. Confirmed by grep across `src/` (see also M5). A guest declaring `(memory 65536)` (4 GiB initial pages) will:
  1. Instantiate without complaint (no limit enforced).
  2. Get its linear memory copied into a host `Vec<u8>` of 4 GiB.
  3. Get fed into `Snapshot::new` (`src/snapshot/manager.rs:41-73`) which immediately hashes the buffer via SHA-256 and zstd-compresses it on the **calling thread** — i.e. on whatever async runtime invoked `execute_tool`.
- **Blast radius:** host OOM, async runtime starvation. Pre-Phase-A code's 64 KiB placeholder was advisory but at least bounded. Phase A removes that bound while keeping the "demo" path in `src/main.rs:294-316` (`max_memory_pages = 1`) that tries to demonstrate a memory cap — a cap that is not enforced.
- **Recommendation:**
  1. Apply a `ResourceLimiter` to the `Store` so `max_memory_pages` is actually enforced *before* the memory is allocated, and
  2. Independently cap `pre_call_memory` length (e.g. early-return `None` if `m.data_size(&store) > snapshot_max_bytes`).

#### H2 — Snapshot creation is unconditional on the success hot path
- **File:line:** `src/hypervisor/mod.rs:234-251`

```234:251:src/hypervisor/mod.rs
let snapshot = if let Some(ref mem) = exec_result.pre_call_memory {
    let fs_diff = FilesystemDiff::new();
    let exec_state = ExecutionState::default();
    let metadata = SnapshotMetadata::new(
        tool.name.clone(),
        format!("{:x}", sha2::Sha256::digest(&tool.wasm_bytes)),
    );
    let snap = self.snapshot_manager.create_snapshot(
        mem.clone(),
        fs_diff,
        exec_state,
        metadata,
    )?;
    *self.current_snapshot.write().unwrap() = Some(snap.clone());
    Some(snap)
} else {
    None
};
```

- **What this costs:** every successful call with a memory export incurs (a) SHA-256 over `wasm_bytes`, (b) `mem.clone()` of the full WASM memory, (c) inside `Snapshot::new`: SHA-256 of `memory` and zstd-level-3 compression of `memory`. For a 32 MiB memory module that is on the order of 50–150 ms per call on commodity hardware; for 100 MiB (the Phase 1 top tier) it is ~300–500 ms. The Phase 1 results in `artifacts/plots/phase1_snapshot_scaling.png` already quantify this for the `snapshot_create` benchmark in isolation.
- **Why the bench hides it:** the `execute_tool/trivial_wasm_start` benchmark (`benches/nexus_validation.rs:140-178`) uses `(module (func (export "_start")))` — **no memory export**, so `pre_call_memory = None`, so the snapshot block is skipped, so the benchmark number does not reflect production latency for any real module. This is a misleading benchmark; the published numbers will not match what users observe with real WASM.
- **Wasted-work nature:** on success the snapshot is never consulted again (the `Snapshot` value is built, pushed into the ring buffer, then dropped at end of function). On failure, the rollback path does not actually restore state either (see H3), so the snapshot is *also* effectively unused. The work has no payoff yet.
- **Recommendation:** gate snapshot creation behind a config flag (`take_snapshot_on_success: bool`, default `false`), and/or defer it until a rollback is actually requested. At minimum, refactor the benchmark module to include `(memory (export "memory") 1)` so the numbers reflect reality.

#### H3 — Rollback path discards the restored state; "rollback_performed" is a tally, not an action
- **File:line:** `src/hypervisor/mod.rs:288-295` and `src/snapshot/manager.rs:410-435`

```288:295:src/hypervisor/mod.rs
let mut rollback_performed = false;
if mode.requires_rollback() {
    if let Some(snap) = snapshot.as_ref() {
        if self.snapshot_manager.rollback_to(&snap.id).is_ok() {
            rollback_performed = true;
        }
    }
}
```

`SnapshotManager::rollback_to` returns a `RollbackResult { memory, fs_operations, ... }` (manager.rs:429-434). **`execute_tool` discards this value via `.is_ok()`.** No WASM memory is restored anywhere (the per-call store was already dropped by the worker thread); no fs operation is applied; the only durable effect of the "rollback" is incrementing `SnapshotStats::total_rollbacks`.

- **Why this matters in Phase A:** the new `execute_tool` docstring at `src/hypervisor/mod.rs:182-197` says "Snapshot is built from the *real* pre-call WASM linear memory" and "Rollback is skipped entirely when ... false" — both of which imply a rollback that does something. Future Phase B/C work (instinct policy in `src/instinct/mod.rs`, pooled hypervisors) will rely on this assumption.
- **Pre-existing component:** the no-op rollback is not a Phase A regression in mechanism — it inherits from the prior code. But Phase A is the first change to document the path as if it were functional, which makes the misalignment a Phase A concern.
- **Recommendation:** either (a) rename `rollback_performed` → `rollback_snapshot_captured` and update the docstring to make it explicit that no state is restored, or (b) wire `RollbackResult.memory` into the next invocation's `Store` via a `seed_memory` parameter on `WasmSandbox::execute`. Option (a) is the honest short-term fix; option (b) is the eventual real fix.

---

### MEDIUM

#### M1 — `runtime_failures_do_trigger_rollback` is flaky for `infinite_loop`
- **File:line:** `tests/phase3_distinct_outputs.rs:182-195`

```182:195:tests/phase3_distinct_outputs.rs
#[test]
fn runtime_failures_do_trigger_rollback() {
    // Every runtime failure with an exported memory should land in the
    // rollback path. This is the inverse assertion of
    // `load_time_failures_dont_trigger_rollback` and pins the Phase A
    // semantics: rollback iff `requires_rollback() && pre_call_memory.is_some()`.
    for name in ["infinite_loop", "trap_unreachable", "div_by_zero", "stack_overflow"] {
        let out = run_scenario(name);
        assert!(
            out.rollback_performed,
            "{name}: expected rollback_performed=true (runtime failure with memory export)"
        );
    }
}
```

For `infinite_loop` (`(loop (br 0))` with `max_fuel = 10_000_000` and `time_limit = 500ms`), the outcome depends on which limiter fires first. The fuel-metering instrumentation adds tens of ns per loop iteration, so wall-clock cost is in the 100–500 ms range on a quiet machine — *but on CI runners or under load the 500 ms watchdog often wins*. When the watchdog wins, the timeout path (M2) returns `ExecutionResult` with `pre_call_memory: None`, no snapshot is built, `rollback_performed = false`, and **this test fails**.

The sibling test `classification_matches_phase3_expectations` (lines 119-153) already acknowledges this duality by accepting both `FuelExhausted` and `Timeout` for the `infinite_loop` scenario; this test does not.

- **Recommendation:** scope the loop to only the three deterministic-trap scenarios (`trap_unreachable`, `div_by_zero`, `stack_overflow`), and add a separate parameterized test that for `infinite_loop` asserts `rollback_performed == matches!(log.failure_mode, FailureMode::FuelExhausted{..})`. Or drop `max_fuel` for the test to ~10_000 so fuel deterministically beats the wall clock.

#### M2 — Timeout path discards the worker's `pre_call_memory`
- **File:line:** `src/sandbox/wasm_runtime.rs:283-293`

```283:293:src/sandbox/wasm_runtime.rs
Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
    // Detach the worker — the WASM is sandboxed so the loop is
    // contained, but we want to return to the caller now.
    drop(handle);
    let limit_ms = time_limit.as_millis() as u64;
    let mode = FailureMode::Timeout {
        limit_ms,
        observed_ms: duration_ms,
    };
    Ok(ExecutionResult::failure_from_mode(mode, 0, duration_ms))
}
```

`pre_call_memory` is captured *inside the worker thread* (`wasm_runtime.rs:220-222`) and bundled into the `ExecReply` enum. When the watchdog fires before the worker sends, the parent has no side-channel to retrieve those bytes — so `ExecutionResult.pre_call_memory` is always `None` on timeout. There is no `.with_pre_call_memory(...)` call on the timeout branch. This is what makes M1 flaky.

- **Recommendation:** either (a) live with the asymmetry and update tests + docs to reflect "no snapshot on timeout", or (b) instantiate twice: once in the parent to snapshot pre-call memory, once in the worker to run. Option (b) doubles cold-start cost — likely not worth it.

#### M3 — Worker-thread leak under timeout pressure
- **File:line:** `src/sandbox/wasm_runtime.rs:286` (`drop(handle);`)

Dropping a `JoinHandle` detaches the thread. The detached worker continues executing until either (a) the WASM call returns naturally (fuel exhaustion will eventually fire because `consume_fuel(true)` is set in `wasm_runtime.rs:147`), or (b) the worker thread is killed by the OS. There is no `std::thread::Builder::stack_size` cap and no thread-pool ceiling. Each timeout therefore leaks one OS thread for up to **fuel_lifetime_ms** beyond the 500 ms wall clock. With the default `max_fuel = 10_000_000`, an empty `(loop (br 0))` will burn fuel in roughly 100-500 ms after the wall-clock cap. Under a workload that hits the timeout path frequently (precisely the case the watchdog exists for), thread count grows unboundedly per unit time, capped only by how fast fuel exhausts.

- **Recommendation:** introduce a bounded `rayon`-style worker pool (or `tokio::task::spawn_blocking` with a configured queue depth) so that excess timeouts back-pressure instead of leak. Combined with a smaller default `max_fuel` (e.g. 1_000_000), this also lowers the per-leaked-thread lifetime.

#### M4 — `execute_function` retains the pre-Phase-A misclassification
- **File:line:** `src/sandbox/wasm_runtime.rs:336-343`

```336:343:src/sandbox/wasm_runtime.rs
Err(e) => {
    return Ok(ExecutionResult::failure(
        format!("WASM error: {}", e),
        start_fuel,
    ));
}
```

This sibling code path (`pub fn execute_function`) routes *all* wasmtime errors — including legitimate `Trap::IntegerDivisionByZero`, `Trap::UnreachableCodeReached`, `Trap::StackOverflow` — through the back-compat `ExecutionResult::failure(error, fuel)` shim, which **forces** `FailureMode::HostError(error)` (`wasm_runtime.rs:117`). That then maps to `HealthStatus::Corrupted` (`failure_mode.rs:218`) — the exact misclassification Phase A was rewriting `execute` to avoid. External callers of `execute_function` (it is `pub`) silently get the broken behavior. Bonus: `fuel_consumed` is reported as `start_fuel` regardless of actual consumption (line 347).

- **Recommendation:** apply the same `FailureMode::from_anyhow_error` classification logic from `execute()` (lines 254-263) to `execute_function`, and compute real `fuel_consumed = max_fuel - store.get_fuel()`.

#### M5 — `SandboxConfig::max_memory_pages` is dead but referenced as a control
- **File:line:** `src/sandbox/wasm_runtime.rs:21,33-34`; `src/hypervisor/recovery.rs:196-201`; `src/main.rs:304`

`max_memory_pages: u32` is declared, defaulted to 512, and the only reader is the demo at `src/main.rs:304` which sets it to `1` and expects an enforcement. No code consumes the field — `WasmSandbox::new` (line 145-156) ignores it, no `Store::limiter` is installed, no `Config::max_memory_size` is called. **The field is purely advisory.** Phase A *added* a recovery-action string that names it:

```196:201:src/hypervisor/recovery.rs
FailureMode::MemoryLimitExceeded { pages, limit_pages } => vec![
    mk(&format!(
        "WASM memory grew to {pages} pages but the per-tool limit is {limit_pages}; raise `max_memory_pages` or fix the guest allocator."
    )),
    ...
],
```

i.e. Phase A now generates advice that references a knob the engine does not honor. (And note: the `MemoryLimitExceeded` variant is never constructed anywhere in the codebase — `FailureMode::from_wasmtime_trap` does not map any `Trap` to it, and no host path emits it. Dead code path advertised in the public taxonomy.)

- **Recommendation:** either wire `max_memory_pages` to a real `ResourceLimiter` impl on the store (and emit `MemoryLimitExceeded` when it fires), or remove the field and the recovery string. The combination of dead enforcement + advice that names it is a misleading API.

#### M6 — `HealthStatus` / `ErrorLog` Serde compatibility regression
- **Files:** `src/hypervisor/validator/health.rs:18-35` (two new variants), `src/hypervisor/validator/error_log.rs:18-32` (new required `failure_mode` field, `recovery_actions: Vec<String>` → `Vec<RecoveryAction>`)

`HealthStatus` derives `Serialize/Deserialize` (line 17). Phase A appends `Trapped` and `InvalidModule` at the end of the enum. Adding variants is **forward-incompatible** for any pre-Phase-A binary deserializing JSON written by a Phase-A binary — `serde` rejects unknown enum variants by default.

`ErrorLog` is worse: it gained a required field `failure_mode: FailureMode` and **changed** the type of `recovery_actions` from `Vec<String>` to `Vec<RecoveryAction>`. Neither direction round-trips. On-disk `Snapshot`s persisted via bincode (`src/snapshot/manager.rs:454`) are unaffected because `Snapshot` does not embed `ErrorLog`, but `ToolOutput::error_log` is serialized to JSON in `examples/capture_error.rs:121` and any external consumer that stored those payloads will not deserialize them with the new binary. Phase 3 evidence files in `artifacts/raw/phase3_*.json` are exactly such payloads from the pre-Phase-A schema; they will not load against Phase A's `ErrorLog`.

- **Recommendation:** add a `#[serde(other)]` fallback to `HealthStatus`, version the `ErrorLog` schema (`schema_version: u32`), and write a one-shot migration for `artifacts/raw/phase3_*.json` if those are intended to be re-parseable.

#### M7 — `lib.rs` references untracked `src/instinct/mod.rs`; Phase A as committed will not build
- **Files:** `src/lib.rs` (diff `+pub mod instinct;` and `+pub use instinct::{Instinct, InstinctPolicy, InstinctStats, InstinctStore};`); `git ls-files src/instinct/` returns empty; `git ls-files --others --exclude-standard src/` reports `src/instinct/mod.rs` as untracked.

```9:12:src/lib.rs
pub mod snapshot;
pub mod telemetry;
pub mod hypervisor;
pub mod instinct;
```

If Phase A is committed as the modified-file set listed in the plan (which does **not** include `src/instinct/mod.rs`), `cargo build` will fail on the missing module. Either the lib.rs diff is leaking Phase B work, or the plan understates Phase A's surface area. The user-supplied Phase A scope explicitly excludes the instinct module ("Phase B adds `InstinctPolicy` and `LLMPolicy`" — recovery.rs:12-14).

- **Recommendation:** drop both `pub mod instinct;` and `pub use instinct::{...}` from `lib.rs` for the Phase A commit, OR explicitly add `src/instinct/mod.rs` to the Phase A commit *and* update the plan. The current state (lib.rs modified, instinct file untracked) is unbuildable.

---

### LOW

#### L1 — `NexusHypervisor::generate_recovery_suggestions` deletion is a `pub` breaking change
User-acknowledged in the audit ask. Worth a one-line release-note callout: any external caller of `hv.generate_recovery_suggestions(op_name, ...)` now gets a compile error. Migration: `hv.recovery_policy().recover(&failure_mode, op_name)` if a public accessor is added (today `recovery_policy` is a private field).

#### L2 — Multiple `pub` API signature changes
For completeness:
- `ExecutionRecord::success(op, dur_ms, fuel) -> success(op, dur_ms, fuel, ResourceSnapshot)` — `src/telemetry/mod.rs:33-38`
- `ExecutionRecord::failure(op, ErrorLog, dur_ms) -> failure(op, ErrorLog, dur_ms, fuel)` — `src/telemetry/mod.rs:54-59`
- `ErrorLog::new(error_type, op, description, HealthStatus, ResourceSnapshot) -> new(op, FailureMode, ResourceSnapshot)` — `src/hypervisor/validator/error_log.rs:34-39`

All three were `pub` and all three break the build for external callers. Acceptable per the Phase A plan, but no `CHANGELOG.md` entry exists.

#### L3 — `record_failure` overcounts `total_rollbacks` after Phase A
- **File:line:** `src/telemetry/mod.rs:136-139`

```136:139:src/telemetry/mod.rs
s.failed_executions += 1;
if record.error.is_some() {
    s.total_rollbacks += 1;
}
```

`record.error.is_some()` is true for every failure record (it is set unconditionally in `ExecutionRecord::failure`). Pre-Phase-A this was a reasonable proxy because every failure produced a rollback. Post-Phase-A, `InvalidModule` and `MissingEntrypoint` failures explicitly skip rollback (`hypervisor/mod.rs:289`) yet still increment `total_rollbacks`. The stat is now miscounted upward by the rate of load-time failures.

- **Recommendation:** add `rollback_performed: bool` to `ExecutionRecord` and gate on that, or expose a separate `total_load_time_failures` counter.

#### L4 — `avg_fuel_per_execution` denominator/numerator mismatch in failure path
- **File:line:** `src/telemetry/mod.rs:132-145`

`record_success` updates `s.avg_fuel_per_execution`; `record_failure` does not, even though it increments `s.total_executions`. The average is therefore biased toward zero by the failure rate. Pre-existing, but Phase A's new failure paths exercise it more.

#### L5 — `HostError → HealthStatus::Corrupted` mapping contradicts the new doc
- **Files:** `src/hypervisor/failure_mode.rs:218`, `src/hypervisor/validator/health.rs:20-22`

`health.rs:20-22` reads "Genuine host-state corruption. Reserve for cases the rollback path is actually fixing real damage; not the default for guest traps." But `failure_mode.rs:218` then routes **every** `HostError(_)` to `Corrupted`, including benign cases like "set_fuel failed: …" (a wasmtime config drift, not state damage) and "worker thread disconnected before sending a result" (a plumbing race). The doc and the mapping disagree.

- **Recommendation:** introduce `HealthStatus::HostError` and reserve `Corrupted` for the rare paths where actual corruption is suspected (`check_corruption` returning Some).

#### L6 — `FailureMode::from_anyhow_error` only handles `Trap`
- **File:line:** `src/hypervisor/failure_mode.rs:162-173`

Many wasmtime errors are not `Trap`: linker resolution errors, resource-limiter errors, custom-host-function errors. These fall through to `FailureMode::HostError(format!("wasm error: {e:#}"))` (`wasm_runtime.rs:254-256`) → `HealthStatus::Corrupted` per L5. The Phase 3 evidence already shows `missing_start` is correctly caught as `MissingEntrypoint`, but anything more exotic (e.g. import-resolution failure once WASI is enabled) will get the wrong category. Defensible for Phase A; worth a TODO.

#### L7 — Recovery advice mixes guest-author audience with operator audience
- **File:line:** `src/hypervisor/recovery.rs`, multiple

Several entries (e.g. `MemoryLimitExceeded` at line 197 saying "raise `max_memory_pages` or fix the guest allocator") give two pieces of advice aimed at two different roles. If the consumer is an AI agent doing self-correction on its own WASM tool, the "raise the limit" half is operator-only advice it cannot act on. Stylistic; not a correctness issue.

#### L8 — `Timeout::observed_ms` is measured from before module compilation
- **File:line:** `src/sandbox/wasm_runtime.rs:164,270,287-291`

`start = Instant::now()` is taken before `Module::from_binary`. If wasmtime compilation is slow (a 100 MiB module can take seconds), `duration_ms` reported in `FailureMode::Timeout.observed_ms` includes the compilation cost, making the field misleading for diagnosis. Recommendation: re-base the timer after `Module::from_binary` returns.

---

## Notes the audit cleared

These items in the audit ask were investigated and **do not** look like new defects:

- **`consume_fuel(true)` correctness:** the engine config + `Store::set_fuel(max_fuel)` per call (`wasm_runtime.rs:147,192`) does prevent infinite loops in production — fuel runs out after ~10 M instructions regardless of wall clock. The `set_fuel` failure path returns a `HostError` instead of silently swallowing the error (good, contrast with `create_store` at `wasm_runtime.rs:367-370` which still uses `eprintln!`).
- **Memory-safety / UB in `drop(handle)`:** the worker thread owns its `Store`, the cloned `Arc<Engine>`, and the `Sender`. Dropping the `JoinHandle` detaches the thread; it eventually finishes (fuel exhaustion bounds the lifetime) and all owned values drop normally. No double-free, no use-after-free. Only concern is the thread-count leak (M3).
- **`HealthStatus` Serde additive variants:** the *new* variants are appended at the end of the enum (good — preserves discriminants for bincode). The forward-incompat issue (M6) only applies to consumers that pin to the old binary.
- **Phase A test `phase3_distinct_outputs::all_five_scenarios_produce_distinct_outputs`** correctly anchors the regression — distinct `FailureMode`, distinct category, distinct first recovery action across the 5 scenarios. Combined with the new `static_policy_emits_distinct_first_actions_per_variant` in `recovery.rs`, the AI-validation defect ("identical two strings for every scenario") is genuinely fixed.

---

## Suggested merge gate

| Block / Strongly-recommend / Nice-to-have | Findings |
|---|---|
| **Block before merge** | H1 (host OOM regression), H2 (success-path overhead + misleading bench), M7 (build is broken) |
| **Strongly recommend** | H3 (rollback docstring vs reality), M1 (flaky test), M4 (`execute_function` regression-preserved), M6 (Serde schema break) |
| **Recommend** | M2, M3, M5, L3 |
| **Nice-to-have** | L1, L2 (note in CHANGELOG), L4, L5, L6, L7, L8 |
