# LVOS V1 known limitations

- The Kit 0.1.1 desktop migration has Windows native popup and synthetic interaction evidence,
  including 120 production popup cycles, theme/scale renders, reduced-motion confirmations,
  and an isolated embedded-resource smoke run. The Windows/macOS Cargo CI matrix also passes
  at the S6 boundary. These checks do not establish macOS native permission/focus recovery,
  real IME/clipboard editing, screen-reader modal behavior, or mixed-display DPI acceptance.
- The Agent/UI process split is packaged with the Agent as the macOS bundle executable, but the
  required macOS 15 arm64 real-device acceptance still has to confirm that Accessibility consent
  and the actual selection-capture action are attributed to that Agent identity after restart.
- A Windows release workload (200 synthetic records, page/scroll activity, 120 generated-window
  cycles, ten seconds idle; three runs per revision) showed no clear regression outside its
  observed baseline envelope. It is a short diagnostic, not a long-duration memory test or an
  approved release performance budget. Actual service/tray workflows remain separate checks.
- Kit 0.1.1 InfoBar does not size its internal border correctly around wrapped messages.
  LVOS uses a short Kit summary plus scrollable full business detail, dismissed together.
- The PolyForm/GPL distribution license compatibility decision and the remaining dependency
  license audit are unresolved. Release ZIPs include the minimum Kit notice inventory;
  publication and inclusion of those texts do not settle these questions. See
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
