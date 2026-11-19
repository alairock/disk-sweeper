#!/bin/sh
# CI release packages. The local tools/package.sh builds the two native app files.
set -eu
cd "$(dirname "$0")/.."
version=${1:?usage: release-macos.sh VERSION}
export MACOSX_DEPLOYMENT_TARGET=11.0
cargo build --locked --release --target aarch64-apple-darwin --target x86_64-apple-darwin
mkdir -p dist
stage=$(mktemp -d "$PWD/dist/stage.XXXXXX")
trap 'rm -rf "$stage"' EXIT
lipo -create target/aarch64-apple-darwin/release/disk-sweeper \
  target/x86_64-apple-darwin/release/disk-sweeper -output "$stage/disk-sweeper"
for arch in arm64 x86_64; do
  lipo "$stage/disk-sweeper" -verify_arch "$arch"
done
tools/bundle-macos.sh "$stage/disk-sweeper" --timestamp

app="dist/Disk Sweeper.app"
base="dist/DiskSweeper-$version-macos-universal"
codesign --verify --strict --all-architectures "$app"
if [ -n "${NOTARY_PROFILE:-}" ]; then
  ditto -c -k --keepParent "$app" "$stage/notary.zip"
  xcrun notarytool submit "$stage/notary.zip" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$app"
fi
ditto -c -k --keepParent "$app" "$base.zip"
mkdir "$stage/image"
ditto "$app" "$stage/image/Disk Sweeper.app"
ln -s /Applications "$stage/image/Applications"
hdiutil create -ov -format UDZO -volname "Disk Sweeper" -srcfolder "$stage/image" "$base.dmg"
if [ -n "${NOTARY_PROFILE:-}" ]; then
  codesign --force --timestamp -s "$SIGN_IDENTITY" "$base.dmg"
  xcrun notarytool submit "$base.dmg" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$base.dmg"
fi
