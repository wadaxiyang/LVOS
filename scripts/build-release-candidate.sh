#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
    echo "error: release-candidate assembly requires macOS arm64" >&2
    exit 1
fi

readonly version="$(python3 scripts/workspace_version.py)"
readonly output="target/release-package"
readonly macos="${output}/LVOS-${version}-macos-arm64.zip"
readonly windows="${output}/LVOS-${version}-windows-x86_64.zip"
readonly manifest="${output}/lvos-update-stable.json"

./scripts/check-before-commit.sh
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --no-deps
./scripts/package-macos-app.sh
./scripts/package-windows-cross.sh
python3 scripts/generate_update_manifest.py \
    --version "${version}" \
    --channel stable \
    --macos "${macos}" \
    --windows "${windows}" \
    --output "${manifest}"
python3 scripts/generate_release_checksums.py \
    --output "${output}/SHA256SUMS" \
    "${macos}" "${windows}" "${manifest}"
python3 scripts/verify_release_candidate.py \
    --directory "${output}" \
    --version "${version}"

echo "LVOS ${version} unsigned release candidate is ready in ${output}"
