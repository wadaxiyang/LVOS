#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
python3 -m unittest scripts/test_ci_checks.py
python3 scripts/check_repository_policy.py
python3 scripts/check_workspace.py
./scripts/check-windows-cross.sh

echo "local pre-commit checks passed"
