//! System / behaviour configuration: update checks, spellcheck, plugins, and
//! the customizable quick-access toolbar
//! ([`UpdateConfig`] / [`SpellcheckConfig`] / [`PluginConfig`] /
//! [`ToolbarConfig`]).

use super::default_true;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    Off,
    /// Default: on launch, do a single telemetry-free version check against the
    /// public GitHub Releases API (no PII) and surface a passive notice when a
    /// newer release exists. Nothing is downloaded without the user acting.
    #[default]
    Notify,
    /// No automatic network connection — the user checks for updates on demand
    /// from Settings.
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
            // Notify by default: a single on-launch GitHub-Releases version check
            // (telemetry-free, no PII) that surfaces a passive notice. Off and
            // Manual are explicit opt-outs of the automatic check; Auto opts into
            // background download.
            mode: UpdateMode::Notify,
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
    /// Whether the "⋯" overflow dropdown is shown on the toolbar. Default ON —
    /// when on, the dropdown appears whenever `menu` is non-empty; turn it off to
    /// hide the dropdown button entirely (the parked actions stay reachable via
    /// the command palette). Toggled in Settings → Toolbar.
    #[serde(default = "default_true")]
    pub show_dropdown: bool,
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
            show_dropdown: true,
            button_size_px: Self::default_button_size(),
            button_spacing_px: Self::default_button_spacing(),
            icon_size_px: Self::default_icon_size(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn spellcheck_on_by_default() {
        assert!(Config::default().spellcheck.enabled);
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
    fn toolbar_dropdown_defaults_visible() {
        assert!(ToolbarConfig::default().show_dropdown);
        // Missing key also defaults ON (default_true), not bool::default()=false.
        let cfg: ToolbarConfig = toml::from_str("").unwrap();
        assert!(
            cfg.show_dropdown,
            "missing show_dropdown key must default ON"
        );
    }
}
