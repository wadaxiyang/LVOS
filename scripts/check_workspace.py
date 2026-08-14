#!/usr/bin/env python3
"""Validate Cargo workspace identity using structured data only."""

from __future__ import annotations

import json
from pathlib import Path, PurePath
import subprocess
import sys


EXPECTED_PACKAGES = [
    "lvos",
    "lvos-auth",
    "lvos-core",
    "lvos-platform",
    "lvos-server",
    "lvos-storage",
    "lvos-sync",
    "lvos-translation",
]

REQUIRED_ENVIRONMENT_KEYS = {
    "LVOS_APP_ENV",
    "LVOS_BIND_ADDR",
    "LVOS_DATABASE_URL",
    "LVOS_DEFAULT_PASSWORD",
    "LVOS_ACCESS_TOKEN_TTL_MINUTES",
    "LVOS_REFRESH_SESSION_IDLE_TTL_DAYS",
    "LVOS_LOGIN_RATE_LIMIT_MAX_FAILURES",
    "LVOS_LOGIN_RATE_LIMIT_WINDOW_SECONDS",
    "LVOS_BACKUP_INTERVAL_HOURS",
    "LVOS_UPDATE_CHANNEL",
    "LVOS_DOCKER_RUST_IMAGE",
    "LVOS_DOCKER_RUNTIME_IMAGE",
    "LVOS_CARGO_REGISTRY_INDEX",
}


def parse_metadata(raw_metadata: str) -> dict[str, object]:
    """Parse Cargo JSON regardless of the host's text newline convention."""
    return json.loads(raw_metadata)


def cargo_metadata() -> dict[str, object]:
    completed = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    return parse_metadata(completed.stdout)


def check_packages(metadata: dict[str, object]) -> None:
    packages = metadata.get("packages")
    if not isinstance(packages, list):
        raise SystemExit("cargo metadata did not contain a package list")

    names = sorted(package["name"] for package in packages)
    if names != EXPECTED_PACKAGES:
        expected = "\n".join(EXPECTED_PACKAGES)
        actual = "\n".join(names)
        raise SystemExit(
            "workspace package names do not match Project Identity\n"
            f"expected:\n{expected}\nactual:\n{actual}"
        )

    for package in packages:
        if package.get("version") != "0.1.0":
            raise SystemExit(f"workspace package has incorrect version: {package!r}")
        if package.get("license") is not None:
            raise SystemExit(f"workspace package must use license-file: {package!r}")

        license_file = package.get("license_file")
        if not isinstance(license_file, str) or PurePath(license_file.replace("\\", "/")).name != "LICENSE":
            raise SystemExit(f"workspace package has incorrect license file: {package!r}")


def check_required_files() -> None:
    for name in (
        "LICENSE",
        ".env.example",
        ".dockerignore",
        "Cargo.server.lock",
        "Cargo.server.toml",
        "DEPLOYMENT.md",
        "Dockerfile",
        "compose.yaml",
    ):
        path = Path(name)
        if not path.is_file() or path.stat().st_size == 0:
            raise SystemExit(f"required file is missing or empty: {name}")


def parse_environment_example(contents: str) -> list[str]:
    keys: list[str] = []
    for raw_line in contents.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        key, separator, _value = line.partition("=")
        if not separator:
            raise SystemExit(f"invalid .env.example line: {line}")
        if not key.startswith("LVOS_"):
            raise SystemExit(f"unprefixed LVOS variable: {key}")
        if key in keys:
            raise SystemExit(f"duplicate environment variable: {key}")
        keys.append(key)

    return keys


def check_environment_example() -> None:
    keys = parse_environment_example(Path(".env.example").read_text(encoding="utf-8"))
    missing = REQUIRED_ENVIRONMENT_KEYS - set(keys)
    if missing:
        raise SystemExit(f"missing environment variables: {sorted(missing)}")


def main() -> int:
    check_packages(cargo_metadata())
    check_required_files()
    check_environment_example()
    print("workspace checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
