#!/usr/bin/env bash
set -euo pipefail

while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  attributes="$(git check-attr text eol -- "$path")"
  if ! grep -Fq ": text: auto" <<<"$attributes" || ! grep -Fq ": eol: lf" <<<"$attributes"; then
    echo "source file does not enforce LF checkout: $path" >&2
    echo "$attributes" >&2
    exit 1
  fi
done < <(rg --files -g '*.rs' -g '*.toml' -g '*.yml' -g '*.yaml' -g '*.sh')

python3 - <<'PY'
from pathlib import Path
import subprocess

paths = subprocess.run(
    ["rg", "--files", "-g", "*.rs", "-g", "*.toml", "-g", "*.yml", "-g", "*.yaml", "-g", "*.sh"],
    check=True,
    capture_output=True,
    text=True,
).stdout.splitlines()

for name in paths:
    if b"\r\n" in Path(name).read_bytes():
        raise SystemExit(f"source file contains CRLF bytes: {name}")
PY

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
