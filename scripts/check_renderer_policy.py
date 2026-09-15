#!/usr/bin/env python3
"""Enforce the public renderer and resource boundaries used by the LVOS UI."""

from __future__ import annotations

from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
UI = ROOT / "apps" / "desktop" / "ui"
WINDOWS = (
    UI / "windows" / "main_window.slint",
    UI / "windows" / "quick_lookup_popup.slint",
    UI / "windows" / "permission_window.slint",
)
FONT_SUFFIXES = {".ttf", ".ttc", ".otf", ".otc", ".woff", ".woff2"}


def main() -> None:
    failures: list[str] = []

    for window in WINDOWS:
        source = window.read_text(encoding="utf-8")
        relative = window.relative_to(ROOT)
        if "default-font-family: Theme.ui_font_family;" not in source:
            failures.append(f"{relative} does not use the renderer-neutral Theme font family")
        if 'Theme.ui_font_family = "";' not in source:
            failures.append(f"{relative} does not preserve platform system-font fallback")

    popup = (UI / "windows" / "quick_lookup_popup.slint").read_text(encoding="utf-8")
    if "FluentTextArea" not in popup:
        failures.append("Quick Lookup no longer uses Quadrant-Kit's TextEdit-backed text area")

    bundled_fonts = sorted(
        path.relative_to(ROOT)
        for path in (ROOT / "apps" / "desktop").rglob("*")
        if path.is_file() and path.suffix.lower() in FONT_SUFFIXES
    )
    if bundled_fonts:
        failures.append(
            "desktop UI bundles font assets instead of using system fallback: "
            + ", ".join(map(str, bundled_fonts))
        )

    handwritten_sources = [
        *UI.rglob("*.slint"),
        *(ROOT / "apps" / "desktop" / "src").rglob("*.rs"),
    ]
    for source_path in handwritten_sources:
        source = source_path.read_text(encoding="utf-8")
        if "@font-face" in source or "register_font_from" in source:
            failures.append(
                f"{source_path.relative_to(ROOT)} adds a runtime or embedded font layer"
            )

    if failures:
        raise SystemExit("\n".join(f"renderer policy: {failure}" for failure in failures))
    print("renderer font policy checks passed")


if __name__ == "__main__":
    main()
