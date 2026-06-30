# Packaging SCR1B3

Per-OS install paths. Release artifacts are built + signed by `.github/workflows/release.yml`
on a `v*` tag. Self-update is enabled only in the **portable** builds; package-manager
installs defer updates to the manager.

| OS | Channel | File | Self-update |
|----|---------|------|-------------|
| Windows | release `setup.exe` · winget / wix (local) | `scr1b3-<tag>-x86_64-setup.exe` (FORGE-WIRE native installer) · `windows/ItashaCorp.SCR1B3.installer.yaml`, `wix/main.wxs` (per-user) | setup.exe / portable .zip: yes · winget/wix: no |
| macOS | Homebrew cask / .dmg | release `scr1b3-<tag>-aarch64.dmg` (built by the `macos-installer` job) · `macos/scr1b3.rb`, `macos/Info.plist` | cask: no · .dmg: yes |
| Linux | AppImage / .deb / script | release `SCR1B3-<tag>-x86_64.AppImage` + `scr1b3-<tag>-amd64.deb` (built by the `linux-installers` job) · `linux/scr1b3.desktop`, `linux/debian-control` | AppImage: opt-in · .deb: no |
| any | one-line installer | `install.sh` | yes |

## Build commands

- **Windows release installer**: the release workflow (`release.yml`) builds the self-elevating **FORGE-WIRE** native installer `scr1b3-<tag>-x86_64-setup.exe` via `framework/scripts/build_native_installer.py` — **not** a stock NSIS/MSI. This is the artifact published on a `v*` tag.
- **Windows local/winget MSI** (separate channel): `cargo install cargo-wix --locked && cargo wix --package scribe-app --nocapture` (from the workspace; uses `crates/scribe-app/wix/main.wxs`, a per-user install). This is a local/winget-only artifact; the release workflow does **not** produce an MSI.
- **macOS .dmg**: the release workflow's `macos-installer` job builds `scr1b3-<tag>-aarch64.dmg` — it assembles `SCR1B3.app` from `macos/Info.plist` (version substituted from `Cargo.toml`) + a `.icns` generated on the runner from the 1024px master PNG via `iconutil`, then `hdiutil create -format UDZO` with a drag-to-`/Applications` layout. (Local equivalent: same steps, or `create-dmg`.)
- **Linux AppImage**: the release workflow's `linux-installers` job builds `SCR1B3-<tag>-x86_64.AppImage` — it stages an `AppDir` from `linux/scr1b3.desktop` + the hicolor icon set + a printf-generated `AppRun`, then runs **pinned** `appimagetool` 1.9.1 (sha256-verified before exec) with `--appimage-extract-and-run` (no FUSE needed on CI).
- **Linux .deb**: the same `linux-installers` job builds `scr1b3-<tag>-amd64.deb` via `dpkg-deb --build --root-owner-group` over a tree of binary + `.desktop` + hicolor icons, with `linux/debian-control` as the control template (version + `Installed-Size` filled in at build time).
- **Icons**: `sh gen-icons.sh` converts the size-tiered SVG family in `assets/svg/` to `.ico` / `.icns` / Linux hicolor PNG set under `assets/icons/`. The family is the **Daemon-Seal Caret-in-Circle** (lore-council DECISION-2026-008): `app-icon.svg` master (full CRT chrome, ≥256px), `app-icon-small.svg` (chrome-stripped, ≤48px legible), `app-icon-mono.svg` (currentColor symbolic for tray / Linux-symbolic), `logomark.svg` (quiet in-app monogram). The script picks the first installed rasterizer (`resvg` preferred → `rsvg-convert` → ImageMagick `magick`) and uses `png2icns`/`icnsutil` for the macOS bundle; exits with `EX_CONFIG (78)` if none are installed so CI can install one and retry.

## Signing

See [`signing.md`](signing.md) — minisign for update authenticity, Authenticode/notarization for OS trust.

## Forge-Wire installer manifest

[`forge-wire-manifest.toml`](forge-wire-manifest.toml) describes every per-OS artifact SCR1B3 publishes so the in-house **F0RG3-W1R3** (FORGE-WIRE) cross-platform installer can pick the right one for the host. The manifest declares the product identity, per-OS artifact identifiers (winget, MSI, portable .zip, .dmg, Homebrew cask, AppImage, .deb, install.sh), icon paths (the Daemon-Seal family per lore-council DECISION-2026-008), the Start-menu / dock / launcher shortcut, the update channel (GitHub Releases + minisign verify), and a hard-zero telemetry block (no install-id, no crash reports, no analytics — matches the v1 D6 'telemetry-free' decision and the brand 'privacy-respecting' axis). Forge-Wire is the only consumer; the in-app updater reads its own GitHub Releases endpoint independently against the same embedded ed25519 public key.
