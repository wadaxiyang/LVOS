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

LVOS is source-available under the unmodified
[PolyForm Noncommercial License 1.0.0](LICENSE). It is not OSI open source.
