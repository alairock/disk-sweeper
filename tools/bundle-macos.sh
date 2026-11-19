#!/bin/sh
# Assemble dist/Disk Sweeper.app from a native or universal executable.
set -eu
cd "$(dirname "$0")/.."
pkgid=$(cargo pkgid --locked -p disk-sweeper)
version=${pkgid##*#}
version=${version##*@}
mkdir -p dist
APP="dist/Disk Sweeper.app/Contents"
rm -rf "dist/Disk Sweeper.app"
mkdir -p "$APP/MacOS" "$APP/Resources"
cp "$1" "$APP/MacOS/Disk Sweeper"
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
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>${version%%-*}</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
</dict></plist>
PLIST
# Signing: set SIGN_IDENTITY to a codesigning identity from your keychain (list them with
# `security find-identity -v -p codesigning`) so macOS privacy grants (Full Disk Access)
# survive rebuilds. Without it the app is signed ad-hoc; that signature changes every
# build, so macOS forgets the grant each time.
if [ -n "${SIGN_IDENTITY:-}" ]; then
  codesign --force --options runtime "${2:---timestamp=none}" -s "$SIGN_IDENTITY" "dist/Disk Sweeper.app"
else
  echo "note: SIGN_IDENTITY not set; signing ad-hoc (Full Disk Access resets on each rebuild)" >&2
  codesign --force -s - "dist/Disk Sweeper.app"
fi
