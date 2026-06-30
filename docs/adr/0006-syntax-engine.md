# ADR 0006 — Syntax Highlighting Engine

**Status:** Accepted

## Context

SCR1B3 needs syntax highlighting for a broad range of languages on day one, across three platforms, with no fragile build step and a small dependency surface. Two mainstream approaches exist in the Rust ecosystem:

- **syntect** — a pure-Rust engine that consumes Sublime/TextMate `.sublime-syntax` and `.tmTheme` definitions, with ~100 languages bundled. No C toolchain, no per-grammar compilation.
- **tree-sitter** — incremental parsers that produce a real syntax tree, enabling faster re-highlighting on edit and structural features (folding by structure, selection by node, structural navigation). Grammars are typically C and need a build step.

## Decision

**Both engines ship in v1 behind one engine-agnostic API.** tree-sitter is the **primary** highlighter for languages with a native grammar wired in (Rust today), producing a concrete-syntax-tree highlight pass; **syntect is the pure-Rust fallback** covering the ~100 bundled languages without one.

Rationale for the syntect baseline:

- **No build step.** Pure Rust with bundled syntaxes means the workspace builds and ships cleanly on Windows, Linux, and macOS without a C toolchain or grammar compilation — directly serving the not-bloated and deliverability constraints.
- **Breadth on day one.** 100+ languages are available immediately via the bundled `SyntaxSet`, with extension-based syntax resolution and a plain-text fallback.
- **Standard formats.** Existing TextMate/Sublime syntaxes work unchanged, so users and the ecosystem can extend coverage without bespoke grammars.

The public API is **engine-agnostic** — callers consume a `Highlighter` that returns per-line color spans (`HlSpan`) and never see which engine ran. **tree-sitter ships as the primary structural backend** (concrete syntax tree → per-line spans) for grammars that are wired, with **syntect as the pure-Rust fallback** — both behind the same interface, so the choice of engine per language is transparent to the renderer and app.

For very large files, the caller highlights only the visible window rather than the whole document, keeping highlight cost bounded.

## Consequences

- Highlighting works out of the box for 100+ languages with zero build friction.
- The dependency surface stays small and the cross-platform build stays simple.
- The engine seam is stable: which engine highlights a given language is an internal choice behind `Highlighter`, transparent to the renderer and app.
- Where a syntect grammar is missing, the editor falls back to plain text rather than failing.
