# Desktop dependency notices

LVOS retains PolyForm Noncommercial 1.0.0 and Quadrant-Kit declares GPL-3.0-only.
Their distribution license compatibility decision remains unresolved. Including these texts
with the release does not resolve that decision or grant additional rights.
The Slint and remaining Rust dependency license audit is also incomplete.

The desktop embeds Slint UI and image assets from Quadrant-Kit **0.1.1**, obtained as a build
dependency from https://github.com/wadaxiyang/Quadrant-Kit.git at
`20cc9d77b737d1326d4320a42f7d26f9a799298f`. Build-only consumption does not remove the embedded
code or its license obligations. No sibling checkout or vendored implementation is used.

The following unchanged upstream texts are retained in `licenses/`:

- `Quadrant-Kit-GPL-3.0.txt`: upstream LICENSE.
- `Quadrant-Kit-NOTICES.md`: upstream derivation/copyright and dependency notices.
- `Fluent-System-Icons-MIT.txt`: Microsoft Fluent UI System Icons license.

The deterministic Windows and macOS archives include this notice, the LVOS license, and those
three texts under top-level `NOTICES/`, beside `LVOS.exe` or `LVOS.app`. The native binary/bundle
identity and signing policy remain unchanged. Package verification rejects missing or modified
notice files. These files are a minimum inventory for the newly embedded Kit assets, not an
assertion that the full desktop dependency audit is complete.
