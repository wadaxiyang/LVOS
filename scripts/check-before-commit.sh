#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
python3 -B -m unittest discover -s scripts -p "test_*.py"
python3 scripts/check_repository_policy.py
python3 scripts/check_renderer_policy.py
python3 scripts/check_workspace.py
python3 scripts/check_kit_adoption.py
python3 scripts/check_server_only.py
python3 scripts/check_stage14_security.py
python3 scripts/check_stage15_scope.py
./scripts/check-windows-cross.sh

echo "local pre-commit checks passed"
