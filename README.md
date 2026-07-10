# DLSS Updater

[![CI](https://github.com/handsomefox/dlss-updater/actions/workflows/ci.yml/badge.svg)](https://github.com/handsomefox/dlss-updater/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A Windows desktop manager for safely updating official NVIDIA Streamline and DLSS DLLs.

## Features

- Discovers supported Steam, Epic Games Store, and GOG installations.
- Supports manually managed game folders.
- Offers strict one-click upgrades and reviewed per-DLL or bulk changes.
- Downloads official NVIDIA Streamline release archives on demand.
- Validates archive paths, sizes, PE architecture, hashes, and Authenticode trust.
- Creates content-addressed backups and supports immediate Undo and older restores.
- Includes a scoped, reversible NVIDIA DLSS indicator control.

## Safety model

Every replacement is planned against the installed DLL hash, backed up before mutation, and verified after replacement. Operations requiring administrator rights are passed to a narrow elevated helper that independently validates the plan and permits only known game paths and the allowlisted indicator setting.

This project is not affiliated with or endorsed by NVIDIA. DLSS, NVIDIA, and Streamline are trademarks of NVIDIA Corporation.

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

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Please report security-sensitive issues according to [SECURITY.md](SECURITY.md).

## License

Licensed under the [MIT License](LICENSE).
