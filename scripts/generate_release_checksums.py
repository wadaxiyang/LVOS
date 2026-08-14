#!/usr/bin/env python3
"""Write the canonical LVOS release checksum set."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import sys


def write_checksums(files: list[Path], output: Path) -> None:
    if not files or len({path.name for path in files}) != len(files):
        raise ValueError("release checksum inputs must have unique basenames")
    lines: list[str] = []
    for path in files:
        if not path.is_file() or path.stat().st_size == 0:
            raise ValueError(f"release checksum input is missing or empty: {path}")
        digest = hashlib.sha256()
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        lines.append(f"{digest.hexdigest()}  {path.name}\n")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text("".join(lines), encoding="ascii", newline="\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("files", nargs="+", type=Path)
    arguments = parser.parse_args()
    try:
        write_checksums(arguments.files, arguments.output)
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(arguments.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
