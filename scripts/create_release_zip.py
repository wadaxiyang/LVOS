#!/usr/bin/env python3
"""Create a deterministic ZIP around one LVOS release file or directory."""

from __future__ import annotations

import argparse
from pathlib import Path
import stat
import sys
import zipfile


FIXED_TIME = (2026, 1, 1, 0, 0, 0)
ROOT = Path(__file__).resolve().parent.parent
NOTICE_FILES = {
    "NOTICES/LVOS-LICENSE.txt": "LICENSE",
    "NOTICES/THIRD_PARTY_NOTICES.md": "THIRD_PARTY_NOTICES.md",
    "NOTICES/Quadrant-Kit-GPL-3.0.txt": "licenses/Quadrant-Kit-GPL-3.0.txt",
    "NOTICES/Quadrant-Kit-NOTICES.md": "licenses/Quadrant-Kit-NOTICES.md",
    "NOTICES/Fluent-System-Icons-MIT.txt": "licenses/Fluent-System-Icons-MIT.txt",
}


def _entry(archive_name: str, mode: int, *, directory: bool = False) -> zipfile.ZipInfo:
    name = archive_name.rstrip("/") + ("/" if directory else "")
    info = zipfile.ZipInfo(name, FIXED_TIME)
    info.create_system = 3
    file_type = stat.S_IFDIR if directory else stat.S_IFREG
    info.external_attr = (file_type | mode) << 16
    info.compress_type = zipfile.ZIP_DEFLATED
    return info


def create_release_zip(
    source: Path,
    output: Path,
    archive_name: str,
    extra_files: dict[str, Path] | None = None,
) -> None:
    if not source.exists() or not archive_name or "/" in archive_name or "\\" in archive_name:
        raise ValueError("invalid release ZIP source or archive name")
    extra_files = extra_files or {}
    if any(
        not path.is_file() or not name or "/" in name or "\\" in name
        for name, path in extra_files.items()
    ) or archive_name in extra_files:
        raise ValueError("invalid release ZIP extra file")
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(output.suffix + ".tmp")
    if temporary.exists():
        temporary.unlink()
    try:
        with zipfile.ZipFile(temporary, "w", strict_timestamps=True) as archive:
            for name, relative in sorted(NOTICE_FILES.items()):
                content = (ROOT / relative).read_bytes()
                if not content:
                    raise ValueError(f"empty distribution notice: {relative}")
                archive.writestr(_entry(name, 0o644), content)
            if source.is_file():
                mode = 0o755 if source.stat().st_mode & stat.S_IXUSR else 0o644
                archive.writestr(_entry(archive_name, mode), source.read_bytes())
            else:
                archive.writestr(_entry(archive_name, 0o755, directory=True), b"")
                for path in sorted(source.rglob("*")):
                    if path.is_symlink():
                        raise ValueError("release ZIP does not accept symbolic links")
                    relative = path.relative_to(source).as_posix()
                    name = f"{archive_name}/{relative}"
                    if path.is_dir():
                        archive.writestr(_entry(name, 0o755, directory=True), b"")
                    elif path.is_file():
                        mode = 0o755 if path.stat().st_mode & stat.S_IXUSR else 0o644
                        archive.writestr(_entry(name, mode), path.read_bytes())
            for name, path in sorted(extra_files.items()):
                mode = 0o755 if path.stat().st_mode & stat.S_IXUSR else 0o644
                archive.writestr(_entry(name, mode), path.read_bytes())
        temporary.replace(output)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--archive-name", required=True)
    parser.add_argument(
        "--extra",
        action="append",
        default=[],
        metavar="ARCHIVE_NAME=SOURCE",
        help="add a sibling file to the ZIP; may be repeated",
    )
    arguments = parser.parse_args()
    try:
        extras: dict[str, Path] = {}
        for value in arguments.extra:
            name, separator, source = value.partition("=")
            if not separator or name in extras:
                raise ValueError("--extra must be unique ARCHIVE_NAME=SOURCE pairs")
            extras[name] = Path(source)
        create_release_zip(
            arguments.source,
            arguments.output,
            arguments.archive_name,
            extras,
        )
    except (OSError, ValueError, zipfile.BadZipFile) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(arguments.output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
