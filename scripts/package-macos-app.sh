#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
    echo "error: macOS arm64 is required" >&2
    exit 1
fi

cargo build --release --locked -p lvos-agent -p lvos

readonly version="$(python3 scripts/workspace_version.py)"

bundle="target/release/LVOS.app"
contents="${bundle}/Contents"
binary="${contents}/MacOS/LVOS"
ui_binary="${contents}/MacOS/lvos-ui"
dmg="target/release-package/LVOS-${version}-macos-arm64.dmg"
staging="target/release-package/dmg-root"

rm -rf "${bundle}"
mkdir -p "${contents}/MacOS" "${contents}/Resources"
cp target/release/lvos-agent "${binary}"
cp target/release/lvos-ui "${ui_binary}"
chmod 755 "${binary}" "${ui_binary}"
mkdir -p "${contents}/Resources/NOTICES"
cp LICENSE "${contents}/Resources/NOTICES/LVOS-LICENSE.txt"
cp THIRD_PARTY_NOTICES.md "${contents}/Resources/NOTICES/THIRD_PARTY_NOTICES.md"
cp licenses/Quadrant-Kit-GPL-3.0.txt "${contents}/Resources/NOTICES/Quadrant-Kit-GPL-3.0.txt"
cp licenses/Quadrant-Kit-NOTICES.md "${contents}/Resources/NOTICES/Quadrant-Kit-NOTICES.md"
cp licenses/Fluent-System-Icons-MIT.txt "${contents}/Resources/NOTICES/Fluent-System-Icons-MIT.txt"

cat > "${contents}/Info.plist" <<PLIST
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
    <string>${version}</string>
    <key>CFBundleVersion</key>
    <string>${version}</string>
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
rm -rf "${staging}"
mkdir -p "${staging}"
cp -R "${bundle}" "${staging}/LVOS.app"
ln -s /Applications "${staging}/Applications"
mkdir -p "$(dirname "${dmg}")"
rm -f "${dmg}"
hdiutil create \
    -volname "LVOS ${version}" \
    -srcfolder "${staging}" \
    -format UDZO \
    -ov \
    "${dmg}"
hdiutil verify "${dmg}"
echo "unsigned macOS arm64 disk image created: ${dmg}"
