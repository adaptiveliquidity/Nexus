# Integrity Audit Skill

Repeatable claim-to-source verification workflow.

## Trigger
`/integrity-audit <file>` — audit claims in the given file against source truth.

## Steps
1. **Extract claims**: Read the target file, list every factual assertion about
   Nexus capabilities, performance, or behavior.
2. **Trace to source**: For each claim, find the implementing code (file:line).
3. **Check live path**: Verify the code is reachable from `execute_tool` or
   another live entry point (not just defined).
4. **Check tests**: Find tests that exercise the behavior (not just the existence).
5. **Check benchmarks**: Find benchmarks that measure the claimed metric.
6. **Classify**: Apply the anti-overclaim taxonomy:
   - benchmarked-primitive | integrated-live | default-on | opt-in/example | roadmap/in-development
7. **Verdict**: For each claim, output VERIFIED, PARTIAL, or FALSE with evidence.

## Output format
```
| # | Claim | Source | Live path? | Test? | Bench? | Classification | Verdict |
```

## Anti-overclaim rules
- "Code exists" != "feature works"
- Underscore-prefixed params (`_input`) = value is dropped
- A bool assertion (`rollback_performed == true`) != behavioral proof
- An orphan file (no `mod` declaration) = dead code
- `let _ = flag;` = feature explicitly disabled
