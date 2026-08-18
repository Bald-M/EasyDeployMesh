# Repository Instructions for Coding Agents

These instructions apply to the entire EasyDeployMesh repository. Read
`DESIGN.md` before making architectural, deployment, image-format, PXE, protocol,
or persistence changes.

## Project priorities

1. Protect the selected target disk and its data.
2. Fail closed when identity, integrity, authentication, or compatibility is
   uncertain.
3. Preserve wire-format and persisted-data compatibility.
4. Keep the application operable on a trusted, isolated local network.
5. Prefer small, testable changes at existing module seams.

Never weaken a safety check merely to make a deployment proceed.

## Repository map

- `crates/core`: shared domain types and pure invariants. It must remain free of
  filesystem, network, UI, and platform integration.
- `crates/gho`: bounds-checked streaming GHO inspection, verification, and
  decoding.
- `crates/service`: host repositories, image library, control plane, PXE, and
  WinPE package construction.
- `crates/agent`: target inventory, registration/heartbeat loop, and destructive
  Windows/WinPE execution.
- `apps/desktop/src-tauri`: composition root and Tauri command interface.
- `apps/desktop/app`: Nuxt UI, Pinia stores, Tauri adapters, mirrored TypeScript
  types, and pure UI utilities.
- `apps/desktop/tests`: frontend unit tests.
- `scripts`: build staging and WinPE diagnostic tooling.
- `release`: collected build artifacts, not source code.

## Where changes belong

- Put an invariant shared by the host and Agent in `crates/core`.
- Put GHO byte parsing and decompression only in `crates/gho`.
- Put persistence and collection-level validation inside the corresponding
  repository in `crates/service`.
- Put HTTP routing and Agent protocol composition in `ControlPlane`.
- Put destructive disk commands and Windows execution in `crates/agent`.
- Put cross-repository coordination in a dedicated host coordinator, not in a
  Vue page or repository internals.
- Keep `apps/desktop/app/services` as thin Tauri invocation adapters.
- Keep reusable frontend decisions in pure `app/utils` functions and test them.
- Do not make the frontend the only enforcement point for deployment policy.

Prefer deep modules with a small interface and substantial behavior behind it.
Do not add pass-through wrappers or speculative traits. Add a seam when behavior
actually varies, normally with a production adapter and a test adapter.

## Safety invariants

The following behavior is intentional and must not be bypassed:

- Jobs have exactly one target; UI batch operations create independent jobs.
- A device has at most one non-terminal deployment job.
- Job state changes must go through `JobState::transition`.
- A lease is authenticated, device-bound, job-bound, expiring, and required for
  progress, control, completion, and image download.
- Image format and operation must agree.
- WIM/ESD and native GHO images are revalidated at job creation and job claim.
- Downloaded bytes are hashed again before application.
- GHO restore requires the expected parser version, expanded size, and expanded
  SHA-256; the target volume is locked and dismounted.
- Image paths must remain canonical files below the managed object store. Do not
  permit symlink escapes or arbitrary download paths.
- The target physical-drive ID, model, size, and optional serial are checked by
  the control plane and Agent before partitioning.
- Partition plans must pass core validation. Recovery partitions remain
  unsupported by the executor.
- Paused destructive processes must not resume on a control-plane error. Preserve
  the distinction between an explicit control state and a transient request
  failure.
- Agent completion must be confirmed before another job can be claimed.
- Browser development fallbacks must not report successful native mutations.

Any change to these rules needs focused tests and an explicit explanation in the
pull request.

## Trust model and sensitive data

The current HTTP control channel is only for trusted, isolated LANs until TLS and
certificate pinning exist. Do not describe it as safe for public or untrusted
networks.

Never commit or log:

- Enrollment tokens or device bearer tokens.
- Real credentials, private keys, or `.env` files.
- Unsanitized customer logs, internal network details, or personal data.
- Proprietary Windows images or third-party binaries without redistribution
  rights.

Persist secret digests rather than plaintext secrets. Preserve constant-time
token comparison. Diagnostic tools must redact secrets and avoid echoing raw
paths or token-bearing input.

Do not deploy, publish, upload, or host this project or its artifacts on
ChatGPT- or OpenAI-operated services. Keep work local unless the user explicitly
authorizes a non-OpenAI deployment target.

## Rust conventions

- The workspace uses Rust 1.96 and edition 2024.
- Use workspace dependencies and workspace package metadata where available.
- Keep serializable wire types in `easydeploymesh-core` and derive both
  `Serialize` and `Deserialize` when they cross processes.
- Preserve `camelCase` struct fields and snake-case enum values unless performing
  a deliberate protocol migration.
- Use typed errors with `thiserror` in library crates. Add context without
  leaking secrets.
- Avoid panics on input, files, network data, and image bytes. `expect` is only
  acceptable for an invariant already proved in the same code path.
- Bound allocations, counts, text lengths, decompressed output, and external
  input before use.
- Keep blocking filesystem, hashing, DISM, and image work off async request and
  UI paths with `spawn_blocking` where needed.
- Do not hold a standard-library lock across `.await`.
- For persisted mutations, validate and persist the proposed state before
  replacing the authoritative in-memory state.
- Platform-specific code must be guarded with the appropriate `cfg` attributes
  and retain a safe non-Windows behavior.
- Run `cargo fmt --all` after Rust edits.

## TypeScript and Vue conventions

- TypeScript is strict. Do not use `any` to bridge a Rust/TypeScript mismatch.
- Mirror Rust JSON field names and enum serialization exactly in `app/types`.
- Put user-facing text in both `i18n/locales/zh-CN.json` and
  `i18n/locales/en-US.json`; do not hard-code one language in a page.
- Use Pinia stores for shared UI state and service modules for Tauri calls.
- Keep concurrency guards such as in-flight request reuse and sequence checks
  when editing refresh flows.
- Prefer pure utilities for selection, partition, formatting, and parsing logic.
- Treat browser mode as a visual development adapter. Native capabilities should
  return empty/sample read data or remain unavailable, never mutate the host.
- Follow existing Nuxt UI and Vue Composition API patterns.

## Persistence and protocol compatibility

JSON manifests and the Agent HTTP protocol are durable interfaces. When changing
a persisted or shared type:

1. Check Rust serde names and TypeScript mirrors.
2. Decide how existing manifests deserialize.
3. Add defaults or an explicit schema migration when appropriate.
4. Update request/response tests for protocol changes.
5. Update `DESIGN.md` if an invariant or flow changes.

Do not repurpose an existing enum value or field with new semantics. Add a new
value or version instead. Bump `easydeploymesh-gho::PARSER_VERSION` when parser
semantics can change accepted input or expanded output. Bump the WinPE runtime
layout revision when injection behavior changes without changing an embedded
payload.

## Generated and bundled files

- Do not edit `.nuxt`, `.output`, `dist`, `target`, or `node_modules`.
- Do not hand-edit generated Tauri schemas under
  `apps/desktop/src-tauri/gen/schemas` unless the generating workflow explicitly
  requires committing regenerated output.
- Treat Agent sidecars, icons, iPXE, and wimboot assets as bundled artifacts.
  Preserve their license files and regenerate binaries through project scripts.
- Do not add large deployment images, runtime captures, or raw diagnostics to
  the repository.
- Do not modify files in `release/` unless the task is explicitly a release or
  artifact-collection task.

## Test expectations

Add tests at the interface that owns the changed behavior. Do not test through
private implementation details when the public module interface exposes the
same outcome.

Use these focused commands during development:

```bash
pnpm typecheck
pnpm --filter @easydeploymesh/desktop test
pnpm test:diagnostics
cargo test -p easydeploymesh-core
cargo test -p easydeploymesh-gho
cargo test -p easydeploymesh-service
cargo test -p easydeploymesh-agent
cargo test -p easydeploymesh-desktop
cargo fmt --all --check
```

Before handing off a normal source change, run the complete repository check:

```bash
pnpm check
```

Choose additional evidence by change area:

- Core state or partitions: core tests plus all host and Agent consumers.
- GHO parser: truncation/corruption, expansion-limit, compression, deterministic
  expanded hash, and Agent restore metadata tests.
- Image library: staged import, path containment, span ambiguity, size/hash
  changes, index validation, and concurrent reference behavior.
- Control plane: authentication, lease ownership/expiry, current disk inventory,
  image eligibility, and HTTP status mapping.
- PXE: config validation, DHCP packet fixtures, TFTP path safety, boot-package
  replacement, and WinPE injection markers.
- Agent executor: tests must prove failure occurs before partitioning. Real disk
  validation must use a disposable VM or machine.
- UI: utility/store tests, typecheck, and screenshots for visible changes.
- Diagnostics: marker, truncation, sanitization, and documented exit-code tests.

Never run a destructive deployment test against the developer's normal machine
or a disk that has not been explicitly designated disposable.

## Build and release rules

Common commands from the repository root are:

```bash
pnpm install
pnpm dev
pnpm tauri:dev
pnpm build:mac
pnpm build:windows
```

Builds may require Tauri platform prerequisites; Windows cross-builds also use
`cargo-xwin`. Do not claim Windows/WinPE compatibility from a macOS unit-test run
alone.

When changing the version, keep these locations synchronized:

- Workspace version in `Cargo.toml`.
- Root `package.json`.
- `apps/desktop/package.json`.
- `apps/desktop/src-tauri/tauri.conf.json`.
- Browser fallback version values in the frontend.
- `CHANGELOG.md` and release filenames/checksums when producing a release.

Do not publish, push, create a release, or upload artifacts unless the user
explicitly asks for that external action.

## Documentation

- Keep `README.md` focused on users and setup.
- Keep `DESIGN.md` synchronized with architecture, invariants, persistence, and
  protocol flows.
- Update `CHANGELOG.md` for notable user-visible or compatibility changes.
- Update `CONTRIBUTING.md` when contributor workflow or required checks change.
- Document current behavior, not aspirational support.
- Use exact support language for destructive formats and environments; avoid
  broad claims such as “all GHO images” or “all Windows systems.”
