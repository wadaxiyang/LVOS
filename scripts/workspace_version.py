#!/usr/bin/env python3
"""Print the authoritative Cargo workspace package version."""

from __future__ import annotations

from pathlib import Path
import sys
import tomllib


def workspace_version(path: Path = Path("Cargo.toml")) -> str:
    document = tomllib.loads(path.read_text(encoding="utf-8"))
    version = document.get("workspace", {}).get("package", {}).get("version")
    if not isinstance(version, str) or not version:
        raise ValueError("Cargo.toml has no workspace.package.version")
    return version


def main() -> int:
    try:
        print(workspace_version())
    except (OSError, ValueError, tomllib.TOMLDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
