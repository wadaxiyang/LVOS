#!/usr/bin/env bash
set -euo pipefail

readonly target="x86_64-pc-windows-msvc"
readonly output="target/windows-package/LVOS.exe"
readonly version="$(python3 scripts/workspace_version.py)"
readonly archive="target/release-package/LVOS-${version}-windows-x86_64.zip"

"$(dirname "$0")/check-windows-cross.sh"
cargo-xwin build \
    --release \
    --locked \
    --target "${target}" \
    -p lvos

mkdir -p "$(dirname "${output}")"
cp "target/${target}/release/lvos.exe" "${output}"
echo "Windows 11 x86_64 executable created: ${output}"
python3 scripts/create_release_zip.py \
    --source "${output}" \
    --archive-name "LVOS.exe" \
    --output "${archive}"
echo "unsigned Windows x86_64 release archive created: ${archive}"
