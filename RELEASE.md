# LVOS unsigned release notes

LVOS V1 desktop packages are intentionally unsigned. Download artifacts only from the
[`wadaxiyang/LVOS` GitHub Releases page](https://github.com/wadaxiyang/LVOS/releases), then compare
the file's SHA-256 with `SHA256SUMS` and `lvos-update-stable.json` before opening it.

## macOS 15 arm64

The ZIP contains `LVOS.app` for Apple silicon and requires macOS 15 or newer. Because it is not
Developer ID signed or notarized, Gatekeeper may block the first launch. After verifying the
download, use Finder's **Open** context-menu action and confirm the one-app warning. Do not disable
Gatekeeper globally.

## Windows 11 x86_64

The ZIP contains the portable `LVOS.exe`. Microsoft Defender SmartScreen may show an unknown
publisher warning. After verifying SHA-256, use **More info** and **Run anyway** only when the file
came from the official Release page. Do not disable SmartScreen globally.

## Update behavior

LVOS checks bounded GitHub Release metadata and opens the Release page when a newer stable version
is available. It never downloads, installs, or replaces the application automatically. Quit the old
version before manually replacing it; Profile databases and settings remain in the OS application
data directory rather than inside the application package.
