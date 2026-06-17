# RFC 0004 - Capability Profile Manifests

- **Status:** Draft (validator-only first slice)
- **Roadmap:** Secure MCP Runtime, Task 3
- **Author:** Nexus

## 1. Summary

Define a TOML manifest that captures a deployment security posture for Nexus:
allowed module directories, daemon authentication posture, maximum capability
token validity, MCP tool exposure, and the exact capability scopes a profile may
grant.

The first implementation slice is intentionally read-only:
`nexus profile validate <profile.toml>` parses and validates the manifest but
does not issue tokens, mutate runtime config, or change execution behavior.

## 2. Motivation

Nexus already has the primitives for least-authority execution:
`Capability`, `CapabilityToken`, MCP module directory allowlisting through
`NEXUS_MCP_MODULE_DIR`, and MCP capability allowlisting through
`NEXUS_MCP_CAPABILITY_ALLOWLIST`. Today, those controls are spread across code
paths and environment variables.

A capability profile makes the posture diffable and testable. Reviewers can
answer "what may this deployment do?" from one artifact before profiles are wired
into execution.

## 3. TOML Schema

```toml
[profile]
name = "local-secure"
version = "0.1.0"
description = "Optional human-readable profile description"

[execution]
module_dirs = ["/srv/nexus/modules"]
daemon_auth_required = false
max_token_validity_secs = 3600

[[capabilities]]
scope = "ReadFile"
path = "/srv/nexus/data"

[[capabilities]]
scope = "HttpGet"
url = "https://api.example.com/v1/*"

[[capabilities]]
scope = "None"

[mcp]
tool_allowlist = ["nexus_execute", "nexus_execute_wasi"]
snapshot_enabled = true
fork_enabled = false
```

### 3.1 `[profile]`

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `name` | string | yes | Stable profile name for review and selection. |
| `version` | string | yes | Profile schema or profile revision chosen by operators. |
| `description` | string | no | Human-readable context. |

### 3.2 `[execution]`

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `module_dirs` | array of path strings | yes | Declarative equivalent of allowed WASM module roots such as `NEXUS_MCP_MODULE_DIR`. |
| `daemon_auth_required` | bool | yes | Whether daemon/API callers must authenticate before profile-controlled execution. |
| `max_token_validity_secs` | unsigned integer | no | Upper bound for profile-issued token lifetime. |

### 3.3 `[[capabilities]]`

Each capability entry has a `scope` plus either `path` or `url`, depending on
the scope.

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `scope` | string enum | yes | One of `ReadFile`, `WriteFile`, `ListDirectory`, `HttpGet`, `HttpPost`, `ExecuteBinary`, `MountTmpfs`, `None`. |
| `path` | path string | for path scopes | Absolute path argument for filesystem, binary, or tmpfs capabilities. |
| `url` | string | for HTTP scopes | URL or URL pattern argument for HTTP capabilities. |

`Capability::All` is not representable in an accepted profile. The literal
scope `All` is rejected by validation.

### 3.4 `[mcp]`

The MCP section is optional. When present, all fields are required.

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `tool_allowlist` | array of strings | yes | MCP tool names allowed by this profile. |
| `snapshot_enabled` | bool | yes | Whether snapshot MCP tools are available under this profile. |
| `fork_enabled` | bool | yes | Whether fork/race MCP tools are available under this profile. |

## 4. Validation Rules

The validator enforces these rules in the first slice:

1. `Capability::All` is forbidden. Profiles must use specific grants or `None`.
2. `scope` must be one of the exact runtime `Capability` variant names:
   `ReadFile`, `WriteFile`, `ListDirectory`, `HttpGet`, `HttpPost`,
   `ExecuteBinary`, `MountTmpfs`, or `None`.
3. `path` values for path-backed capabilities must be absolute.
4. Every `execution.module_dirs` value must be absolute.
5. `execution.max_token_validity_secs`, when set, must be at most `86400`.
6. `execution.module_dirs` must not be empty when
   `execution.daemon_auth_required=false`.
7. HTTP capabilities must provide a `url` argument.

The validator does not canonicalize paths, check filesystem existence, resolve
symlinks, issue capability tokens, or apply profile settings to live runtimes.

## 5. Sample Profile

```toml
[profile]
name = "local-mcp-development"
version = "0.1.0"
description = "Local development profile with narrow filesystem and HTTP access"

[execution]
module_dirs = ["/srv/nexus/modules", "/opt/nexus/tools"]
daemon_auth_required = false
max_token_validity_secs = 3600

[[capabilities]]
scope = "ReadFile"
path = "/srv/nexus/data"

[[capabilities]]
scope = "ListDirectory"
path = "/srv/nexus/modules"

[[capabilities]]
scope = "HttpGet"
url = "https://api.example.com/v1/*"

[mcp]
tool_allowlist = ["nexus_execute", "nexus_execute_wasi", "nexus_issue_token"]
snapshot_enabled = true
fork_enabled = false
```

## 6. Test Strategy

Unit tests cover:

- A valid profile parses, maps entries to `Capability`, and validates.
- `All` is rejected before it can become a profile grant.
- Relative capability paths are rejected.
- `max_token_validity_secs = 90000` fails the `86400` ceiling.
- Empty `module_dirs` fails when `daemon_auth_required=false`.

CLI validation is covered by running:

```bash
cargo run --bin nexus -- profile validate docs/rfcs/examples/sample-profile.toml
```

Future integration tests should start MCP or daemon processes from a profile and
assert that generated environment/tool constraints deny out-of-profile actions.

## 7. Next Hook Point

The next implementation step should translate a validated
`CapabilityProfileManifest` into runtime configuration at the boundary where MCP
currently reads `NEXUS_MCP_MODULE_DIR` and `NEXUS_MCP_CAPABILITY_ALLOWLIST`.
That hook should remain explicit and opt-in so profile validation stays separate
from execution until the denial-path tests exist.
