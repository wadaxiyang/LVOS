# LVOS 0.1.6 unsigned release notes

This release migrates the desktop to Quadrant-Kit 0.1.1 Fluent components.
Remaining native acceptance gaps are documented in [KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md).
Dependency license compatibility and audit questions remain open as recorded in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md); publication does not resolve them.

Both platform ZIPs include a top-level `NOTICES/` directory with the LVOS license, the desktop
dependency notice, the Kit GPL license and derivation notice, and the Fluent System Icons MIT
license. Windows retains `LVOS.exe`; macOS retains the ad-hoc signed `LVOS.app`. The ZIP creator
and candidate verifier require these notice files; do not strip them when redistributing.

## Highlights

- Unified Kit navigation, History and Favorites lists, and a Fluent settings page with a single
  scrolling layout instead of a second navigation sidebar.
- Kit password fields for credentials, with temporary field contents cleared when leaving or saving.
- Kit lookup, permission, progress, error and confirmation UI; stronger popup focus and dismissal
  handling and modal lifecycle protection.
- Official Git dependency pinned to Kit 0.1.1, embedded UI assets, and isolated server-only builds.
- Required dependency notices and verification in both platform archives.
- Existing local lookup, optional private-server synchronization, portable data and manual updates
  remain available.

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
