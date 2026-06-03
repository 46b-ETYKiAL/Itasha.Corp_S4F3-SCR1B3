# ADR 0005 — Theming and Customization

**Status:** Accepted

## Context

Deep customization is a first-class pillar — but it must not become bloat. Users want full control over colors, fonts, effects, and behavior without recompiling, without a heavy plugin marketplace, and without a brittle theme format that can blank the editor when it has an error.

## Decision

Customization is **config-driven and theming is a Helix-style three-namespace TOML schema**.

- **Themes are TOML** with three orthogonal tables: `[palette]` (named base colors), `[ui]` (chrome), and `[syntax]` (token scopes). Values are either `#`-hex literals (`#RGB` / `#RRGGBB` / `#RRGGBBAA`) or references to a palette name — define a color once, reference it everywhere.
- **UI-toolkit-agnostic colors.** The engine stores colors as RGBA; the render layer maps them onto egui. The engine carries no UI dependency.
- **Longest-matching-scope-wins** syntax resolution: `function.builtin.static` falls back to `function.builtin`, then `function`, then a default. Themes can be broad or finely refined.
- **Never blanks the editor.** A compiled-in default theme (`itasha-void`) is the fallback. A broken or malformed user theme surfaces an error and keeps the default rather than rendering an unusable blank screen. Missing UI keys fall back to defaults, so a theme can be small.
- **Effects are separate from themes.** The CRT/retro post-process (scanline, phosphor glow, bloom, vignette, curvature, chromatic aberration) was envisioned as a config-driven layer in `[effects]`, orthogonal to color themes so any theme could run flat or under CRT.

> **Update (not shipped):** the `[effects]` scaffold was **removed rather than shipped as dead toggles**. No GPU/WGSL post-process shader was implemented, so there is no `[effects]` config table or `EffectsConfig` in `scribe-core` — a user's `[effects]` keys would have been silently ignored. The retro aesthetic is instead carried by the color themes themselves (e.g. `phosphor-amber`, `terminal-lock`). The only motion-related config that ships is `[motion]` (`MotionConfig`), which scales egui's native animation time and is OFF by default. (This mirrors the same decision recorded in `crates/scribe-core/src/config.rs` for the per-effect motion catalog: features without a renderer implementation were dropped, not shipped as no-op toggles.) If a post-process pass lands later it will be re-introduced as a real, documented feature.
- **Appearance and behavior** (fonts, ligatures, line height, tab width, minimap, word wrap, session restore, frameless titlebar) are plain config keys — no code, no plugins required.
- **The default theme is `itasha-void`** — the brand CRT palette of void black (`#08060d`), signal cyan (`#00fffe`), and status green (`#01fe36`).

Code-loading extensibility (the user plugin/mod system) is handled separately and is capability-sandboxed; it is not required for deep customization. This keeps the not-bloated promise: most customization needs no code at all.

## Consequences

- Users theme and reconfigure live, without recompiling and without a marketplace.
- A bad theme degrades gracefully to the default; the editor is always usable.
- Themes authored for other Helix-style editors are easy to port.
- The retro aesthetic is carried by opt-in color themes (the `[effects]` post-process pass was not shipped), so it is never imposed and stays accessibility-aware.
