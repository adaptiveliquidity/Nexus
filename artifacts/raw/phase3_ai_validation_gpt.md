# Phase 3 AI Telemetry Validation — GPT

Model: gpt-5.2
Date (UTC): 2026-06-07T20:40:00Z
Input: artifacts/raw/phase3_index.json (5 scenarios)

## Per-scenario analysis
### 1. infinite_loop
- Observed: Execution timed out at 500ms ("infinite loop prevented"); rollback_performed=true.
- trigger_status correctness: No — classified as `Corrupted`, but this is a **Timeout / watchdog abort** (and possibly fuel/time budget exhaustion), not sandbox or state corruption.
- Recovery actions evaluated:
  1. "Break the operation into smaller steps" — Technically weak for an infinite loop: smaller steps don't address non-termination. Not optimal; encourages retry patterns that can repeatedly burn timeouts.
  2. "Add validation before execution" — Vague; could mean adding loop bounds/complexity checks, but as written it's not an actionable recovery step and doesn't preserve progress/state.
- Better alternatives or missing steps:
  - Classify as `Timeout` (or `FuelExhausted` if fuel is the actual limiter) and **avoid "corruption" remediations** (e.g., heavy resets).
  - Provide a deterministic recovery: **abort + rollback to last snapshot (if side effects)**; otherwise no rollback needed.
  - Suggest concrete prevention: add **iteration bounds**, convert to **bounded/streaming algorithm**, add **cooperative yield points**, or reduce input size; only consider increasing timeout/fuel with explicit safeguards.
- Score: 2/10
- Justification: Actions are generic and not tightly coupled to non-termination; no concrete "stop/retry safely" guidance, no state-preserving strategy, and misclassification would drive wrong operational playbooks.

### 2. trap_unreachable
- Observed: Deterministic WASM execution trap (scenario `trap_unreachable`); rollback_performed=true. Error text is generic ("error while executing") and does not explicitly say "unreachable executed".
- trigger_status correctness: No — `Corrupted` is wrong. This is a **WASM trap** (logic/assertion failure), typically `Trap/Unreachable`.
- Recovery actions evaluated:
  1. "Break the operation into smaller steps" — Not technically connected to an `unreachable` trap; does not repair the failing code path or preconditions.
  2. "Add validation before execution" — Potentially relevant only if the trap is due to violated preconditions, but it's too vague and framed as "validation" rather than targeted precondition checks or code fix.
- Better alternatives or missing steps:
  - Surface as `Trap: Unreachable` and recommend **fixing the module logic** or **ensuring preconditions** for the code path (inputs/state invariants).
  - Recommend **capturing minimal repro inputs**, and (if available) a **symbolicated backtrace / function index mapping** to locate the failing instruction.
  - Operationally: **do not auto-retry** unless the inputs change; rollback only if side effects occurred.
- Score: 2/10
- Justification: Advice is generic and misses the core recovery: identify violated invariants / fix code path; also status misclassifies a deterministic trap as corruption.

### 3. div_by_zero
- Observed: Deterministic WASM execution trap (scenario `div_by_zero`); rollback_performed=true. Error text is generic and does not explicitly mention divide-by-zero.
- trigger_status correctness: No — `Corrupted` is wrong. This is an **Arithmetic trap** (`Trap: IntegerDivideByZero` for integer division).
- Recovery actions evaluated:
  1. "Break the operation into smaller steps" — Irrelevant to divide-by-zero; does not change denominator validity.
  2. "Add validation before execution" — Directionally correct (check denominator / guard inputs), but too vague to be actionable and not framed as a minimal-blast-radius recovery.
- Better alternatives or missing steps:
  - Recommend concrete guardrails: **if denom == 0 return error**, use **checked division** patterns, or redesign to avoid division when denom can be zero.
  - Improve telemetry: include trap subtype ("integer divide by zero") so suggested actions can be specific without relying on scenario labeling.
  - Operationally: **do not retry** with the same inputs; rollback only if side effects occurred.
- Score: 3/10
- Justification: One action is loosely relevant, but both are overly generic; missing the obvious specific step (denominator guard) and misclassified status.

### 4. stack_overflow
- Observed: Deep recursive backtrace repeating `rec` (scenario `stack_overflow`); rollback_performed=true. Error text is generic ("error while executing") and does not explicitly say "call stack exhausted".
- trigger_status correctness: No — `Corrupted` is wrong. This is a **Stack overflow / call stack exhaustion** trap (resource exhaustion inside execution), not state corruption.
- Recovery actions evaluated:
  1. "Break the operation into smaller steps" — Not a meaningful recovery for stack overflow; the problem is recursion depth / stack usage, not "step size".
  2. "Add validation before execution" — Too vague; might mean recursion-depth checks, but does not specify how to prevent or remediate.
- Better alternatives or missing steps:
  - Recommend: convert recursion to **iteration**, add **explicit depth limits**, reduce stack usage, or (carefully) adjust stack limits if the sandbox supports it.
  - Improve telemetry: include a distinct `error_type` subtype for stack overflow so remediation can be specific.
  - Operationally: **do not retry** unchanged; rollback only if side effects occurred.
- Score: 1/10
- Justification: Actions are disconnected from the failure mode; there's no concrete remediation path for recursion/stack usage, and status is misclassified.

### 5. missing_start
- Observed: Module missing `_start`/`main` entrypoint ("No _start or main function found"); rollback_performed=true.
- trigger_status correctness: No — `Corrupted` is wrong. This is an **Invalid module / instantiation configuration** error (link/entrypoint), not corruption.
- Recovery actions evaluated:
  1. "Break the operation into smaller steps" — Incorrect; this is not a workload-size or runtime progress issue.
  2. "Add validation before execution" — Conceptually correct (validate module exports/entrypoint before attempting to run), but it's vague and framed as general validation rather than "verify entrypoint/export + fail fast".
- Better alternatives or missing steps:
  - Provide specific step: **validate that `_start` or configured entry export exists**; if not, **fail fast without retry**.
  - If the sandbox supports calling an exported function other than `_start`, recommend configuring the executor to call the correct export explicitly.
  - Operationally: rollback likely unnecessary (no execution occurred); preserve state by avoiding heavy remediation.
- Score: 4/10
- Justification: One action is directionally right, but the guidance is still generic; missing explicit "no-retry; fix build/export" step and status misclassifies.

## Overall verdict
- Aggregate accuracy rate: 24% — where X = (sum of scores) / (5 * 10) * 100
- Average score: 2.4
- Top systemic problem: The recovery-action generator appears to be a **single default fallback** ("break into smaller steps", "add validation") that is not conditioned on the observed failure mode (`error_type`/scenario) and is paired with a broad, incorrect `trigger_status: Corrupted`. This combination is risky operationally: it can route deterministic user-code traps and entrypoint misconfigurations into "corruption" playbooks (potentially triggering unnecessary resets/rollbacks), while providing recovery suggestions that won't actually resolve the issue or minimize retries/timeouts.

- Recommendations for the Nexus team:
  - Implement a **real error taxonomy mapping**:
    - infinite_loop → `Timeout` (or `FuelExhausted` if fuel is the limiter)
    - div_by_zero → `Trap: IntegerDivideByZero`
    - trap_unreachable → `Trap: Unreachable`
    - stack_overflow → `Trap: StackOverflow`
    - missing_start → `InvalidModule: MissingEntrypoint`
  - Make `recovery_actions` **failure-mode-specific** and operational:
    - Include "**do not retry** with same inputs" for deterministic traps/invalid modules
    - Include "**abort + rollback only if side effects**" to minimize blast radius
    - Provide concrete, minimal fixes (denominator guards, recursion → iteration, entrypoint/export validation, loop bounds/yields).
  - Improve `error_type` fidelity by extracting/recording **trap subtypes** from the WASM engine so recovery suggestions can be specific without relying on scenario labels.
  - Ensure state signals align (e.g., rollback indicators vs "reverted" flags) so "preserve state" recommendations are enforceable and auditable.

## Methodology note
Scoring scale: 10 = actions are technically precise for the failure mode, minimal blast radius, fast recovery, preserve state, and include key missing steps; 5 = partially correct but generic/underspecified; 1 = incorrect or not meaningfully connected to the failure mode. Caveat: several `error_type` strings are generic ("error while executing") and do not expose the trap subtype; that lack of specificity itself is considered a telemetry deficiency because it prevents accurate automated recovery guidance.
