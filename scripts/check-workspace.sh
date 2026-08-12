#!/usr/bin/env bash
set -euo pipefail

# Keep a convenient local shell entry point. CI invokes the Python checker
# directly so Windows line-ending behavior cannot affect shell comparisons.
exec python3 scripts/check_workspace.py
