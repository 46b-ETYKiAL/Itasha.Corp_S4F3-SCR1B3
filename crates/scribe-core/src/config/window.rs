//! Window appearance configuration: translucency mode, opacity, and tint
//! overlay ([`WindowMode`] + [`WindowConfig`]).

use serde::{Deserialize, Serialize};

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
    /// Surface opacity for translucent modes (0.0..=1.0; the 0.0 floor lets the
    /// window go fully transparent — the editor text is painted opaque on top,
    /// so it stays legible even at zero chrome alpha).
    pub opacity: f32,
    /// Master on/off switch for the window colour tint. When `false` no tint is
    /// applied regardless of `tint`/`tint_strength`, so the user can toggle the
    /// effect without losing their chosen colour + strength. Default ON (the
    /// tint only shows once `tint_strength` is raised above 0).
    pub tint_enabled: bool,
    /// Tint color (`#RRGGBB`) blended into the window background at `tint_strength`.
    pub tint: String,
    /// Tint strength (0.0 = none .. 1.0 = strong).
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
            // Fresh installs default to fully opaque. Inert unless the user
            // enables transparency (`transparency_enabled` defaults false).
            // Fresh-configs-only: an already-persisted config keeps whatever
            // opacity it stored (serde deserializes it untouched) — this default
            // only applies to a brand-new / never-set config.
            opacity: 1.0,
            tint_enabled: true,
            tint: "#08060d".to_string(),
            tint_strength: 0.0,
            always_on_top: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

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
}
