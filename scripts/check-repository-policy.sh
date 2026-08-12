#!/usr/bin/env bash
set -euo pipefail

if git ls-files -- AGENTS.md LVOS_development_spec.md docs | grep -q .; then
  echo "internal documentation is tracked" >&2
  git ls-files -- AGENTS.md LVOS_development_spec.md docs >&2
  exit 1
fi

for forbidden in electron tauri webview; do
  if rg -i --glob 'Cargo.toml' "$forbidden" . >/dev/null; then
    echo "forbidden desktop dependency found: $forbidden" >&2
    exit 1
  fi
done

if rg --hidden --glob '!target/**' --glob '!.git/**' \
  '(BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY|Bearer [A-Za-z0-9._-]{20,}|AIza[0-9A-Za-z_-]{30,})' . >/dev/null; then
  echo "possible credential material found" >&2
  exit 1
fi

echo "repository policy checks passed"
