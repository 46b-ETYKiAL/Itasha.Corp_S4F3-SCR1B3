# SCR1B3 Appearance Parity — Result

Branch: `feat/appearance-parity` → **PR #340** (base `master`)
https://github.com/46b-ETYKiAL/Itasha.Corp_S4F3-SCR1B3/pull/340

## Per-task summary

### 1. Stationary theme prev/next arrows — DONE
`crates/scribe-app/src/settings.rs` (theme row). Added left/right Phosphor caret
(`CARET_LEFT`/`CARET_RIGHT`) buttons that cycle the built-in themes in place
(`rem_euclid` wraparound; a custom theme name lands on first/last built-in,
mirroring C0PL4ND). **Stationarity** is guaranteed by pinning the theme
`ComboBox` between the two buttons to a fixed `.width(168.0)`, so a longer theme
name is clipped inside the box instead of shifting the "next" arrow. Cycling
clears both background overrides, matching the dropdown path.

### 2. App-name recolors BOTH halves — DONE
`crates/scribe-app/src/app/frame_tick.rs`. Root cause: the split-tone wordmark
colored `S C R ` with the theme accent but `1 B 3` with a FIXED violet fallback
because **no built-in theme defines `accent_alt`** (verified: 0 occurrences in
`theme.rs`). The `accent_alt` fallback now derives from the active theme's
`keyword` syntax hue (`self.theme.syntax_color("keyword", …)`) — defined by every
theme and chosen to contrast the accent — so both halves recolor on theme change.
A theme may still set `accent_alt` explicitly to override.

### 3. Lower window-opacity floor — ALREADY AT MAXIMUM (no change)
Verified the opacity slider (`settings.rs`), `panel_fill` (`render_support.rs`),
and `apply_window_opacity` (`scribe-render/src/lib.rs`) all floor at **0.0**
(fully transparent chrome; editor text painted opaque on top stays legible). A
code comment records that a prior **0.30** floor was already dropped to 0.0 in a
shipped release. That is already below the requested ~0.05–0.10 target, so no
change was needed — lowering further is impossible. Confirmed default (1.0)
unchanged.

### 4. Node-mesh color picker + reset-to-theme — DONE
- `crates/scribe-core/src/config/motion.rs`: new `mesh_color: Option<[u8;3]>`
  (Copy-friendly byte array so `MotionConfig` stays `Copy`; `None` = follow the
  theme accent). Added `resolved_mesh_color(theme_accent)` + a unit test
  (default/None follow, pinned override, serde backfill, TOML round-trip).
- `crates/scribe-app/src/app/frame_tick.rs`: the wired-mesh painter now uses
  `resolved_mesh_color([accent.r(),g,b])`.
- `crates/scribe-app/src/settings.rs`: Settings → Motion color picker seeded with
  the active theme's accent; a **"Reset to theme"** button appears once a color is
  pinned and clears the override back to `None`. Registered in the settings
  wiring-guard (`WIRED` + `consumed()` alias `resolved_mesh_color`).
- Persisted in config (serde default `None` backfills existing configs).

### 5. Subtle divider between notes/tabs — DONE (rotated column added)
The top/side tab strip (`draw_tab_strip`) already paints a faint 1px inter-chip
hairline (`muted` @ 0.30) — verified present. The **rotated** side-tab column
(`draw_rotated_side_tabs`) had none; added the same theme-tinted hairline at each
inter-row gap midpoint, painted as a stroke (not a panel fill) so it stays
visible in transparency mode. File: `crates/scribe-app/src/app/tab_strip_render.rs`.

## Verification
- `cargo check --workspace`: clean.
- `cargo build --release`: **compiles cleanly** (7m51s).
- `cargo test -p scribe-core -p scribe-app`: **869 pass** incl. the settings
  `wiring_guard::every_wired_setting_has_a_runtime_consumer` and the new
  `mesh_color_defaults_to_follow_theme_and_round_trips` test.
- One failure — `cli_with_no_files_opens_a_single_scratch_tab` — was confirmed
  **pre-existing on clean origin/master** (`git stash` + rerun still fails);
  environment-dependent session/scratch restore, unrelated to this diff.

## Commits
- `ac48604` feat(tabs): subtle hairline divider between adjacent rotated note tabs
- `e70195d` feat(settings): stationary prev/next theme arrow buttons
- `338a411` fix(chrome): recolor BOTH wordmark halves on theme change
- `d8e1767` feat(motion): node-mesh color picker with reset-to-theme

## Caveats
- Task 3 (opacity floor) required no code change — already at floor 0.0.
- Task 5 (tab dividers) was already implemented for the standard/horizontal strip
  in origin/master; this diff extends it to the rotated side-tab column.
- Did not touch app-icon assets or `build.rs` icon lines (out of scope).
- No bespoke egui_kittest snapshot was added for the arrow/divider render —
  stationarity is structurally guaranteed by the fixed-width combo, and the
  existing tab-strip visual-regression tests (which pass) cover the analogous
  horizontal divider. Verification rested on the release build + full test pass.
