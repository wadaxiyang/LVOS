# LVOS V1 known limitations

- The Kit 0.1.1 desktop migration has Windows synthetic interaction evidence. macOS native
  permission/focus recovery, real IME and clipboard editing, screen-reader modal behavior,
  mixed-display DPI, and release performance acceptance are still pending. Compilation and
  screenshots do not establish those results.
- Kit 0.1.1 InfoBar does not size its internal border correctly around wrapped messages.
  LVOS uses a short Kit summary plus scrollable full business detail, dismissed together.
- Public distribution of the migrated desktop is pending the PolyForm/GPL license decision
  and the remaining dependency notice audit. Local engineering ZIPs include the minimum Kit
  notice inventory; their existence does not make a release ready. See
  [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

- Desktop release targets are limited to Windows 11 x86_64 and macOS 15 arm64. Linux, Intel macOS,
  older Windows/macOS versions, mobile clients, and Web/PWA clients are not supported in V1.
- Desktop artifacts are unsigned. macOS is ad-hoc signed only to stabilize local bundle identity;
  it is not Developer ID signed or notarized. Windows uses a portable executable without an
  Authenticode signature or installer.
- Windows portable notifications may use the system notifier fallback because V1 does not install
  a Start Menu shortcut carrying a branded AppUserModelID.
- Provider API availability, model availability, pricing, quotas, and language support are owned by
  Tencent TokenHub and can change. Users supply and pay for their own API keys.
- The Server is privately deployed through Docker Compose and has no registration page, public Web
  UI, bundled TLS, or bundled reverse proxy. Operators own HTTPS and network exposure.
- Updates are manual. LVOS opens the official GitHub Release page but never downloads, installs, or
  replaces itself.
- Favorite tombstones, change-log records, and processed sync events are retained indefinitely in
  V1; automatic garbage collection is intentionally absent.
- Enrichment, dictionary corpora, LLM analysis, LearningState, SRS, ReviewHistory, tags, notes,
  Responsive Web/PWA, public registration, and Server administration UI are out of scope.

Report reproducible problems with the target OS, LVOS version, and redacted diagnostics. Never
include passwords, API keys, Access Tokens, Refresh Tokens, or private exported data.
