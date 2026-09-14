#!/usr/bin/env python3
"""Build and deeply verify one native installer/disk image without publishing it."""
from __future__ import annotations

import os
from pathlib import Path
import platform
import plistlib
import shutil
import struct
import subprocess
import tempfile
import uuid

if __package__:
    from .create_release_zip import NOTICE_FILES
    from .verify_release_candidate import digest, verify_macos_dmg, verify_pe_gui, verify_windows_installer
    from .workspace_version import workspace_version
else:
    from create_release_zip import NOTICE_FILES
    from verify_release_candidate import digest, verify_macos_dmg, verify_pe_gui, verify_windows_installer
    from workspace_version import workspace_version

ROOT = Path(__file__).resolve().parent.parent


def verify_macos_bundle(bundle: Path, version: str) -> None:
    metadata = plistlib.loads((bundle / "Contents/Info.plist").read_bytes())
    expected = {
        "CFBundleDisplayName": "LVOS",
        "CFBundleExecutable": "LVOS",
        "CFBundleIdentifier": "site.niuniu770.lvos",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": version,
        "LSMinimumSystemVersion": "15.0",
    }
    if any(metadata.get(key) != value for key, value in expected.items()):
        raise ValueError("macOS Info.plist disagrees with frozen release identity")
    for name in ("LVOS", "lvos-ui"):
        binary = (bundle / "Contents/MacOS" / name).read_bytes()
        if len(binary) < 8 or binary[:4] != b"\xcf\xfa\xed\xfe":
            raise ValueError(f"macOS executable is not a 64-bit little-endian Mach-O: {name}")
        if struct.unpack_from("<I", binary, 4)[0] != 0x0100000C:
            raise ValueError(f"macOS executable is not arm64: {name}")
    notices = bundle / "Contents/Resources/NOTICES"
    for archive_name, source_name in NOTICE_FILES.items():
        installed = notices / Path(archive_name).name
        source = ROOT / source_name
        if not installed.is_file() or installed.read_bytes() != source.read_bytes():
            raise ValueError(f"macOS bundle notice is missing or modified: {installed.name}")


def verify_macos_package(image: Path, version: str) -> None:
    verify_macos_dmg(image)
    attached = subprocess.run(
        ["hdiutil", "attach", "-readonly", "-nobrowse", "-plist", image],
        check=True,
        stdout=subprocess.PIPE,
    )
    document = plistlib.loads(attached.stdout)
    mount_points = [
        entity.get("mount-point")
        for entity in document.get("system-entities", [])
        if entity.get("mount-point")
    ]
    if len(mount_points) != 1:
        for mount_point in reversed(mount_points):
            subprocess.run(["hdiutil", "detach", mount_point], check=False)
        raise ValueError("macOS disk image did not expose exactly one mounted volume")
    mount = Path(mount_points[0])
    try:
        verify_macos_bundle(mount / "LVOS.app", version)
        applications = mount / "Applications"
        if not applications.is_symlink() or os.readlink(applications) != "/Applications":
            raise ValueError("macOS disk image lacks the Applications drag target")
        subprocess.run(["codesign", "--verify", "--deep", "--strict", mount / "LVOS.app"], check=True)
    finally:
        subprocess.run(["hdiutil", "detach", mount], check=True)


def verify_windows_package(installer: Path) -> None:
    verify_windows_installer(installer)
    script = (ROOT / "packaging/windows/LVOS.iss").read_text(encoding="utf-8")
    for required in ("DisableDirPage=no", "DisableProgramGroupPage=no", "desktopicon", "{group}\\LVOS"):
        if required not in script:
            raise ValueError(f"Windows installer omits required wizard/shortcut behavior: {required}")

    with tempfile.TemporaryDirectory(prefix="lvos-installer-check-") as raw:
        install = Path(raw) / "chosen-install-directory"
        group = f"LVOS Engineering Check {uuid.uuid4().hex}"
        subprocess.run(
            [
                installer,
                "/VERYSILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/SP-",
                f"/DIR={install}",
                f"/GROUP={group}",
                "/TASKS=!desktopicon",
            ],
            check=True,
            timeout=120,
        )
        appdata = Path(os.environ["APPDATA"])
        shortcut = appdata / "Microsoft/Windows/Start Menu/Programs" / group / "LVOS.lnk"
        uninstaller = install / "unins000.exe"
        shortcut_left_after_uninstall = False
        try:
            verify_pe_gui(install / "LVOS.exe", require_x86_64=True)
            verify_pe_gui(install / "lvos-ui.exe", require_x86_64=True)
            for archive_name, source_name in NOTICE_FILES.items():
                installed = install / "NOTICES" / Path(archive_name).name
                source = ROOT / source_name
                if not installed.is_file() or installed.read_bytes() != source.read_bytes():
                    raise ValueError(f"Windows installer notice is missing or modified: {installed.name}")
            if not shortcut.is_file():
                raise ValueError("Windows installer did not create the Start Menu shortcut")
        finally:
            if uninstaller.is_file():
                subprocess.run(
                    [uninstaller, "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"],
                    check=True,
                    timeout=120,
                )
            shortcut_left_after_uninstall = shortcut.exists()
            group_directory = shortcut.parent
            if group_directory.exists():
                shutil.rmtree(group_directory)
        if shortcut_left_after_uninstall:
            raise ValueError("Windows uninstall left its Start Menu shortcut behind")


def main() -> None:
    version = workspace_version(ROOT / "Cargo.toml")
    if platform.system() == "Darwin" and platform.machine() == "arm64":
        subprocess.run(["bash", "scripts/package-macos-app.sh"], cwd=ROOT, check=True)
        package = ROOT / f"target/release-package/LVOS-{version}-macos-arm64.dmg"
        verify_macos_package(package, version)
    elif platform.system() == "Windows" and platform.machine().lower() in {"amd64", "x86_64"}:
        subprocess.run(
            ["powershell", "-NoProfile", "-File", "scripts/package-windows-installer.ps1"],
            cwd=ROOT,
            check=True,
        )
        package = ROOT / f"target/release-package/LVOS-{version}-windows-x86_64-setup.exe"
        verify_windows_package(package)
    else:
        raise SystemExit("native package verification requires Windows x86_64 or macOS arm64")
    print(f"engineering package verified: {package.name}; bytes={package.stat().st_size}; sha256={digest(package)}")
    print("No artifact was published; signing, notarization, and native permission acceptance remain separate gates.")


if __name__ == "__main__":
    main()
