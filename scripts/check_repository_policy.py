#!/usr/bin/env python3
"""Validate repository policy using only Python and Git."""

from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import sys


SOURCE_SUFFIXES = {".rs", ".toml", ".yml", ".yaml", ".sh", ".py"}
FORBIDDEN_CARGO_TERMS = ("electron", "tauri", "webview")
SECRET_PATTERNS = (
    re.compile(rb"BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY"),
    re.compile(rb"Bearer [A-Za-z0-9._-]{20,}"),
    re.compile(rb"AIza[0-9A-Za-z_-]{30,}"),
)


def git(*arguments: str) -> bytes:
    """Run Git and return stdout, failing with Git's original diagnostic."""
    return subprocess.run(
        ["git", *arguments],
        check=True,
        stdout=subprocess.PIPE,
    ).stdout


def repository_files() -> list[Path]:
    """Return tracked and non-ignored untracked files, preserving unusual names."""
    output = git("ls-files", "-co", "--exclude-standard", "-z")
    return [Path(os.fsdecode(name)) for name in output.split(b"\0") if name]


def check_internal_documents_untracked() -> None:
    tracked = git("ls-files", "-z", "--", "AGENTS.md", "LVOS_development_spec.md", "docs")
    paths = [os.fsdecode(name) for name in tracked.split(b"\0") if name]
    if paths:
        joined = "\n".join(paths)
        raise SystemExit(f"internal documentation is tracked:\n{joined}")


def attributes(path: Path) -> dict[str, str]:
    output = git("check-attr", "-z", "text", "eol", "--", os.fspath(path))
    fields = [os.fsdecode(field) for field in output.split(b"\0") if field]
    if len(fields) % 3:
        raise SystemExit(f"unexpected git check-attr output for {path}")
    return {fields[index + 1]: fields[index + 2] for index in range(0, len(fields), 3)}


def check_source_newlines(files: list[Path]) -> None:
    for path in files:
        if path.suffix.lower() not in SOURCE_SUFFIXES:
            continue
        values = attributes(path)
        if values.get("text") != "auto" or values.get("eol") != "lf":
            raise SystemExit(
                f"source file does not enforce LF checkout: {path} "
                f"(text={values.get('text')}, eol={values.get('eol')})"
            )
        if b"\r\n" in path.read_bytes():
            raise SystemExit(f"source file contains CRLF bytes: {path}")


def check_forbidden_dependencies(files: list[Path]) -> None:
    for path in files:
        if path.name != "Cargo.toml":
            continue
        contents = path.read_text(encoding="utf-8").lower()
        for forbidden in FORBIDDEN_CARGO_TERMS:
            if forbidden in contents:
                raise SystemExit(f"forbidden desktop dependency found: {forbidden} in {path}")


def check_secrets(files: list[Path]) -> None:
    for path in files:
        try:
            contents = path.read_bytes()
        except OSError as error:
            raise SystemExit(f"could not inspect {path}: {error}") from error
        if b"\0" in contents:
            continue
        for pattern in SECRET_PATTERNS:
            if pattern.search(contents):
                raise SystemExit(f"possible credential material found in {path}")


def main() -> int:
    files = repository_files()
    check_internal_documents_untracked()
    check_source_newlines(files)
    check_forbidden_dependencies(files)
    check_secrets(files)
    print("repository policy checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
