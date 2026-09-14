#!/usr/bin/env bash
set -euo pipefail

readonly target="x86_64-pc-windows-msvc"
readonly output="target/windows-package/LVOS.exe"

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
echo "Build the installable EXE on Windows with scripts/package-windows-installer.ps1"
