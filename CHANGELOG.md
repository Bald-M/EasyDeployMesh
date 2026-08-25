# Changelog

All notable changes to EasyDeployMesh will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.6] - 2026-08-26

### Added

- Public project documentation and contribution guidelines.
- A GitHub repository shortcut in the desktop header, using the official GitHub
  mark and opening the project page in the system browser.
- A host-aware `pnpm build` entry point for building all supported native and
  cross-compiled Windows installers, with optional platform and target selectors.
- Native macOS WinPE and third-party PE import using pinned Intel and Apple
  Silicon `wimlib-imagex` sidecars, with runtime version, architecture, and
  SHA-256 capability checks.
- A bounded, platform-independent Rust REGF module for generating canonical
  WinPE BCD stores and updating the restricted `SYSTEM\Setup\CmdLine` fallback.
- Device operational-status classification and selection safeguards in the
  desktop application, with matching Simplified Chinese and English messages.

### Changed

- Updated the deployment demonstration animation in the English and Simplified
  Chinese README files.
- Documented the tested PE compatibility boundary: EasyU 3.6 is currently
  supported for Agent-based deployment, while WePE 2.2 is unsupported because
  its vendor intentionally omits the Windows network module. Source PE media is
  never modified in place.
- Removed GHO/GHS import and restore support. Norton Ghost is no longer
  maintained, its proprietary image variants cannot be restored safely without
  a supported official engine, and users must convert existing images to WIM or
  ESD in an isolated environment before import.
- Recorded successful full-workflow field tests for FirPE v2.1.1 and HotPE
  v2.8.251018. USM v5F is documented as partial compatibility: it can reach the
  PE desktop, but its external tool package is unavailable through managed PXE.

### Fixed

- Receive DHCP broadcasts on the selected macOS interface while keeping the
  privileged PXE sockets interface-bound, so remote PXE clients can be
  discovered without exposing the service on other adapters.
- Start macOS DHCP and TFTP through a narrowly scoped administrator-authorized
  socket helper, and keep failed imports in staging so an existing boot package
  remains unchanged.
- Detect and free an occupied Nuxt development port before `pnpm tauri:dev`,
  preventing the desktop window from loading a stale or unavailable frontend.
- Import Edgeless UDF media through its complete filesystem view, use the boot
  manager embedded in its WIM, and embed its external runtime resources into
  the managed WinPE image so PXE boots do not stop at the missing-folder prompt.
- Preserve a media-specific wimboot policy so EasyU receives its required source
  `bootmgr`, while Edgeless continues using the compatible Boot Manager embedded
  in its WIM.
- Streamed TFTP files without blocking the async service on large WinPE images,
  and now report exhausted ACK retries as failures before logging success or
  advancing a `boot.wim` client to the Agent-waiting stage.
- Improved Agent startup diagnostics and native Windows MAC-address fallback
  behavior for compatible reduced WinPE environments, fixed diagnostic-directory
  detection on RAM disks, and hid the managed shell host console.
- Added a native ISO boot path for WEPE64 v2.2 media whose private `BOOTMGR`
  requires `\WEPE\B64` and the original ISO directory layout. The importer now
  leaves the source unchanged and can recreate a managed ISO with the original
  BIOS and UEFI El Torito boot images for HTTP `sanboot` with bounded Range
  support. This does not make WePE Agent-capable: WePE 2.2 remains unsupported
  for automated deployment because its Windows network stack is absent.
- Selected the matching cargo-xwin SDK architecture for every Windows desktop
  target so cross-building the x86 installer includes the required x86 libraries.
- Reject partition plans that cannot fit the selected target disk before job
  creation, with per-device capacity details for batch deployment, while
  retaining the Agent's pre-destructive capacity check.
- Use an MBR extended partition for three-volume templates so the temporary
  image cache does not exceed the four-primary-partition limit during restore.
- Calculate custom-template limits and remaining-space previews from actual disk
  bytes and deployment reserves, and include bounded command output when a
  Windows deployment tool fails.
- Import USM v5F only when its selected WIM and renamed Windows Boot Manager
  generation match. Preserve its `SC6`, `nointegritychecks`, and `testsigning`
  boot requirements without weakening the standard WinPE path.

## [0.2.4] - 2026-08-18

### Added

- Cross-platform desktop application built with Nuxt 4, Nuxt UI, and Tauri 2.
- Simplified Chinese and English interface with runtime language switching.
- Local-network PC discovery, persistent device inventory, authenticated
  heartbeats, and online-presence tracking.
- Explicit-interface Agent control service with ephemeral enrollment tokens.
- Cross-platform Rust Agent with a verified Windows x64 build target.
- Deployment job state machine, durable job storage, and deployment activity
  history.
- Persistent GHO, WIM, ESD, and SWM image catalog with SHA-256 verification.
- Unattended deployment execution for verified WIM and ESD images.
- Native, streaming verification and manual restore support for compatible
  password-free, partition-level NTFS GHO images.
- Support for GHO Z0, Z1, and Z3-Z9 compression without bundling or executing
  Ghost, Symantec, or Broadcom software.
- Expanded partition hashing during GHO import and restore without creating a
  raw-image cache.
- GHS split-image discovery and catalog support.
- PXE and WinPE media import workflow with automatic Agent injection.
- WinPE runtime package verification, diagnostic collection, and sanitized
  diagnostic analysis tools.
- macOS application builds and cross-compiled Windows x64 NSIS installers.
- Automated TypeScript, Vue, diagnostic-script, and Rust test suites.

### Security

- Fail-closed validation for unsupported or malformed GHO images.
- Image hashes and expanded content are verified again before a deployment lease
  is issued.
- Target volumes are locked and dismounted before native GHO restore operations.
- Enrollment tokens are redacted from the provided diagnostic tooling.

### Known limitations

- GHS and SWM files can be cataloged but are not currently deployable.
- GHO deployment is manual-only and supports a limited, explicitly validated
  image profile.
- Whole-disk GHO images, encrypted images, unsupported filesystems, and image
  creation are not supported.
- The HTTP control channel is intended only for trusted, isolated local networks
  until TLS and certificate pinning are implemented.

[Unreleased]: https://github.com/Bald-M/EasyDeployMesh/compare/v0.2.6...HEAD
[0.2.6]: https://github.com/Bald-M/EasyDeployMesh/releases/tag/v0.2.6
[0.2.4]: https://github.com/Bald-M/EasyDeployMesh/releases/tag/v0.2.4
