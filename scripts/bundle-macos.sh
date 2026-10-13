#!/bin/sh
# Wrap a built po binary in a macOS .app bundle, so the Dock shows its icon.
# Usage: scripts/bundle-macos.sh <binary> <version> <output dir>
# Needs sips and iconutil, which ship with macOS.
set -eu

bin=$1 version=${2#v} out=$3
root=$(cd "$(dirname "$0")/.." && pwd)
app="$out/po.app"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/po"

iconset=$(mktemp -d)/po.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$root/assets/icon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
  sips -z "$((size * 2))" "$((size * 2))" "$root/assets/icon.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/po.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>po</string>
  <key>CFBundleDisplayName</key><string>po</string>
  <key>CFBundleIdentifier</key><string>com.sumofbytes.po</string>
  <key>CFBundleExecutable</key><string>po</string>
  <key>CFBundleIconFile</key><string>po</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
