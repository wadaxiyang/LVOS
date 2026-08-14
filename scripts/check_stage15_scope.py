#!/usr/bin/env python3
"""Verify the V1 scope freeze and release-identity declarations."""

from __future__ import annotations

import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SOURCE_ROOTS = [ROOT / "apps", ROOT / "crates"]
FORBIDDEN_DIRECT_DEPENDENCIES = re.compile(
    r"^\s*(electron|wry|webview2|tauri)\s*=", re.IGNORECASE | re.MULTILINE
)
FORBIDDEN_PRODUCT_MARKERS = [
    re.compile(pattern, re.IGNORECASE)
    for pattern in (
        r"struct\s+LearningState\b",
        r"struct\s+ReviewHistory\b",
        r"EnrichmentQueue",
        r"route\([^\n]*['\"]/(?:api/[^'\"]*/)?register",
        r"self[_-]?install",
    )
]


def main() -> int:
    failures: list[str] = []
    manifests = sorted(ROOT.glob("**/Cargo.toml"))
    for manifest in manifests:
        source = manifest.read_text(encoding="utf-8")
        if FORBIDDEN_DIRECT_DEPENDENCIES.search(source):
            failures.append(f"forbidden V1 Desktop/Web dependency: {manifest.relative_to(ROOT)}")
    for base in SOURCE_ROOTS:
        for path in sorted(base.rglob("*.rs")):
            source = path.read_text(encoding="utf-8")
            for marker in FORBIDDEN_PRODUCT_MARKERS:
                if marker.search(source):
                    failures.append(
                        f"out-of-scope V2+ product marker: {path.relative_to(ROOT)} ({marker.pattern})"
                    )
    web_assets = [
        path.relative_to(ROOT)
        for base in SOURCE_ROOTS
        for path in base.rglob("*")
        if path.suffix.lower() in {".html", ".js", ".jsx", ".ts", ".tsx", ".vue", ".svelte"}
    ]
    if web_assets:
        failures.append(f"unexpected V1 Web UI assets: {web_assets}")
    identity = "\n".join(
        path.read_text(encoding="utf-8")
        for path in (ROOT / "apps" / "desktop" / "src").glob("*.rs")
    )
    package = (ROOT / "scripts" / "package-macos-app.sh").read_text(encoding="utf-8")
    for value in ("site.niuniu770.lvos",):
        if value not in package:
            failures.append(f"macOS package lacks frozen identity: {value}")
    if "SOFTWARE_VERSION" not in identity:
        failures.append("Desktop library does not consume the shared software version")
    if failures:
        for failure in failures:
            print(f"stage15 scope check failed: {failure}")
        return 1
    print("stage15 V1 scope and release identity checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
