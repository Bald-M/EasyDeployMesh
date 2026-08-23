# Changelog

All notable changes to EasyDeployMesh will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Public project documentation and contribution guidelines.
- A host-aware `pnpm build` entry point for building all supported native and
  cross-compiled Windows installers, with optional platform and target selectors.

### Changed

- Updated the deployment demonstration animation in the English and Simplified
  Chinese README files.
- Documented the tested PE compatibility boundary: EasyU 3.6 is currently
  supported for Agent-based deployment, while WePE 2.2 is unsupported because
  its vendor intentionally omits the Windows network module. Source PE media is
  never modified in place.

### Fixed

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

[Unreleased]: https://github.com/Bald-M/EasyDeployMesh/compare/v0.2.4...HEAD
[0.2.4]: https://github.com/Bald-M/EasyDeployMesh/releases/tag/v0.2.4
