# LVOS unsigned release notes

This is a stable LVOS V1 release for Windows 11 x86_64 and macOS 15 arm64.

## Highlights

- Native global selection capture with a compact, non-intrusive Lookup Card.
- Local-first, deduplicated History, QueryStats, Favorites, search, and clear behavior.
- User-configured Tencent TokenHub and Google translation Providers with native credential storage.
- Optional private multi-User Server with persistent login, Device control, offline Outbox,
  revisioned bidirectional Favorite/QueryStats sync, migration, backup, and recovery.
- Versioned Portable Data export/import and bounded GitHub Release update discovery.

## Verify the download

LVOS V1 desktop packages are intentionally unsigned. Download artifacts only from the
[`wadaxiyang/LVOS` GitHub Releases page](https://github.com/wadaxiyang/LVOS/releases), then compare
the file's SHA-256 with `SHA256SUMS` and `lvos-update-stable.json` before opening it.

Expected assets for the published version:

- `LVOS-<version>-macos-arm64.zip`
- `LVOS-<version>-windows-x86_64.zip`
- `lvos-update-stable.json`
- `SHA256SUMS`

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

Before upgrading, quit LVOS and make a Portable Data export if desired. Private Server operators
should also create a consistent Server backup before rebuilding.

See the [V1 known limitations](https://github.com/wadaxiyang/LVOS/blob/main/KNOWN_LIMITATIONS.md)
before distribution.
