#!/usr/bin/env python3
"""Generate the strict LVOS GitHub Release update manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys


VERSION = re.compile(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
)
CHANNEL = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*")
RELEASE_ROOT = "https://github.com/wadaxiyang/LVOS/releases"


def artifact(path: Path, version: str, platform: str, architecture: str) -> dict[str, object]:
    expected = f"LVOS-{version}-{platform}-{architecture}.zip"
    if path.name != expected or not path.is_file():
        raise ValueError(f"expected release artifact {expected}")
    contents = path.read_bytes()
    if not contents:
        raise ValueError(f"release artifact is empty: {path}")
    return {
        "platform": platform,
        "architecture": architecture,
        "name": expected,
        "size_bytes": len(contents),
        "sha256": hashlib.sha256(contents).hexdigest(),
        "download_url": f"{RELEASE_ROOT}/download/v{version}/{expected}",
    }


def build_manifest(
    version: str,
    channel: str,
    macos: Path,
    windows: Path,
) -> dict[str, object]:
    if VERSION.fullmatch(version) is None:
        raise ValueError("release version must be plain SemVer")
    if CHANNEL.fullmatch(channel) is None or channel != "stable":
        raise ValueError("V1 supports only the stable update channel")
    return {
        "manifest_version": 1,
        "product": "LVOS",
        "channel": channel,
        "version": version,
        "release_page": f"{RELEASE_ROOT}/tag/v{version}",
        "artifacts": [
            artifact(macos, version, "macos", "arm64"),
            artifact(windows, version, "windows", "x86_64"),
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--channel", default="stable")
    parser.add_argument("--macos", required=True, type=Path)
    parser.add_argument("--windows", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    arguments = parser.parse_args()
    try:
        manifest = build_manifest(
            arguments.version,
            arguments.channel,
            arguments.macos,
            arguments.windows,
        )
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(
            json.dumps(manifest, indent=2, ensure_ascii=True) + "\n",
            encoding="utf-8",
            newline="\n",
        )
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(arguments.output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
