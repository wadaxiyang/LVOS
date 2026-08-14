#!/usr/bin/env python3
"""Cross-platform regression tests for CI policy helpers."""

from __future__ import annotations

import unittest
from pathlib import Path
import tempfile
import zipfile

from scripts.create_release_zip import create_release_zip
from scripts.generate_update_manifest import build_manifest

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


if __name__ == "__main__":
    unittest.main()
