# ADR 0001: Capability Model Hardening for WASI and MCP

Status: Draft
Date: 2026-06-16

## Context

Three high-severity audit findings were confirmed in the current capability
model. Two are policy/model decisions that need human review before changing
runtime behavior. One is a bounded mechanical path-normalization fix included
with this draft.

This ADR documents the confirmed gaps and the expected behavior tests that
should pass once each decision is implemented.

## H2: MCP `execute_wasi` Self-Grants Caller-Chosen Filesystem Capabilities

Confirmed gap:

- `src/bin/nexus_mcp.rs:278` parses caller-supplied capability specs from the
  `nexus_execute_wasi` request.
- `src/bin/nexus_mcp.rs:285` issues fresh tokens for each requested capability
  under `mcp_client`.
- `src/bin/nexus_mcp.rs:293` attaches those same caller-chosen capabilities to
  the WASI tool as required capabilities.
- `src/bin/nexus_mcp.rs:481` only rejects `Capability::All` and clamps token
  validity. It does not require a parent token, an operator allowlist, or a
  pre-authorized policy grant for the requested filesystem scope.

Why this is a design decision, not a mechanical bug:

The current behavior makes the MCP server a capability issuer for WASI calls.
That can be intentional in a trusted local development mode, but it is not a
least-privilege delegation model. Fixing it changes MCP API semantics: existing
clients that pass `capabilities` inline and expect the server to mint matching
tokens would be denied until they provide a parent token or satisfy an allowlist.

Recommended fix and implementation split:

1. Add an explicit MCP capability policy mode:
   `trusted-self-grant` for local development, and `delegated` for production.
2. In `delegated` mode, require either a caller-provided parent token id that is
   attenuated into the requested capability, or an operator-configured allowlist
   whose path scopes are normalized and checked before token issue.
3. Separate token issue from execution: `nexus_execute_wasi` should consume
   existing valid tokens or explicit attenuations rather than minting arbitrary
   requested tokens as part of execution.
4. Keep `Capability::All` rejection and TTL clamping as defense-in-depth.

Risk and back-compat notes:

- Breaking for existing MCP clients that rely on inline self-grant.
- Requires a migration path for local demos and tests.
- The policy mode must be visible in server startup/configuration so operators
  know whether MCP is running in trusted development or delegated mode.

Expected-behavior test:

- `tests/mcp_server.rs:307`
  `execute_wasi_rejects_caller_chosen_capability_without_parent_token_or_allowlist`
  is ignored with `#[ignore = "C4 ADR: pending design approval"]`. It should pass
  once MCP delegated mode rejects self-granted caller-chosen capabilities.

## H3: Path Attenuation Used Raw Lexical Prefix Checks

Confirmed gap:

- Before this draft, `src/security/capability.rs` used raw
  `PathBuf::starts_with` for `ReadFile`, `WriteFile -> ReadFile`, and
  `ListDirectory` scope checks. A child such as `/safe/../outside` could pass a
  raw prefix check against `/safe`.
- `src/security/capability.rs:88` derives `is_subset_of` from
  `parent.allows(self)`, so the raw path comparison affected token attenuation
  and authorization.
- Before this draft, `src/sandbox/wasi.rs:280` mapped raw `ReadFile`,
  `ListDirectory`, and `WriteFile` paths directly into WASI preopens.

Why this is a design decision vs mechanical bug:

Resolving `.` and `..` without touching the filesystem is a mechanical
normalization fix. It preserves support for non-existent capability paths and
does not resolve symlinks. Broader changes such as filesystem canonicalization,
symlink policy, denylisting relative paths, or rejecting all traversal syntax
would be design decisions because they alter legitimate path scopes and runtime
mount behavior.

Recommended fix and implementation split:

Implemented in this draft:

- Add pure lexical path normalization in `src/security/capability.rs:119`.
- Use normalized paths for capability containment and write-path equality in
  `src/security/capability.rs:39` and `src/security/capability.rs:45`.
- Normalize capability-derived WASI preopen host paths before dedupe and mapping
  in `src/sandbox/wasi.rs:286` and `src/sandbox/wasi.rs:296`.

Deferred design decisions:

- Whether capability paths must be absolute.
- Whether raw traversal syntax should be rejected rather than normalized.
- Whether execution-time WASI mounts should use canonical filesystem paths,
  which would require a symlink and existence policy.

Risk and back-compat notes:

- `/safe/../outside` now compares as `/outside`, so it is no longer treated as a
  subset of `/safe`.
- Equivalent lexical paths dedupe to one WASI preopen, e.g. `/safe/./data` and
  `/safe/data/nested/..`.
- The fix intentionally does not call `std::fs::canonicalize`; capability paths
  may refer to paths that do not yet exist.

Expected-behavior tests:

- `src/security/capability.rs:554`
  `subset_rejects_lexical_parent_escape` proves `/safe/../outside` is not a
  subset of `/safe`.
- `src/security/capability.rs:562`
  `lexical_normalization_keeps_valid_child_subset` proves legitimate lexical
  equivalents still authorize.
- `src/sandbox/wasi.rs:670`
  `from_capabilities_normalizes_lexical_parent_segments` proves WASI preopens
  receive the normalized path.
- `src/sandbox/wasi.rs:680`
  `from_capabilities_dedupes_after_lexical_normalization` proves dedupe and
  write-upgrade operate on normalized paths.

## H4: WASI Required-Capability Derivation Creates Host Directories Before Authorization

Status: fixed in `security/h4-auth-ordering`.

Resolution:

- `WasiToolConfig::required_capabilities()` is side-effect free with respect to
  filesystem writes. It validates guest mount aliases and derives required
  `ReadFile`/`WriteFile` capabilities without calling any helper that can create
  host mount directories.
- Missing host mount directories are prepared only by
  `WasiToolConfig::prepare_mounts()`, the explicit post-authorization mount
  preparation phase. `validate()` remains a compatibility wrapper around this
  preparation path for public callers that intentionally validate and prepare a
  config outside execution.
- `NexusHypervisor::execute_tool_wasi_with_config()` now derives the combined
  tool + WASI required capabilities, authorizes the caller tokens, and only then
  calls `prepare_mounts()` to create any missing mount directories and build the
  validated WASI preopen config.

Evidence:

- `required_capabilities_must_not_create_host_directories_before_authorization`
  is enabled and verifies required-capability derivation does not create a
  missing host mount directory.
- `wasi_public_hypervisor::denied_wasi_config_does_not_create_missing_mount_dir`
  verifies the public hypervisor path returns `CapabilityDenied` and leaves a
  missing mount directory absent when authorization fails.

## Decision

For `security/h4-auth-ordering`:

- H2 is documented only. Runtime behavior is unchanged pending human approval.
- H3 receives a mechanical, pure lexical normalization fix with passing tests.
- H4 is fixed: required-capability derivation is side-effect free, and WASI
  mount creation is post-authorization.
