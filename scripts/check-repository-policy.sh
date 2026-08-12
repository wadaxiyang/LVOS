#!/usr/bin/env bash
set -euo pipefail

# Keep the CI entry point shell-compatible on macOS and Windows Git Bash, while
# implementing all repository inspection with the guaranteed Python + Git tools.
exec python3 scripts/check_repository_policy.py
