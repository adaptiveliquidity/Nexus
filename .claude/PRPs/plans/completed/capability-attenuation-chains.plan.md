# Plan: Capability Attenuation Chains

## Summary
Let a holder of a `CapabilityToken` mint a strictly-weaker child token whose capability is a subset of the parent's, with a capped chain depth. Validation walks the parent chain, verifying each link's signature, subset relationship, depth, expiry, and revocation. All changes are confined to the security module.

## User Story
As a tool running inside Nexus, I want to delegate a narrowed slice of my authority to a sub-task, so that the sub-task runs with least privilege and cannot escalate beyond what I hold.

## Problem → Solution
Today a `CapabilityToken` is a flat, single-level grant (no delegation). → A token can `attenuate()` into a child bound to its parent by signature, and `CapabilityManager::validate()` enforces the whole chain is monotonically narrowing, depth-bounded, and that no ancestor is expired or revoked.

## Metadata
- **Complexity**: Medium
- **Source PRD**: N/A (from Nexus Enhancement Handoff — Feature 2)
- **PRD Phase**: standalone
- **Estimated Files**: 3 (`capability.rs` primary; `security/mod.rs`, `lib.rs` re-exports)

---

## UX Design
Internal change — no user-facing UX transformation. Library-API only (mirrors how `CapabilityToken`/`CapabilityManager` are used today).

---

## Mandatory Reading

| Priority | File | Lines | Why |
|---|---|---|---|
| P0 | `src/security/capability.rs` | 38-89 | `Capability` enum + `allows()` — `is_subset_of` is its strict inverse and must reuse this path logic |
| P0 | `src/security/capability.rs` | 92-165 | `CapabilityToken` fields, the signed `bincode` tuple, `verify_signature`, `is_valid`, `allows` |
| P0 | `src/security/capability.rs` | 185-286 | `CapabilityManager` — `issue`, `validate`, `authorize`, `revoke`, key storage |
| P1 | `src/security/capability.rs` | 288-325 | Existing test patterns (`CapabilityManager::new()` → `issue` → `validate` → `revoke`) |
| P1 | `src/error.rs` | 39-45 | `InvalidCapability(String)` / `CapabilityDenied(String)` — reuse these |
| P2 | `src/security/mod.rs` | 1-8 | Re-export site |

## External Documentation
No external research needed — feature uses established internal patterns (ed25519-dalek signing already in use, `bincode` tuple signing already in use).

---

## Patterns to Mirror

### NAMING_CONVENTION
```rust
// SOURCE: src/security/capability.rs:40,76
pub fn allows(&self, requested: &Capability) -> bool { ... }
pub fn description(&self) -> String { ... }
// snake_case methods, PascalCase types, doc-comment on every pub item.
```

### SIGNED_TUPLE (the critical pattern to extend)
```rust
// SOURCE: src/security/capability.rs:126-134 (sign) and 140-153 (verify)
let data_to_sign = bincode::serialize(&(
    &token.id,
    &token.capability,
    &token.granted_by,
    &token.issued_at,
    &token.expires_at,
))
.map_err(|e| NexusError::SerializationError(format!("token signing: {e}")))?;
token.signature = signing_key.sign(&data_to_sign).to_bytes().to_vec();
// verify_signature MUST serialize the IDENTICAL tuple, in the same order.
```

### ERROR_HANDLING
```rust
// SOURCE: src/security/capability.rs:227-230
return Err(NexusError::InvalidCapability(format!(
    "Token {} was revoked at {}", token.id, revoked_at
)));
```

### VALIDATE_STRUCTURE (revoke → expiry → signature → capability)
```rust
// SOURCE: src/security/capability.rs:224-258
pub fn validate(&self, token: &CapabilityToken, requested: &Capability) -> Result<()> {
    // 1. revoked? 2. expired (is_valid)? 3. signature? 4. allows(requested)?
    Ok(())
}
```

### TEST_STRUCTURE
```rust
// SOURCE: src/security/capability.rs:300-324
#[test]
fn test_token_lifecycle() {
    let mut manager = CapabilityManager::new();
    let token = manager.issue(Capability::ReadFile(PathBuf::from("/project")), "test-agent",
        std::time::Duration::from_secs(3600)).unwrap();
    assert!(manager.validate(&token, &Capability::ReadFile(PathBuf::from("/project"))).is_ok());
    manager.revoke(token.id);
    assert!(manager.validate(&token, &Capability::ReadFile(PathBuf::from("/project"))).is_err());
}
```

---

## Files to Change

| File | Action | Justification |
|---|---|---|
| `src/security/capability.rs` | UPDATE | Add fields, `is_subset_of`, `attenuate`, chain-walking validation, tests |
| `src/security/mod.rs` | UPDATE | Re-export `DEFAULT_MAX_CHAIN_DEPTH` if made pub (types already exported) |
| `src/lib.rs` | UPDATE | Only if a new pub const/type needs crate-root re-export (likely none) |

## NOT Building
- **No separate `attenuation_proof` field.** Folding `parent_id` + `chain_depth` into the existing signed tuple binds the chain cryptographically with one signature — simpler and fewer verification paths. (Deviation from handoff, with rationale; documented in Notes.)
- **No `WriteFile` path-prefix narrowing.** `allows()` treats `WriteFile` as exact-match (capability.rs:55); `is_subset_of` stays consistent. Path-hierarchy narrowing for writes is a future enhancement, not this PR.
- **No HTTP wildcard semantics.** `HttpGet`/`HttpPost` are exact-string (capability.rs:63-64); `is_subset_of` requires equality. `HttpGet("*")` is NOT special.
- No persistence/serialization migration (tokens are in-memory only; per-session keys).
- No CLI surface.

---

## Step-by-Step Tasks

### Task 1: `Capability::is_subset_of`
- **ACTION**: Add `pub fn is_subset_of(&self, parent: &Capability) -> bool` to the `impl Capability` block (after `allows`, ~capability.rs:73).
- **IMPLEMENT**:
```rust
/// True if `self` grants no more than `parent` — the relation a child
/// capability must satisfy to be attenuated from `parent`. Strict inverse
/// of `allows`, with explicit handling for the `None`/`All` lattice ends.
pub fn is_subset_of(&self, parent: &Capability) -> bool {
    match (self, parent) {
        (Capability::None, _) => true,   // deny-all is a subset of everything
        (_, Capability::All) => true,    // everything is a subset of All
        (Capability::All, _) => false,   // All is only a subset of All (above)
        // Otherwise: parent must grant self (reuses path/scope logic).
        _ => parent.allows(self),
    }
}
```
- **MIRROR**: NAMING_CONVENTION; reuses `allows` (capability.rs:40-73).
- **GOTCHA**: Order matters — `(None, _)` before `(_, All)` so `None` under any parent is a subset; `(All, _) => false` must come after `(_, All)` so `All ⊆ All` stays true. Do NOT define `is_subset_of` as plain `parent.allows(self)` — that would wrongly reject `None` children (allows() default-denies `None`).
- **VALIDATE**: `cargo test --lib capability::tests::subset`.

### Task 2: Extend `CapabilityToken` with chain fields
- **ACTION**: Add two fields to `CapabilityToken` (capability.rs:93-106) and include them in BOTH the sign (126-132) and verify (140-146) tuples.
- **IMPLEMENT**:
  - Fields: `pub parent_id: Option<Uuid>,` and `pub chain_depth: u32,`.
  - In `new()`, initialise `parent_id: None`, `chain_depth: 0`, and extend the signed tuple to `(&id, &capability, &granted_by, &issued_at, &expires_at, &parent_id, &chain_depth)`.
  - In `verify_signature()`, serialize the **identical** 7-tuple.
- **MIRROR**: SIGNED_TUPLE.
- **IMPORTS**: none new (`Uuid` already imported, capability.rs:11).
- **GOTCHA**: The sign and verify tuples MUST stay byte-identical and field-order-identical or every signature fails. Update both in the same change. `new()` is the only constructor — no other literal `CapabilityToken { .. }` exists in the crate (verified), so no other call sites break.
- **VALIDATE**: existing `test_token_lifecycle` still passes (`cargo test --lib capability`).

### Task 3: `CapabilityToken::attenuate`
- **ACTION**: Add a method on `CapabilityToken` and a `DEFAULT_MAX_CHAIN_DEPTH` const.
- **IMPLEMENT**:
```rust
/// Default maximum attenuation-chain depth (root = 0).
pub const DEFAULT_MAX_CHAIN_DEPTH: u32 = 5;

impl CapabilityToken {
    /// Mint a strictly-weaker child token bound to this token as its parent.
    /// Fails if `narrower` is not a subset of this token's capability, or if
    /// the resulting depth would exceed `max_depth`.
    pub fn attenuate(
        &self,
        narrower: Capability,
        granted_by: &str,
        validity_duration: std::time::Duration,
        signing_key: &SigningKey,
        max_depth: u32,
    ) -> Result<CapabilityToken> {
        if !narrower.is_subset_of(&self.capability) {
            return Err(NexusError::InvalidCapability(format!(
                "attenuated capability {:?} is not a subset of parent {:?}",
                narrower, self.capability
            )));
        }
        let child_depth = self.chain_depth + 1;
        if child_depth > max_depth {
            return Err(NexusError::InvalidCapability(format!(
                "attenuation chain depth {child_depth} exceeds max {max_depth}"
            )));
        }
        let now = Utc::now();
        // Child expiry cannot outlive the parent.
        let expires_at = (now + validity_duration).min(self.expires_at);
        let mut token = CapabilityToken {
            id: Uuid::new_v4(),
            capability: narrower,
            granted_by: granted_by.to_string(),
            issued_at: now,
            expires_at,
            parent_id: Some(self.id),
            chain_depth: child_depth,
            signature: Vec::new(),
        };
        let data_to_sign = bincode::serialize(&(
            &token.id, &token.capability, &token.granted_by,
            &token.issued_at, &token.expires_at, &token.parent_id, &token.chain_depth,
        )).map_err(|e| NexusError::SerializationError(format!("attenuate signing: {e}")))?;
        token.signature = signing_key.sign(&data_to_sign).to_bytes().to_vec();
        Ok(token)
    }
}
```
- **MIRROR**: `CapabilityToken::new` (capability.rs:110-136) for the construct-then-sign shape.
- **GOTCHA**: Clamp child `expires_at` to the parent's (`.min(self.expires_at)`) so a child can't outlive its parent. Reuse the exact 7-tuple from Task 2.
- **VALIDATE**: `cargo test --lib capability::tests::attenuate`.

### Task 4: Chain-walking validation in `CapabilityManager`
- **ACTION**: Add `fn validate_chain(&self, token: &CapabilityToken, max_depth: u32) -> Result<()>` and call it at the top of `validate()` (capability.rs:224) when `token.parent_id.is_some()`. Keep the existing single-token checks for the leaf.
- **IMPLEMENT** (walk from leaf to root via `active_tokens`):
```rust
fn validate_chain(&self, token: &CapabilityToken, max_depth: u32) -> Result<()> {
    if token.chain_depth > max_depth {
        return Err(NexusError::InvalidCapability(format!(
            "chain depth {} exceeds max {max_depth}", token.chain_depth)));
    }
    let mut child = token.clone();
    while let Some(pid) = child.parent_id {
        // Revoked ancestor invalidates the chain (check BEFORE active_tokens
        // lookup — revoke() removes from active_tokens but records here).
        if let Some(at) = self.revoked_tokens.get(&pid) {
            return Err(NexusError::InvalidCapability(format!(
                "ancestor {pid} was revoked at {at}")));
        }
        let parent = self.active_tokens.get(&pid).ok_or_else(|| {
            NexusError::InvalidCapability(format!("broken attenuation chain: parent {pid} not found"))
        })?;
        if !parent.verify_signature(&self.verifying_key) {
            return Err(NexusError::InvalidCapability(format!("ancestor {pid} has invalid signature")));
        }
        if !parent.is_valid() {
            return Err(NexusError::InvalidCapability(format!("ancestor {pid} expired at {}", parent.expires_at)));
        }
        if child.chain_depth != parent.chain_depth + 1 {
            return Err(NexusError::InvalidCapability(format!("non-monotonic chain depth at {pid}")));
        }
        if !child.capability.is_subset_of(&parent.capability) {
            return Err(NexusError::InvalidCapability(format!(
                "link {:?} not a subset of parent {:?}", child.capability, parent.capability)));
        }
        child = parent.clone();
    }
    Ok(())
}
```
  Then in `validate()`, before the capability check, add:
```rust
    if token.parent_id.is_some() {
        self.validate_chain(token, DEFAULT_MAX_CHAIN_DEPTH)?;
    }
```
- **MIRROR**: VALIDATE_STRUCTURE; ERROR_HANDLING.
- **GOTCHA**: `revoke()` (capability.rs:277-280) removes the token from `active_tokens` AND records it in `revoked_tokens`. So check `revoked_tokens` for each ancestor *before* the `active_tokens` lookup — otherwise a revoked ancestor reads as "broken chain" instead of "revoked". Ancestors MUST have been registered via `issue`/manager-`attenuate` to be found.
- **VALIDATE**: `cargo test --lib capability`.

### Task 5: (Convenience) `CapabilityManager::attenuate`
- **ACTION**: Add `pub fn attenuate(&mut self, parent_id: Uuid, narrower: Capability, granted_by: &str, validity: std::time::Duration) -> Result<CapabilityToken>` that looks up the parent in `active_tokens`, calls `parent.attenuate(.., &self.signing_key, DEFAULT_MAX_CHAIN_DEPTH)`, inserts the child into `active_tokens`, and returns it.
- **MIRROR**: `issue` (capability.rs:210-221).
- **GOTCHA**: This is what registers child tokens so a deeper grandchild can later be chain-validated. Without registration, multi-level chains can't be walked. Return `InvalidCapability` if `parent_id` not found.
- **VALIDATE**: covered by Task 6 multi-level test.

### Task 6: Tests
- **ACTION**: Extend `#[cfg(test)] mod tests` (capability.rs:288) with the cases below.
- **VALIDATE**: `cargo test --lib capability`.

---

## Testing Strategy

### Unit Tests
| Test | Input | Expected | Edge? |
|---|---|---|---|
| `subset_path_narrowing` | `ReadFile("/home/user").is_subset_of(ReadFile("/home"))` | `true` | |
| `subset_rejects_broader` | `ReadFile("/home").is_subset_of(ReadFile("/home/user"))` | `false` | |
| `subset_none_and_all` | `None ⊆ ReadFile`, `ReadFile ⊆ All`, `All ⊄ ReadFile` | `true,true,false` | ✓ |
| `subset_read_under_write` | `ReadFile("/d/f").is_subset_of(WriteFile("/d"))` | `true` | |
| `attenuate_narrower_ok` | `ReadFile("/home")` → `ReadFile("/home/user")` | `Ok`, `parent_id=Some`, `chain_depth=1` | |
| `attenuate_broader_fails` | `ReadFile("/home/user")` → `ReadFile("/home")` | `Err(InvalidCapability)` | ✓ |
| `attenuate_depth_cap` | chain to depth 5 ok; depth 6 | last `Err` | ✓ |
| `validate_full_chain` | manager-issued root → attenuate ×2, validate leaf | `Ok` | |
| `validate_expired_parent` | parent validity ~0s, child validity 1h | leaf `Err` (ancestor expired) | ✓ |
| `validate_revoked_parent` | revoke root, validate child | `Err` (ancestor revoked) | ✓ |
| `child_expiry_clamped` | child validity 10h, parent 1h | child `expires_at == parent.expires_at` | ✓ |

### Edge Cases Checklist
- [x] Broader-than-parent rejected
- [x] Depth boundary (5 ok / 6 fails)
- [x] Expired ancestor
- [x] Revoked ancestor
- [x] `None`/`All` lattice ends
- [x] Child can't outlive parent

---

## Validation Commands

### Static Analysis
```bash
cargo fmt --check
```
EXPECT: zero diffs

### Unit Tests
```bash
cargo test --lib capability
```
EXPECT: all pass (existing + new)

### Full Test Suite
```bash
cargo test --all-features
```
EXPECT: no regressions (esp. `tests/capability_enforcement.rs`)

### Lint
```bash
cargo clippy --all-targets --all-features -- -D warnings
```
EXPECT: clean

### Manual Validation
- [ ] `git grep -n "CapabilityToken {"` shows only `new` and `attenuate` constructing literals (both updated with the new fields).

---

## Acceptance Criteria
- [ ] `is_subset_of`, `attenuate`, chain validation implemented
- [ ] `parent_id` + `chain_depth` in struct AND signed tuple (sign + verify)
- [ ] All validation commands pass
- [ ] New tests written and passing; `tests/capability_enforcement.rs` still green
- [ ] No type/lint errors

## Completion Checklist
- [ ] Mirrors existing sign/verify/validate patterns
- [ ] Errors use `NexusError::InvalidCapability`
- [ ] Tests follow the `CapabilityManager::new() → issue → validate` style
- [ ] No separate `attenuation_proof` field (folded into main signature — see Notes)
- [ ] No out-of-scope items (no WriteFile hierarchy, no HTTP wildcards)

## Risks
| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Sign/verify tuples drift out of sync | Med | High (all tokens invalid) | Update both in Task 2 together; existing `test_token_lifecycle` catches it immediately |
| Chain walk needs ancestors registered in `active_tokens` | Med | Med | Provide `CapabilityManager::attenuate` (Task 5) that registers children; document that detached tokens can't be chain-validated |
| `revoke` removes parent from `active_tokens` → wrong error | Low | Low | Check `revoked_tokens` before the `active_tokens` lookup (Task 4 gotcha) |
| `is_subset_of` mis-handles `None`/`All` → privilege escalation | Low | High | Explicit match arms + dedicated `subset_none_and_all` test |

## Notes
- **Deviation from handoff**: the handoff proposed a separate `attenuation_proof: Vec<u8>` signed over `(parent_id, child_capability, chain_depth)`. We instead fold `parent_id` + `chain_depth` into the token's existing signed tuple. Rationale: the child's own signature already covers its capability; adding the two chain fields to that same signature binds the child to its parent and depth with one verification path instead of two. If a future requirement needs a link's proof to be verifiable *without* the child's main signature (e.g., third-party offline audit of a single link), reintroduce the separate field.
- All capability tokens are in-memory and keys are per-session (`CapabilityManager::new` generates fresh keys), so the tuple change needs no serialization migration.
- Confidence for single-pass implementation: **8/10** — only real unknown is whether multi-level chains in `tests/capability_enforcement.rs` exercise paths needing the manager-level `attenuate` registration; Task 5 covers it.
