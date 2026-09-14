#!/usr/bin/env python3
"""Fail-closed verifier for an LVOS V1 unsigned release-candidate directory."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import struct
import sys
import zipfile

if __package__:
    from .create_release_zip import NOTICE_FILES, ROOT
else:
    from create_release_zip import NOTICE_FILES, ROOT


RELEASE_ROOT = "https://github.com/wadaxiyang/LVOS/releases"
SHA256 = re.compile(r"[0-9a-f]{64}")
FIXED_ZIP_TIME = (2026, 1, 1, 0, 0, 0)
MAX_ARTIFACT_BYTES = 536_870_912
MAX_ARCHIVE_CONTENT_BYTES = 536_870_912
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)")


def verify_notices(archive: zipfile.ZipFile) -> None:
    for name, relative in NOTICE_FILES.items():
        if name not in archive.namelist() or archive.read(name) != (ROOT / relative).read_bytes():
            raise ValueError(f"missing or modified distribution notice: {name}")


def digest(path: Path) -> str:
    result = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def safe_entries(archive: zipfile.ZipFile) -> list[zipfile.ZipInfo]:
    entries = archive.infolist()
    if not entries:
        raise ValueError("release ZIP is empty")
    if sum(entry.file_size for entry in entries) > MAX_ARCHIVE_CONTENT_BYTES:
        raise ValueError("release ZIP expands beyond the configured safety limit")
    names: set[str] = set()
    for entry in entries:
        path = PurePosixPath(entry.filename)
        if (
            entry.filename in names
            or path.is_absolute()
            or ".." in path.parts
            or "\\" in entry.filename
            or entry.date_time != FIXED_ZIP_TIME
            or (entry.external_attr >> 16) & 0o170000 == 0o120000
        ):
            raise ValueError(f"unsafe or non-deterministic ZIP entry: {entry.filename}")
        names.add(entry.filename)
    return entries


def verify_macos_dmg(path: Path) -> None:
    if path.stat().st_size < 512:
        raise ValueError("macOS disk image is too small")
    with path.open("rb") as image:
        image.seek(-512, 2)
        if image.read(4) != b"koly":
            raise ValueError("macOS artifact lacks an UDIF koly trailer")


def verify_pe_gui(path: Path, *, require_x86_64: bool) -> None:
    binary = path.read_bytes()
    if len(binary) < 0x100 or binary[:2] != b"MZ":
        raise ValueError(f"Windows executable lacks an MZ header: {path.name}")
    pe_offset = struct.unpack_from("<I", binary, 0x3C)[0]
    if pe_offset + 96 > len(binary) or binary[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise ValueError(f"Windows executable lacks a valid PE header: {path.name}")
    machine = struct.unpack_from("<H", binary, pe_offset + 4)[0]
    if require_x86_64 and machine != 0x8664:
        raise ValueError(f"Windows executable is not x86_64: {path.name}")
    if not require_x86_64 and machine not in {0x014C, 0x8664}:
        raise ValueError(f"Windows installer has an unsupported bootstrap architecture: {path.name}")
    optional = pe_offset + 24
    expected_magic = 0x20B if machine == 0x8664 else 0x10B
    if struct.unpack_from("<H", binary, optional)[0] != expected_magic:
        raise ValueError(f"Windows executable optional header is invalid: {path.name}")
    if struct.unpack_from("<H", binary, optional + 68)[0] != 2:
        raise ValueError(f"Windows executable is not a GUI subsystem binary: {path.name}")


def verify_windows_installer(path: Path) -> None:
    verify_pe_gui(path, require_x86_64=False)


def verify_checksums(directory: Path, expected_names: list[str]) -> None:
    checksum_path = directory / "SHA256SUMS"
    rows: dict[str, str] = {}
    for line in checksum_path.read_text(encoding="ascii").splitlines():
        parts = line.split("  ", 1)
        if len(parts) != 2 or SHA256.fullmatch(parts[0]) is None or parts[1] in rows:
            raise ValueError("SHA256SUMS has an invalid or duplicate row")
        rows[parts[1]] = parts[0]
    if list(rows) != expected_names:
        raise ValueError("SHA256SUMS order or file set is not canonical")
    for name, expected in rows.items():
        if digest(directory / name) != expected:
            raise ValueError(f"checksum mismatch: {name}")


def verify_candidate(directory: Path, version: str) -> None:
    if VERSION.fullmatch(version) is None:
        raise ValueError("release version must be plain SemVer")
    mac_name = f"LVOS-{version}-macos-arm64.dmg"
    windows_name = f"LVOS-{version}-windows-x86_64-setup.exe"
    manifest_name = "lvos-update-stable.json"
    expected_names = [mac_name, windows_name, manifest_name]
    for name in [*expected_names, "SHA256SUMS"]:
        path = directory / name
        if (
            not path.is_file()
            or path.stat().st_size == 0
            or path.stat().st_size > MAX_ARTIFACT_BYTES
        ):
            raise ValueError(f"release candidate file is missing or empty: {name}")
    verify_macos_dmg(directory / mac_name)
    verify_windows_installer(directory / windows_name)
    manifest = json.loads((directory / manifest_name).read_text(encoding="utf-8"))
    if set(manifest) != {
        "manifest_version", "product", "channel", "version", "release_page", "artifacts"
    }:
        raise ValueError("update manifest top-level schema is not exact")
    if manifest != {
        "manifest_version": 2,
        "product": "LVOS",
        "channel": "stable",
        "version": version,
        "release_page": f"{RELEASE_ROOT}/tag/v{version}",
        "artifacts": manifest["artifacts"],
    }:
        raise ValueError("update manifest identity is invalid")
    expected_artifacts = [
        ("macos", "arm64", mac_name),
        ("windows", "x86_64", windows_name),
    ]
    if not isinstance(manifest["artifacts"], list) or len(manifest["artifacts"]) != 2:
        raise ValueError("update manifest must contain both target artifacts")
    for artifact, (platform, architecture, name) in zip(
        manifest["artifacts"], expected_artifacts, strict=True
    ):
        path = directory / name
        expected = {
            "platform": platform,
            "architecture": architecture,
            "name": name,
            "size_bytes": path.stat().st_size,
            "sha256": digest(path),
            "download_url": f"{RELEASE_ROOT}/download/v{version}/{name}",
        }
        if artifact != expected:
            raise ValueError(f"update manifest artifact mismatch: {name}")
    verify_checksums(directory, expected_names)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--directory", required=True, type=Path)
    parser.add_argument("--version", required=True)
    arguments = parser.parse_args()
    try:
        verify_candidate(arguments.directory, arguments.version)
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError, zipfile.BadZipFile) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"verified LVOS {arguments.version} release candidate: {arguments.directory}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
