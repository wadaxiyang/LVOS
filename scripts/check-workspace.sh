#!/usr/bin/env bash
set -euo pipefail

metadata="$(cargo metadata --format-version 1 --no-deps)"

expected=(
  lvos
  lvos-auth
  lvos-core
  lvos-platform
  lvos-server
  lvos-storage
  lvos-sync
  lvos-translation
)

actual="$(printf '%s' "$metadata" | python3 -c '
import json, sys
data = json.load(sys.stdin)
for package in sorted(data["packages"], key=lambda item: item["name"]):
    print(package["name"])
')"

if [[ "$actual" != "$(printf '%s\n' "${expected[@]}")" ]]; then
  echo "workspace package names do not match Project Identity" >&2
  printf 'expected:\n%s\nactual:\n%s\n' "$(printf '%s\n' "${expected[@]}")" "$actual" >&2
  exit 1
fi

printf '%s' "$metadata" | python3 -c '
import json, sys
data = json.load(sys.stdin)
for package in data["packages"]:
    assert package["version"] == "0.1.0", package
    assert package["license"] is None, package
    license_file = package["license_file"].replace("\\", "/")
    assert license_file.endswith("/LICENSE"), package
' 

test -s LICENSE
test -s .env.example

python3 - <<'PY'
from pathlib import Path

keys = []
for line in Path(".env.example").read_text().splitlines():
    line = line.strip()
    if not line or line.startswith("#"):
        continue
    key, separator, _value = line.partition("=")
    assert separator, f"invalid .env.example line: {line}"
    assert key.startswith("LVOS_"), f"unprefixed LVOS variable: {key}"
    assert key not in keys, f"duplicate environment variable: {key}"
    keys.append(key)

required = {
    "LVOS_APP_ENV",
    "LVOS_BIND_ADDR",
    "LVOS_DATABASE_URL",
    "LVOS_DEFAULT_PASSWORD",
    "LVOS_ACCESS_TOKEN_TTL_MINUTES",
    "LVOS_REFRESH_SESSION_IDLE_TTL_DAYS",
}
assert required <= set(keys), f"missing environment variables: {required - set(keys)}"
PY

echo "workspace checks passed"
