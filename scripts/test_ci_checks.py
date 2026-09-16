#!/usr/bin/env python3
"""Cross-platform regression tests for CI policy helpers."""

from __future__ import annotations

import json
from pathlib import Path
import struct
import tempfile
import unittest
import zipfile

from scripts.create_release_zip import NOTICE_FILES, create_release_zip
from scripts.check_release_tag import check_release_tag
from scripts.generate_update_manifest import MAX_ARTIFACT_BYTES, build_manifest
from scripts.generate_release_checksums import write_checksums
from scripts.verify_release_candidate import verify_candidate, verify_notices
from scripts.check_stage14_security import REQUIRED_CALLBACKS, ui_contract_failures

from scripts.check_workspace import (
    EXPECTED_PACKAGES,
    check_packages,
    parse_environment_example,
    parse_metadata,
)


class UiContractCheckTests(unittest.TestCase):
    ENTRY = 'export { MainWindow } from "windows/main_window.slint";'

    def window(self, callbacks: set[str]) -> str:
        return "export component MainWindow inherits Window {\n" + "\n".join(
            f"    callback {name};" for name in sorted(callbacks)
        ) + "\n}"

    def test_accepts_exported_window_contract(self) -> None:
        self.assertEqual(ui_contract_failures(self.ENTRY, self.window(REQUIRED_CALLBACKS)), [])

    def test_child_callback_cannot_replace_window_callback(self) -> None:
        entry_with_child = self.ENTRY + "\ncomponent Child {\n callback login-requested;\n}"
        failures = ui_contract_failures(
            entry_with_child, self.window(REQUIRED_CALLBACKS - {"login-requested"})
        )
        self.assertEqual(failures, ["missing required UI callbacks: ['login-requested']"])

    def test_rejects_disconnected_window(self) -> None:
        failures = ui_contract_failures(
            'export { MainWindow } from "other.slint";', self.window(REQUIRED_CALLBACKS)
        )
        self.assertEqual(failures, ["app.slint does not export the audited MainWindow"])


class WorkspaceCheckTests(unittest.TestCase):
    def metadata(self, *, license_path: str) -> dict[str, object]:
        return {
            "packages": [
                {
                    "name": name,
                    "version": "0.1.9",
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
        self.assertIn("  pull_request:\n", continuous)
        self.assertIn("  push:\n", continuous)
        self.assertNotIn("actions/checkout@v4", release + continuous)
        self.assertIn("actions/checkout@v7", release + continuous)
        self.assertIn("actions/upload-artifact@v7", release)
        self.assertIn("actions/download-artifact@v8", release)
        self.assertIn("macos-arm64.dmg", release)
        self.assertIn("windows-x86_64-setup.exe", release)
        self.assertNotIn("LVOS-*.zip", release)

    def test_windows_installer_keeps_directory_and_shortcut_choices_visible(self) -> None:
        installer = (
            Path(__file__).parent.parent / "packaging/windows/LVOS.iss"
        ).read_text(encoding="utf-8")
        self.assertIn("DisableDirPage=no", installer)
        self.assertIn("DisableProgramGroupPage=no", installer)
        self.assertIn('Name: "desktopicon"', installer)
        self.assertIn('Name: "{group}\\LVOS"', installer)
        self.assertIn('DestName: "LVOS.exe"', installer)
        self.assertIn('DestName: "lvos-ui.exe"', installer)

    def test_release_tag_must_match_workspace_version(self) -> None:
        check_release_tag("v0.1.4", "0.1.4")
        with self.assertRaisesRegex(ValueError, "does not match"):
            check_release_tag("v0.1.3", "0.1.4")
        with self.assertRaisesRegex(ValueError, "plain v<SemVer>"):
            check_release_tag("0.1.4", "0.1.4")

    def test_translation_surface_is_tokenhub_only(self) -> None:
        root = Path(__file__).parent.parent
        self.assertFalse((root / "crates/translation/src/google.rs").exists())
        self.assertFalse((root / "crates/translation/src/registry.rs").exists())
        sources = "\n".join(
            path.read_text(encoding="utf-8")
            for base in (root / "apps", root / "crates")
            for path in base.rglob("*")
            if path.suffix in {".rs", ".slint"}
        )
        for removed_surface in (
            "GoogleBasicV2Provider",
            "GoogleApiKey",
            "DEFAULT_FALLBACK_PROVIDER",
            "permits_fallback",
            "fallback-provider",
            "google-configured",
        ):
            self.assertNotIn(removed_surface, sources)

    def test_release_manifest_accepts_installable_artifacts_and_hashes_them(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            mac_archive = directory / "LVOS-0.1.0-macos-arm64.dmg"
            windows_archive = directory / "LVOS-0.1.0-windows-x86_64-setup.exe"
            mac_archive.write_bytes(b"macos disk image")
            windows_archive.write_bytes(b"windows installer")
            manifest = build_manifest(
                "0.1.0", "stable", mac_archive, windows_archive
            )
            self.assertEqual(manifest["manifest_version"], 2)
            self.assertEqual(len(manifest["artifacts"]), 2)
            self.assertEqual(len(manifest["artifacts"][0]["sha256"]), 64)
            with mac_archive.open("wb") as oversized:
                oversized.truncate(MAX_ARTIFACT_BYTES + 1)
            with self.assertRaises(ValueError):
                build_manifest("0.1.0", "stable", mac_archive, windows_archive)

    def test_notice_inventory_rejects_missing_and_modified_text(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            source = directory / "LVOS.exe"
            source.write_bytes(b"synthetic")
            archive_path = directory / "preview.zip"
            create_release_zip(source, archive_path, "LVOS.exe")
            with zipfile.ZipFile(archive_path) as archive:
                contents = {name: archive.read(name) for name in archive.namelist()}
            for missing in (True, False):
                changed = dict(contents)
                notice = next(iter(NOTICE_FILES))
                if missing:
                    del changed[notice]
                else:
                    changed[notice] = b"modified"
                with zipfile.ZipFile(archive_path, "w") as archive:
                    for name, data in changed.items():
                        archive.writestr(name, data)
                with zipfile.ZipFile(archive_path) as archive:
                    with self.assertRaisesRegex(ValueError, "distribution notice"):
                        verify_notices(archive)

    def test_workspace_version_mismatch_is_rejected(self) -> None:
        with self.assertRaises(SystemExit):
            check_packages(self.metadata(license_path="LICENSE"), "9.9.9")

    def test_release_candidate_verifier_checks_both_native_identities(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            windows = bytearray(256)
            windows[:2] = b"MZ"
            struct.pack_into("<I", windows, 0x3C, 0x80)
            windows[0x80:0x84] = b"PE\0\0"
            struct.pack_into("<H", windows, 0x84, 0x8664)
            struct.pack_into("<H", windows, 0x98, 0x20B)
            struct.pack_into("<H", windows, 0x98 + 68, 2)
            mac = bytearray(1024)
            mac[-512:-508] = b"koly"
            mac_archive = directory / "LVOS-0.1.0-macos-arm64.dmg"
            mac_archive.write_bytes(mac)
            windows_archive = directory / "LVOS-0.1.0-windows-x86_64-setup.exe"
            windows_archive.write_bytes(windows)
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
            checksum_path.write_text("0" * 64 + "  bad.dmg\n", encoding="ascii")
            with self.assertRaises(ValueError):
                verify_candidate(directory, "0.1.0")


if __name__ == "__main__":
    unittest.main()
