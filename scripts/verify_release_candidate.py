#!/usr/bin/env python3
"""Fail-closed verifier for an LVOS V1 unsigned release-candidate directory."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import plistlib
import re
import struct
import sys
import zipfile


RELEASE_ROOT = "https://github.com/wadaxiyang/LVOS/releases"
SHA256 = re.compile(r"[0-9a-f]{64}")
FIXED_ZIP_TIME = (2026, 1, 1, 0, 0, 0)
MAX_ARTIFACT_BYTES = 536_870_912
MAX_ARCHIVE_CONTENT_BYTES = 536_870_912
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)")


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


def verify_macos(path: Path, version: str) -> None:
    with zipfile.ZipFile(path) as archive:
        entries = safe_entries(archive)
        names = {entry.filename for entry in entries}
        plist_name = "LVOS.app/Contents/Info.plist"
        binary_name = "LVOS.app/Contents/MacOS/LVOS"
        if plist_name not in names or binary_name not in names:
            raise ValueError("macOS archive lacks the LVOS bundle identity or executable")
        metadata = plistlib.loads(archive.read(plist_name))
        expected = {
            "CFBundleDisplayName": "LVOS",
            "CFBundleExecutable": "LVOS",
            "CFBundleIdentifier": "site.niuniu770.lvos",
            "CFBundleShortVersionString": version,
            "LSMinimumSystemVersion": "15.0",
        }
        if any(metadata.get(key) != value for key, value in expected.items()):
            raise ValueError("macOS Info.plist disagrees with frozen release identity")
        binary = archive.read(binary_name)
        if len(binary) < 8 or binary[:4] != b"\xcf\xfa\xed\xfe":
            raise ValueError("macOS executable is not a 64-bit little-endian Mach-O")
        if struct.unpack_from("<I", binary, 4)[0] != 0x0100000C:
            raise ValueError("macOS executable is not arm64")


def verify_windows(path: Path) -> None:
    with zipfile.ZipFile(path) as archive:
        entries = safe_entries(archive)
        files = [entry for entry in entries if not entry.is_dir()]
        if [entry.filename for entry in files] != ["LVOS.exe"]:
            raise ValueError("Windows archive must contain exactly LVOS.exe")
        binary = archive.read("LVOS.exe")
    if len(binary) < 0x100 or binary[:2] != b"MZ":
        raise ValueError("Windows executable lacks an MZ header")
    pe_offset = struct.unpack_from("<I", binary, 0x3C)[0]
    if pe_offset + 96 > len(binary) or binary[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise ValueError("Windows executable lacks a valid PE header")
    if struct.unpack_from("<H", binary, pe_offset + 4)[0] != 0x8664:
        raise ValueError("Windows executable is not x86_64")
    optional = pe_offset + 24
    if struct.unpack_from("<H", binary, optional)[0] != 0x20B:
        raise ValueError("Windows executable is not PE32+")
    if struct.unpack_from("<H", binary, optional + 68)[0] != 2:
        raise ValueError("Windows executable is not a GUI subsystem binary")


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
    mac_name = f"LVOS-{version}-macos-arm64.zip"
    windows_name = f"LVOS-{version}-windows-x86_64.zip"
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
    verify_macos(directory / mac_name, version)
    verify_windows(directory / windows_name)
    manifest = json.loads((directory / manifest_name).read_text(encoding="utf-8"))
    if set(manifest) != {
        "manifest_version", "product", "channel", "version", "release_page", "artifacts"
    }:
        raise ValueError("update manifest top-level schema is not exact")
    if manifest != {
        "manifest_version": 1,
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
