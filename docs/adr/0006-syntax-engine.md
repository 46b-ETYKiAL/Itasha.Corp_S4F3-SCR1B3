# ADR 0006 — Syntax Highlighting Engine

**Status:** Accepted

## Context

SCR1B3 needs syntax highlighting for a broad range of languages on day one, across three platforms, with no fragile build step and a small dependency surface. Two mainstream approaches exist in the Rust ecosystem:

- **syntect** — a pure-Rust engine that consumes Sublime/TextMate `.sublime-syntax` and `.tmTheme` definitions, with ~100 languages bundled. No C toolchain, no per-grammar compilation.
- **tree-sitter** — incremental parsers that produce a real syntax tree, enabling faster re-highlighting on edit and structural features (folding by structure, selection by node, structural navigation). Grammars are typically C and need a build step.

## Decision

**syntect is the v1 syntax engine.** tree-sitter is the planned structural enhancement, layered behind the same API.

Rationale for syntect first:

- **No build step.** Pure Rust with bundled syntaxes means the workspace builds and ships cleanly on Windows, Linux, and macOS without a C toolchain or grammar compilation — directly serving the not-bloated and deliverability constraints.
- **Breadth on day one.** 100+ languages are available immediately via the bundled `SyntaxSet`, with extension-based syntax resolution and a plain-text fallback.
- **Standard formats.** Existing TextMate/Sublime syntaxes work unchanged, so users and the ecosystem can extend coverage without bespoke grammars.

The public API is **engine-agnostic** — callers consume a `Highlighter` that returns per-line color spans (`HlSpan`). This means a **tree-sitter incremental backend can be added behind the same interface** for faster on-edit re-highlighting and structural features, without changing any caller. tree-sitter is positioned as a structural *enhancement* on top of the working syntect baseline, not a v1 dependency.

For very large files, the caller highlights only the visible window rather than the whole document, keeping highlight cost bounded.

## Consequences

- Highlighting works out of the box for 100+ languages with zero build friction.
- The dependency surface stays small and the cross-platform build stays simple.
- The engine seam is stable: introducing tree-sitter later is an internal change behind `Highlighter`, transparent to the renderer and app.
- Where a syntect grammar is missing, the editor falls back to plain text rather than failing.
