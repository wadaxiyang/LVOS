# Renderer architecture

LVOS owns its renderer selection in the desktop application. Quadrant-Kit supplies reusable
Slint components and assets, but it must remain renderer-neutral.

## Supported platform paths

| Platform | Window backend | Renderer | Graphics API |
| --- | --- | --- | --- |
| Windows | Winit | Skia | Direct3D 12 |
| macOS | Winit | Skia | Metal |

The desktop backend selector explicitly requests both layers. Windows calls `require_d3d()` and
macOS calls `require_metal()`; choosing Skia while relying on an implicit graphics-API fallback is
not supported. WGPU, Vulkan, direct `skia-safe` use, and private `i-slint-*` renderer crates are
outside the application architecture.

## Window and process lifecycle

The Agent starts `lvos-ui` on demand and injects `SLINT_DESTROY_WINDOW_ON_HIDE=1` into that child
process. Hiding a Slint window therefore destroys its native window and associated graphics
surface instead of retaining those resources indefinitely. The Rust Slint component remains lazy:
the popup, main window, and permission window are created only when first requested.

Every subsequent show can create a new native window. LVOS must reapply popup behavior after each
recreation, including no-activate policy, taskbar exclusion, placement, Escape dismissal, outside
click dismissal, and foreground restoration. When all hosts are hidden and no operation is
pending, the existing Agent/UI idle-exit handshake is still responsible for terminating the UI
process.

## Resources and fonts

Application artwork remains SVG so Slint and Skia own decoding and caching. LVOS does not add a
parallel image cache or configure Skia private cache APIs. UI windows use the empty Slint font
family to preserve the platform system-font fallback; large bundled CJK fonts are not part of the
default renderer policy.

## Version and upgrade policy

Slint is pinned exactly to 1.17.1 because destroy-on-hide, Winit window recreation, and Skia
surface lifecycle behavior are version-sensitive. Before changing that pin, re-run the native
popup recreation checks on Windows and macOS and review the generated dependency graph for
renderer changes.

Run `python scripts/check_renderer_policy.py` for the static architecture contract. The complete
pre-commit suite also runs this check, and CI runs it on both supported operating systems. Native
interactive checks remain required for lifecycle behavior that compilation cannot exercise.
