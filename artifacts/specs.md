# Environment Specs (captured 2026-06-07T21:36:53Z)

- WSL2: true
- Kernel: `5.15.167.4-microsoft-standard-WSL2`
- CPU: `AMD Ryzen 7 7800X3D 8-Core Processor` (16 cores, max unknown MHz)
- CPU governor: `unavailable`
- RAM: 15.4 GiB
- dd write (1 GiB, fdatasync): `1073741824 bytes (1.1 GB, 1.0 GiB) copied, 1.07607 s, 998 MB/s`
- /dev/kvm: present

## Toolchain
- rustc: `rustc 1.96.0 (ac68faa20 2026-05-25)`
- cargo: `cargo 1.96.0 (30a34c682 2026-05-25)`
- hyperfine: `hyperfine 1.18.0`
- wasmtime: `wasmtime 45.0.1 (83166ba31 2026-06-05)`
- docker (client/server): `Docker version 29.5.2, build 79eb04c` / `29.5.2`
- wat2wasm: `1.0.36`
- jq: `jq-1.7.1`
- python3: `Python 3.12.3`
- perf: `not installed`
- valgrind: `not installed`
- cpupower: `not installed`

## Repo
- git commit: `7e70cab21911b84ff6073978b0b0a195fb438b3b`
- git dirty: true

## Documented deviations
- WSL2: cpufreq sysfs typically unavailable; CPU governor cannot be locked to performance mode.
- WSL2: perf often unavailable or limited; perf-counters phase skipped if missing.
- WSL2: Firecracker not measured (no bare-metal KVM ownership in WSL2 environment).
- Cloudflare Workers not measured (requires hosted environment; out of scope).
