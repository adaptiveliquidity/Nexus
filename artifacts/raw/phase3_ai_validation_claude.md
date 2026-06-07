# Phase 3 AI Telemetry Validation — Claude

Model: Claude Opus 4.8
Date (UTC): 2026-06-07T20:39:00Z
Input: artifacts/raw/phase3_index.json (5 scenarios)

## Per-scenario analysis

### 1. infinite_loop
- Observed: `TIMEOUT: Execution exceeded 500ms - infinite loop prevented`; execution_time_ms=502; rollback performed. The time/fuel guard fired correctly and contained the run.
- trigger_status correctness: **No.** Status is `Corrupted`, but this is a clean timeout/fuel-exhaustion event. The guard *worked*, isolation held, and rollback succeeded — nothing was corrupted. Correct classification would be `Timeout` / `FuelExhausted` / `ResourceLimit`. Labeling it `Corrupted` overstates severity and would misroute alerting.
- Recovery actions evaluated:
  1. "Break the operation into smaller steps" — **Incorrect / misleading.** A non-terminating loop has no termination condition; chunking it into smaller steps does not make it halt. Actively wrong guidance for this mode.
  2. "Add validation before execution" — **Weak.** Pre-execution validation cannot generally detect an infinite loop (halting problem). Static loop-bound heuristics could *sometimes* help, but the phrasing is generic and not the real lever here.
- Better alternatives or missing steps: Surface that the fuel/time budget guard already mitigated this (the system behaved correctly); tune/lower the instruction-fuel or wall-clock budget; enforce a per-tool fuel cap; reject or retry with a reduced limit; optionally return partial results. None of these appear.
- Score: 2/10
- Justification: The sandbox's behavior was correct, but the telemetry misclassifies the event and one recovery action is actively counterproductive while the other is generic.

### 2. trap_unreachable
- Observed: WASM trap at `wasm function 0` (`unreachable` instruction); execution_time_ms=3; rollback performed. Deterministic guest-side trap/abort.
- trigger_status correctness: **No.** This is a `Trap` (unreachable). Under sandbox isolation, host state is intact and rollback handled the instance — `Corrupted` is the wrong host-level classification. Correct: `Trap` / `Unreachable`.
- Recovery actions evaluated:
  1. "Break the operation into smaller steps" — **Incorrect.** An explicit `unreachable`/abort is reached regardless of step size; chunking changes nothing.
  2. "Add validation before execution" — **Marginal.** Won't catch a runtime trap in the general case.
- Better alternatives or missing steps: Decode and surface the trap reason; do **not** auto-retry identical input (deterministic — it will trap again); flag the guest module/assertion for author review; treat as a non-retryable terminal failure.
- Score: 2/10
- Justification: Misclassified, deterministic-retry hazard unaddressed, both actions generic and one is wrong.

### 3. div_by_zero
- Observed: WASM trap at `0x27` in `wasm function 0`; execution_time_ms=2; rollback performed. Integer divide-by-zero trap. Deterministic.
- trigger_status correctness: **No.** This is a `Trap` (`IntegerDivisionByZero`), not corruption. Correct: `Trap` / `IntegerDivisionByZero`.
- Recovery actions evaluated:
  1. "Break the operation into smaller steps" — **Incorrect.** Irrelevant to a divide-by-zero trap.
  2. "Add validation before execution" — **Coincidentally relevant.** Guarding/validating the divisor (input validation) genuinely can prevent this failure mode. This is the one action that maps onto the actual fault — though it reads as generic boilerplate rather than targeted advice.
- Better alternatives or missing steps: Recommend an explicit divisor!=0 guard / input-domain validation; mark deterministic and non-retryable for identical inputs; surface the operands if telemetry allows.
- Score: 4/10
- Justification: One action accidentally fits the fault, lifting the score; classification still wrong and the first action is noise.

### 4. stack_overflow
- Observed: WASM trap from deeply nested recursion (`rec` repeated thousands of times); execution_time_ms=23; rollback performed. Call-stack exhaustion. The runtime's stack limit caught it.
- trigger_status correctness: **No.** This is `StackOverflow` / `ResourceExhausted`. The stack-depth guard contained it and rollback succeeded — not corruption. Correct: `StackOverflow`.
- Recovery actions evaluated:
  1. "Break the operation into smaller steps" — **Weak/partial.** Conceptually adjacent (convert recursion to iteration / cap depth), but as phrased it does nothing for unbounded recursion lacking a base case.
  2. "Add validation before execution" — **Weak.** Static detection of unbounded recursion is hard and not what this string implies.
- Better alternatives or missing steps: Enforce/lower the recursion-depth or stack budget; reject modules with unbounded recursion; advise iterative rewrite or explicit depth cap; only raise stack limits for legitimately deep-but-bounded workloads.
- Score: 3/10
- Justification: The guard worked; recovery advice is generic with only loose conceptual relevance, and classification is wrong.

### 5. missing_start
- Observed: `No _start or main function found`; execution_time_ms=2; rollback performed. This is a **load/linking/entrypoint-resolution** failure — nothing ever executed.
- trigger_status correctness: **No (worst case).** Nothing ran, so nothing could be corrupted, and `rollback_performed=true` is arguably spurious (no state mutation to roll back). Correct: `InvalidModule` / `MissingEntrypoint` / a load/validation error — not a runtime `Corrupted` status.
- Recovery actions evaluated:
  1. "Break the operation into smaller steps" — **Incorrect.** Completely irrelevant to a missing-entrypoint load error.
  2. "Add validation before execution" — **Correct (ironically the best fit of any scenario).** Validating that the module exports a valid `_start`/`main` entrypoint before instantiation is exactly the right preventive control.
- Better alternatives or missing steps: Pre-flight module validation (export/entrypoint check) at load time; reject with a precise "missing entrypoint" error; skip the rollback path entirely since no execution occurred; clearer load-vs-runtime error taxonomy.
- Score: 3/10
- Justification: One action is genuinely correct, but the failure is misclassified as a runtime corruption, the unnecessary rollback signals a taxonomy gap, and the first action is noise.

## Overall verdict
- Aggregate accuracy rate: 28% — (2 + 2 + 4 + 3 + 3) / (5 * 10) * 100
- Average score: 2.8 / 10
- Top systemic problem: The AI telemetry layer is not failure-mode aware. All five scenarios — a timeout, three distinct traps, a stack exhaustion, and a pre-execution load error — return the *exact same* `trigger_status` ("Corrupted") and the *exact same* two `recovery_actions`. This is hardcoded boilerplate, not analysis. `Corrupted` is wrong in 5/5 cases: in every scenario the sandbox guard fired correctly, isolation held, and rollback succeeded, which proves state was recoverable, not corrupted. The recovery strings happen to land on the right fault only by coincidence (div_by_zero, missing_start) and are actively misleading for the others (infinite_loop, trap_unreachable). The system is generating plausible-sounding text decoupled from the captured error signal it already has in hand.
- Recommendations for the Nexus team:
  - Drive `trigger_status` from the actual error variant. Introduce a real taxonomy: `Timeout`/`FuelExhausted`, `Trap(reason)` (with `Unreachable`, `IntegerDivisionByZero`, etc.), `StackOverflow`, `InvalidModule`/`MissingEntrypoint`. Reserve `Corrupted` for genuine host/state-integrity loss.
  - Make `recovery_actions` a function of the classified failure mode (a lookup/policy keyed on error variant), not a constant. The current identical output across modes is the core defect.
  - Distinguish load/validation failures from runtime failures; do not run the rollback path (or set `rollback_performed=true`) when nothing executed (missing_start).
  - Mark deterministic traps (unreachable, div_by_zero) as non-retryable for identical inputs to avoid pointless retry loops.
  - For resource-limit events (infinite_loop, stack_overflow), report that the guard succeeded and expose the tunable (fuel/time/stack budget) rather than implying user-side restructuring.
  - Add a regression test asserting that the five scenarios produce *distinct* statuses and recovery sets — that test would fail on today's output.

## Methodology note
Scoring is 1–10 where 10 = correct classification + optimal, failure-specific recovery (minimal blast radius, fastest recovery, state preserved where possible) and 1 = incorrect/misleading. I weighted (a) technical correctness for the observed mode, (b) optimality/blast radius, and (c) completeness vs. obvious missing steps. Scores were lifted modestly where a generic action coincidentally matched the true fault (div_by_zero, missing_start). I evaluated only the trimmed index; I did not open the full per-scenario files (stack_overflow's was truncated ~1.1M chars) and did not execute any captured WASM, so judgments rest on the error_type/trigger_status/recovery_actions fields as recorded. Aggregate accuracy is the rubric-defined sum-of-scores / 50.
