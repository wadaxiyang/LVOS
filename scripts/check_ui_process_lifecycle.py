#!/usr/bin/env python3
"""Exercise the native Agent/UI idle-exit handshake without using production profile data."""

from __future__ import annotations

from pathlib import Path
import platform
import subprocess


ROOT = Path(__file__).resolve().parent.parent


def main() -> None:
    system = platform.system()
    if system not in {"Darwin", "Windows"}:
        raise SystemExit("UI process lifecycle check requires Windows or macOS")
    subprocess.run(
        ["cargo", "build", "--locked", "-p", "lvos-agent", "-p", "lvos"],
        cwd=ROOT,
        check=True,
    )
    executable = ROOT / "target" / "debug" / (
        "lvos-agent.exe" if system == "Windows" else "lvos-agent"
    )
    subprocess.run(
        [executable, "--ui-process-check"],
        cwd=ROOT,
        check=True,
        timeout=90,
    )
    print("UI process lifecycle check passed")


if __name__ == "__main__":
    main()
