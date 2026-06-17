# Security Policy

Nexus is a security-sensitive runtime: it executes WebAssembly in a
capability-gated sandbox and issues Ed25519-signed capability tokens. We take
vulnerability reports seriously and appreciate responsible disclosure.

## Supported Versions

Nexus is pre-1.0 software under active development. Security fixes are applied to
the `main` branch and the latest tagged release. Older pre-1.0 versions are not
maintained — please reproduce on `main` before reporting.

| Version | Supported |
| ------- | --------- |
| `main` / latest release | ✅ |
| Older 0.x | ❌ |

## Reporting a Vulnerability

**Please do not open a public issue for security vulnerabilities.**

Preferred: use GitHub's private vulnerability reporting —
**Security → Advisories → "Report a vulnerability"** on this repository. This
keeps the report private until a fix is available.

Alternatively, email **contact@adaptiveliquidity.com** with:

- a description of the issue and its impact,
- steps to reproduce (a minimal WASM module or token sequence is ideal),
- affected component (see scope below), and
- any suggested remediation.

### What to expect

- **Acknowledgement:** within 3 business days.
- **Triage + severity assessment:** within 7 business days.
- **Fix or mitigation plan:** communicated after triage; timeline depends on
  severity and complexity.
- **Credit:** we will credit reporters in the release notes / advisory unless
  you prefer to remain anonymous.

## Scope

In scope — the Rust runtime and its trust boundaries:

- **Capability model** (`src/security/capability.rs`) — token signing/verification,
  attenuation (narrowing-only) chains, expiry, and revocation.
- **WASI execution** (`src/sandbox/`) — capability→pre-open mapping and filesystem
  isolation; sandbox escape from a guest module.
- **Snapshot/rollback** (`src/snapshot/`) — integrity of captured state and
  content-addressed digests.
- **Daemon** (`nexus-agentd`, `src/daemon/`) — the Unix-socket / Windows
  named-pipe protocol (e.g. resource exhaustion from crafted frames).
- **MCP server** (`nexus-mcp`) — the stdio tool surface that exposes hypervisor
  operations.

Out of scope:

- The `dashboard/` site — a **static** Next.js export (`output: "export"`) served
  by GitHub Pages with no server runtime. Advisories against `next`/`postcss`
  that require a running Next.js server are not exploitable in this static
  deployment and are tracked separately.
- Issues requiring a trusted local operator to already control the host running
  the daemon/MCP server (these are local trust boundaries, not remote surfaces).
- Denial of service from a caller that has been explicitly granted unbounded
  resource capabilities.

## Security Model (summary)

- **Capability tokens are Ed25519-signed**; verification is mandatory before any
  authorization decision.
- **Authorization precedes filesystem side effects** — on the WASI execution
  path, required capabilities are derived without filesystem writes, and any
  host mount directory creation happens only after capability authorization
  succeeds.
- **Symlink trust boundary for WASI paths** — capability token path checks are
  lexical: `.` and `..` are normalized, but symlinks are not resolved. Raw
  capability-derived preopens are compatibility/trusted-path mode. Use
  `execute_tool_wasi_with_config` / `WasiToolConfig` for untrusted or
  symlink-sensitive host mounts; this path derives required capabilities and
  prepared preopens from canonical host paths. Wasmtime/cap-std confines guest
  traversal inside an opened preopen, but does not choose whether a symlinked
  preopen root is the intended host directory.
- **Attenuation narrows, never widens** — a delegated token can only be a subset
  of its parent.
- **WASM isolation** — each execution runs in an isolated sandbox with fuel +
  wall-clock limits (infinite-loop and runaway-execution guards).
- **Snapshots contain raw guest memory** — treat snapshot artifacts as
  **confidential** and transport them only over authenticated/encrypted channels.

See [README.md](README.md) and `docs/rfcs/` for architecture details.
