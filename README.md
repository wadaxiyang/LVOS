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

LVOS 0.1.7 uses Quadrant-Kit 0.1.1. Full native acceptance and the distribution license
compatibility decision remain incomplete. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)
and [KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md) for the outstanding limitations.

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

For package resource verification, `LVOS.exe --ui-smoke` (or the macOS executable with the same
flag) creates synthetic windows and exits after rendering all three. It bypasses profile,
credential, update, hotkey, and service initialization. This checks packaged UI resources; it
does not validate the live tray, capture permissions, external services, or accessibility.
`python scripts/check_native_package.py` builds and deeply verifies the native package without
publishing it. On Windows it compiles the installable EXE, installs it into a temporary custom
directory, checks its Start Menu shortcut and notices, then uninstalls it. On macOS it mounts the
DMG and checks the Agent/UI app bundle, notices, signature, and Applications drag target.

The render diagnostic accepts `dark`, `reduced`, `narrow`, and `collapsed` options after its
output path; `SLINT_SCALE_FACTOR` can exercise synthetic scale changes. These are separate
from actual mixed-monitor testing. `ui_performance_check` provides an identical synthetic
workload for pre-Kit and migrated builds. On Windows, `scripts/measure-ui-performance.ps1`
collects three baseline runs before three candidate runs, including CPU, memory and 120
show/hide cycles. Use matching release configuration and run without competing builds/UI
fixtures. Its observed baseline envelope is exploratory and does not constitute an approved
performance budget.

Phase 5 uses `scripts/measure-phase5-performance.ps1` against optimized Agent/UI binaries. It
measures rendered cold and warm lookup latency, Agent-only memory, 500 Popup lifecycle cycles,
Main UI churn, appearance variants, and automatic GUI exit. The current measurements and exact
scope are recorded in [PERFORMANCE_BASELINE.md](PERFORMANCE_BASELINE.md).
