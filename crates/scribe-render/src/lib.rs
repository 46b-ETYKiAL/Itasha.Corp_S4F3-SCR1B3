//! SCR1B3 render layer: maps the engine-agnostic `Theme` onto egui `Visuals`,
//! converts colors, and carries CRT post-process parameters. Keeps the
//! `egui`-specific mapping out of `scribe-core`.

use egui::{Color32, Stroke, Visuals};
use scribe_core::theme::{Appearance, Rgba, Theme};

/// Convert an engine `Rgba` to an egui `Color32`.
#[inline]
pub fn color32(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

fn ui(theme: &Theme, key: &str, default: Rgba) -> Color32 {
    color32(theme.ui(key, default))
}

/// Build an egui `Visuals` from a SCR1B3 theme. High-value chrome colors are
/// mapped from the theme; the rest is derived so a user theme need only define
/// a small key set (anti-bloat).
pub fn theme_to_visuals(theme: &Theme) -> Visuals {
    let mut v = match theme.appearance {
        Appearance::Dark => Visuals::dark(),
        Appearance::Light => Visuals::light(),
    };
    let bg = ui(theme, "background", Rgba::new(0x08, 0x06, 0x0d, 255));
    let panel = ui(theme, "panel", Rgba::new(0x0d, 0x0b, 0x14, 255));
    let fg = ui(theme, "foreground", Rgba::new(0xd6, 0xe2, 0xf0, 255));
    let accent = ui(theme, "accent", Rgba::new(0x00, 0xff, 0xfe, 255));
    let selection = ui(theme, "selection", Rgba::new(0x00, 0xff, 0xfe, 0x33));

    v.extreme_bg_color = bg;
    v.panel_fill = panel;
    v.window_fill = panel;
    v.faint_bg_color = panel;
    v.override_text_color = Some(fg);
    v.hyperlink_color = accent;
    v.selection.bg_fill = selection;
    v.selection.stroke = Stroke::new(1.0, accent);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, accent);
    v.widgets.active.bg_stroke = Stroke::new(1.0, accent);
    v.error_fg_color = ui(theme, "error", Rgba::new(0xff, 0x00, 0x40, 255));
    v.warn_fg_color = ui(theme, "warning", Rgba::new(0xfb, 0xbf, 0x24, 255));
    v
}

/// Map a syntect span RGB to an egui color, optionally re-tinted by the active
/// SCR1B3 theme's syntax palette (kept simple: pass-through for v1).
#[inline]
pub fn syntax_color32(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

/// Lower the alpha of the surface fills so a translucent/glass window reveals
/// what is behind it. `opacity` is clamped to [0.30, 1.0].
pub fn apply_window_opacity(v: &mut Visuals, opacity: f32) {
    let a = (opacity.clamp(0.30, 1.0) * 255.0).round() as u8;
    let with_a = |c: Color32| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a);
    v.panel_fill = with_a(v.panel_fill);
    v.window_fill = with_a(v.window_fill);
    v.extreme_bg_color = with_a(v.extreme_bg_color);
    v.faint_bg_color = with_a(v.faint_bg_color);
}

/// CRT post-process parameters (consumed by the optional wgpu post-pass).
/// When `enabled` is false the post-pass is skipped entirely (zero cost).
#[derive(Debug, Clone, Copy)]
pub struct CrtParams {
    pub enabled: bool,
    pub scanline: f32,
    pub phosphor_glow: f32,
    pub bloom: f32,
    pub vignette: f32,
    pub curvature: f32,
    pub chromatic_aberration: f32,
}

impl CrtParams {
    /// Build from config, zeroing animated terms when the OS requests reduced
    /// motion (accessibility).
    pub fn from_effects(e: &scribe_core::config::EffectsConfig, reduced_motion: bool) -> Self {
        let gate = |v: f32| {
            if reduced_motion && e.respect_reduced_motion {
                0.0
            } else {
                v
            }
        };
        CrtParams {
            enabled: e.crt_enabled,
            scanline: gate(e.scanline),
            phosphor_glow: e.phosphor_glow,
            bloom: e.bloom,
            vignette: e.vignette,
            curvature: e.curvature,
            chromatic_aberration: e.chromatic_aberration,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_conversion() {
        assert_eq!(
            color32(Rgba::new(1, 2, 3, 4)),
            Color32::from_rgba_unmultiplied(1, 2, 3, 4)
        );
    }

    #[test]
    fn visuals_from_brand_theme() {
        let v = theme_to_visuals(&Theme::itasha_void());
        assert_eq!(v.extreme_bg_color, Color32::from_rgb(0x08, 0x06, 0x0d));
    }

    #[test]
    fn reduced_motion_zeros_scanline() {
        let e = scribe_core::config::EffectsConfig {
            crt_enabled: true,
            scanline: 0.5,
            ..Default::default()
        };
        let p = CrtParams::from_effects(&e, true);
        assert_eq!(p.scanline, 0.0);
        assert!(p.enabled);
    }
}
