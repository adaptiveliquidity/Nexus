# PRD: Nexus-IQ as a Consumer-Facing Product

**Status**: draft
**Owner**: contact@adaptiveliquidity.com
**Created**: 2026-06-28
**Format**: problem-first PRD (ecc:plan-prd)

> Nexus-IQ = **Nexus** (WASM snap-rollback execution engine) **+ AEON-IQ** (memory/proof plane),
> packaged. This PRD covers the *product*, not the engine alone.

---

## Component & Repo Map

Every requirement below is tagged with the repo it lands in. There are three existing repos plus
two new surfaces.

| Component | Repo | Status today |
|---|---|---|
| **Nexus** runtime + MCP server | `Nexus` (this repo, v0.2.0) | MCP server is **stdio-only** ([`src/bin/nexus_mcp.rs:3`](../src/bin/nexus_mcp.rs)); `rmcp = "1.7"` with `transport-io` only ([`Cargo.toml:64`](../Cargo.toml)) |
| **AEON-IQ** memory/proof plane | `aeon-memory` (separate) | Bridged via [`crates/aeon_nexus_bridge`](../crates/aeon_nexus_bridge/src/lib.rs); SSRF egress guard PR in flight (AEON-IQ#29) |
| **Nexus-IQ self-host kit** | `nexusiq` (separate) | Docker stack fusing both; **already hardened** (Option A host-uid, glibc bookworm pin); Sprints 1–2 merged |
| **MCP gateway** (auth/tenancy/egress) | **NEW** service | does not exist |
| **Control plane** (web app) | **NEW** Next.js | does not exist — current [`dashboard/`](../dashboard) is a benchmark chart app only |

**Key fact**: the local worker (P4) and the self-host kit (`nexusiq`) are the *same* Docker stack —
one with a thin onboarding CLI in front. We are not building two deployment systems.

---

## Problem

Nexus-IQ's only user path today is: clone repo → run Docker → hand-edit `mcp.json` → connect a local
stdio MCP server. That gates the product to developers who will build Rust and run containers. Normal
users, teams, and prospects have **no front door** — no signup, no hosted try-it, no dashboard to see
the memory and proof capsules that *are* the pitch.

## Hypothesis

If we expose Nexus-IQ as **(a) a hosted web control plane + (b) a remote MCP endpoint**, while keeping
the Docker kit as the advanced/enterprise path, then non-technical users can adopt it where they
already work (Claude/Cursor/ChatGPT) without managing Docker — and the self-host kit becomes a *trust*
signal rather than a *barrier*.

## Why now

The active milestone is **Secure MCP Runtime hardening**, with **SSRF egress as the open HIGH**. Any
remote exposure *amplifies* exactly that surface — so the security work and the productization are the
same critical path. We harden once, then expose.

---

## Goals

- A hosted control plane (`app.nexusiq.ai`) for signup, memory viewer, proof viewer, and connect-config.
- A remote MCP endpoint (`mcp.nexusiq.ai`) — **read-only tools first**, execution gated behind a worker.
- A one-command local worker (`npx nexusiq connect`) wrapping the existing `nexusiq` kit.
- The self-host kit demoted to the *advanced* path, not removed.

## Non-Goals

- A standalone Claude/ChatGPT chat-app replacement (demo/workbench only, later).
- Public shared-cloud `execute_wasi` **before** per-tenant isolation is proven (explicit P5 gate).
- Rebuilding the already-hardened `nexusiq` Docker kit.

## Users

| User | Path | Privacy |
|---|---|---|
| Normal user / prospect / investor | hosted web app + remote MCP | medium |
| Power / privacy-focused user | local worker connected to cloud account | high |
| Enterprise / researcher / security | full self-host kit (Docker, VPC/on-prem) | highest |

---

## Solution Overview

```
app.nexusiq.ai   → Vercel/Next.js: auth, onboarding, memory(AEON)+proof(Nexus) viewer, billing, connect-config
mcp.nexusiq.ai   → container host: streamable-HTTP MCP gateway — auth + tenant routing + egress allowlist
runtime tier     → Nexus hypervisor + AEON-IQ memory plane + Postgres/pgvector
local worker     → `npx nexusiq connect` wraps the nexusiq Docker stack; cloud holds NO execution caps
self-host kit    → nexusiq repo = advanced/enterprise artifact (already hardened)
```

**Hard rule**: cloud account ≠ cloud execution. Public MCP exposes *read/proof* tools by default;
execution with WASI capabilities (`http_get`/`http_post`/`write_file`) routes to the user's local
worker or an enterprise-isolated runtime — never the shared cloud sandbox until P5 sign-off.

---

## Delivery Milestones

Risk-ordered (security before features). `/ecc:plan` consumes this table and flips a row to
`in-progress` when planning that milestone.

| Milestone | Repo(s) | Status | Plan |
|---|---|---|---|
| **P0** — Close SSRF/egress HIGH | `Nexus` runtime + `aeon-memory` egress paths | in-progress | — |
| **P1** — HTTP transport, read-only tools | `Nexus` (`nexus-mcp` → new HTTP bin) | pending | — |
| **P2** — Auth + multi-tenancy gateway | NEW gateway service | pending | — |
| **P3** — Control plane (web app) | NEW Next.js (`app.nexusiq.ai`) | pending | — |
| **P4** — Local worker | `nexusiq` kit + onboarding CLI | pending | — |
| **P5** — Gated cloud execution | `Nexus` runtime (per-tenant) | pending | — |
| **P6** — Billing / teams / enterprise | control plane + `nexusiq` | pending | — |

**Sequencing correction vs. the original ChatGPT proposal**: its plan ships cloud execution (its
Phase 3) before the local worker (its Phase 4). We swap them — the local worker is the *prerequisite*
that makes remote execution safe, and the public endpoint must start read-only.

---

## Risks

| Risk | Severity | Mitigation |
|---|---|---|
| Public `execute_wasi` before tenant isolation = RCE/SSRF at scale | Critical | Read-only HTTP first (P1); execution stays local until P5 sign-off |
| No multi-tenancy in current runtime (single module dir, single env allowlist) | High | P2 gateway owns auth + per-tenant routing before any shared execution |
| Provider-key vault leakage | High | Dedicated secrets backend; never in Vercel env/edge |
| Duplicating already-done self-host work | Medium | `nexusiq` is the enterprise artifact; P4 wraps it, doesn't rebuild |
| Vendor MCP availability shifts (Claude connectors / ChatGPT MCP-app beta) | Low | Verify connector tiers at P3 build time, not now |

## Success Metrics

- Time-to-first-tool-call for a non-developer: from "clone + Docker + edit mcp.json" → connect a URL.
- Remote endpoint exposes **zero** capability-granting execution tools until P5.
- Self-host kit adoption retained (enterprise/advanced) while hosted path drives new signups.

## Open Questions

- Claude remote-MCP-first vs. ChatGPT-MCP-app-first for launch (ChatGPT's stated ~90%-confidence risk).
- Managed-credits vs. bring-your-own-key as the default provider model in P3.
- Postgres/pgvector host: Neon vs. Supabase vs. RDS.

---

## Appendix: Verification of the ChatGPT proposal

Directionally correct (~80%): two-product split, keep self-host kit as advanced path, plug into
existing agents first, Vercel = control plane only. **Corrected**: remote MCP is not a URL — it is an
auth + tenancy + egress subsystem and the *largest* security project here (it does not exist; transport
is stdio-only). Its tool names (`memory.search`, `proof.fetch`) are aspirational; real tools are
`nexus_execute*`, `nexus_snapshot_*`, `mcp_aeon_timeline`. Its "Phase 1 self-host UX" is largely
already done in `nexusiq`. Its phase order inverts risk (cloud execution before local worker).
