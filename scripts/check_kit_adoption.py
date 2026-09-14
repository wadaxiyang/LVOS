#!/usr/bin/env python3
"""Fail closed at the LVOS/Kit source boundary; native UX is checked separately."""
from __future__ import annotations

from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parent.parent
UI = "apps/desktop/ui/"
KIT_URL = "https://github.com/wadaxiyang/Quadrant-Kit.git"
KIT_REV = "20cc9d77b737d1326d4320a42f7d26f9a799298f"
KIT_DEP = {"git": KIT_URL, "rev": KIT_REV, "version": "=0.1.1", "optional": True}
# These are public 0.1.1 exports, not private implementation names.
KIT_NAMES = set("Theme ThemeMode Motion Typography UiConstants SurfaceCard FluentIcon FluentIcons IconButton FluentProgressRing FluentScrollView FluentInfoBar InfoBarKind PageHeader SectionHeader SettingRow FluentButton FluentTextField FluentComboBox FluentSwitch InputType Badge BadgeKind EmptyState NavigationView NavigationEntry NavigationEntryKind NavigationPaneMode ModalManager ModalKind".split())
# Exact product compositions, rather than an exemption for a whole directory.
DECLARATIONS = {
    "windows/main_window.slint": {"MainWindow": "Window"},
    "windows/permission_window.slint": {"PermissionWindow": "Window"},
    "windows/quick_lookup_popup.slint": {"QuickLookupPopup": "Window"},
    "components/record_card.slint": {"RecordCard": "SurfaceCard"},
    "pages/history_page.slint": {"HistoryPage": "VerticalLayout"},
    "pages/favorites_page.slint": {"FavoritesPage": "VerticalLayout"},
    "pages/settings_page.slint": {"SettingsPage": "VerticalLayout"},
    **{f"pages/settings/{name}_page.slint": {f"{name.title()}SettingsPage": "VerticalLayout"}
       for name in ("general", "translation", "account", "sync", "devices", "history", "data", "update")},
    "model/ui_types.slint": {"UiRecord": "struct", "DeviceRecord": "struct", "FeedbackKind": "enum"},
    "product/navigation.slint": {"LvosNavigation": "global"},
    "app.slint": {},
}
# Brand and lookup/device semantics missing from the pinned public icon set.
PRODUCT_ASSETS = {"logo.svg", "history.svg", "star.svg", "star-filled.svg", "translate.svg", "refresh.svg", "devices.svg"}
IDENT = r"[A-Za-z_][\w-]*"
TOKEN = re.compile(r'\s+|//[^\n]*|/\*[\s\S]*?\*/|"(?:\\.|[^"\\])*"|' + IDENT + r'|\d+(?:\.\d+)?(?:px|ms|s|%)?|<=>|:=|=>|->|==|!=|<=|>=|&&|\|\||[{}()\[\];:,.<>+*/%=!?@\-]')


def tokens(source: str) -> list[str]:
    result, pos = [], 0
    while pos < len(source):
        match = TOKEN.match(source, pos)
        if match is None:
            raise ValueError(f"unrecognized UI syntax at offset {pos}")
        token = match.group()
        if token == "/" and source[pos:pos+2] == "/*":
            raise ValueError("unterminated UI comment")
        if not token.isspace() and not token.startswith(("//", "/*")):
            result.append(token)
        pos = match.end()
    return result


def imports(ts: list[str]) -> tuple[list[tuple[str, list[str]]], list[str]]:
    result, body, i = [], [], 0
    while i < len(ts):
        if ts[i] == "import" or (ts[i] == "export" and ts[i+1:i+2] == ["{"]):
            start = i
            i += 1
            if ts[i:i+1] != ["{"]:
                raise ValueError("only explicit named UI imports are allowed")
            i += 1
            names = []
            while i < len(ts) and ts[i] != "}":
                if re.fullmatch(IDENT, ts[i]) is None or ts[i] == "as":
                    raise ValueError("UI aliases/unknown import syntax need explicit review")
                names.append(ts[i]); i += 1
                if ts[i:i+1] == [","]:
                    i += 1
                elif ts[i:i+1] != ["}"]:
                    raise ValueError("UI aliases/unknown import syntax need explicit review")
            if ts[i:i+2] != ["}", "from"] or len(ts) <= i+3 or ts[i+3] != ";":
                raise ValueError(f"malformed UI import at token {start}")
            target = ts[i+2]
            if not target.startswith('"') or "\\" in target:
                raise ValueError("UI import path must be literal and unescaped")
            result.append((target[1:-1], names)); i += 4
        else:
            body.append(ts[i]); i += 1
    return result, body


def scan_ui(files: dict[str, str]) -> None:
    for name, source in files.items():
        if not name.startswith(UI) or name[len(UI):] not in DECLARATIONS:
            raise ValueError(f"unreviewed UI source: {name}")
        relative = name[len(UI):]
        expected = DECLARATIONS[relative]
        imported, body = imports(tokens(source))
        permitted = {"Text", "VerticalLayout", "HorizontalLayout", *expected, *expected.values()}
        for target, names in imported:
            if target == "@quadrant-kit":
                available = KIT_NAMES
            elif target == "std-widgets.slint":
                available = {"Palette"}
            else:
                resolved = (ROOT / name).parent.joinpath(target).resolve()
                if not resolved.is_relative_to(ROOT / UI):
                    raise ValueError(f"UI import escapes owned source: {name}: {target}")
                key = resolved.relative_to(ROOT).as_posix()
                if key not in files:
                    raise ValueError(f"missing owned import: {name}: {target}")
                available = set(DECLARATIONS.get(key[len(UI):], {}))
            if not set(names) <= available:
                raise ValueError(f"non-public/native/unknown UI import: {name}: {names}")
            permitted.update(names)
        declared = {}
        for i, token in enumerate(body):
            if token in {"component", "global", "struct", "enum"}:
                kind = token
                if i+1 >= len(body):
                    raise ValueError("incomplete declaration")
                symbol = body[i+1]
                if kind == "component":
                    if body[i+2:i+3] != ["inherits"] or i+3 >= len(body):
                        raise ValueError("components must declare a reviewed base")
                    kind = body[i+3]
                if symbol in declared:
                    raise ValueError("duplicate UI declaration")
                declared[symbol] = kind
            # Only this product FocusScope can disable the modal background. No key handler.
            if token == "FocusScope":
                if relative != "windows/main_window.slint" or body[i-2:i] != ["background-focus", ":="]:
                    raise ValueError("unreviewed FocusScope")
                permitted.add(token)
            # A single bounded layout locator, not a perpetual animation timer.
            if token == "Timer":
                if relative != "pages/settings_page.slint" or body.count("Timer") != 1:
                    raise ValueError("unreviewed Timer")
                normalized = " ".join(body)
                for contract in ("interval : 16ms ;", "running : root . locate-pending ;", "root . locate-pending = false ;"):
                    if contract not in normalized:
                        raise ValueError("settings locator must remain bounded")
                permitted.add(token)
            if token in {"TouchArea", "TextInput", "Rectangle", "Image", "Path", "Canvas", "states", "transitions", "animate", "key-pressed", "key-released", "has-hover", "pressed", "border-color", "border-width", "border-radius", "drop-shadow-color"}:
                raise ValueError(f"local generic control/style is forbidden: {name}: {token}")
            if body[i+1:i+2] == ["{"] and re.fullmatch(r"[A-Z][\w-]*", token):
                if token not in permitted:
                    raise ValueError(f"unreviewed visual owner: {name}: {token}")
            if token in {"font-size", "font-weight", "color", "icon_color", "background"} and body[i+1:i+2] == [":"]:
                allowed = {"Theme", "Typography"}
                if relative == "windows/quick_lookup_popup.slint" and token == "background":
                    allowed.add("transparent")
                if body[i+2:i+3] and body[i+2] not in allowed:
                    raise ValueError(f"business visuals must use Kit tokens: {name}: {token}")
            if token == "image-url":
                path = body[i+2] if body[i+1:i+2] == ["("] else ""
                if not path.startswith('"') or "\\" in path:
                    raise ValueError("nonliteral UI asset")
                asset = (ROOT / name).parent.joinpath(path[1:-1]).resolve()
                if asset.parent != ROOT / UI / "icons" or asset.name not in PRODUCT_ASSETS or not asset.is_file():
                    raise ValueError(f"unreviewed product asset: {name}: {path}")
        if declared != expected:
            raise ValueError(f"unreviewed composition/wrapper: {name}: {declared}")


def walk(value, path=()):
    if isinstance(value, dict):
        for key, child in value.items():
            yield (*path, key), child
            yield from walk(child, (*path, key))


def check_dependencies(documents: dict[str, dict]) -> None:
    count = 0
    for name, doc in documents.items():
        if doc.get("patch") or doc.get("replace") or doc.get("source"):
            raise ValueError(f"dependency substitution requires review: {name}")
        for path, value in walk(doc):
            key = path[-1]
            package = value.get("package", key) if isinstance(value, dict) else key
            if not isinstance(package, str):
                continue
            if package == "quadrant-kit":
                if name != "apps/desktop/Cargo.toml" or path != ("build-dependencies", "quadrant-kit") or value != KIT_DEP:
                    raise ValueError("Kit must be the exact official build-only dependency")
                count += 1
            if package in {"slint", "slint-build"}:
                if (
                    isinstance(value, dict)
                    and value.get("workspace") is True
                    and set(value) <= {"workspace", "optional"}
                    and name == "apps/desktop/Cargo.toml"
                ):
                    continue
                version = value.get("version") if isinstance(value, dict) else value
                if version != "=1.17.1" or (isinstance(value, dict) and any(k in value for k in ("git", "path", "branch", "tag", "rev"))):
                    raise ValueError("Slint must remain pinned to registry 1.17.1")
                if name == "Cargo.server.toml":
                    raise ValueError("server-only graph must not contain Slint")
    if count != 1:
        raise ValueError("exactly one Kit build dependency is required")


def check_locks(lock: dict, server: dict) -> None:
    packages = lock.get("package", [])
    for name, version in (("quadrant-kit", "0.1.1"), ("slint", "1.17.1"), ("slint-build", "1.17.1")):
        found = [p for p in packages if p["name"] == name]
        if len(found) != 1 or found[0]["version"] != version:
            raise ValueError(f"wrong locked package identity: {name}")
        source = found[0].get("source", "")
        expected = f"git+{KIT_URL}?rev={KIT_REV}#{KIT_REV}" if name == "quadrant-kit" else "registry+https://github.com/rust-lang/crates.io-index"
        if source != expected:
            raise ValueError(f"wrong locked source: {name}")
    if any("slint" in p["name"] or p["name"] == "quadrant-kit" for p in server.get("package", [])):
        raise ValueError("server-only lock contains UI dependencies")


def main() -> int:
    try:
        names = subprocess.check_output(["git", "ls-files", "-co", "--exclude-standard", "-z"], cwd=ROOT).decode().split("\0")
        paths = sorted({name for name in names if name and (ROOT / name).is_file()})
        docs = {name: tomllib.loads((ROOT / name).read_text(encoding="utf-8")) for name in paths if name.endswith("Cargo.toml") or name == "Cargo.server.toml" or name in {".cargo/config", ".cargo/config.toml"}}
        check_dependencies(docs)
        check_locks(*(tomllib.loads((ROOT / name).read_text(encoding="utf-8")) for name in ("Cargo.lock", "Cargo.server.lock")))
        scan_ui({name: (ROOT / name).read_text(encoding="utf-8") for name in paths if name.endswith(".slint")})
        host = (ROOT / "apps/desktop/src/ui_session.rs").read_text(encoding="utf-8")
        if re.search(r"\b(?:AsyncMessageDialog|MessageDialog|MessageButtons|MessageDialogResult)\b", host):
            raise ValueError("application confirmations must use the Kit broker")
        build = (ROOT / "apps/desktop/build.rs").read_text(encoding="utf-8")
        for required in ("quadrant_kit::slint_library_path()", "quadrant_kit::SLINT_LIBRARY_NAME", '.with_style("fluent".into())', "EmbedResourcesKind::EmbedFiles"):
            if required not in build:
                raise ValueError(f"official embedded Kit build contract missing: {required}")
    except (ValueError, OSError, IndexError, tomllib.TOMLDecodeError) as error:
        print(f"Kit adoption check failed: {error}", file=sys.stderr)
        return 1
    print("Kit adoption checks passed (source boundary only; native verification is separate)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
