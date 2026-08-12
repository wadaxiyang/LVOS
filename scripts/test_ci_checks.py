#!/usr/bin/env python3
"""Cross-platform regression tests for CI policy helpers."""

from __future__ import annotations

import unittest

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


if __name__ == "__main__":
    unittest.main()
