#!/usr/bin/env python3
"""Deterministic Stage 14 integration, idle, and secret-logging policy gate."""

from __future__ import annotations

import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
DESKTOP_MAIN = ROOT / "apps" / "desktop" / "src" / "main.rs"
DESKTOP_UI = ROOT / "apps" / "desktop" / "ui" / "app.slint"

REQUIRED_CALLBACKS = {
    "history-search",
    "favorites-search",
    "favorite-toggled",
    "clear-history-requested",
    "persist-provider-settings",
    "test-provider",
    "login-requested",
    "logout-requested",
    "manual-sync-requested",
    "test-connection-requested",
    "revoke-device-requested",
    "regenerate-device-identity-requested",
    "export-data-requested",
    "import-data-requested",
    "check-update-requested",
    "update-global-hotkey",
    "update-start-at-login",
    "update-launch-minimized",
}

SECRET_NAMES = re.compile(
    r"\b(password|access_token|refresh_token|api_key|tokenhub_key)\b",
    re.IGNORECASE,
)
TRACE_MACRO = re.compile(r"tracing::(?:trace|debug|info|warn|error)!\((.*?)\);", re.DOTALL)


def production_rust_files() -> list[Path]:
    return sorted(
        path
        for base in (ROOT / "apps", ROOT / "crates")
        for path in base.rglob("*.rs")
        if "tests" not in path.parts and "examples" not in path.parts
    )


def main() -> int:
    failures: list[str] = []
    ui = DESKTOP_UI.read_text(encoding="utf-8")
    main_source = DESKTOP_MAIN.read_text(encoding="utf-8")
    callbacks = set(re.findall(r"^\s*callback\s+([a-z0-9-]+)", ui, re.MULTILINE))
    missing_declarations = sorted(REQUIRED_CALLBACKS - callbacks)
    if missing_declarations:
        failures.append(f"missing required UI callbacks: {missing_declarations}")
    missing_handlers = sorted(
        callback
        for callback in REQUIRED_CALLBACKS
        if f"on_{callback.replace('-', '_')}" not in main_source
        and f"on_{callback.replace('-', '_')}" not in (ROOT / "apps" / "desktop" / "src" / "ui.rs").read_text(encoding="utf-8")
    )
    if missing_handlers:
        failures.append(f"production Desktop callbacks are unwired: {missing_handlers}")
    if "show_captured_provider_error" in main_source:
        failures.append("production capture still uses the pre-integration Provider error stub")

    desktop_sources = "\n".join(
        path.read_text(encoding="utf-8")
        for path in (ROOT / "apps" / "desktop" / "src").glob("*.rs")
    )
    if "tokio::time::interval(" in desktop_sources:
        failures.append("Desktop contains a fixed Tokio interval; idle work must be event-driven")
    if "Clipboard monitoring while idle" in desktop_sources:
        failures.append("unexpected idle clipboard monitor marker")

    for path in production_rust_files():
        source = path.read_text(encoding="utf-8")
        for macro in TRACE_MACRO.finditer(source):
            if SECRET_NAMES.search(macro.group(1)):
                relative = path.relative_to(ROOT)
                line = source.count("\n", 0, macro.start()) + 1
                failures.append(f"secret-shaped value referenced by tracing macro: {relative}:{line}")

    server = (ROOT / "apps" / "server" / "src" / "lib.rs").read_text(encoding="utf-8")
    transport = (ROOT / "apps" / "desktop" / "src" / "sync_transport.rs").read_text(encoding="utf-8")
    for field in ("server_api_version", "server_version", "minimum_desktop_version"):
        if field not in server or field not in transport:
            failures.append(f"compatibility field is not shared by Server/Desktop: {field}")

    if failures:
        for failure in failures:
            print(f"stage14 audit failed: {failure}")
        return 1
    print("stage14 integration/security policy checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
