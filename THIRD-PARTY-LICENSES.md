# Third-Party Licenses

SCR1B3 bundles or links the following third-party assets and code. Each
license is reproduced in `licenses/` for reference; this document is the
index.

## Fonts

### JetBrains Mono

- **Source**: https://github.com/JetBrains/JetBrainsMono
- **License**: SIL Open Font License 1.1 (OFL-1.1)
- **Text**: [`licenses/OFL-1.1-JetBrainsMono.txt`](licenses/OFL-1.1-JetBrainsMono.txt)
- **Copyright**: Copyright 2020 The JetBrains Mono Project Authors

### Hack (via `epaint_default_fonts`)

- **Source**: https://github.com/source-foundry/Hack
- **License**: MIT (Hack modifications) — derived from Bitstream Vera Sans Mono
- **Text**: [`licenses/MIT-Hack.txt`](licenses/MIT-Hack.txt)
- **Copyright**: Copyright 2018 Source Foundry Authors (Hack modifications);
  Bitstream Vera font is dedicated to the public domain by Bitstream Inc.

### Ubuntu Mono / Ubuntu Sans (via `epaint_default_fonts`)

- **Source**: https://design.ubuntu.com/font/
- **License**: Ubuntu Font Licence 1.0 (Ubuntu-font-1.0) — a permissive
  font license equivalent in spirit to OFL-1.1
- **Text**: bundled inside `epaint_default_fonts`; refer to the upstream
  `epaint_default_fonts` crate for the full license text
- **Notice**: This is the legal notice required by the Ubuntu Font Licence
  §3 for redistribution as part of a software bundle.

## Rust Dependencies

The full transitive license inventory for Cargo dependencies is generated
at release time via [`cargo-about`](https://github.com/EmbarkStudios/cargo-about)
and shipped inside the installer payload:

- **Windows**: `THIRD-PARTY-LICENSES-RUST.html` alongside the `.exe` in
  the install directory.
- **macOS**: inside the `.app` bundle's `Contents/Resources/`.
- **Linux** (DEB / AppImage): under `/usr/share/doc/scr1b3/` or
  `usr/share/doc/scr1b3/` inside the AppImage.

The allow-list and rejection rules for licenses on the dependency
graph are enforced in CI via [`deny.toml`](deny.toml) (`cargo deny check`).
Each commit blocks any dependency whose license is outside the
allow-list.

## License Compatibility

All bundled third-party licenses (MIT, Apache-2.0, BSD-2/3-Clause, ISC,
Zlib, Unicode-3.0, MPL-2.0, CC0-1.0, BSL-1.0, OFL-1.1, Ubuntu-font-1.0)
are permissive and compatible with SCR1B3's own MIT OR Apache-2.0
dual-license. No GPL-licensed code is bundled or linked.

## Reporting

If you believe a third-party license is missing from this inventory or
is mis-attributed, please open an issue or follow the disclosure
process in [`SECURITY.md`](SECURITY.md).
