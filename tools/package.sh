#!/bin/sh
# Builds both versions into dist/: "Disk Sweeper.app" (macOS) and "Disk Sweeper.exe" (Windows).
set -e
cd "$(dirname "$0")/.."
cargo build --release
# Windows cross-build with zig as linker and dlltool (see README). Absolute paths because
# cargo runs rustc from each dependency's own folder.
TOOLS="$(pwd)/tools"
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="$TOOLS/zig-win-cc.sh" \
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-C dlltool=$TOOLS/zig-dlltool.sh" \
  cargo build --release --target x86_64-pc-windows-gnu
tools/bundle-macos.sh target/release/disk-sweeper
cp target/x86_64-pc-windows-gnu/release/disk-sweeper.exe "dist/Disk Sweeper.exe"
echo "Built: dist/Disk Sweeper.app and dist/Disk Sweeper.exe"
