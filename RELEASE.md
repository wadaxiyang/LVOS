# LVOS 0.1.7 unsigned release notes

This release separates the long-running native Agent from the on-demand Slint UI process and
publishes installable desktop packages.
Remaining native acceptance gaps are documented in [KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md).
Dependency license compatibility and audit questions remain open as recorded in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md); publication does not resolve them.

Both packages include the LVOS license, desktop dependency notice, Kit GPL license and derivation
notice, and Fluent System Icons MIT license. Windows installs the Agent entry point `LVOS.exe`
beside `lvos-ui.exe`; macOS keeps the Agent as `LVOS.app/Contents/MacOS/LVOS` beside its `lvos-ui`
helper. Native package checks verify this process pair and the notice inventory.

## Highlights

- Unified Kit navigation, History and Favorites lists, and a Fluent settings page with a single
  scrolling layout instead of a second navigation sidebar.
- Kit password fields for credentials, with temporary field contents cleared when leaving or saving.
- Kit lookup, permission, progress, error and confirmation UI; stronger popup focus and dismissal
  handling and modal lifecycle protection.
- Official Git dependency pinned to Kit 0.1.1, embedded UI assets, and isolated server-only builds.
- Required dependency notices and verification in both platform archives.
- Background capture, tray, hotkey, sync and storage now live in the Agent process. Slint windows
  run in the authenticated, on-demand UI helper.
- Popup, Main UI and Permission UI are created lazily; the UI helper exits after every host is
  gone. Query presentation acknowledgements measure actual rendered cold/warm latency.
- Phase 5 records 10 cold starts, 500 Popup lifecycle cycles, Agent-only memory, theme/scale
  variants and automatic GUI exit in `PERFORMANCE_BASELINE.md`.
- Windows now uses an installer with a selectable destination, mandatory Start Menu entry and an
  optional desktop shortcut. macOS now uses a drag-to-Applications DMG.
- The update descriptor is version 2 because release artifacts changed from archives to native
  installer formats. Existing versions can still be updated manually from the Release page.
- Existing local lookup, optional private-server synchronization, portable data and manual updates
  remain available.

## Verify the download

LVOS V1 desktop packages are intentionally unsigned. Download artifacts only from the
[`wadaxiyang/LVOS` GitHub Releases page](https://github.com/wadaxiyang/LVOS/releases), then compare
the file's SHA-256 with `SHA256SUMS` and `lvos-update-stable.json` before opening it.

Expected assets for the published version:

- `LVOS-<version>-macos-arm64.dmg`
- `LVOS-<version>-windows-x86_64-setup.exe`
- `lvos-update-stable.json`
- `SHA256SUMS`

## macOS 15 arm64

Open the DMG and drag `LVOS.app` to Applications. It requires Apple silicon and macOS 15 or newer.
Because it is not Developer ID signed or notarized, Gatekeeper may block the first launch. After
verifying the download, use Finder's **Open** context-menu action and confirm the one-app warning.
Do not disable Gatekeeper globally.

## Windows 11 x86_64

Run the setup EXE. The wizard lets you choose the installation directory and Start Menu folder,
and offers an optional desktop shortcut. LVOS is always registered in the selected Start Menu
folder and includes an uninstaller. Microsoft Defender SmartScreen may show an unknown publisher
warning. After verifying SHA-256, use **More info** and **Run anyway** only when the file came from
the official Release page. Do not disable SmartScreen globally.

## Update behavior

LVOS checks bounded GitHub Release metadata and opens the Release page when a newer stable version
is available. It never downloads, installs, or replaces the application automatically. Quit the old
version before running the installer or replacing the app; Profile databases and settings remain
in the OS application data directory rather than inside the application package.

Before upgrading, quit LVOS and make a Portable Data export if desired. Private Server operators
should also create a consistent Server backup before rebuilding.

See the [V1 known limitations](https://github.com/wadaxiyang/LVOS/blob/main/KNOWN_LIMITATIONS.md)
before distribution.
