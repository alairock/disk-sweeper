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
mkdir -p dist
APP="dist/Disk Sweeper.app/Contents"
rm -rf "dist/Disk Sweeper.app"
mkdir -p "$APP/MacOS" "$APP/Resources"
cp target/release/disk-sweeper "$APP/MacOS/Disk Sweeper"
cp assets/AppIcon.icns "$APP/Resources/AppIcon.icns"
cat > "$APP/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Disk Sweeper</string>
  <key>CFBundleDisplayName</key><string>Disk Sweeper</string>
  <key>CFBundleIdentifier</key><string>local.disksweeper</string>
  <key>CFBundleExecutable</key><string>Disk Sweeper</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
</dict></plist>
PLIST
# Signing: set SIGN_IDENTITY to a codesigning identity from your keychain (list them with
# `security find-identity -v -p codesigning`) so macOS privacy grants (Full Disk Access)
# survive rebuilds. Without it the app is signed ad-hoc; that signature changes every
# build, so macOS forgets the grant each time.
if [ -n "${SIGN_IDENTITY:-}" ]; then
  codesign --force --options runtime --timestamp=none -s "$SIGN_IDENTITY" "dist/Disk Sweeper.app"
else
  echo "note: SIGN_IDENTITY not set; signing ad-hoc (Full Disk Access resets on each rebuild)" >&2
  codesign --force -s - "dist/Disk Sweeper.app"
fi
cp target/x86_64-pc-windows-gnu/release/disk-sweeper.exe "dist/Disk Sweeper.exe"
echo "Built: dist/Disk Sweeper.app and dist/Disk Sweeper.exe"
