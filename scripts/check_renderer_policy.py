#!/usr/bin/env python3
"""Enforce the public renderer and resource boundaries used by the LVOS UI."""

from __future__ import annotations

from pathlib import Path
import re


ROOT = Path(__file__).resolve().parent.parent
UI = ROOT / "apps" / "desktop" / "ui"
WORKSPACE_MANIFEST = ROOT / "Cargo.toml"
DESKTOP_UI = ROOT / "apps" / "desktop" / "src" / "ui.rs"
AGENT_UI_PROCESS = ROOT / "apps" / "agent" / "src" / "ui_process.rs"
WINDOWS = (
    UI / "windows" / "main_window.slint",
    UI / "windows" / "quick_lookup_popup.slint",
    UI / "windows" / "permission_window.slint",
)
FONT_SUFFIXES = {".ttf", ".ttc", ".otf", ".otc", ".woff", ".woff2"}


def main() -> None:
    failures: list[str] = []

    workspace_manifest = WORKSPACE_MANIFEST.read_text(encoding="utf-8")
    slint_pin = re.search(
        r'^slint\s*=\s*\{[^\n]*version\s*=\s*"=1\.17\.1"',
        workspace_manifest,
        re.MULTILINE,
    )
    if slint_pin is None:
        failures.append("workspace Slint dependency is not pinned exactly to 1.17.1")

    desktop_ui = DESKTOP_UI.read_text(encoding="utf-8")
    for contract in (
        '.backend_name("winit".into())',
        '.renderer_name("skia".into())',
        '#[cfg(target_os = "windows")]',
        ".require_d3d()",
        '#[cfg(target_os = "macos")]',
        ".require_metal()",
    ):
        if contract not in desktop_ui:
            failures.append(f"desktop backend selection contract is missing: {contract}")

    agent_ui_process = AGENT_UI_PROCESS.read_text(encoding="utf-8")
    if 'command.env("SLINT_DESTROY_WINDOW_ON_HIDE", "1")' not in agent_ui_process:
        failures.append("Agent no longer injects destroy-on-hide into the UI child")

    for window in WINDOWS:
        source = window.read_text(encoding="utf-8")
        relative = window.relative_to(ROOT)
        if "default-font-family: Theme.ui_font_family;" not in source:
            failures.append(f"{relative} does not use the renderer-neutral Theme font family")
        if 'Theme.ui_font_family = "";' not in source:
            failures.append(f"{relative} does not preserve platform system-font fallback")

    popup = (UI / "windows" / "quick_lookup_popup.slint").read_text(encoding="utf-8")
    if "ActionTextArea" not in popup or "read_only: true;" not in popup:
        failures.append("Quick Lookup no longer uses Quadrant-Kit's read-only ActionTextArea")
    if "outlined: false;" not in popup:
        failures.append("Quick Lookup result surface no longer uses Kit's borderless appearance")
    for removed in ("selectable-result", "edited(value)", "FluentTextArea"):
        if removed in popup:
            failures.append(f"Quick Lookup retains the old editable-result workaround: {removed}")

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

    manifests = [
        WORKSPACE_MANIFEST,
        *ROOT.glob("apps/*/Cargo.toml"),
        *ROOT.glob("crates/*/Cargo.toml"),
    ]
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

    ui_host_source = desktop_ui
    for field in ("popup", "main", "permission"):
        if f"{field}: None," not in ui_host_source:
            failures.append(f"UiHosts no longer starts with lazy {field} allocation")
    for ensure, component in (
        ("ensure_main", "MainWindow"),
        ("ensure_popup", "QuickLookupPopup"),
        ("ensure_permission", "PermissionWindow"),
    ):
        if ui_host_source.count(f"{component}::new()") != 1:
            failures.append(f"production source must have one lazy {component} constructor")
        ensure_pattern = rf"fn {ensure}\b[\s\S]*?{component}::new\(\)"
        if re.search(ensure_pattern, ui_host_source) is None:
            failures.append(f"{component} construction escaped {ensure}()")

    main_window = (UI / "windows" / "main_window.slint").read_text(encoding="utf-8")
    for condition in (
        "if root.active-page == 0: history-page := HistoryPage",
        "if root.active-page == 1: favorites-page := FavoritesPage",
        "if root.active-page == 2: settings := SettingsPage",
    ):
        if condition not in main_window:
            failures.append(f"MainWindow lost conditional page construction: {condition}")

    lifecycle_tests = ui_host_source + (ROOT / "apps" / "desktop" / "tests" / "ui_lifecycle.rs").read_text(
        encoding="utf-8"
    )
    for test_name in (
        "coordinator_starts_without_any_window_host",
        "coordinator_starts_cold_and_recreates_the_management_host",
    ):
        if test_name not in lifecycle_tests:
            failures.append(f"lazy host regression coverage is missing: {test_name}")

    agent_manifest = (ROOT / "apps" / "agent" / "Cargo.toml").read_text(encoding="utf-8")
    if 'lvos = { path = "../desktop", default-features = false }' not in agent_manifest:
        failures.append("Agent no longer disables the desktop crate's UI feature")
    for forbidden in ("slint", "quadrant-kit", "renderer-skia"):
        if forbidden in agent_manifest:
            failures.append(f"Agent manifest directly links GUI dependency: {forbidden}")

    for contract in (
        "send_if_ready",
        "It never starts a process",
        "approve_idle_exit",
        "UiProcessPhase::Stopping",
    ):
        if contract not in agent_ui_process:
            failures.append(f"Agent on-demand/idle lifecycle contract is missing: {contract}")

    ui_session = (ROOT / "apps" / "desktop" / "src" / "ui_session.rs").read_text(
        encoding="utf-8"
    )
    for contract in (
        "!self.ui.has_live_ui()",
        "UiToAgent::RequestIdleExit",
        "AgentToUi::IdleExitApproved",
        "UiToAgent::CancelIdleExit",
        "slint::quit_event_loop()",
    ):
        if contract not in ui_session:
            failures.append(f"UI idle-exit handshake contract is missing: {contract}")

    current_product_text = "\n".join(
        [
            (ROOT / "README.md").read_text(encoding="utf-8"),
            desktop_ui,
            agent_ui_process,
            *(path.read_text(encoding="utf-8") for path in UI.rglob("*.slint")),
        ]
    ).lower()
    for obsolete in ("renderer-femtovg", "winit-femtovg", "femtovg"):
        if obsolete in current_product_text:
            failures.append(f"current product documentation/source still references {obsolete}")

    if failures:
        raise SystemExit("\n".join(f"renderer policy: {failure}" for failure in failures))
    print("renderer resource, lazy-host, and idle-exit policy checks passed")


if __name__ == "__main__":
    main()
