#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target="x86_64-pc-windows-msvc"
artifact="${CARGO_TARGET_DIR:-$HOME/.cache/.cargo-build}/${target}/release/dlss-updater.exe"
dist="$root/dist"

cd "$root"
cargo xwin build --workspace --release --target "$target"
mkdir -p "$dist"
cp "$artifact" "$dist/dlss-updater.exe"
(
  cd "$dist"
  sha256sum dlss-updater.exe > SHA256SUMS
  zip -9 -q dlss-updater-windows-x86_64.zip dlss-updater.exe SHA256SUMS
)
