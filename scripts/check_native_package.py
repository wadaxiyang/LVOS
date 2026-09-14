#!/usr/bin/env python3
"""Build and verify one native engineering package without publishing an artifact."""
from pathlib import Path
import platform
import subprocess

if __package__:
    from .create_release_zip import create_release_zip
    from .verify_release_candidate import digest, verify_macos, verify_windows
    from .workspace_version import workspace_version
else:
    from create_release_zip import create_release_zip
    from verify_release_candidate import digest, verify_macos, verify_windows
    from workspace_version import workspace_version

ROOT = Path(__file__).resolve().parent.parent


def main() -> None:
    version = workspace_version(ROOT / "Cargo.toml")
    if platform.system() == "Darwin" and platform.machine() == "arm64":
        subprocess.run(["bash", "scripts/package-macos-app.sh"], cwd=ROOT, check=True)
        archive = ROOT / f"target/release-package/LVOS-{version}-macos-arm64.zip"
        verify_macos(archive, version)
    elif platform.system() == "Windows" and platform.machine().lower() in {"amd64", "x86_64"}:
        subprocess.run(
            ["cargo", "build", "--release", "--locked", "-p", "lvos-agent", "-p", "lvos"],
            cwd=ROOT,
            check=True,
        )
        archive = ROOT / f"target/release-package/LVOS-{version}-windows-x86_64.zip"
        create_release_zip(
            ROOT / "target/release/lvos-agent.exe",
            archive,
            "LVOS.exe",
            {"lvos-ui.exe": ROOT / "target/release/lvos-ui.exe"},
        )
        verify_windows(archive)
    else:
        raise SystemExit("native package verification requires Windows x86_64 or macOS arm64")
    print(f"engineering package verified: {archive.name}; bytes={archive.stat().st_size}; sha256={digest(archive)}")
    print("No artifact was published; license and native interaction acceptance remain separate gates.")


if __name__ == "__main__":
    main()
