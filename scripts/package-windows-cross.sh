#!/usr/bin/env bash
set -euo pipefail

readonly target="x86_64-pc-windows-msvc"
readonly output="target/windows-package/LVOS.exe"

"$(dirname "$0")/check-windows-cross.sh"
cargo-xwin build \
    --release \
    --locked \
    --target "${target}" \
    -p lvos

mkdir -p "$(dirname "${output}")"
cp "target/${target}/release/lvos.exe" "${output}"
echo "Windows 11 x86_64 executable created: ${output}"
