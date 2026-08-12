#!/usr/bin/env bash
set -euo pipefail

readonly target="x86_64-pc-windows-msvc"
readonly required_xwin_version="0.22.0"

if [[ "$(uname -s)" == "Darwin" && -d "/opt/homebrew/opt/llvm/bin" ]]; then
    export PATH="/opt/homebrew/opt/llvm/bin:${PATH}"
fi

if ! command -v rustup >/dev/null 2>&1; then
    echo "error: rustup is required" >&2
    exit 1
fi

if ! rustup target list --installed | python3 -c 'import sys; target=sys.argv[1]; raise SystemExit(0 if target in {line.strip() for line in sys.stdin} else 1)' "${target}"; then
    echo "error: missing Rust target ${target}" >&2
    echo "install it with: rustup target add ${target}" >&2
    exit 1
fi

if ! command -v cargo-xwin >/dev/null 2>&1; then
    echo "error: cargo-xwin ${required_xwin_version} is required" >&2
    echo "install it with: cargo install cargo-xwin --version ${required_xwin_version} --locked" >&2
    exit 1
fi

for tool in clang-cl llvm-lib; do
    if ! command -v "${tool}" >/dev/null 2>&1; then
        echo "error: ${tool} is required by cargo-xwin and bundled SQLite" >&2
        echo "on macOS install the full LLVM toolchain with: brew install llvm" >&2
        exit 1
    fi
done

actual_xwin_version="$(cargo-xwin --version | python3 -c 'import sys; fields=sys.stdin.read().split(); print(fields[1] if len(fields) >= 2 else "")')"
if [[ "${actual_xwin_version}" != "${required_xwin_version}" ]]; then
    echo "error: cargo-xwin ${required_xwin_version} is required; found ${actual_xwin_version:-unknown}" >&2
    exit 1
fi

echo "building all LVOS targets for ${target} with cargo-xwin ${required_xwin_version}"
cargo-xwin build \
    --workspace \
    --all-targets \
    --all-features \
    --locked \
    --target "${target}"

echo "Windows x86_64 MSVC cross-compilation passed"
