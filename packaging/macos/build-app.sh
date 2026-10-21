#!/bin/sh
# Build monctl.app (menu bar app + CLI inside) and monctl-macos.dmg.
# Universal (arm64 + x86_64) when both Rust targets are installed, else the
# host architecture. Ad-hoc signed: first launch needs right-click → Open.
#
#   packaging/macos/build-app.sh [OUT_DIR]       (default: dist/)
set -e
cd "$(dirname "$0")/../.."
OUT="${1:-dist}"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
WORK="$OUT/macos-work"
APP="$OUT/monctl.app"
rm -rf "$WORK" "$APP"
mkdir -p "$WORK" "$APP/Contents/MacOS" "$APP/Contents/Resources"

# CLI
archs=""
for t in aarch64-apple-darwin x86_64-apple-darwin; do
    if cargo build --release --target "$t" >/dev/null 2>&1; then
        archs="$archs target/$t/release/monctl"
    fi
done
[ -n "$archs" ] || { cargo build --release; archs="target/release/monctl"; }
# CI must ship real universal binaries; locally a single arch is fine
if [ -n "$CI" ] && [ "$(echo $archs | wc -w)" -ne 2 ]; then
    echo "error: universal build needs both aarch64 and x86_64 Rust targets" >&2
    exit 1
fi
# shellcheck disable=SC2086
lipo -create $archs -output "$APP/Contents/MacOS/monctl"

# menu bar app (macOS 12+: virtual displays need 11, SF Symbols/CI need 11+)
trays=""
for a in arm64 x86_64; do
    if swiftc -O -target "$a-apple-macos12" macos-tray/tray.swift -o "$WORK/tray-$a" 2>/dev/null; then
        trays="$trays $WORK/tray-$a"
    fi
done
if [ -n "$CI" ] && [ "$(echo $trays | wc -w)" -ne 2 ]; then
    echo "error: universal tray build failed" >&2
    exit 1
fi
# shellcheck disable=SC2086
lipo -create $trays -output "$APP/Contents/MacOS/monctl-tray"

swift packaging/macos/make-icon.swift "$WORK/AppIcon.iconset"
iconutil -c icns "$WORK/AppIcon.iconset" -o "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>cn.williamwong.monctl</string>
    <key>CFBundleName</key><string>monctl</string>
    <key>CFBundleDisplayName</key><string>monctl</string>
    <key>CFBundleExecutable</key><string>monctl-tray</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>MIT License</string>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$APP"

# dmg: the app plus an Applications link to drag it onto
STAGE="$WORK/dmg"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$OUT/monctl-macos.dmg"
hdiutil create -quiet -volname "monctl $VERSION" -srcfolder "$STAGE" -format UDZO "$OUT/monctl-macos.dmg"

# plain CLI tarball too
tar -C "$APP/Contents/MacOS" -czf "$OUT/monctl-macos-universal.tar.gz" monctl
rm -rf "$WORK"
echo "built $APP, $OUT/monctl-macos.dmg ($(lipo -archs "$APP/Contents/MacOS/monctl"))"
