# Repository Discoverability

Levers to make SCR1B3 easy to find and evaluate on GitHub.

## Repository description (one line, SEO)

> SCR1B3 — a fast, GPU-rendered, telemetry-free code & text editor for Windows, Linux, and macOS. A modern Notepad++ alternative in Rust: multi-GB large files, 100+ language syntax highlighting, deep TOML theming, no account, no telemetry.

## Topics / tags

Set these in the repo's "About" → Topics:

```
text-editor code-editor rust egui wgpu cross-platform windows linux macos
notepad-alternative syntax-highlighting ropey syntect telemetry-free privacy
gpu-accelerated large-files themeable crt retro desktop-app lain
```

## Social preview image

Upload `assets/svg/social-preview.svg` (rasterize to 1280×640 PNG via any
SVG→PNG tool) under Settings → Social preview. This is the card shown when the
repo is shared.

## README hero

`README.md` references `assets/svg/banner.svg` as the hero image and links the
per-OS install one-liners high above the fold.

## Release artifacts

Tag-driven releases (`v*`) attach per-OS binaries + installers + `SHA256SUMS`
so the "Releases" page and `packaging/install.sh` resolve the latest build
automatically.

## Recommended GitHub settings

- Enable **Issues** and **Discussions**.
- Add the description + topics above.
- Pin a "Getting started" discussion linking the install section.
- Add `good first issue` / `help wanted` labels for contributors.
