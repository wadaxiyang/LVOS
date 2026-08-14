#!/usr/bin/env python3
"""Fail closed when a release tag disagrees with the Cargo workspace version."""

from __future__ import annotations

from pathlib import Path
import re
import sys

if __package__:
    from scripts.workspace_version import workspace_version
else:
    from workspace_version import workspace_version


TAG_PATTERN = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")


def check_release_tag(tag: str, version: str) -> None:
    match = TAG_PATTERN.fullmatch(tag)
    if match is None:
        raise ValueError(f"release tag must be plain v<SemVer>: {tag!r}")
    if tag[1:] != version:
        raise ValueError(f"release tag {tag!r} does not match workspace version {version!r}")


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: check_release_tag.py v<SemVer>", file=sys.stderr)
        return 2
    try:
        version = workspace_version(Path("Cargo.toml"))
        check_release_tag(sys.argv[1], version)
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"release tag {sys.argv[1]} matches workspace version {version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
