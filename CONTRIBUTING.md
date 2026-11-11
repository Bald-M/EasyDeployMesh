# Contributing to EasyDeployMesh

Thank you for helping improve EasyDeployMesh. Contributions of all sizes are
welcome, including bug reports, documentation, tests, translations, and code.

## Before you start

- Search existing issues and pull requests before opening a new one.
- For a substantial feature or architectural change, open an issue first so the
  approach and scope can be discussed before implementation.
- Keep each pull request focused on one problem. Unrelated changes should be
  submitted separately.
- Never include credentials, enrollment tokens, private network information,
  proprietary Windows images, or third-party software that cannot legally be
  redistributed.

## Development setup

You will need:

- Node.js 22 or later
- pnpm 11 or later
- Rust 1.96 or later
- The platform prerequisites for Tauri 2

Install the JavaScript dependencies from the repository root:

```bash
pnpm install
```

Run the web interface in a browser:

```bash
pnpm dev
```

Run the application in the Tauri desktop shell:

```bash
pnpm tauri:dev
```

Build every installer supported by the current host with:

```bash
pnpm build
```

Use a selector such as `pnpm build -- windows` or
`pnpm build -- windows-x64` for a narrower build. macOS and Linux packages
require their respective native hosts; Windows packages can be cross-compiled
from non-Windows hosts with `cargo-xwin`.

The browser development mode uses safe fallbacks for native Tauri commands, so
features that depend on the host operating system should also be tested in the
desktop shell.

## Making changes

1. Fork the repository and create a descriptive branch from the default branch.
2. Make the smallest complete change that solves the problem.
3. Add or update tests for behavior that changed.
4. Update user-facing documentation and both locales when applicable.
5. Run the project checks before opening a pull request.

Suggested branch names include `fix/device-refresh`, `feat/image-validation`,
and `docs/winpe-setup`.

Please follow the existing code style and structure. Format Rust code with:

```bash
cargo fmt --all
```

User-visible text should use the localization files in
`apps/desktop/i18n/locales/` rather than being hard-coded in a component.

## Tests and checks

Run the complete validation suite from the repository root:

```bash
pnpm check
```

This runs the TypeScript checks, diagnostic-script tests, desktop tests, Rust
workspace tests, and Nuxt generation. You can also run narrower checks while
developing:

```bash
pnpm typecheck
pnpm test
pnpm test:rust
pnpm --filter @easydeploymesh/desktop test:watch
cargo fmt --all --check
```

Platform-specific deployment or WinPE changes should include clear manual test
steps and results in the pull request. Do not claim support for an image format,
firmware mode, partition layout, or Windows environment that has not been tested.

## Safety requirements

EasyDeployMesh performs disk and operating-system deployment operations. Changes
in this area must fail safely and make the selected device, image, disk, and
partition unambiguous before destructive work begins.

- Do not weaken image integrity checks, target-disk validation, enrollment, or
  authentication controls merely to make a workflow succeed.
- Avoid logging secrets, enrollment tokens, raw private paths, or unnecessary
  device information.
- Preserve explicit network-interface selection and safe behavior on untrusted
  input.
- Treat disk writes, partition changes, boot configuration, and PXE operations
  as destructive. Tests should use fixtures, temporary directories, mocks, or
  disposable virtual machines whenever possible.
- The current HTTP control channel is intended only for trusted, isolated local
  networks until TLS and certificate pinning are implemented.

If you discover a security vulnerability, do not publish exploit details or
sensitive data in a public issue. Contact the maintainer privately through their
GitHub profile first.

## Issues

Open issues through the forms in `.github/ISSUE_TEMPLATE`; blank issues are
disabled.

**Title**: `[Type][Priority]One-line description`, with no spaces between the
prefixes or before the description. Chinese or English descriptions are both
fine.

```text
[Bug][P1]Legacy BIOS boot fails when boot.wim TFTP transfer times out
[Enhancement][P2]Restore file-system-based GHO images on NTFS partitions
[Documentation][P3]Align GHO support statements across README and DESIGN
```

- A bug describes the condition and the symptom; an enhancement describes the
  outcome to reach.
- Do not use Conventional Commits prefixes such as `feat(...)` in issue titles;
  they are only for commits and pull request titles.
- Leave `[P?]` when unsure; a maintainer triages it and edits the title.
- A closed issue that was never triaged may keep only the type prefix.

**Labels**: every open issue carries one type label and one priority label that
match its title.

- The issue form adds the type label.
- `.github/workflows/issue-priority-label.yml` syncs the priority label from
  `[P0]`–`[P3]` in the title, so changing priority only needs a title edit.

| Type label      | Meaning                                       |
| --------------- | --------------------------------------------- |
| `bug`           | Existing behavior does not match expectations |
| `enhancement`   | New feature or improvement                    |
| `documentation` | Documentation problem                         |

| Priority | Meaning                                                                                                |
| -------- | ------------------------------------------------------------------------------------------------------ |
| `P0`     | Urgent: wrong-disk writes, data loss, or a security issue; handle immediately                          |
| `P1`     | High: a core flow (discovery, PXE boot, enrollment, deployment) is broken; handle in the current cycle |
| `P2`     | Medium: a workaround exists or the impact is limited; schedule it                                      |
| `P3`     | Low: UX polish or technical debt; handle when time allows                                              |

GitHub's default labels such as `duplicate`, `invalid`, `wontfix`, and
`question` keep their usual meaning for triage and closing.

### Bug reports

A useful bug report includes:

- EasyDeployMesh version and operating system
- Whether the problem occurs in browser development mode, the desktop app,
  Windows, or WinPE
- Expected and actual behavior
- Minimal reproduction steps
- Relevant sanitized logs or diagnostic output
- Image format, firmware mode, and network topology when relevant

Remove credentials, enrollment tokens, personal data, internal addresses, and
other secrets before attaching logs.

## Pull requests

Commit messages and pull request titles use
[Conventional Commits](https://www.conventionalcommits.org/) in the imperative
mood:

```text
<type>(<scope>): <summary>
```

Common types are `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, and
`release`. Scopes follow the affected area, such as `pxe`, `deployment`,
`partition`, `agent`, `desktop`, `images`, or `gho`. For example:

```text
fix(partition): prevent invalid target disk layouts
feat(pxe): add macOS WinPE import
release: v0.2.6
```

Fill in `.github/pull_request_template.md`. A pull request should include:

- A concise explanation of the problem and solution
- Links to related issues (`Closes #123` or `Refs #123`)
- Tests added or updated
- Commands run and their results
- Screenshots for visible UI changes
- Manual verification details for Windows, WinPE, PXE, or destructive workflows
- Any compatibility, migration, or security implications

Maintainers may request changes to keep the project safe, maintainable, and
within its supported scope.

## License

By submitting a contribution, you agree that your contribution is licensed
under the [Apache License 2.0](LICENSE), the same license as this project. You
also confirm that you have the right to submit the contribution under those
terms.
