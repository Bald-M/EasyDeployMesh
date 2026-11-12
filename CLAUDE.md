# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`AGENTS.md` is the authoritative rulebook for coding agents (safety invariants,
where changes belong, Rust/TS conventions, protocol compatibility, test
expectations per area). Read it before any change, and read `DESIGN.md` before
architectural, deployment, image-format, PXE, protocol, or persistence changes.
This file only adds orientation; it does not override either document.

## What this is

A local-first Tauri 2 desktop app (Rust host + Nuxt 4 SPA) that discovers PCs on
a trusted LAN, PXE-boots them into WinPE, and drives a Rust Agent on the target
to perform **destructive** disk partitioning and Windows image deployment
(WIM/ESD via DISM), plus a narrow Ubuntu Server 24.04 autoinstall flow. Wrong
code here can wipe the wrong disk — fail closed.

## Commands

Toolchain: Node 22+, pnpm 11, Rust 1.96 (edition 2024, pinned in
`rust-toolchain.toml`). Run from the repository root.

```bash
pnpm install
pnpm dev              # Nuxt UI in a browser (native commands use safe fallbacks)
pnpm tauri:dev        # full desktop shell; checks port 3000 and stages wimlib first
pnpm check            # full gate: typecheck + diagnostics tests + vitest + cargo test --workspace + nuxt generate
pnpm typecheck
pnpm test:diagnostics # node --test for scripts/*.test.mjs
pnpm --filter @easydeploymesh/desktop test
cargo test -p easydeploymesh-service   # crates: core, gho, bcd, service, agent, desktop
cargo fmt --all --check
```

Single tests (vitest and `typecheck` need the generated `.nuxt/tsconfig.json`;
on a fresh checkout run `pnpm --filter @easydeploymesh/desktop exec nuxt prepare`
first, otherwise vitest fails with "Tsconfig not found"):

```bash
pnpm --filter @easydeploymesh/desktop exec vitest run tests/partition-plans.test.ts
pnpm --filter @easydeploymesh/desktop exec vitest run -t "name substring"
cargo test -p easydeploymesh-core job::            # filter by path/name substring
node --test scripts/stage-wimlib.test.mjs
```

Builds (`pnpm build`, `pnpm build -- windows-x64`, `pnpm build:mac`, …) go through
`scripts/build-desktop.mjs` / `build-installers.mjs`: stage the Agent sidecar,
generate Nuxt, bundle Tauri, and copy installers into `release/`. Windows targets
cross-compile with `cargo-xwin`; macOS/Linux packages need their native host.

## Architecture (big picture)

Process/trust boundaries:

```
Nuxt UI ──Tauri invoke──▶ Tauri host (apps/desktop/src-tauri, composition root)
                            ├─ repositories in crates/service (JSON under app-data dir)
                            ├─ ControlPlane: HTTP Agent protocol + installer-session protocol
                            └─ PxeService: DHCP/ProxyDHCP + TFTP → iPXE/wimboot → WinPE
WinPE target ── Agent (crates/agent) ──HTTP──▶ ControlPlane   (register, heartbeat, claim lease, download, progress, complete)
Ubuntu installer ──HTTP──▶ ControlPlane via InstallerDeployment (Agent never runs Linux jobs)
```

Crate dependency direction: `core` (pure types, job state machine, partition
plans — shared wire types for host and Agent) ← `gho` (bounded GHO decoder) /
`bcd` (bounded REGF/BCD hive editing for WinPE boot entries) ← `service` and
`agent` ← `apps/desktop/src-tauri`.

Key flows that span several files:

- **Enrollment**: host issues a random enrollment token baked into the managed
  WinPE bootstrap → Agent registers once → receives per-device bearer token
  (only its SHA-256 is persisted in `devices.json`).
- **Image import**: `ImageLibrary` stages copies into `library/objects/<uuid>`,
  hashes and validates, then atomically moves; manifests only store managed
  paths. Validation repeats at job creation and again at job claim; the Agent
  hashes downloaded bytes before applying.
- **Deployment**: `create_job` (under `DeploymentMutationCoordinator`, which
  serializes image deletion vs. job creation) → `JobRepository` single-target job
  → Agent claims via heartbeat → `ControlPlane` revalidates image + disk
  fingerprint and issues an expiring, device- and job-bound lease → Agent
  re-reads inventory, partitions (DiskPart), downloads to a temp cache
  partition, verifies, applies (DISM), runs BCDBoot, reports completion.
- **PXE/WinPE**: `BootPackage` imports user PE media and injects the Agent
  runtime; changing injection behavior without changing a payload requires
  bumping the WinPE runtime layout revision.

Frontend (`apps/desktop/app`): pages → Pinia stores (refresh coordination with
in-flight reuse and sequence checks) → `services/` (thin Tauri `invoke`
adapters with browser fallbacks that must never fake native mutations).
`types/` mirrors Rust serde output exactly (camelCase fields, snake_case enums).
Pure decision logic lives in `utils/` and is what `apps/desktop/tests` covers.
All user-visible strings go in both `i18n/locales/zh-CN.json` and `en-US.json`.

## Repo-specific notes

- GHO restore is a preview limited to zlib-compressed Ghost 11.x
  single-partition NTFS images; `docs/gho-format.md` is the format reference.
  Validate parser changes against a real image
  (`EASYDEPLOYMESH_GHO_PATH=... cargo test -p easydeploymesh-gho -- --ignored`).
  The local `decompiled/` directory holds third-party decompiled code, is
  git-ignored, and must never be committed or copied into source.
- `scripts/` diagnostic marker strings and exit codes are a machine-readable
  interface; change them only together with their tests and docs.
- Version bumps must stay in sync across `Cargo.toml`, both `package.json`
  files, `tauri.conf.json`, frontend browser-fallback versions, and
  `CHANGELOG.md`.
- `release/`, `decompiled/`, `.nuxt`, `.output`, `target`, and generated
  `src-tauri/gen/schemas` are not source; don't edit them.
- Never run deployment code against the developer machine's disks; real disk
  validation needs a disposable VM or machine. macOS unit tests don't prove
  Windows/WinPE behavior.
