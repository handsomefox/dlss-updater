# Contributing

Thanks for helping improve DLSS Updater.

## Before opening a change

- Use an issue for substantial behavior or architecture changes.
- Keep platform-neutral behavior in `dlss-core` and Windows APIs in `dlss-platform`.
- Preserve the elevated helper's allowlist and independent plan validation.
- Do not weaken archive, signature, hash, backup, or path validation for convenience.

## Verification

Run the portable checks before submitting a pull request:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Changes to Windows-only behavior should also pass:

```sh
cargo xwin build --workspace --release --target x86_64-pc-windows-msvc
```

Describe any manual Windows testing in the pull request, especially for discovery, downloads, DLL replacement, Undo, UAC, and registry controls.
