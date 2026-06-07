# Phase E — Bare-metal baseline runner

**Status**: outline + scaffolding. Ready to execute when a bare-metal
Linux host with KVM is available. Today the validation report explicitly
flags Firecracker, gVisor, and Cloudflare Workers as "Not Measured"
because WSL2 (the current validation host) cannot run them honestly.

## Why this isn't measured on WSL2

- **Firecracker** needs `/dev/kvm` ownership at the host level. WSL2
  exposes `/dev/kvm` (see `artifacts/specs.json -> host.kvm_dev`) but
  the device requires nested virt support that the Windows hypervisor
  underneath does not pass through reliably. Mismeasured.
- **gVisor / `runsc`** depends on ptrace and seccomp-bpf in a kernel
  where it is the actual host kernel. WSL2's kernel honors most of
  these but the timing is not representative.
- **Cloudflare Workers (production)** requires deploying to the
  Cloudflare edge, with its own network and CPU placement. Local
  `workerd` (open source) gives a representative number but is still
  not the hosted product.

The honest path is to run these on bare-metal Linux. This document is
the runbook for that day.

---

## Phase E provisioning script (to be added)

`scripts/provision_baremetal.sh` — installs the per-platform pieces on
top of the existing `scripts/install_toolchain.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
# Phase E provisioning. Run on a bare-metal Linux host with KVM.

# Reuse the per-user installer from Phase 0.
bash "$(dirname "$0")/install_toolchain.sh"

# Firecracker binary (currently v1.10.x).
FC_VER="v1.10.1"
curl -fsSL -o /tmp/firecracker.tgz \
    "https://github.com/firecracker-microvm/firecracker/releases/download/${FC_VER}/firecracker-${FC_VER}-x86_64.tgz"
tar -xzf /tmp/firecracker.tgz -C /tmp
install -m 0755 "/tmp/release-${FC_VER}-x86_64/firecracker-${FC_VER}-x86_64" "$HOME/.local/bin/firecracker"

# Pre-built rootfs and kernel (matched pair from the Firecracker
# project's test artifacts). Pin the SHA so the bench is reproducible.
mkdir -p "$HOME/.cache/nexus-firecracker"
curl -fsSL -o "$HOME/.cache/nexus-firecracker/vmlinux.bin" \
    "https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/x86_64/kernels/vmlinux.bin"
curl -fsSL -o "$HOME/.cache/nexus-firecracker/rootfs.ext4" \
    "https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/x86_64/rootfs/bionic.rootfs.ext4"

# gVisor.
curl -fsSL -o /tmp/runsc \
    "https://storage.googleapis.com/gvisor/releases/release/latest/$(uname -m)/runsc"
install -m 0755 /tmp/runsc "$HOME/.local/bin/runsc"

# workerd (Cloudflare Workers runtime, open-source).
WORKERD_VER="v1.20251005.0" # pin in CI
curl -fsSL -o /tmp/workerd \
    "https://github.com/cloudflare/workerd/releases/download/${WORKERD_VER}/workerd-linux-64"
install -m 0755 /tmp/workerd "$HOME/.local/bin/workerd"

# Verify.
firecracker --version
runsc --version
workerd --version
```

## Firecracker VM config (to be added)

`scripts/nexus-bench-firecracker.json`:

```json
{
  "boot-source": {
    "kernel_image_path": "/home/USER/.cache/nexus-firecracker/vmlinux.bin",
    "boot_args": "console=ttyS0 reboot=k panic=1 pci=off init=/bin/sh -- -c 'wasmtime /app/test_payload.wasm && reboot -f'"
  },
  "drives": [{
    "drive_id": "rootfs",
    "path_on_host": "/home/USER/.cache/nexus-firecracker/rootfs.ext4",
    "is_root_device": true,
    "is_read_only": false
  }],
  "machine-config": {
    "vcpu_count": 1,
    "mem_size_mib": 256
  }
}
```

Note: the rootfs must contain the same wasmtime binary version used by
the Docker baseline, and `/app/test_payload.wasm` must be mounted. The
launching wrapper script (`run_firecracker.sh`) handles that.

## workerd config

`scripts/workerd.capnp`:

```capnp
using Workerd = import "/workerd/workerd.capnp";

const config :Workerd.Config = (
  services = [
    ( name = "bench",
      worker = (
        modules = [ ( name = "main.wasm", wasm = embed "../test_payload.wasm" ) ],
        compatibilityDate = "2024-01-01",
      )
    )
  ],
  sockets = [ ( name = "http", address = "*:8787", service = "bench" ) ]
);
```

Then the hyperfine command becomes `workerd run scripts/workerd.capnp`
(with a per-run startup harness that exits after one HTTP request, so
the measurement reflects per-invocation cost rather than long-running
server latency).

## Phase 2 hyperfine extension

`scripts/run_phase2_hyperfine.sh` gains three new commands when the
new binaries are present:

```bash
if command -v firecracker >/dev/null 2>&1; then
    FC_CMD="firecracker --no-api --config-file scripts/nexus-bench-firecracker.json"
fi
if command -v runsc >/dev/null 2>&1; then
    GVISOR_CMD="runsc --rootless run nexus-bench"
fi
if command -v workerd >/dev/null 2>&1; then
    WORKERD_CMD="workerd run scripts/workerd.capnp"
fi
# Then append --command-name "firecracker_vm" "$FC_CMD" etc.
```

## Analyzer / report updates

`scripts/analyze_and_report.py::speedup_summary` already handles
arbitrary command names; the new commands appear in the §2.2 table
automatically. The §4.4 "Not Measured" section in the report
shrinks accordingly when the hyperfine JSON contains the new entries.

To make the §4.4 dynamic, change `build_report` to read
`specs.json -> deviations[]` only when the relevant binaries are
absent. Easy follow-up.

## Reproduction

On a fresh bare-metal Linux host (KVM-capable):

```bash
bash scripts/install_toolchain.sh    # Phase 0 toolchain
bash scripts/provision_baremetal.sh  # Phase E additions
bash validate.sh                      # full run with all baselines
```

Expected outcome: the report's §2.2 table grows to ~7 rows
(`nexus_cold`, `nexus_warm`, `wasmtime`, `docker_wasmtime`,
`firecracker_vm`, `gvisor_runsc`, `workerd`); §4.4 "Not Measured"
shrinks to just the production Cloudflare edge (which we can never
measure without paying the hosted bill).

## Why this isn't blocking the Phase A→D plan

The Phase 2 results we already have show:
- `nexus_warm` (daemon) is **1.35× slower** than raw wasmtime and **9.6× faster** than Docker.
- The "missing" baselines (Firecracker, gVisor) are all *slower* than Docker in published benchmarks for tiny-WASM-payload cold-start, so adding them is unlikely to change the qualitative story.

But Phase E removes the §4.4 caveat, which is what peer reviewers will
ask about first. It is the right next thing once bare-metal access lands.
