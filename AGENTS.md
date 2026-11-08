# Repository guidelines

The [README](README.md) covers what the app does, its safety model, and the build and check commands. This page covers the rules for changing it.

## Where code goes

The workspace is layered so that everything portable stays testable on Linux:

- `dlss-core` holds the domain model, the provider traits, and the plan-and-swap engine.
- `dlss-catalog` downloads and validates official Streamline release archives.
- `dlss-platform` implements the core traits, and confines Win32 and registry calls to `windows.rs`.
- `dlss-app` is the egui desktop binary, and the only crate that depends on egui.
- The shared crates come from [handsomefox/app-internals](https://github.com/handsomefox/app-internals), pinned to a tag: `app-update` updates the app from its GitHub releases, `app-update-ui` draws the update dialog's content, `app-diagnostics` writes the log files, and `app-folders` finds `%LOCALAPPDATA%` and opens folders. That repository's `AGENTS.md` covers changing them. `ui/update.rs` holds this app's chip, its dialog, and the `Look` that themes the content.

Keep platform-neutral behavior in `dlss-core` and Windows APIs in `dlss-platform`. A new Win32 call goes in `windows.rs`, not wherever it is convenient, because that division is the only reason the test suite runs off Windows at all.

## Do not weaken the checks

The archive, signature, hash, backup, and path checks are the product. Do not relax one for convenience.

The elevated helper must keep its allowlist and must keep validating each plan on its own. It cannot trust the process that launched it, so "the GUI already checked this" is not a reason to skip a check there.

The swap is planned against the hash of the installed file and re-read afterward to confirm. If the installed file changed between the plan and the write, the swap must fail rather than overwrite something unexpected.

## Tests

Unit tests live beside their implementations in `#[cfg(test)]` modules. Use `tempfile` for anything touching the filesystem.

Cover the rejection path, not only the success path. Archive traversal, hash mismatches, path validation, elevation plans, backups, and restores are where a regression is expensive and silent. No coverage percentage is required, but a safety regression should have a test that catches it.

CI cannot reach the Windows-only paths. Exercise discovery, downloads, DLL replacement, undo, UAC, and the registry controls by hand on Windows. The same goes for an app update: run a debug build with `APP_UPDATE_PRETEND_VERSION` set to an older version.

## CI and releases

`.github/workflows/ci.yml` and `release.yml` only call the shared workflows in [handsomefox/app-workflows](https://github.com/handsomefox/app-workflows), pinned by commit. The checks, the release steps, and the CI tool pins live there, and its `AGENTS.md` covers changing them. Dependabot moves the pin when that repository releases.

CI runs actionlint and `zizmor --persona pedantic` on every push. Run both before you push a workflow change.
