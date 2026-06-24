//! Appearance + font configuration: the [`AppearanceConfig`] (theme / chrome)
//! and [`FontConfig`] (editor + UI font families and sizing) sections.

use super::default_true;
use serde::{Deserialize, Serialize};

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
    /// `frameless` on (the titlebar exists only then). Default ON (the buttons
    /// render identically to the standalone toolbar row).
    #[serde(default = "default_true")]
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
            toolbar_in_titlebar: true,
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
    /// default. Default: "IBM Plex Mono".
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
    // Default the app UI to the same face as the note body (IBM Plex Mono) so the
    // whole app reads as one typeface out of the box. Users can still pick
    // "System default" or any other bundled family in Settings → Fonts.
    "IBM Plex Mono".to_string()
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

impl FontConfig {
    /// Editor font size clamped to a sane band, so a hand-edited TOML with
    /// `editor_size = 0` (or negative) can't render an invisible editor. Mirrors
    /// the `clamped_*` discipline every other config struct uses.
    pub fn clamped_editor_size(&self) -> f32 {
        self.editor_size.clamp(6.0, 96.0)
    }

    /// Line-height multiplier clamped to a sane band (a `0` would collapse every
    /// row to zero height; a huge value would explode the gutter).
    pub fn clamped_line_height(&self) -> f32 {
        self.line_height.clamp(0.8, 4.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn default_note_font_is_ibm_plex_mono() {
        assert_eq!(Config::default().fonts.editor_family, "IBM Plex Mono");
    }

    #[test]
    fn toolbar_in_titlebar_defaults_on() {
        // The compact single-row chrome (toolbar in the custom titlebar) is the
        // default. frameless is also on by default, so the titlebar exists to
        // host it.
        assert!(AppearanceConfig::default().toolbar_in_titlebar);
        assert!(AppearanceConfig::default().frameless);
        // A config that OMITS the key must also default ON (via `default_true`),
        // not fall back to bool::default() = false.
        let cfg: AppearanceConfig = toml::from_str("").unwrap();
        assert!(
            cfg.toolbar_in_titlebar,
            "missing toolbar_in_titlebar key must default ON"
        );
    }

    #[test]
    fn font_size_and_line_height_are_clamped() {
        // A hand-edited TOML with 0/negative values must not produce an invisible
        // (zero-size) or zero-height editor.
        let bad = FontConfig {
            editor_size: 0.0,
            line_height: 0.0,
            ..FontConfig::default()
        };
        assert_eq!(bad.clamped_editor_size(), 6.0);
        assert_eq!(bad.clamped_line_height(), 0.8);
        let huge = FontConfig {
            editor_size: 9999.0,
            line_height: 99.0,
            ..FontConfig::default()
        };
        assert_eq!(huge.clamped_editor_size(), 96.0);
        assert_eq!(huge.clamped_line_height(), 4.0);
        // The default is within band (unchanged).
        let d = FontConfig::default();
        assert_eq!(d.clamped_editor_size(), d.editor_size);
        assert_eq!(d.clamped_line_height(), d.line_height);
    }

    #[test]
    fn note_and_ui_font_default_to_ibm_plex_mono() {
        let f = FontConfig::default();
        assert_eq!(f.editor_family, "IBM Plex Mono");
        assert_eq!(f.ui_family, "IBM Plex Mono");
        // A config missing the keys resolves to the same defaults.
        let parsed: FontConfig = toml::from_str("").unwrap();
        assert_eq!(parsed.editor_family, "IBM Plex Mono");
        assert_eq!(parsed.ui_family, "IBM Plex Mono");
    }
}
