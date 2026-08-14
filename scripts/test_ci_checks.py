#!/usr/bin/env python3
"""Cross-platform regression tests for CI policy helpers."""

from __future__ import annotations

import json
from pathlib import Path
import plistlib
import struct
import tempfile
import unittest
import zipfile

from scripts.create_release_zip import create_release_zip
from scripts.generate_update_manifest import MAX_ARTIFACT_BYTES, build_manifest
from scripts.generate_release_checksums import write_checksums
from scripts.verify_release_candidate import verify_candidate

from scripts.check_workspace import (
    EXPECTED_PACKAGES,
    check_packages,
    parse_environment_example,
    parse_metadata,
)


class WorkspaceCheckTests(unittest.TestCase):
    def metadata(self, *, license_path: str) -> dict[str, object]:
        return {
            "packages": [
                {
                    "name": name,
                    "version": "0.1.0",
                    "license": None,
                    "license_file": license_path,
                }
                for name in reversed(EXPECTED_PACKAGES)
            ]
        }

    def test_parses_crlf_metadata(self) -> None:
        raw = '{\r\n  "packages": []\r\n}\r\n'
        self.assertEqual(parse_metadata(raw), {"packages": []})

    def test_accepts_windows_license_path(self) -> None:
        check_packages(self.metadata(license_path=r"D:\a\LVOS\LVOS\LICENSE"))

    def test_accepts_posix_license_path(self) -> None:
        check_packages(self.metadata(license_path="/home/runner/work/LVOS/LICENSE"))

    def test_parses_crlf_environment_example(self) -> None:
        contents = "LVOS_APP_ENV=development\r\nLVOS_BIND_ADDR=0.0.0.0:7770\r\n"
        self.assertEqual(
            parse_environment_example(contents),
            ["LVOS_APP_ENV", "LVOS_BIND_ADDR"],
        )

    def test_tag_workflow_publishes_without_repeating_ci(self) -> None:
        release = (Path(__file__).parent.parent / ".github/workflows/release.yml").read_text(
            encoding="utf-8"
        )
        continuous = (Path(__file__).parent.parent / ".github/workflows/ci.yml").read_text(
            encoding="utf-8"
        )
        self.assertNotIn("  quality:\n", release)
        self.assertNotIn("--draft", release)
        self.assertIn("--latest", release)
        self.assertIn("branches:", continuous)

    def test_release_zip_and_manifest_are_deterministic_and_hashed(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            mac_source = directory / "LVOS.app"
            mac_source.mkdir()
            (mac_source / "LVOS").write_bytes(b"macos")
            windows_source = directory / "LVOS.exe"
            windows_source.write_bytes(b"windows")
            mac_archive = directory / "LVOS-0.1.0-macos-arm64.zip"
            windows_archive = directory / "LVOS-0.1.0-windows-x86_64.zip"
            create_release_zip(mac_source, mac_archive, "LVOS.app")
            first = mac_archive.read_bytes()
            create_release_zip(mac_source, mac_archive, "LVOS.app")
            self.assertEqual(first, mac_archive.read_bytes())
            create_release_zip(windows_source, windows_archive, "LVOS.exe")
            with zipfile.ZipFile(windows_archive) as archive:
                self.assertEqual(archive.namelist(), ["LVOS.exe"])
            manifest = build_manifest(
                "0.1.0", "stable", mac_archive, windows_archive
            )
            self.assertEqual(manifest["manifest_version"], 1)
            self.assertEqual(len(manifest["artifacts"]), 2)
            self.assertEqual(len(manifest["artifacts"][0]["sha256"]), 64)
            with mac_archive.open("wb") as oversized:
                oversized.truncate(MAX_ARTIFACT_BYTES + 1)
            with self.assertRaises(ValueError):
                build_manifest("0.1.0", "stable", mac_archive, windows_archive)

    def test_release_candidate_verifier_checks_both_native_identities(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            bundle = directory / "bundle" / "LVOS.app"
            (bundle / "Contents" / "MacOS").mkdir(parents=True)
            (bundle / "Contents" / "Info.plist").write_bytes(
                plistlib.dumps(
                    {
                        "CFBundleDisplayName": "LVOS",
                        "CFBundleExecutable": "LVOS",
                        "CFBundleIdentifier": "site.niuniu770.lvos",
                        "CFBundleShortVersionString": "0.1.0",
                        "LSMinimumSystemVersion": "15.0",
                    }
                )
            )
            (bundle / "Contents" / "MacOS" / "LVOS").write_bytes(
                b"\xcf\xfa\xed\xfe" + struct.pack("<I", 0x0100000C)
            )
            windows = bytearray(256)
            windows[:2] = b"MZ"
            struct.pack_into("<I", windows, 0x3C, 0x80)
            windows[0x80:0x84] = b"PE\0\0"
            struct.pack_into("<H", windows, 0x84, 0x8664)
            struct.pack_into("<H", windows, 0x98, 0x20B)
            struct.pack_into("<H", windows, 0x98 + 68, 2)
            executable = directory / "LVOS.exe"
            executable.write_bytes(windows)
            mac_archive = directory / "LVOS-0.1.0-macos-arm64.zip"
            windows_archive = directory / "LVOS-0.1.0-windows-x86_64.zip"
            create_release_zip(bundle, mac_archive, "LVOS.app")
            create_release_zip(executable, windows_archive, "LVOS.exe")
            manifest = build_manifest(
                "0.1.0", "stable", mac_archive, windows_archive
            )
            manifest_path = directory / "lvos-update-stable.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            checksum_path = directory / "SHA256SUMS"
            write_checksums(
                [mac_archive, windows_archive, manifest_path], checksum_path
            )
            verify_candidate(directory, "0.1.0")
            checksum_path.write_text("0" * 64 + "  bad.zip\n", encoding="ascii")
            with self.assertRaises(ValueError):
                verify_candidate(directory, "0.1.0")


if __name__ == "__main__":
    unittest.main()
