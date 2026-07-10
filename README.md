# DLSS Updater

Windows-first desktop manager for official NVIDIA Streamline/DLSS DLLs. The UI supports strict one-click upgrades, reviewed bulk changes, mixed per-DLL profiles, versioned backups with Undo, and the reversible global DLSS indicator control.

## Development

Portable logic and archive-security tests run on Linux:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Build the Windows 10/11 x86-64 application from Arch Linux with `cargo-xwin`:

```sh
cargo xwin build --workspace --release --target x86_64-pc-windows-msvc
```

Create the portable executable, checksum, and ZIP under `dist/`:

```sh
bash scripts/package-windows.sh
```

The application downloads only official `NVIDIA-RTX/Streamline` release assets. Historical tags remain metadata-only until explicitly downloaded and validated. Local ZIP imports and Microsoft Store/Xbox discovery are intentionally unsupported in v1.
