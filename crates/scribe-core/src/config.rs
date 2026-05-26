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
    pub effects: EffectsConfig,
    pub window: WindowConfig,
    pub updates: UpdateConfig,
    pub spellcheck: SpellcheckConfig,
    pub plugins: PluginConfig,
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
    pub mode: WindowMode,
    /// Surface opacity for translucent modes (0.30..=1.0).
    pub opacity: f32,
    /// Tint color (`#RRGGBB`) painted over the window at `tint_strength`.
    pub tint: String,
    /// Tint overlay strength (0.0 = none .. 1.0 = strong).
    pub tint_strength: f32,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            mode: WindowMode::Opaque,
            opacity: 0.92,
            tint: "#08060d".to_string(),
            tint_strength: 0.0,
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
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            theme: "itasha-void".to_string(),
            follow_os_theme: true,
            frameless: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FontConfig {
    pub editor_family: Vec<String>,
    pub ui_family: Vec<String>,
    pub editor_size: f32,
    pub line_height: f32,
    pub ligatures: bool,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            editor_family: vec![
                "JetBrains Mono".into(),
                "Cascadia Code".into(),
                "Consolas".into(),
            ],
            ui_family: vec!["Inter".into()],
            editor_size: 14.0,
            line_height: 1.4,
            ligatures: true,
        }
    }
}

/// CRT/retro post-process. Disabled by default (zero cost when off).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EffectsConfig {
    pub crt_enabled: bool,
    pub scanline: f32,
    pub phosphor_glow: f32,
    pub bloom: f32,
    pub vignette: f32,
    pub curvature: f32,
    pub chromatic_aberration: f32,
    pub respect_reduced_motion: bool,
}

impl Default for EffectsConfig {
    fn default() -> Self {
        Self {
            crt_enabled: false,
            scanline: 0.30,
            phosphor_glow: 0.20,
            bloom: 0.15,
            vignette: 0.25,
            curvature: 0.0,
            chromatic_aberration: 0.05,
            respect_reduced_motion: true,
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
        assert_eq!(c.appearance.theme, "itasha-void");
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
    fn crt_off_by_default() {
        assert!(!Config::default().effects.crt_enabled);
    }
}
