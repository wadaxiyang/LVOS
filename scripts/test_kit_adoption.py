"""Mutation tests for the dependency and UI boundary, including wrapper bypasses."""
import copy
from pathlib import Path
import tomllib
import unittest

from scripts.check_kit_adoption import ROOT, UI, KIT_DEP, check_dependencies, check_locks, scan_ui, tokens


class KitBoundaryTests(unittest.TestCase):
    def ui(self):
        return {p.relative_to(ROOT).as_posix(): p.read_text(encoding="utf-8")
                for p in (ROOT / UI).rglob("*.slint")}

    def docs(self):
        return {"apps/desktop/Cargo.toml": {"build-dependencies": {"quadrant-kit": copy.deepcopy(KIT_DEP)}}}

    def reject_ui(self, text, path="pages/history_page.slint"):
        files = self.ui()
        files[UI + path] += "\n" + text
        with self.assertRaises(ValueError):
            scan_ui(files)

    def test_current_public_import_closure(self):
        scan_ui(self.ui())

    def test_quick_lookup_uses_read_only_action_surface(self):
        popup = self.ui()[UI + "windows/quick_lookup_popup.slint"]
        self.assertIn("result-editor := ActionTextArea {", popup)
        self.assertIn("text: root.canonical-result;", popup)
        self.assertIn("read_only: true;", popup)
        self.assertIn("outlined: false;", popup)
        self.assertIn('accessible_name: "Copy translation";', popup)
        self.assertIn('accessible_name: "Refresh translation";', popup)
        for removed in ("selectable-result", "edited(value)", "FluentTextArea"):
            self.assertNotIn(removed, popup)

    def test_native_alias(self):
        self.reject_ui('import { Button as FluentButton } from "std-widgets.slint";')

    def test_hidden_wrapper(self):
        self.reject_ui('component BusinessEditor inherits LineEdit { }')

    def test_extra_wrapper_file(self):
        files = self.ui()
        files[UI + "pages/wrapper.slint"] = 'export component BusinessEditor inherits LineEdit {}'
        with self.assertRaises(ValueError):
            scan_ui(files)

    def test_touch_and_style_bypasses(self):
        for snippet in ("TouchArea {}", "TextInput {}", "Rectangle {}", "border-color: red;", "font-size: 14px;", "Text { color: #fff; }", "key-pressed(event) => { }"):
            with self.subTest(snippet=snippet):
                self.reject_ui(snippet)

    def test_private_escaped_and_external_imports(self):
        for target in ("@quadrant-kit/private.slint", "../../../../other.slint", r"\u{40}quadrant-kit", "https://example.invalid/ui.slint"):
            with self.subTest(target=target):
                self.reject_ui(f'import {{ FluentButton }} from "{target}";')

    def test_unknown_syntax_and_unterminated_comment(self):
        for source in ('import * from "@quadrant-kit";', '/* never closed', '€'):
            with self.subTest(source=source):
                self.reject_ui(source)

    def test_comments_and_strings_are_not_controls(self):
        tokens('// TouchArea {}\nText { text: "Button // harmless"; }')
        self.reject_ui('Text { text: "harmless"; }\nTouchArea {}')

    def test_unbounded_timer(self):
        files = self.ui()
        key = UI + "pages/settings_page.slint"
        files[key] = files[key].replace("running: root.locate-pending", "running: true")
        with self.assertRaises(ValueError):
            scan_ui(files)

    def test_official_build_only_dependency(self):
        check_dependencies(self.docs())

    def test_path_patch_sha_version_and_runtime_rejected(self):
        for mutation in ({"path": "../Quadrant-Kit"}, {"rev": "abc"}, {"version": "0.1.1"}, {"branch": "main"}, {"tag": "v0.1.1"}, {"git": "https://example.invalid/Kit.git"}):
            docs = self.docs()
            docs["apps/desktop/Cargo.toml"]["build-dependencies"]["quadrant-kit"].update(mutation)
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                check_dependencies(docs)
        docs = self.docs()
        docs["Cargo.toml"] = {"patch": {KIT_DEP["git"]: {"quadrant-kit": {"path": "../Kit"}}}}
        with self.assertRaises(ValueError):
            check_dependencies(docs)
        docs = self.docs()
        docs["apps/desktop/Cargo.toml"]["dependencies"] = docs["apps/desktop/Cargo.toml"].pop("build-dependencies")
        with self.assertRaises(ValueError):
            check_dependencies(docs)

    def test_renamed_kit_and_source_replacement(self):
        docs = self.docs()
        docs["Cargo.toml"] = {"dependencies": {"innocent": {"package": "quadrant-kit", "path": "../Kit"}}}
        with self.assertRaises(ValueError):
            check_dependencies(docs)
        docs = self.docs()
        docs[".cargo/config.toml"] = {"source": {"upstream": {"replace-with": "vendor"}}}
        with self.assertRaises(ValueError):
            check_dependencies(docs)

    def test_lock_source_and_server_separation(self):
        lock, server = [tomllib.loads((ROOT / p).read_text(encoding="utf-8")) for p in ("Cargo.lock", "Cargo.server.lock")]
        check_locks(lock, server)
        server["package"].append({"name": "slint"})
        with self.assertRaises(ValueError):
            check_locks(lock, server)
        server["package"].pop()
        next(p for p in lock["package"] if p["name"] == "quadrant-kit")["source"] = "path+../Kit"
        with self.assertRaises(ValueError):
            check_locks(lock, server)


if __name__ == "__main__":
    unittest.main()
