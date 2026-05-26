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
- **Icons**: `sh gen-icons.sh` converts `assets/svg/app-icon.svg` → `.ico` / `.icns` / PNG set (needs rsvg-convert/ImageMagick/iconutil).

## Signing

See [`signing.md`](signing.md) — minisign for update authenticity, Authenticode/notarization for OS trust.
