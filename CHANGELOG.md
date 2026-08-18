# Changelog

All notable changes to EasyDeployMesh will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Public project documentation and contribution guidelines.

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
