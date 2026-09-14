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
    -p lvos-agent \
    -p lvos

mkdir -p "$(dirname "${output}")"
cp "target/${target}/release/lvos-agent.exe" "${output}"
cp "target/${target}/release/lvos-ui.exe" "target/windows-package/lvos-ui.exe"
echo "Windows 11 x86_64 Agent/UI pair created in target/windows-package"
python3 scripts/create_release_zip.py \
    --source "${output}" \
    --archive-name "LVOS.exe" \
    --extra "lvos-ui.exe=target/windows-package/lvos-ui.exe" \
    --output "${archive}"
echo "unsigned Windows x86_64 release archive created: ${archive}"
