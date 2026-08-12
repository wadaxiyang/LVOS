#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
    echo "error: macOS arm64 is required" >&2
    exit 1
fi

cargo build --release -p lvos

bundle="target/release/LVOS.app"
contents="${bundle}/Contents"
binary="${contents}/MacOS/LVOS"

rm -rf "${bundle}"
mkdir -p "${contents}/MacOS" "${contents}/Resources"
cp target/release/lvos "${binary}"
chmod 755 "${binary}"

cat > "${contents}/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>LVOS</string>
    <key>CFBundleExecutable</key>
    <string>LVOS</string>
    <key>CFBundleIdentifier</key>
    <string>site.niuniu770.lvos</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>LVOS</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSMinimumSystemVersion</key>
    <string>15.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

plutil -lint "${contents}/Info.plist"
codesign \
    --force \
    --deep \
    --sign - \
    --identifier site.niuniu770.lvos \
    --requirements '=designated => identifier "site.niuniu770.lvos"' \
    "${bundle}"
codesign --verify --deep --strict "${bundle}"
echo "macOS app bundle created: ${bundle}"
