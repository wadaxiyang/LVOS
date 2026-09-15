# Skia Migration Behavior Baseline

This document freezes the observable desktop behavior that must remain unchanged while LVOS
migrates from FemtoVG to Skia. It is a regression contract, not a new performance measurement.

## Scope

The baseline applies to the `lvos-ui` process on Windows and macOS. The background Agent remains
separate and starts the UI process only when a graphical surface is requested.

## Quick Lookup Popup

- The popup is frameless and always on top.
- Showing the popup does not activate it or steal focus from the source application.
- Escape, an outside click, and the existing dismiss actions hide the popup.
- Favorite, copy, and refresh actions remain available.
- Loading, ready, and error states render with the same behavior and content.
- The popup is constructed lazily on first use.
- A dismissed popup enters warm retention for the configured idle timeout and can be reused during
  that interval.
- When the timeout expires, the popup host is dropped rather than retained indefinitely.
- Native outside-click monitors are removed on dismiss and installed for the current native window
  whenever the popup is shown again.

## Main Window

- The management window is constructed on demand and can be reopened after closing.
- Closing the management window releases its UI host and does not stop the Agent.
- History, Favorites, and Settings retain their current navigation and interactions.
- Settings section navigation preserves the expected scroll position.
- Closing a modal restores focus to the expected control.

## Permission Window

- The permission window is constructed only when requested.
- Closing it releases its host.
- A later request constructs and shows a new permission window successfully.

## UI Process Lifecycle

- Starting the Agent does not eagerly create a Slint window or start `lvos-ui`.
- A UI request starts one `lvos-ui` process and creates only the requested host.
- Main, popup, and permission hosts have independent lifetimes.
- When no host remains, the existing idle-exit handshake is preserved and the UI process exits
  after Agent approval.

## Renderer-Migration Acceptance

The migration is acceptable only if repeated show/hide/show cycles preserve popup placement,
non-activation, click interaction, outside-click dismissal, and Escape dismissal. Main and
permission windows must also survive close/reopen cycles. Dark mode, DPI scaling, CJK text, emoji
fallback, and existing SVG icon rendering must remain functional.

Automated unit and policy checks cover the platform-independent portions of this contract. Native
window behavior must additionally be exercised with the repository's interactive diagnostics on
each supported operating system.
