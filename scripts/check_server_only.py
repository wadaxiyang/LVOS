#!/usr/bin/env python3
"""Build the Dockerfile's isolated Cargo graph without requiring a Docker daemon."""
from pathlib import Path
import argparse
import json
import shutil
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parent.parent


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--release", action="store_true")
    args = parser.parse_args()
    spec = tomllib.loads((ROOT / "Cargo.server.toml").read_text(encoding="utf-8"))
    # Keep diagnostic sources/build outputs outside the repository source inventory.
    with tempfile.TemporaryDirectory(prefix="lvos-server-only-") as temporary:
        isolated = Path(temporary)
        for source, destination in (("Cargo.server.toml", "Cargo.toml"), ("Cargo.server.lock", "Cargo.lock"), ("LICENSE", "LICENSE"), ("rust-toolchain.toml", "rust-toolchain.toml")):
            shutil.copy2(ROOT / source, isolated / destination)
        for member in spec["workspace"]["members"]:
            shutil.copytree(ROOT / member, isolated / member)
        raw = subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1"], cwd=isolated)
        packages = json.loads(raw)["packages"]
        if any("slint" in package["name"] or package["name"] == "quadrant-kit" for package in packages):
            raise ValueError("server-only resolved graph contains desktop UI dependencies")
        command = ["cargo", "build", "--locked", "-p", "lvos-server", "--target-dir", str(ROOT / "target/server-only")]
        if args.release:
            command.append("--release")
        subprocess.run(command, cwd=isolated, check=True)
    print("server-only isolated locked graph and build passed (Docker runtime not exercised)")


if __name__ == "__main__":
    main()
