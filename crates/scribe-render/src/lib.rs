//! SCR1B3 render layer: maps the engine-agnostic `Theme` onto egui `Visuals`,
//! converts colors, and hosts the rope-editor widget. Keeps the
//! `egui`-specific mapping out of `scribe-core`.
//!
//! Phase 21 T21.2 P1 — `#![forbid(unsafe_code)]`. This crate is pure-safe
//! Rust: theme → Visuals, color math, rope-editor paint. No mmap,
//! no FFI, no transmute path is needed; the forbid is unconditional.

#![forbid(unsafe_code)]

pub mod rope_editor;

pub use rope_editor::{
    apply_event, BufferModeSeen, EventOutcome, RopeEditor, RopeEditorResponse, RopeEditorState,
};

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
/// what is behind it. `opacity` is clamped to [0.02, 1.0] — the 0.02 floor
/// matches the settings slider minimum so the whole slider travel is live
/// (a previous 0.30 floor made the bottom quarter of the slider a no-op; #24
/// dropped 0.05 → 0.02 so the lowest setting is genuinely near-glass), while
/// staying just above fully-invisible so the window can never be lost (the
/// editor text itself is painted opaque on top, so it stays legible even at the
/// floor — only the chrome/background fills go translucent).
pub fn apply_window_opacity(v: &mut Visuals, opacity: f32) {
    // Floor at 0.0 so the window can be made FULLY transparent (max see-through).
    // The editor text itself is painted opaque on top, so it stays legible even
    // at zero chrome alpha — only the background/panel fills vanish.
    let a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    let with_a = |c: Color32| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a);
    // Only the PANEL surface goes translucent (it is what sits over the desktop).
    v.panel_fill = with_a(v.panel_fill);
    v.extreme_bg_color = with_a(v.extreme_bg_color);
    v.faint_bg_color = with_a(v.faint_bg_color);
    // `window_fill` is DELIBERATELY left opaque. egui draws combo-box dropdowns,
    // context menus, and tooltips with `Frame::menu`/`Frame::popup`, both of which
    // take their fill from `window_fill` — lowering its alpha makes every dropdown
    // and tooltip see-through and unreadable, and makes the floating Settings
    // window darken toward black as opacity drops (it composites over the panels
    // behind it). Keeping it solid means popups/tooltips/the Settings window stay
    // legible and hold their colour regardless of the opacity slider; only the
    // main panels reveal the desktop.
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
        let v = theme_to_visuals(&Theme::wired_noir());
        assert_eq!(v.extreme_bg_color, Color32::from_rgb(0x07, 0x0a, 0x0c));
    }
}
