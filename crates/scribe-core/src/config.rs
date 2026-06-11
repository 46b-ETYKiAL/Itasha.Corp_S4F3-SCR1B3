//! User configuration (TOML, live-reloadable). Great defaults out of the box;
//! everything overridable. Parsing never panics — malformed config falls back
//! to defaults with a surfaced error.

use crate::error::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Root config. `#[serde(default)]` everywhere so a partial user file merges
/// onto defaults rather than failing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub editor: EditorConfig,
    pub appearance: AppearanceConfig,
    pub fonts: FontConfig,
    pub window: WindowConfig,
    pub updates: UpdateConfig,
    pub spellcheck: SpellcheckConfig,
    pub plugins: PluginConfig,
    pub toolbar: ToolbarConfig,
    #[serde(default)]
    pub motion: MotionConfig,
    #[serde(default)]
    pub scroll: ScrollConfig,
}

/// Scroll behaviour (Wave 2). `speed` is egui's `line_scroll_speed` wheel-notch
/// multiplier — the single biggest wheel-speed lever (egui's `40.0` default is
/// noticeably slower than the Windows system feel; `75.0` ≈ 1.9× that). It is
/// applied PRE-smoothing, so egui's built-in `reach 90% in 0.1s` wheel smoothing
/// still applies — no double-smoothing. `animate_jumps` eases programmatic
/// jump-scrolls (goto-line / find-next) via egui's `scroll_animation`; it does
/// NOT affect plain wheel speed. Middle-click autoscroll (the Windows
/// wheel-click → drift behaviour) is opt-out via `autoscroll`, with a
/// distance→velocity `sensitivity` (points/sec per screen-pixel of offset) and a
/// `dead_zone` radius so a still pointer doesn't drift.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ScrollConfig {
    pub speed: f32,
    pub animate_jumps: bool,
    pub autoscroll: bool,
    pub autoscroll_sensitivity: f32,
    pub autoscroll_dead_zone: f32,
}

impl Default for ScrollConfig {
    fn default() -> Self {
        Self {
            speed: 75.0,
            animate_jumps: true,
            autoscroll: true,
            autoscroll_sensitivity: 6.0,
            autoscroll_dead_zone: 12.0,
        }
    }
}

impl ScrollConfig {
    /// Wheel speed clamped to the settings-slider band so a malformed toml can't
    /// produce a twitchy (too fast) or near-dead (too slow) scroll.
    pub fn clamped_speed(&self) -> f32 {
        self.speed.clamp(10.0, 200.0)
    }
    /// Autoscroll distance→velocity gain, clamped to the slider band.
    pub fn clamped_sensitivity(&self) -> f32 {
        self.autoscroll_sensitivity.clamp(2.0, 15.0)
    }
    /// Autoscroll dead-zone radius (screen px), clamped to the slider band.
    pub fn clamped_dead_zone(&self) -> f32 {
        self.autoscroll_dead_zone.clamp(4.0, 40.0)
    }
}

/// Motion / animation catalog (Phase 17 T17.3). Master switch + per-effect
/// toggles + global intensity. **OFF by default** — animation is opt-in so
/// the editor matches DECISION-2026-005's "calm, legible surface; chrome is
/// instrumentation, not decoration" through-line and so idle frames cost the
/// same as plain egui. When `enabled` is true, `intensity` scales egui's global
/// animation time (hover fades, value lerps, panel collapses, …) and
/// `cursor_blink` toggles the blinking text caret.
///
/// Only effects egui drives natively are exposed. An earlier scaffold carried a
/// 12-entry per-effect catalog (panel slide, tab-underline glide, error glitch,
/// ASCII boot splash, …) plus OS reduced-motion / on-battery gates, but none of
/// those had a renderer implementation and egui exposes no API to honor the OS
/// flags, so they were removed rather than shipped as dead toggles.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MotionConfig {
    /// Master switch. When false, egui's animation time is zeroed (instant, no
    /// fades) and the caret stops blinking — idle frames cost the same as plain
    /// egui.
    pub enabled: bool,
    /// 0.0..=1.0 scale applied to egui's global animation time when enabled.
    pub intensity: f32,
    /// Blink the text caret (vs. a steady caret) while motion is enabled.
    pub cursor_blink: bool,
    /// CRT-style horizontal scanlines drawn over the editor (a calm retro
    /// post-effect, ported from C0PL4ND). Off by default; gated behind
    /// [`enabled`](Self::enabled).
    #[serde(default)]
    pub crt_scanlines: bool,
    /// Scanline darkness (0.0 = invisible .. 1.0 = strong dark bands).
    #[serde(default = "default_scanline_darkness")]
    pub scanline_darkness: f32,
    /// Subtle full-window brightness flicker (CRT-style). Off by default.
    #[serde(default)]
    pub flicker: bool,
    /// Flicker strength (0.0 = none .. capped at 0.20 for accessibility).
    #[serde(default = "default_flicker_strength")]
    pub flicker_strength: f32,
    /// VHS-style horizontal tracking lines that drift down the window. Off by
    /// default.
    #[serde(default)]
    pub vhs_tracking: bool,
    /// Animated wired node-mesh ambient background (Lain-inspired). Off by
    /// default; drawn at Background order behind the editor.
    #[serde(default)]
    pub wired_ambient: bool,
    /// Node-mesh density (0.0 = sparse .. 1.0 = dense, clamped). Drives the
    /// node count of the wired-ambient background.
    #[serde(default = "default_mesh_density")]
    pub mesh_density: f32,
    /// Caret ghost-trail: a fading echo follows the caret as it moves. Off by
    /// default.
    #[serde(default)]
    pub caret_trail: bool,
    /// One-shot boot "glitch" sweep on the first frames after launch. Off by
    /// default; self-terminates.
    #[serde(default)]
    pub boot_glitch: bool,
}

/// Default CRT scanline darkness — subtle, readable bands.
fn default_scanline_darkness() -> f32 {
    0.3
}

/// Default flicker strength — barely perceptible.
fn default_flicker_strength() -> f32 {
    0.06
}

/// Default node-mesh density — a calm, sparse field.
fn default_mesh_density() -> f32 {
    0.4
}

impl MotionConfig {
    /// Clamped intensity so a malformed user config can't drive an animation
    /// outside its design band.
    pub fn clamped_intensity(&self) -> f32 {
        self.intensity.clamp(0.0, 1.0)
    }

    /// Flicker strength clamped to a calm, accessibility-safe ceiling.
    pub fn clamped_flicker_strength(&self) -> f32 {
        self.flicker_strength.clamp(0.0, 0.20)
    }

    /// Node-mesh density clamped to its design band.
    pub fn clamped_mesh_density(&self) -> f32 {
        self.mesh_density.clamp(0.0, 1.0)
    }
}

impl Default for MotionConfig {
    fn default() -> Self {
        // Animations ON by default (subtle motion is part of the intended feel);
        // users who prefer a fully static surface can toggle it off.
        Self {
            enabled: true,
            intensity: 0.6,
            cursor_blink: true,
            crt_scanlines: false,
            scanline_darkness: default_scanline_darkness(),
            flicker: false,
            flicker_strength: default_flicker_strength(),
            vhs_tracking: false,
            wired_ambient: false,
            mesh_density: default_mesh_density(),
            caret_trail: false,
            boot_glitch: false,
        }
    }
}

/// Window translucency mode. `Opaque` is the default; the rest reveal what's
/// behind the window to varying degrees (cross-platform `Transparent`; OS blur
/// for `Glass`/`Mica`/`Vibrancy`, which degrade to transparent where absent).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum WindowMode {
    #[default]
    Opaque,
    Transparent,
    Glass,
    Mica,
    Vibrancy,
}

/// Window appearance: translucency mode, opacity, and a color tint overlay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WindowConfig {
    /// Master on/off switch for the whole transparency system. When `false`
    /// the window paints fully opaque regardless of `mode` (safe, fast, and
    /// avoids the layered-window ghost-on-close failure mode on Windows).
    /// Default OFF — translucency is opt-in.
    pub transparency_enabled: bool,
    pub mode: WindowMode,
    /// Surface opacity for translucent modes (0.05..=1.0; the 0.05 floor keeps
    /// the window from becoming fully invisible).
    pub opacity: f32,
    /// Tint color (`#RRGGBB`) painted over the window at `tint_strength`.
    pub tint: String,
    /// Tint overlay strength (0.0 = none .. 1.0 = strong).
    pub tint_strength: f32,
    /// F-035 from docs/audits/overlooked-surfaces-2026-05-29.md: keep the
    /// SCR1B3 window on top of other windows. Default OFF.
    #[serde(default)]
    pub always_on_top: bool,
}

impl WindowConfig {
    /// Whether translucency should actually be rendered. Transparency is now a
    /// single enable/disable toggle: when on, the frameless transparent surface
    /// reveals the desktop through the translucent panels. There is no OS
    /// blur/backdrop mode — applying a DWM material (Mica/Acrylic/Tabbed) re-added
    /// the native caption buttons over the custom titlebar (the "double caption"
    /// bug) AND the materials were visually indistinguishable in practice, so the
    /// modes were collapsed to this toggle. The legacy `mode` field is retained
    /// only for config back-compat and no longer selects a surface. This is the
    /// single predicate every render path consults.
    pub fn effective_translucent(&self) -> bool {
        self.transparency_enabled
    }
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            transparency_enabled: false,
            mode: WindowMode::Opaque,
            opacity: 0.92,
            tint: "#08060d".to_string(),
            tint_strength: 0.0,
            always_on_top: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EditorConfig {
    pub tab_width: usize,
    pub insert_spaces: bool,
    pub show_line_numbers: bool,
    pub show_minimap: bool,
    pub word_wrap: bool,
    pub auto_save: bool,
    pub restore_session: bool,
    /// Where the open-tab strip lives: top (default, inline with the toolbar),
    /// bottom (status-side), left, or right. Phase 18 T18.4.
    pub tab_bar_position: TabBarPosition,
    /// When the tab bar is on the Left or Right, ROTATE each tab's label 90° so
    /// the text reads vertically (bottom-to-top), while the tabs stay stacked in
    /// a single column. `false` (default) keeps the labels horizontal — the
    /// familiar look. No effect for the Top/Bottom positions.
    #[serde(default, alias = "side_tabs_vertical")]
    pub side_tabs_rotated: bool,
    /// Note (editor) syntax colour theme — the text colour scheme for the note
    /// body, independent of the app chrome theme (#104). One of the bundled
    /// syntect themes; an unknown value falls back to the default.
    #[serde(default = "default_note_theme")]
    pub note_theme: String,
    /// Phase 18 T18.2 — enable the multi-note grid. When ON, the central
    /// editor surface renders every open tab as a movable, resizable pane
    /// inside an egui_tiles tree (up to 6 panes). Default OFF — the
    /// existing single-pane render path is unchanged for users who don't
    /// opt in.
    #[serde(default)]
    pub grid_enabled: bool,
    /// #R6 — persisted multi-note grid layout (a JSON-serialised
    /// `egui_tiles::Tree<Pane>` from `grid::to_json`). Restored on launch when
    /// the grid is enabled and the persisted panes match the reopened doc set,
    /// so a split arrangement survives a restart. `None` until a grid layout has
    /// been used.
    #[serde(default)]
    pub grid_layout: Option<String>,
    /// F-012 from docs/audits/overlooked-surfaces-2026-05-29.md: MRU
    /// list of recently-opened file paths. Capped at
    /// [`RECENT_FILES_MAX`]; freshly opened paths push to the front and
    /// duplicates collapse to the front position.
    #[serde(default)]
    pub recent_files: Vec<PathBuf>,
    /// F-013 from docs/audits/overlooked-surfaces-2026-05-29.md: set true
    /// after the welcome modal is dismissed. Used to suppress the welcome
    /// modal on subsequent launches.
    #[serde(default)]
    pub first_run_completed: bool,
    /// F-021 from docs/audits/overlooked-surfaces-2026-05-29.md: per-file
    /// scroll-offset map (path string → vertical pixel offset). Captured
    /// on tab close + open, restored on next open of the same path. Capped
    /// at [`SCROLL_POS_CAP`].
    #[serde(default)]
    pub scroll_positions: std::collections::HashMap<String, f32>,
    /// KEYSTONE — opt into the in-house rope editor (own cursor / selection /
    /// undo) instead of egui's `TextEdit` for normal-size files. Default OFF:
    /// the egui path stays the default while the owned editor matures (it does
    /// not yet have IME / mouse-selection parity). Read-only huge files always
    /// use the rope browse path regardless of this flag.
    #[serde(default)]
    pub experimental_rope_editor: bool,
    /// Wave-3 perf: byte size above which an *editable* buffer is auto-routed
    /// through the viewport-culled rope editor even when `experimental_rope_editor`
    /// is off — so a multi-MiB file does not pay the per-frame O(n) egui `TextEdit`
    /// cost. The rope path trades away a few large-file niceties (breadcrumb bar,
    /// sticky-scroll headers — both already disabled past 500 KiB anyway — plus
    /// spellcheck squiggles and Tab→spaces) for O(viewport) rendering, which is
    /// the right call at this size. `0` disables auto-promotion entirely. Default
    /// 16 MiB (aligns with the core mmap threshold).
    #[serde(default = "default_rope_auto_threshold")]
    pub rope_editor_auto_threshold_bytes: usize,
    /// Persist UNSAVED buffer content (incl. untitled scratch notes) so it
    /// survives a restart or crash without an explicit save — the Notepad++
    /// "session snapshot" / VS Code "Hot Exit" behaviour. Backups live in
    /// `<config>/backup/`; deleted once the buffer is saved. Default ON.
    #[serde(default = "default_true")]
    pub session_backup: bool,
    /// Strip trailing spaces/tabs from every line on save. Default OFF.
    #[serde(default)]
    pub trim_trailing_whitespace_on_save: bool,
    /// Ensure the file ends with a single newline on save. Default OFF.
    #[serde(default)]
    pub final_newline_on_save: bool,
    /// Remember + restore the caret char index per file path (extends the
    /// scroll-position memory). Default ON.
    #[serde(default = "default_true")]
    pub restore_cursor_position: bool,
    /// Per-file caret char index, restored on reopen (companion to
    /// `scroll_positions`). Capped at [`SCROLL_POS_CAP`].
    #[serde(default)]
    pub cursor_positions: std::collections::HashMap<String, usize>,
    /// Render visible whitespace markers (a faint `·` per space, `→` per
    /// tab) in the OWNED rope editor. Default OFF — the markers are an
    /// opt-in overlay; the egui TextEdit path and the real buffer text are
    /// untouched whether on or off.
    #[serde(default)]
    pub render_whitespace: bool,
    /// Enable Tab-trigger snippet expansion in the in-house editor. A Tab
    /// pressed right after a known prefix from `<config>/snippets.toml` expands
    /// the snippet instead of indenting. Default ON (the feature is inert when
    /// no snippets file is present), and ON for configs written before the
    /// field existed.
    #[serde(default = "default_true")]
    pub snippets_enabled: bool,
    /// Highlight the line the caret is on with a faint full-width band. Default
    /// OFF (the calm-surface default; opt-in like the other overlays).
    #[serde(default)]
    pub current_line_highlight: bool,
    /// Caret shape drawn over egui's native caret. Default `Bar` = egui's own
    /// look (so the default is a visual no-op).
    #[serde(default)]
    pub caret_style: CaretStyle,
    /// Caret stroke width in points for the Bar/Underline styles (Block ignores
    /// it — it fills the cell). Clamped to [1.0, 4.0] at render time.
    #[serde(default = "default_caret_width")]
    pub caret_width: f32,
    /// Draw faint vertical indent-guide lines at each `tab_width` column.
    /// Default OFF.
    #[serde(default)]
    pub indent_guides: bool,
    /// Box-highlight the bracket matching the one next to the caret. Default OFF.
    #[serde(default)]
    pub bracket_match: bool,
    /// Smooth (eased) wheel scrolling. Default ON — egui's native feel. Off makes
    /// the wheel jump in discrete notches (snappier, no glide).
    #[serde(default = "default_true")]
    pub smooth_scroll: bool,
    /// Scrollbar chrome style for the editor surface.
    #[serde(default)]
    pub scrollbar_style: ScrollbarStyle,
}

/// Caret shape rendered over the editor's native caret. `Bar` reproduces egui's
/// default thin vertical caret (so it is a visual no-op when selected).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CaretStyle {
    #[default]
    Bar,
    /// Full-cell filled rectangle — the retro terminal look.
    Block,
    /// A thick underline at the caret's baseline.
    Underline,
}

/// Editor scrollbar chrome. `Auto` = egui default (shows on hover/scroll);
/// `Thin` = a slimmer bar; `Hidden` = no visible bar (scroll still works).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ScrollbarStyle {
    #[default]
    Auto,
    Thin,
    Hidden,
}

/// serde default for the caret stroke width.
fn default_caret_width() -> f32 {
    1.0
}

impl EditorConfig {
    /// Caret stroke width clamped to a sane band.
    pub fn clamped_caret_width(&self) -> f32 {
        self.caret_width.clamp(1.0, 4.0)
    }
}

/// serde default for the note syntax-colour theme (#104).
fn default_note_theme() -> String {
    "base16-eighties.dark".to_string()
}

/// serde default for opt-OUT booleans (fields that should be ON unless the
/// user turns them off, and ON for configs written before the field existed).
fn default_true() -> bool {
    true
}

/// Wave-3: default byte threshold (16 MiB) above which an editable buffer is
/// auto-promoted to the viewport-culled rope editor. Aligns with the core
/// `Buffer::MMAP_THRESHOLD`. `0` (user-set) disables auto-promotion.
fn default_rope_auto_threshold() -> usize {
    16 * 1024 * 1024
}

/// Cap on the scroll-position memory map (F-021). Older entries are evicted
/// in arbitrary order — the map is best-effort, not history.
pub const SCROLL_POS_CAP: usize = 200;

/// Insert / update `path`'s scroll offset, capping the map at
/// [`SCROLL_POS_CAP`] entries.
pub fn record_scroll_pos(map: &mut std::collections::HashMap<String, f32>, path: &str, y: f32) {
    if map.len() >= SCROLL_POS_CAP && !map.contains_key(path) {
        if let Some(first) = map.keys().next().cloned() {
            map.remove(&first);
        }
    }
    map.insert(path.to_string(), y);
}

/// Cap on the recent-files MRU list. 20 is the universal editor
/// convention (VSCode, Sublime, Notepad++).
pub const RECENT_FILES_MAX: usize = 20;

/// Push `path` to the front of `recent` (MRU semantics), dedup by exact
/// path equality, and cap the list at [`RECENT_FILES_MAX`]. Pure helper so
/// the open-path codepath stays testable without the egui shell.
pub fn record_recent_file(recent: &mut Vec<PathBuf>, path: PathBuf) {
    recent.retain(|p| p != &path);
    recent.insert(0, path);
    if recent.len() > RECENT_FILES_MAX {
        recent.truncate(RECENT_FILES_MAX);
    }
}

/// Tab-strip position relative to the editor surface. `Top` keeps the tab
/// strip inline with the toolbar (the v1 layout); the other three host the
/// strip in its own dedicated panel.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TabBarPosition {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

impl TabBarPosition {
    /// True when the strip should render as a vertical list of tabs (one tab
    /// per row) — used for the side positions.
    pub fn is_vertical(self) -> bool {
        matches!(self, TabBarPosition::Left | TabBarPosition::Right)
    }
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            tab_width: 4,
            insert_spaces: true,
            show_line_numbers: true,
            show_minimap: true,
            word_wrap: true,
            auto_save: false,
            restore_session: true,
            tab_bar_position: TabBarPosition::Top,
            side_tabs_rotated: false,
            note_theme: default_note_theme(),
            grid_enabled: false,
            grid_layout: None,
            recent_files: Vec::new(),
            first_run_completed: false,
            scroll_positions: std::collections::HashMap::new(),
            experimental_rope_editor: false,
            rope_editor_auto_threshold_bytes: default_rope_auto_threshold(),
            session_backup: true,
            trim_trailing_whitespace_on_save: false,
            final_newline_on_save: false,
            restore_cursor_position: true,
            cursor_positions: std::collections::HashMap::new(),
            render_whitespace: false,
            snippets_enabled: true,
            current_line_highlight: false,
            caret_style: CaretStyle::Bar,
            caret_width: default_caret_width(),
            indent_guides: false,
            bracket_match: false,
            smooth_scroll: true,
            scrollbar_style: ScrollbarStyle::Auto,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AppearanceConfig {
    /// Theme name (built-in or a user theme file stem).
    pub theme: String,
    /// "system" | "dark" | "light".
    pub follow_os_theme: bool,
    /// Frameless window with custom brand titlebar.
    pub frameless: bool,
    /// Render quick-access toolbar items as Phosphor (Thin) icons rather than
    /// their text labels. Default OFF — words are universally readable; icons
    /// are an opt-in compact mode. (Phase 16 T16.3.)
    pub toolbar_icons: bool,
    /// Annotate toolbar items with small, dim, English-redundant **kanji
    /// instrument labels** (Phase 17 T17.5). The annotation is additive — the
    /// English (or icon) label remains the primary read; the kanji sits to
    /// the right at smaller size and reduced contrast as a typographic ornament
    /// (instrument plates on a control panel). Per the Folklore-Consultant gate
    /// (DECISION-2026-005 cond #4) only verified-canonical kanji ship; actions
    /// whose canonical kanji is uncertain stay English-only. Default OFF.
    pub jp_glyph_labels: bool,
    /// Optional app-background colour override (hex `#rrggbb`), INDEPENDENT of
    /// the theme. `None` = follow the active theme's background. Set to a colour
    /// to pin the app background regardless of theme; switching themes clears
    /// this back to `None` so the background follows the newly-chosen theme.
    #[serde(default)]
    pub background_override: Option<String>,
    /// Optional NOTE (editor well) background colour override (hex `#rrggbb`),
    /// used only when `link_backgrounds` is false. `None` = follow the theme's
    /// editor background. Cleared on theme change like `background_override`.
    #[serde(default)]
    pub note_background_override: Option<String>,
    /// When true (default), the note background follows the app background — one
    /// control changes both. When false, the note uses `note_background_override`
    /// independently of the app background.
    #[serde(default = "default_true")]
    pub link_backgrounds: bool,
    /// Move the quick-access toolbar INTO the custom titlebar (between the app
    /// wordmark and the window caption buttons), suppressing the separate
    /// toolbar bar — a compact single-row chrome. Only takes effect with
    /// `frameless` on (the titlebar exists only then). Default OFF.
    #[serde(default)]
    pub toolbar_in_titlebar: bool,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            theme: "itasha-corp".to_string(),
            follow_os_theme: true,
            frameless: true,
            toolbar_icons: false,
            jp_glyph_labels: false,
            background_override: None,
            note_background_override: None,
            link_backgrounds: true,
            toolbar_in_titlebar: false,
        }
    }
}

/// Editor font sizing. NOTE: font *family* selection is intentionally not a
/// config field — egui renders through `ab_glyph`, which does no font-family
/// fallback chains or OpenType shaping, so the editor uses the bundled
/// JetBrains Mono (see `ScribeApp::new`). For the same reason there is no
/// `ligatures` option: ligature substitution requires a shaping engine egui
/// does not have, so the flag could never do anything.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FontConfig {
    pub editor_size: f32,
    pub line_height: f32,
    /// Editor monospace font family — a "font theme". One of the bundled
    /// (OFL-licensed) family display names; an unknown value falls back to the
    /// default. Default: "JetBrains Mono".
    #[serde(default = "default_editor_family")]
    pub editor_family: String,
    /// App-UI font family (the proportional text everywhere EXCEPT the note
    /// body): toolbar, settings, status bar, menus. One of the bundled family
    /// names, or "System default" to keep egui's built-in UI font. Separate from
    /// `editor_family` so the note text and the UI can use different fonts.
    #[serde(default = "default_ui_family")]
    pub ui_family: String,
}

fn default_editor_family() -> String {
    "IBM Plex Mono".to_string()
}

fn default_ui_family() -> String {
    "System default".to_string()
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            editor_size: 14.0,
            // #108 — keep this near the font's natural row height (~1.15-1.2) by
            // default. In egui the caret + selection rectangles ARE the line
            // height, so a large multiplier makes them noticeably taller than the
            // glyphs (the extra leading sits below the text). 1.2 gives a tidy
            // caret that tracks the text; raise it for more line spacing at the
            // cost of a taller caret.
            line_height: 1.2,
            editor_family: default_editor_family(),
            ui_family: default_ui_family(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    Off,
    Notify,
    /// Default: SCR1B3 makes NO automatic network connection — the user checks
    /// for updates on demand from Settings (on-brand for a telemetry-free app).
    #[default]
    Manual,
    Auto,
}

/// Telemetry-free auto-update preferences. The version check hits ONLY the
/// public GitHub Releases API and sends zero PII.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UpdateConfig {
    pub mode: UpdateMode,
    pub check_interval_hours: u32,
    /// Unix-seconds timestamp of the last time the app reminded the user to
    /// check for a release (or they pressed "Check now"). This is persisted
    /// *state*, not a user-facing preference, so it has no Settings control —
    /// it just lets the interval below be honored across sessions. `None` until
    /// the first reminder fires.
    pub last_check_unix: Option<u64>,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            // Manual by default: no automatic/background network. Notify and Auto
            // are explicit opt-ins to an on-launch GitHub-Releases version check.
            mode: UpdateMode::Manual,
            check_interval_hours: 24,
            last_check_unix: None,
        }
    }
}

/// Privacy-respecting spellcheck. OFF by default; fully offline when on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SpellcheckConfig {
    pub enabled: bool,
    pub language: String,
    pub check_comments: bool,
    pub check_strings: bool,
    pub check_identifiers: bool,
    pub custom_dict_path: Option<PathBuf>,
}

impl Default for SpellcheckConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            language: "en_US".to_string(),
            check_comments: true,
            check_strings: true,
            check_identifiers: false,
            custom_dict_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PluginConfig {
    pub enabled: bool,
    /// Plugin ids the user has explicitly disabled.
    pub disabled: Vec<String>,
    /// Trust-on-first-use approvals: plugin id -> the SHA-256 of the entry
    /// script the user approved. A discovered plugin is only ever RUN when its
    /// current entry-script hash matches the approved one — so dropping a new
    /// (or silently-modified) plugin folder into the plugins dir does NOT
    /// auto-execute it; the user must approve it first. This is the real
    /// consent gate the security docs describe.
    pub trusted: std::collections::BTreeMap<String, String>,
    /// Strict mode: when true, a plugin must additionally carry a valid minisign
    /// signature over its manifest from a pinned author key (the manifest's
    /// `signature` + `author_pubkey`). Default off so existing unsigned local
    /// script plugins keep working under the TOFU gate above.
    pub require_signed: bool,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            disabled: Vec::new(),
            trusted: std::collections::BTreeMap::new(),
            require_signed: false,
        }
    }
}

/// Customizable quick-access toolbar. `items` is an ordered list of action ids
/// (see `app::TOOLBAR_ACTIONS`); the literal id `"sep"` renders a divider. The
/// user reorders/adds/removes entries from Settings; the layout persists here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ToolbarConfig {
    pub items: Vec<String>,
    /// User-curated "more actions" dropdown. Action ids placed here are reached
    /// via a single "⋯" menu button on the toolbar instead of taking a slot —
    /// keeps the bar clean. Empty by default (no dropdown shown). Same id space
    /// as `items` (`app::TOOLBAR_ACTIONS`).
    #[serde(default)]
    pub menu: Vec<String>,
    /// Minimum height of each quick-access button in logical pixels. Clamped
    /// to [16.0, 64.0] at render time. Phase 18 T18.5.
    #[serde(default = "ToolbarConfig::default_button_size")]
    pub button_size_px: f32,
    /// Horizontal spacing between adjacent items in logical pixels. Clamped
    /// to [0.0, 24.0] at render time. Phase 18 T18.5.
    #[serde(default = "ToolbarConfig::default_button_spacing")]
    pub button_spacing_px: f32,
    /// Icon glyph size in logical pixels — only consulted when
    /// `appearance.toolbar_icons` is on. Clamped to [10.0, 32.0]. T18.5.
    #[serde(default = "ToolbarConfig::default_icon_size")]
    pub icon_size_px: f32,
}

impl ToolbarConfig {
    pub fn default_button_size() -> f32 {
        24.0
    }
    pub fn default_button_spacing() -> f32 {
        6.0
    }
    pub fn default_icon_size() -> f32 {
        14.0
    }

    /// Clamped values applied at render time so a malformed user config can't
    /// produce a 4000px-tall toolbar or zero-padded buttons.
    pub fn clamped_button_size(&self) -> f32 {
        self.button_size_px.clamp(16.0, 64.0)
    }
    pub fn clamped_button_spacing(&self) -> f32 {
        self.button_spacing_px.clamp(0.0, 24.0)
    }
    pub fn clamped_icon_size(&self) -> f32 {
        self.icon_size_px.clamp(10.0, 32.0)
    }
}

impl Default for ToolbarConfig {
    fn default() -> Self {
        Self {
            items: [
                "new",
                "open",
                "save",
                "sep",
                "find",
                "palette",
                "sep",
                "split",
                "minimap",
                "wrap",
                "sep",
                "spellcheck",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            menu: Vec::new(),
            button_size_px: Self::default_button_size(),
            button_spacing_px: Self::default_button_spacing(),
            icon_size_px: Self::default_icon_size(),
        }
    }
}

impl Config {
    /// Parse from a TOML string; on error, return defaults plus the error so
    /// the caller can surface it without losing the editor.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        toml::from_str(s).map_err(|e| CoreError::ConfigParse(e.to_string()))
    }

    pub fn to_toml_string(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Default per-OS config directory: e.g. `%APPDATA%/scr1b3` /
    /// `~/.config/scr1b3` / `~/Library/Application Support/scr1b3`.
    ///
    /// `SCR1B3_CONFIG_DIR` overrides the resolved path when set to a non-empty
    /// value. This enables a portable / "bring your own config dir" mode and is
    /// the supported way to point the app at an isolated directory (e.g. for QA
    /// or testing) — the `directories` crate resolves the Windows config root
    /// via `SHGetKnownFolderPath`, which ignores the `APPDATA` environment
    /// variable, so an env-redirect of `APPDATA` alone does NOT relocate config.
    pub fn config_dir() -> Option<PathBuf> {
        Self::config_dir_from(std::env::var_os("SCR1B3_CONFIG_DIR"))
    }

    /// Resolve the config dir given an explicit override value (pure — no env
    /// read), so the precedence is unit-testable without mutating process-global
    /// state. A non-empty override wins; otherwise fall back to the OS default.
    fn config_dir_from(override_dir: Option<std::ffi::OsString>) -> Option<PathBuf> {
        if let Some(override_dir) = override_dir {
            if !override_dir.is_empty() {
                return Some(PathBuf::from(override_dir));
            }
        }
        directories::ProjectDirs::from("com", "ItashaCorp", crate::CONFIG_DIR_NAME)
            .map(|d| d.config_dir().to_path_buf())
    }

    pub fn config_file_path() -> Option<PathBuf> {
        Self::config_dir().map(|d| d.join("scr1b3.toml"))
    }

    /// Load config from the OS config file, or defaults if absent/broken.
    /// Returns `(config, Option<error_message>)` — never fails to produce a
    /// usable config.
    pub fn load_or_default() -> (Self, Option<String>) {
        let Some(path) = Self::config_file_path() else {
            return (Self::default(), None);
        };
        match std::fs::read_to_string(&path) {
            Ok(s) => match Self::from_toml_str(&s) {
                Ok(cfg) => (cfg, None),
                Err(e) => (Self::default(), Some(e.to_string())),
            },
            Err(_) => (Self::default(), None), // absent = use defaults silently
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip() {
        let c = Config::default();
        let s = c.to_toml_string();
        let back = Config::from_toml_str(&s).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn partial_merges_onto_defaults() {
        let c = Config::from_toml_str("[editor]\ntab_width = 2\n").unwrap();
        assert_eq!(c.editor.tab_width, 2);
        // unspecified fields keep defaults
        assert!(c.editor.show_line_numbers);
        assert_eq!(c.appearance.theme, "itasha-corp");
    }

    #[test]
    fn malformed_is_error_not_panic() {
        assert!(Config::from_toml_str("editor = [[[").is_err());
    }

    #[test]
    fn spellcheck_on_by_default() {
        assert!(Config::default().spellcheck.enabled);
    }

    #[test]
    fn word_wrap_and_animations_on_by_default() {
        assert!(Config::default().editor.word_wrap);
        assert!(Config::default().motion.enabled);
    }

    #[test]
    fn default_note_font_is_ibm_plex_mono() {
        assert_eq!(Config::default().fonts.editor_family, "IBM Plex Mono");
    }

    #[test]
    fn config_dir_override_wins_when_non_empty() {
        // A non-empty SCR1B3_CONFIG_DIR value relocates the config dir verbatim
        // (portable / isolated-QA mode). Tested via the pure helper so no
        // process-global env mutation is needed (keeps the parallel test runner
        // deterministic).
        let custom = std::ffi::OsString::from(r"C:\tmp\scr1b3-qa");
        assert_eq!(
            Config::config_dir_from(Some(custom)),
            Some(std::path::PathBuf::from(r"C:\tmp\scr1b3-qa"))
        );
    }

    #[test]
    fn config_dir_override_ignored_when_empty() {
        // An empty override must fall through to the OS default, never resolve to
        // "" (which would put config at the process CWD).
        let from_empty = Config::config_dir_from(Some(std::ffi::OsString::new()));
        let from_none = Config::config_dir_from(None);
        assert_eq!(from_empty, from_none);
    }

    #[test]
    fn scroll_defaults_are_sane_and_clamp() {
        let s = ScrollConfig::default();
        assert_eq!(s.speed, 75.0);
        assert!(s.animate_jumps && s.autoscroll);
        // Out-of-band values clamp to the slider range.
        let wild = ScrollConfig {
            speed: 9000.0,
            autoscroll_sensitivity: -3.0,
            autoscroll_dead_zone: 999.0,
            ..ScrollConfig::default()
        };
        assert_eq!(wild.clamped_speed(), 200.0);
        assert_eq!(wild.clamped_sensitivity(), 2.0);
        assert_eq!(wild.clamped_dead_zone(), 40.0);
    }

    #[test]
    fn scroll_config_absent_section_defaults() {
        // A toml without a [scroll] table must still load with scroll defaults.
        let cfg = Config::from_toml_str("[editor]\ntab_width = 4\n").unwrap();
        assert_eq!(cfg.scroll, ScrollConfig::default());
    }

    #[test]
    fn motion_master_on_by_default() {
        // Animations are part of the intended feel; users can opt out.
        assert!(MotionConfig::default().enabled, "master defaults ON");
    }

    #[test]
    fn motion_intensity_clamps_to_unit_band() {
        let lo = MotionConfig {
            intensity: -5.0,
            ..MotionConfig::default()
        };
        let hi = MotionConfig {
            intensity: 42.0,
            ..MotionConfig::default()
        };
        assert_eq!(lo.clamped_intensity(), 0.0);
        assert_eq!(hi.clamped_intensity(), 1.0);
        assert_eq!(MotionConfig::default().clamped_intensity(), 0.6);
    }

    #[test]
    fn toolbar_sizing_clamps_extreme_values() {
        // T18.5: a malformed user toml can't produce a 4000-px-tall toolbar or
        // a zero-padded mess. Clamped helpers enforce the safe band.
        let huge = ToolbarConfig {
            button_size_px: 9999.0,
            button_spacing_px: -50.0,
            icon_size_px: 0.5,
            ..Default::default()
        };
        assert_eq!(huge.clamped_button_size(), 64.0);
        assert_eq!(huge.clamped_button_spacing(), 0.0);
        assert_eq!(huge.clamped_icon_size(), 10.0);
        let defaults = ToolbarConfig::default();
        assert_eq!(defaults.clamped_button_size(), 24.0);
        assert_eq!(defaults.clamped_button_spacing(), 6.0);
        assert_eq!(defaults.clamped_icon_size(), 14.0);
    }

    #[test]
    fn tab_bar_defaults_to_top_horizontal() {
        // T18.4: the v1 layout puts the tab strip inline with the toolbar at
        // the top. is_vertical() flips only for the side positions.
        assert_eq!(
            EditorConfig::default().tab_bar_position,
            TabBarPosition::Top
        );
        assert!(!TabBarPosition::Top.is_vertical());
        assert!(!TabBarPosition::Bottom.is_vertical());
        assert!(TabBarPosition::Left.is_vertical());
        assert!(TabBarPosition::Right.is_vertical());
        // Side-tab rotation defaults OFF — labels stay horizontal (the familiar
        // look) until the user opts into vertical text.
        assert!(!EditorConfig::default().side_tabs_rotated);
    }

    #[test]
    fn side_tabs_rotated_round_trips_and_accepts_legacy_alias() {
        // Absent → default false.
        let older = "tab_width = 2\n";
        let cfg: EditorConfig = toml::from_str(older).unwrap();
        assert!(!cfg.side_tabs_rotated, "absent field defaults to false");
        // Explicit new name.
        let explicit = "tab_width = 2\nside_tabs_rotated = true\n";
        let cfg2: EditorConfig = toml::from_str(explicit).unwrap();
        assert!(cfg2.side_tabs_rotated);
        // The old `side_tabs_vertical` name is accepted via serde alias so
        // existing configs don't error.
        let legacy = "tab_width = 2\nside_tabs_vertical = true\n";
        let cfg3: EditorConfig = toml::from_str(legacy).unwrap();
        assert!(cfg3.side_tabs_rotated, "legacy alias maps to the new field");
    }

    #[test]
    fn transparency_off_by_default() {
        // Master toggle defaults OFF: a normal opaque window (no ghost-on-close
        // risk, no perf cost). See T19.1/T19.2.
        assert!(!Config::default().window.transparency_enabled);
        assert!(!Config::default().window.effective_translucent());
    }

    #[test]
    fn effective_translucent_tracks_the_single_toggle() {
        // Transparency is a single enable/disable toggle now — the legacy `mode`
        // field no longer participates (it is back-compat only). Off => opaque.
        let w = WindowConfig {
            transparency_enabled: false,
            ..Default::default()
        };
        assert!(!w.effective_translucent(), "toggle off stays opaque");
        // Toggle on => translucent, regardless of the vestigial mode value.
        let w = WindowConfig {
            transparency_enabled: true,
            ..Default::default()
        };
        assert!(w.effective_translucent());
        // The mode field does not change the outcome either way.
        let w = WindowConfig {
            mode: WindowMode::Opaque,
            transparency_enabled: true,
            ..Default::default()
        };
        assert!(w.effective_translucent());
    }

    /// F-035: always_on_top defaults OFF and round-trips through TOML.
    #[test]
    fn always_on_top_default_off_and_round_trips() {
        let c = Config::default();
        assert!(!c.window.always_on_top);
        let mut c2 = c.clone();
        c2.window.always_on_top = true;
        let s = c2.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert!(back.window.always_on_top);
    }

    /// F-012 helper: record_recent_file pushes to the front, dedups, caps.
    #[test]
    fn record_recent_file_mru_dedup_cap() {
        use super::{record_recent_file, RECENT_FILES_MAX};
        let mut r: Vec<PathBuf> = Vec::new();
        record_recent_file(&mut r, PathBuf::from("/a/b.txt"));
        record_recent_file(&mut r, PathBuf::from("/c/d.txt"));
        record_recent_file(&mut r, PathBuf::from("/a/b.txt")); // dedup → front
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], PathBuf::from("/a/b.txt"));
        assert_eq!(r[1], PathBuf::from("/c/d.txt"));
        // Cap test.
        for n in 0..(RECENT_FILES_MAX + 5) {
            record_recent_file(&mut r, PathBuf::from(format!("/fill/{n}.txt")));
        }
        assert_eq!(r.len(), RECENT_FILES_MAX);
    }

    /// F-012: recent_files round-trips through TOML.
    #[test]
    fn recent_files_round_trip() {
        let mut c = Config::default();
        c.editor.recent_files = vec![PathBuf::from("/x/y.rs"), PathBuf::from("/p/q.py")];
        let s = c.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert_eq!(back.editor.recent_files.len(), 2);
        assert_eq!(back.editor.recent_files[0], PathBuf::from("/x/y.rs"));
    }

    /// F-013: first_run_completed defaults false + round-trips.
    #[test]
    fn first_run_completed_default_false_and_round_trips() {
        let c = Config::default();
        assert!(!c.editor.first_run_completed);
        let mut c2 = c.clone();
        c2.editor.first_run_completed = true;
        let s = c2.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert!(back.editor.first_run_completed);
    }

    /// render_whitespace defaults OFF and round-trips through TOML.
    #[test]
    fn render_whitespace_default_off_and_round_trips() {
        let c = Config::default();
        assert!(!c.editor.render_whitespace);
        let mut c2 = c.clone();
        c2.editor.render_whitespace = true;
        let s = c2.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert!(back.editor.render_whitespace);
    }

    #[test]
    fn wave6_editor_customization_defaults() {
        let e = EditorConfig::default();
        assert!(!e.current_line_highlight && !e.indent_guides && !e.bracket_match);
        assert_eq!(e.caret_style, CaretStyle::Bar); // visual no-op default
        assert!(e.smooth_scroll); // ON by default
        assert_eq!(e.scrollbar_style, ScrollbarStyle::Auto);
        assert_eq!(e.clamped_caret_width(), 1.0);
        let wide = EditorConfig {
            caret_width: 99.0,
            ..EditorConfig::default()
        };
        assert_eq!(wide.clamped_caret_width(), 4.0);
    }

    #[test]
    fn wave6_customization_round_trips() {
        let mut c = Config::default();
        c.editor.caret_style = CaretStyle::Block;
        c.editor.scrollbar_style = ScrollbarStyle::Thin;
        c.editor.indent_guides = true;
        let s = c.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert_eq!(back.editor.caret_style, CaretStyle::Block);
        assert_eq!(back.editor.scrollbar_style, ScrollbarStyle::Thin);
        assert!(back.editor.indent_guides);
    }

    /// F-021 helper: record_scroll_pos inserts + caps at SCROLL_POS_CAP.
    #[test]
    fn record_scroll_pos_caps_and_round_trips() {
        use super::{record_scroll_pos, SCROLL_POS_CAP};
        let mut m = std::collections::HashMap::<String, f32>::new();
        record_scroll_pos(&mut m, "/a/b.rs", 100.0);
        record_scroll_pos(&mut m, "/c/d.rs", 200.0);
        record_scroll_pos(&mut m, "/a/b.rs", 150.0); // update in place
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("/a/b.rs").copied(), Some(150.0));
        for n in 0..(SCROLL_POS_CAP + 10) {
            record_scroll_pos(&mut m, &format!("/fill/{n}.rs"), n as f32);
        }
        assert_eq!(m.len(), SCROLL_POS_CAP);
        // Round-trip a small map.
        let mut c = Config::default();
        c.editor
            .scroll_positions
            .insert("/x/y.rs".to_string(), 250.0);
        let s = c.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert_eq!(
            back.editor.scroll_positions.get("/x/y.rs").copied(),
            Some(250.0)
        );
    }
}
