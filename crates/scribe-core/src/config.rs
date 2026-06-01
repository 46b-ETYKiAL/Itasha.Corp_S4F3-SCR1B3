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
}

/// Motion / animation catalog (Phase 17 T17.3). Master switch + per-effect
/// toggles + global intensity. **OFF by default** — animation is opt-in so
/// the editor matches DECISION-2026-005's "calm, legible surface; chrome is
/// instrumentation, not decoration" through-line and so idle frames cost
/// the same as plain egui. When `enabled` is true, individual effects
/// follow their own toggle, the global `intensity` scales their amplitude,
/// and the `respect_reduced_motion` + `respect_battery` flags zero-out
/// animation when the OS asks for it or the device is on battery.
///
/// The per-effect implementations are wired progressively as follow-up
/// increments. This struct is the load-bearing scaffold every motion
/// site consults via `MotionConfig::active_for(effect, …)`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MotionConfig {
    /// Master switch. When false, every animation is suppressed regardless
    /// of the per-effect flags below — keeps the editor cost identical to
    /// the no-motion build.
    pub enabled: bool,
    /// 0.0..=1.0 amplitude scale every effect honours.
    pub intensity: f32,
    /// Honour the OS reduced-motion request (e.g. macOS Reduce Motion,
    /// Windows Animate windows when minimizing OFF, Linux GNOME tweaks).
    pub respect_reduced_motion: bool,
    /// Suppress animation when the device reports it's on battery (laptop
    /// power-save). Verified at runtime; falls back to `enabled` on desktops.
    pub respect_battery: bool,

    // ---- 12-effect catalog (the named animations from the plan) ----
    pub hover: bool,
    pub focus_ring: bool,
    pub panel_slide: bool,
    pub tab_underline: bool,
    pub palette_lift: bool,
    pub cursor_blink: bool,
    pub status_breathe: bool,
    pub toast: bool,
    pub error_glitch: bool,
    pub ascii_boot_splash: bool,
    pub idle_pulse: bool,
    pub transition_fade: bool,
}

/// The 12 named motion effects. Mirrors the boolean fields on `MotionConfig`
/// 1:1 so `active_for(effect, …)` can route each enquiry to the right flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionEffect {
    Hover,
    FocusRing,
    PanelSlide,
    TabUnderline,
    PaletteLift,
    CursorBlink,
    StatusBreathe,
    Toast,
    ErrorGlitch,
    AsciiBootSplash,
    IdlePulse,
    TransitionFade,
}

impl MotionConfig {
    /// True when motion is on AND not gated by reduced-motion or battery.
    /// The caller supplies the live `reduced_motion` + `on_battery` flags
    /// (the config itself stays pure / platform-agnostic).
    pub fn effective(&self, reduced_motion: bool, on_battery: bool) -> bool {
        if !self.enabled {
            return false;
        }
        if self.respect_reduced_motion && reduced_motion {
            return false;
        }
        if self.respect_battery && on_battery {
            return false;
        }
        true
    }

    /// True when the master is effective AND this specific effect's toggle
    /// is on. Every motion site should consult this before driving an
    /// animation — never branch on `enabled` alone.
    pub fn active_for(&self, effect: MotionEffect, reduced_motion: bool, on_battery: bool) -> bool {
        self.effective(reduced_motion, on_battery)
            && match effect {
                MotionEffect::Hover => self.hover,
                MotionEffect::FocusRing => self.focus_ring,
                MotionEffect::PanelSlide => self.panel_slide,
                MotionEffect::TabUnderline => self.tab_underline,
                MotionEffect::PaletteLift => self.palette_lift,
                MotionEffect::CursorBlink => self.cursor_blink,
                MotionEffect::StatusBreathe => self.status_breathe,
                MotionEffect::Toast => self.toast,
                MotionEffect::ErrorGlitch => self.error_glitch,
                MotionEffect::AsciiBootSplash => self.ascii_boot_splash,
                MotionEffect::IdlePulse => self.idle_pulse,
                MotionEffect::TransitionFade => self.transition_fade,
            }
    }

    /// Clamped intensity so a malformed user config can't drive an animation
    /// outside its design band.
    pub fn clamped_intensity(&self) -> f32 {
        self.intensity.clamp(0.0, 1.0)
    }
}

impl Default for MotionConfig {
    fn default() -> Self {
        // Master OFF by default — animation is opt-in. Per-effect flags
        // carry the sensible "when motion is on" defaults so the user
        // doesn't have to tick every box on first activation.
        Self {
            enabled: false,
            intensity: 0.6,
            respect_reduced_motion: true,
            respect_battery: true,
            hover: true,
            focus_ring: true,
            panel_slide: false,
            tab_underline: true,
            palette_lift: false,
            cursor_blink: true,
            status_breathe: false,
            toast: true,
            error_glitch: false,
            ascii_boot_splash: false,
            idle_pulse: false,
            transition_fade: true,
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

impl WindowMode {
    /// Whether this mode wants a transparent surface (non-opaque).
    pub fn is_translucent(self) -> bool {
        !matches!(self, WindowMode::Opaque)
    }
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
    /// Surface opacity for translucent modes (0.30..=1.0).
    pub opacity: f32,
    /// Tint color (`#RRGGBB`) painted over the window at `tint_strength`.
    pub tint: String,
    /// Tint overlay strength (0.0 = none .. 1.0 = strong).
    pub tint_strength: f32,
    /// F-020 from docs/audits/overlooked-surfaces-2026-05-29.md: last
    /// window position + size. Persisted on save_config and restored on
    /// next launch. `None` means "use the hard-coded default size" — the
    /// pre-audit behaviour. Tuple is `(x, y, width, height)` in logical
    /// pixels (eframe's surface unit).
    #[serde(default)]
    pub last_geometry: Option<(f32, f32, f32, f32)>,
    /// F-035 from docs/audits/overlooked-surfaces-2026-05-29.md: keep the
    /// SCR1B3 window on top of other windows. Default OFF.
    #[serde(default)]
    pub always_on_top: bool,
}

impl WindowConfig {
    /// Whether translucency should actually be rendered: the master toggle is
    /// on AND the chosen mode wants a non-opaque surface. This is the single
    /// predicate every render path consults so the master switch is honoured
    /// uniformly (chrome fills, surface request, and the opacity pass).
    pub fn effective_translucent(&self) -> bool {
        self.transparency_enabled && self.mode.is_translucent()
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
            last_geometry: None,
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
    /// Phase 18 T18.2 — enable the multi-note grid. When ON, the central
    /// editor surface renders every open tab as a movable, resizable pane
    /// inside an egui_tiles tree (up to 6 panes). Default OFF — the
    /// existing single-pane render path is unchanged for users who don't
    /// opt in.
    #[serde(default)]
    pub grid_enabled: bool,
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
}

/// serde default for opt-OUT booleans (fields that should be ON unless the
/// user turns them off, and ON for configs written before the field existed).
fn default_true() -> bool {
    true
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
            word_wrap: false,
            auto_save: false,
            restore_session: true,
            tab_bar_position: TabBarPosition::Top,
            grid_enabled: false,
            recent_files: Vec::new(),
            first_run_completed: false,
            scroll_positions: std::collections::HashMap::new(),
            experimental_rope_editor: false,
            session_backup: true,
            trim_trailing_whitespace_on_save: false,
            final_newline_on_save: false,
            restore_cursor_position: true,
            cursor_positions: std::collections::HashMap::new(),
            render_whitespace: false,
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
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            theme: "wired-noir".to_string(),
            follow_os_theme: true,
            frameless: true,
            toolbar_icons: false,
            jp_glyph_labels: false,
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
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            editor_size: 14.0,
            line_height: 1.4,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    Off,
    #[default]
    Notify,
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
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            mode: UpdateMode::Notify,
            check_interval_hours: 24,
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
            enabled: false,
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
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            disabled: Vec::new(),
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
    pub fn config_dir() -> Option<PathBuf> {
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
        assert_eq!(c.appearance.theme, "wired-noir");
    }

    #[test]
    fn malformed_is_error_not_panic() {
        assert!(Config::from_toml_str("editor = [[[").is_err());
    }

    #[test]
    fn spellcheck_off_by_default() {
        assert!(!Config::default().spellcheck.enabled);
    }

    #[test]
    fn motion_master_gates_all_effects() {
        // T17.3: master switch OFF must zero every per-effect query, even
        // when the per-effect toggle is on and reduced-motion / battery are
        // both fine. Defaults: master off => everything off.
        let m = MotionConfig::default();
        assert!(!m.enabled, "master defaults OFF");
        for effect in [
            MotionEffect::Hover,
            MotionEffect::CursorBlink,
            MotionEffect::Toast,
            MotionEffect::TransitionFade,
            MotionEffect::IdlePulse,
        ] {
            assert!(
                !m.active_for(effect, false, false),
                "master off must gate {effect:?}"
            );
        }

        // Master on, reduced-motion on => still off (respect_reduced_motion).
        let m_on = MotionConfig {
            enabled: true,
            ..MotionConfig::default()
        };
        assert!(m_on.active_for(MotionEffect::Hover, false, false));
        assert!(!m_on.active_for(MotionEffect::Hover, true, false));
        assert!(!m_on.active_for(MotionEffect::Hover, false, true));

        // Per-effect toggle OFF must gate even when master is effective.
        let m_partial = MotionConfig {
            enabled: true,
            hover: false,
            ..MotionConfig::default()
        };
        assert!(!m_partial.active_for(MotionEffect::Hover, false, false));
        assert!(m_partial.active_for(MotionEffect::CursorBlink, false, false));
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
    }

    #[test]
    fn transparency_off_by_default() {
        // Master toggle defaults OFF: a normal opaque window (no ghost-on-close
        // risk, no perf cost). See T19.1/T19.2.
        assert!(!Config::default().window.transparency_enabled);
        assert!(!Config::default().window.effective_translucent());
    }

    #[test]
    fn effective_translucent_requires_master_toggle_and_mode() {
        // Translucent mode alone is NOT enough — the master toggle gates it.
        let w = WindowConfig {
            mode: WindowMode::Glass,
            ..Default::default()
        };
        assert!(
            !w.effective_translucent(),
            "mode without toggle stays opaque"
        );
        // Toggle on + translucent mode => translucent.
        let w = WindowConfig {
            mode: WindowMode::Glass,
            transparency_enabled: true,
            ..Default::default()
        };
        assert!(w.effective_translucent());
        // Toggle on but Opaque mode => still opaque.
        let w = WindowConfig {
            mode: WindowMode::Opaque,
            transparency_enabled: true,
            ..Default::default()
        };
        assert!(!w.effective_translucent());
    }

    /// F-020: default `last_geometry` is None (first-launch falls back to
    /// hard-coded size) and the field round-trips through TOML.
    #[test]
    fn window_last_geometry_default_none_and_round_trips() {
        let mut c = Config::default();
        assert_eq!(c.window.last_geometry, None);
        c.window.last_geometry = Some((100.0, 200.0, 1280.0, 720.0));
        let s = c.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert_eq!(
            back.window.last_geometry,
            Some((100.0, 200.0, 1280.0, 720.0))
        );
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
