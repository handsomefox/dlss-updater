# DLSS Updater

[![CI](https://github.com/handsomefox/dlss-updater/actions/workflows/ci.yml/badge.svg)](https://github.com/handsomefox/dlss-updater/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A Windows desktop app that replaces the official NVIDIA Streamline and DLSS DLLs in your installed games, with a backup and a verification step around every write.

## Features

- Finds games installed through Steam, the Epic Games Store, and GOG.
- Accepts game folders you add by hand.
- Offers one-click upgrades to strictly newer DLLs, plus reviewed changes per DLL or in bulk.
- Downloads official NVIDIA Streamline release archives on demand.
- Validates archive paths, file sizes, PE architecture, hashes, and Authenticode signatures.
- Backs up every DLL it replaces, so you can undo the last run or restore an older version.
- Toggles the NVIDIA DLSS on-screen indicator, and can toggle it back.

## Safety model

Before the app replaces a DLL, it hashes the installed file and plans the swap against that hash. It copies the current file into a content-addressed backup store, writes the new DLL, then re-reads the result to confirm the replacement. If the installed file changed between the plan and the write, the swap fails instead of overwriting something unexpected.

Some game folders need administrator rights. For those, the app writes a plan file and relaunches itself as an elevated helper under UAC. The helper does not trust the plan it was handed. It re-validates every path, and it accepts exactly one system setting: the DLSS on-screen indicator.

This project is not affiliated with or endorsed by NVIDIA. DLSS, NVIDIA, and Streamline are trademarks of NVIDIA Corporation.

## Development

The portable logic and the archive-security tests run on any OS, including Linux:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

CI runs those three commands on Ubuntu and Windows, plus a native release build on Windows and `cargo audit`.

To cross-build the Windows 10/11 x86-64 app from Linux, use `cargo-xwin`:

```sh
cargo xwin build --workspace --release --target x86_64-pc-windows-msvc
```

To produce the portable executable, its SHA-256 checksum, and a ZIP under `dist/`:

```sh
bash scripts/package-windows.sh
```

The app downloads only release assets from the official `NVIDIA-RTX/Streamline` repository. Older release tags stay metadata-only until you download one, and the app validates it then. Version 1 does not import local ZIP files and does not discover Microsoft Store or Xbox installations. Both omissions are deliberate.

## License

Licensed under the [MIT License](LICENSE).
