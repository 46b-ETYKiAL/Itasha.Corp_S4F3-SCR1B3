# ADR 0001 — Stack and Architecture

**Status:** Accepted

## Context

SCR1B3 is a standalone, cross-platform (Windows / Linux / macOS) code and text editor — a modern "better Notepad++". It must be fast and responsive, memory-light, not bloated (no embedded browser), able to open multi-gigabyte files without freezing, telemetry-free, secure (minimal attack surface), modern (syntax highlighting, find/replace, multi-tab), and deeply customizable. These constraints rule out webview/Chromium-based stacks, which carry a large runtime, a wide attack surface, and a telemetry-by-default heritage.

## Decision

- **Language: Rust.** Memory-safe, fast, single-binary distribution, excellent cross-platform tooling.
- **UI shell: egui via eframe** (`eframe = winit + wgpu + egui`, the official bundle). egui has a mature ecosystem, trivial runtime theming via its `Visuals`/`Style` model, and runs on **wgpu**, so a real GPU post-process (the CRT shader) is a fragment-shader pass rather than faked CSS.
- **Text engine: [ropey](https://crates.io/crates/ropey) 1.x.** A rope buffer gives single-digit-microsecond edits on multi-megabyte-to-gigabyte texts. (1.x is battle-tested; 2.0 is beta and not adopted.)
- **Large files: [memmap2](https://crates.io/crates/memmap2).** Files at or above a threshold (256 MiB) open read-only via `mmap` for browsing; the first edit copies needed text into the rope, so editing stays fast and the on-disk file is never mutated underneath the user. This defeats the 2 GB cap / 4×-RAM failure mode of legacy editors.
- **Encoding: [encoding_rs](https://crates.io/crates/encoding_rs) + [chardetng](https://crates.io/crates/chardetng).** Statistical detection plus BOM sniffing; UTF-8 is the canonical in-memory form; the original encoding and BOM are preserved for round-trip saves.
- **Workspace shape:** four crates — `scribe-core` (engine: buffer, file I/O, encoding/EOL, config, theme, syntax, search, update logic; **no UI dependency**), `scribe-render` (maps the engine `Theme` onto egui `Visuals` and carries CRT parameters), `scribe-app` (the binary: window, tabs, find bar, status bar, frameless titlebar), and `scribe-win32-chrome` (the only `unsafe`-permitted crate: a thin Win32 FFI shim for the frameless-titlebar chrome on Windows; every other crate is `#![forbid(unsafe_code)]`). The core/UI seam is kept clean so the rendering shell is replaceable without re-architecting the engine.

Tauri and Electron were rejected: a system webview / Chromium runtime contradicts the not-bloated, fast-startup, minimal-attack-surface, and telemetry-free constraints.

## Consequences

- A tiny, native, GPU-rendered binary with no embedded browser and a small dependency surface.
- The engine (`scribe-core`) is independently testable and the UI is swappable.
- The team owns a wgpu/egui rendering path shared in spirit with sibling Itasha.Corp tools, enabling a coherent CRT visual system.
- A future renderer change (should egui's large-scroll performance ever fall short) is contained to `scribe-render` + `scribe-app`; `scribe-core` is unaffected.
