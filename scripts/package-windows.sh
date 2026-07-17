#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target="x86_64-pc-windows-msvc"
artifact="${CARGO_TARGET_DIR:-$HOME/.cache/.cargo-build}/${target}/release/dlss-updater.exe"
dist="$root/dist"

cd "$root"
cargo xwin build --workspace --release --target "$target" --locked
mkdir -p "$dist"
cp "$artifact" "$dist/dlss-updater.exe"
rm -f "$dist/dlss-updater-windows-x86_64.zip" "$dist/SHA256SUMS"
(
  cd "$dist"
  zip -9 -q dlss-updater-windows-x86_64.zip dlss-updater.exe
  sha256sum dlss-updater.exe dlss-updater-windows-x86_64.zip > SHA256SUMS
  sha256sum --check SHA256SUMS
)
