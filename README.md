# LVOS

LVOS (Lightweight Vocabulary Overlay & Sync) is a lightweight, local-first desktop lookup,
collection, and synchronization tool. Select English text in another application, press the global
shortcut, and LVOS presents a compact Chinese translation card while retaining deduplicated local
History and optional synchronized Favorites.

## V1 platforms and behavior

- Windows 11 x86_64: default shortcut `Alt+D`.
- macOS 15 arm64: default shortcut `⌥D` and Accessibility permission for selection capture.
- Tencent TokenHub is the sole V1 translation Provider. Its model defaults to `hy-mt2-lite`, and
  the user's API key is stored in the OS Credential Store.
- History stays local. Only content that enters the Favorite synchronization domain and its
  Device-scoped QueryStats can be uploaded to the user's private Server.
- Each account has an isolated local Profile database. The application remains useful for cached
  lookup and local data when the Server is unavailable.
- On Windows, LVOS temporarily replaces the current clipboard and restores text only; images,
  files, and rich-text clipboard formats are not restored.

## Installation

Download only from the [official GitHub Releases page](https://github.com/wadaxiyang/LVOS/releases)
and verify `SHA256SUMS` before opening the archive. V1 artifacts are intentionally unsigned; follow
the target-specific instructions in [RELEASE.md](RELEASE.md) without disabling OS security
features globally.

The Desktop has no first-run Provider wizard. Open **Settings → Translation**, enter at least the
TokenHub API key, and save. The private Server is optional for local lookup and is
required only for account/device synchronization.

## Privacy and security boundary

Provider keys and persistent Server credentials are stored in the native OS Credential Store, not
SQLite, settings JSON, exports, or release packages. Portable Export contains user data but excludes
passwords, credentials, Device identity, Sessions, Outbox events, and sync cursors.

## Documentation

Private Server deployment and recovery: [DEPLOYMENT.md](DEPLOYMENT.md)

Unsigned desktop release verification and manual update behavior: [RELEASE.md](RELEASE.md)

V1 limitations and excluded features: [KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md)

## License

The current desktop source uses Quadrant-Kit 0.1.1 and is pending full native acceptance and a
distribution license decision. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and
[KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md) before packaging this migration.

LVOS is source-available under the unmodified
[PolyForm Noncommercial License 1.0.0](LICENSE). It is not OSI open source.

## Build the Kit desktop

Use the checked-in Rust toolchain and `cargo build --locked -p lvos` on a supported host.
`apps/desktop/Cargo.toml` consumes the official Quadrant-Kit Git repository as a build dependency,
with exact version `=0.1.1` and full revision `20cc9d77b737d1326d4320a42f7d26f9a799298f`.
No adjacent Kit checkout is needed. Cargo's Git cache is allowed; sibling paths, patches,
replacement sources, and copied UI implementations are rejected by `scripts/check_kit_adoption.py`.

The official build helper exposes `@quadrant-kit`; Slint 1.17.1 compiles Fluent UI and embeds
its assets into the binary. Production uses winit/femtovg and the platform default font. The
pinned winit 0.30 accessor is enabled to create popup windows without activation. Generic
controls, settings rows, navigation, feedback and confirmations come from Kit. The remaining
local compositions bind LVOS records/settings, and seven product SVGs cover brand or semantic
icons unavailable in Kit's public 0.1.1 set.

Run `scripts/check-before-commit.sh` with its documented cross-build prerequisites, or run its
native Cargo/Python checks individually. CI runs locked checks on Windows and macOS. In an
interactive desktop session run `ui_navigation_check`, `ui_modal_check`, and (Windows)
`ui_window_check` with `cargo run --locked -p lvos --example <name>`. Run these diagnostics
sequentially: parallel windows can steal focus. They use synthetic data, and compiling them
does not count as executing their native interactions. `ui_render_check <scene> <output.ppm>`
renders synthetic pages and popup states. Server-only builds use `Cargo.server.toml` and
`Cargo.server.lock` as in the Dockerfile; they do not consume Kit or Slint.
