#!/usr/bin/env python3
"""Enforce the public renderer and resource boundaries used by the LVOS UI."""

from __future__ import annotations

from pathlib import Path
import re


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

    image_references: list[tuple[Path, str]] = []
    for source_path in UI.rglob("*.slint"):
        source = source_path.read_text(encoding="utf-8")
        image_references.extend(
            (source_path, match)
            for match in re.findall(r'@image-url\("([^"]+)"\)', source)
        )
    for source_path, reference in image_references:
        asset = (source_path.parent / reference).resolve()
        if not asset.is_relative_to(UI.resolve()) or not asset.is_file():
            failures.append(
                f"{source_path.relative_to(ROOT)} references an unavailable UI image: {reference}"
            )
        elif asset.suffix.lower() != ".svg":
            failures.append(
                f"{source_path.relative_to(ROOT)} bitmapizes a UI image instead of retaining SVG: "
                f"{reference}"
            )

    bitmap_assets = sorted(
        path.relative_to(ROOT)
        for path in UI.rglob("*")
        if path.is_file() and path.suffix.lower() in {".png", ".jpg", ".jpeg", ".webp"}
    )
    if bitmap_assets:
        failures.append(
            "Slint UI contains bitmap assets that can increase texture residency: "
            + ", ".join(map(str, bitmap_assets))
        )

    rust_sources = list((ROOT / "apps" / "desktop" / "src").rglob("*.rs"))
    for source_path in rust_sources:
        source = source_path.read_text(encoding="utf-8")
        if any(
            token in source
            for token in ("Image::load_from", "load_from_memory", "TextureAtlas")
        ):
            failures.append(
                f"{source_path.relative_to(ROOT)} adds a renderer-side image decode/cache layer"
            )
        if "SLINT_SKIA_PARTIAL_RENDERING" in source:
            failures.append(
                f"{source_path.relative_to(ROOT)} enables partial rendering as production policy"
            )

    manifests = [ROOT / "Cargo.toml", *ROOT.glob("apps/*/Cargo.toml"), *ROOT.glob("crates/*/Cargo.toml")]
    manifest_source = "\n".join(path.read_text(encoding="utf-8") for path in manifests)
    if '"renderer-skia"' not in manifest_source:
        failures.append("workspace no longer enables Slint's public Skia renderer feature")
    for forbidden in (
        "renderer-femtovg",
        "renderer-wgpu",
        "skia-safe",
        "i-slint-renderer-skia",
        "vulkan",
    ):
        if forbidden in manifest_source:
            failures.append(f"Cargo manifests contain forbidden renderer dependency/feature: {forbidden}")

    renderer_neutral_sources = [
        *UI.rglob("*.slint"),
        *(ROOT / "crates").rglob("*.rs"),
        *(ROOT / "crates").rglob("Cargo.toml"),
    ]
    for source_path in renderer_neutral_sources:
        if "skia" in source_path.read_text(encoding="utf-8").lower():
            failures.append(
                f"{source_path.relative_to(ROOT)} binds renderer-neutral UI/platform code to Skia"
            )

    if failures:
        raise SystemExit("\n".join(f"renderer policy: {failure}" for failure in failures))
    print("renderer font and resource policy checks passed")


if __name__ == "__main__":
    main()
