# RFC 0002 — WASM Call-Stack Capture

- **Status:** Draft (research / design only — no production code)
- **Roadmap:** P3, Research
- **Author:** Nexus

## 1. Summary

Investigate capturing the WASM call stack at snapshot time, to enrich
time-travel debugging and failure diagnostics beyond the linear memory, globals,
and tables captured today.

**Headline finding:** with wasmtime 45's public API, **diagnostic call-stack
*capture* is feasible; call-stack *serialize-and-restore* is not.** This RFC
recommends shipping capture-for-diagnostics and explicitly *not* attempting to
make the stack a restorable part of a snapshot.

## 2. Context — what state we capture today

From `src/snapshot/manager.rs`, `ExecutionState` captures:

```text
ExecutionState {
    captured_globals: Vec<GlobalSnapshot>,  // exported globals
    captured_tables:  Vec<TableSnapshot>,   // exported tables
}
```

Notably **the call stack is not captured.** `Snapshot.execution_state` documents
"stack, registers, etc." aspirationally, but the actual fields are globals and
tables only. The execute path (`src/sandbox/wasm_runtime.rs`) runs the guest on a
worker thread and captures globals/tables *after* the entry function returns or
traps — i.e. at a point where the WASM stack has already unwound.

## 3. wasmtime 45 API reality check

What wasmtime exposes:

- **`WasmBacktrace`** — a diagnostic backtrace of WASM frames. Each frame gives
  the defining module, the function index, and the code offset (module offset +
  function offset); symbol names if debug info is present. Available via
  `WasmBacktrace::capture(&store)` and attached to traps
  (`error.downcast_ref::<WasmBacktrace>()`, or `Trap` + backtrace on the error).
- **`Trap`** — the trap code (e.g. `UnreachableCodeReached`, `MemoryOutOfBounds`,
  `StackOverflow`), already consumed by `FailureMode::from_anyhow_error`.

What wasmtime does **not** expose (the blockers for restore):

- No public API to read **operand-stack values** or **local variables** of live
  frames.
- No public API to **reconstruct or resume** a call stack. Execution runs on the
  native machine stack via the Cranelift JIT; there is no "set the WASM call
  stack to this serialized form" entry point.
- Backtraces are a **point-in-time diagnostic snapshot of frame identities**, not
  a serializable continuation.

Conclusion: a backtrace tells you *where* execution is (function + offset). It
does not tell you the *values* needed to resume from there, and wasmtime offers
no resume-from-stack primitive regardless.

## 4. Options

### Option A — Diagnostic capture only (recommended)

Capture a `WasmBacktrace` at the moment of trap/failure (and optionally at
checkpoint boundaries), normalize it into a serializable structure, and attach it
to the failure record / trace — **not** to the restorable snapshot state.

Proposed serializable shape (new, additive):

```text
CallStackTrace {
    frames: Vec<StackFrame>,
    captured_at: CaptureSite,   // Trap | Checkpoint
}
StackFrame {
    module_name: Option<String>,
    func_index: u32,
    func_name: Option<String>,  // when debug symbols present
    module_offset: u32,
}
```

Integration points:
- Populate it in the trap arm of `execute_module` where the error is already
  inspected by `FailureMode::from_anyhow_error`.
- Surface it on `ErrorLog` (`to_llm_context()` could include the top N frames —
  directly useful for the AI-recovery path) and on the replay trace
  (`src/telemetry/trace.rs`).
- It is **diagnostic metadata**, so it does not affect `memory_checksum`,
  rollback correctness, or determinism.

Cost: a `WasmBacktrace::capture` is cheap relative to execution but **not free** —
it walks frames. It must be gated to failure/explicit-checkpoint sites, never on
the hot success path. Full fidelity (function names) requires compiling with
debug info (`Config::debug_info(true)` / `wasm_backtrace_details`), which has a
compile-time and binary-size cost; default to indices-only.

### Option B — Full stack serialize + restore (not feasible)

Would require reading operand stacks + locals and a resume primitive. wasmtime
exposes neither. Approximations:

- *Deterministic replay to a fuel index* — we already have the building blocks
  (`execute_to_fuel` / `StepCapture` in `wasm_runtime.rs`, and the fuel-indexed
  replay plan). For a *deterministic* (pure-compute) module you can reconstruct
  the logical position by re-running to a fuel count rather than serializing the
  stack. This is strictly better than stack serialization for the replay use case
  and is already the project's chosen direction.
- *Asyncify / stack-switching transforms* — a guest-side transform (Binaryen
  Asyncify, or the stack-switching proposal) can make a module save/restore its
  own stack, but it requires rewriting the guest and changes semantics. Out of
  scope for a host-side snapshot feature.

Verdict: **rejected** for v1. Where "resume from a point" is genuinely needed,
use fuel-indexed deterministic replay, not stack serialization.

## 5. Recommendation

1. Implement **Option A** (diagnostic capture) as an additive, opt-in feature.
2. Do **not** extend `Snapshot.execution_state` with stack data — keep snapshots
   restorable and deterministic. Attach `CallStackTrace` to failure/trace records
   instead.
3. For "time-travel to a prior execution point," rely on **fuel-indexed
   deterministic replay** (existing direction), not stack capture.
4. Update the `Snapshot.execution_state` doc comment to stop implying stack/
   register capture, to match reality.

## 6. SnapshotMetadata / API impact

- No change required to `Snapshot` or `ExecutionState` for Option A.
- Optional: a `capture_backtrace: bool` knob on `SandboxConfig` (default off on
  the hot path; implied on for failure sites).
- `ErrorLog` gains an optional `call_stack: Option<CallStackTrace>`.

## 7. Overhead estimate (to validate empirically)

- `WasmBacktrace::capture` at trap sites only: negligible vs the cost of the trap
  itself.
- Per-checkpoint capture (if enabled in the trace engine): O(stack depth) frame
  walk; must be benchmarked before enabling by default. Suggested bench: extend
  the (manual) density / replay benches rather than the PR-gated suite.
- Debug-info builds (for names): measurable compile-time + binary-size hit; keep
  behind a feature/flag.

## 8. Open questions

- Should `to_llm_context()` include the backtrace top-frames by default? It likely
  improves AI-recovery suggestions, but adds tokens — make it configurable.
- Do we want backtraces on *successful* checkpoints for richer time-travel, given
  the frame-walk cost? Default no; opt-in for debugging sessions.

## 9. References

- wasmtime `WasmBacktrace`:
  <https://docs.rs/wasmtime/latest/wasmtime/struct.WasmBacktrace.html>
- wasmtime `Trap`:
  <https://docs.rs/wasmtime/latest/wasmtime/enum.Trap.html>
- Binaryen Asyncify (guest-side stack save/restore, considered & rejected):
  <https://github.com/WebAssembly/binaryen/blob/main/src/passes/Asyncify.cpp>
- Existing in-repo direction: `execute_to_fuel` / `StepCapture`
  (`src/sandbox/wasm_runtime.rs`) and the fuel-indexed replay plan.
