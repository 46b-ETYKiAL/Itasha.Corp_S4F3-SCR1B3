# Packaging SCR1B3

Per-OS install paths. Release artifacts are built + signed by `.github/workflows/release.yml`
on a `v*` tag. Self-update is enabled only in the **portable** builds; package-manager
installs defer updates to the manager.

| OS | Channel | File | Self-update |
|----|---------|------|-------------|
| Windows | winget / MSI | `windows/ItashaCorp.SCR1B3.installer.yaml`, WiX MSI | MSI: no · portable .zip: yes |
| macOS | Homebrew cask / .dmg | `macos/scr1b3.rb`, `macos/Info.plist` | cask: no · .dmg: yes |
| Linux | AppImage / .deb / script | `linux/scr1b3.desktop`, `linux/debian-control` | AppImage: opt-in · .deb: no |
| any | one-line installer | `install.sh` | yes |

## Build commands

- **Windows MSI**: `cargo install cargo-wix --locked && cargo wix --package scribe-app --nocapture` (from the workspace; uses `crates/scribe-app/wix/main.wxs`). The release workflow builds it with `--no-build` to reuse the already-built release binary. Output: `scr1b3-<target>.msi`.
- **macOS .dmg**: build release, assemble `SCR1B3.app` with `macos/Info.plist`, then `hdiutil create` (or `create-dmg`).
- **Linux AppImage**: build release, stage with `linux/scr1b3.desktop` + icon, run `appimagetool`.
- **Linux .deb**: stage binary + `.desktop` + icon under a tree matching `linux/debian-control`, run `dpkg-deb --build`.
- **Icons**: `sh gen-icons.sh` converts the size-tiered SVG family in `assets/svg/` to `.ico` / `.icns` / Linux hicolor PNG set under `assets/icons/`. The family is the **Daemon-Seal Caret-in-Circle** (lore-council DECISION-2026-008): `app-icon.svg` master (full CRT chrome, ≥256px), `app-icon-small.svg` (chrome-stripped, ≤48px legible), `app-icon-mono.svg` (currentColor symbolic for tray / Linux-symbolic), `logomark.svg` (quiet in-app monogram). The script picks the first installed rasterizer (`resvg` preferred → `rsvg-convert` → ImageMagick `magick`) and uses `png2icns`/`icnsutil` for the macOS bundle; exits with `EX_CONFIG (78)` if none are installed so CI can install one and retry.

## Signing

See [`signing.md`](signing.md) — minisign for update authenticity, Authenticode/notarization for OS trust.

## Forge-Wire installer manifest

[`forge-wire-manifest.toml`](forge-wire-manifest.toml) describes every per-OS artifact SCR1B3 publishes so the in-house **F0RG3-W1R3** (FORGE-WIRE) cross-platform installer can pick the right one for the host. The manifest declares the product identity, per-OS artifact identifiers (winget, MSI, portable .zip, .dmg, Homebrew cask, AppImage, .deb, install.sh), icon paths (the Daemon-Seal family per lore-council DECISION-2026-008), the Start-menu / dock / launcher shortcut, the update channel (GitHub Releases + minisign verify), and a hard-zero telemetry block (no install-id, no crash reports, no analytics — matches the v1 D6 'telemetry-free' decision and the brand 'privacy-respecting' axis). Forge-Wire is the only consumer; the in-app updater reads its own GitHub Releases endpoint independently against the same embedded ed25519 public key.
