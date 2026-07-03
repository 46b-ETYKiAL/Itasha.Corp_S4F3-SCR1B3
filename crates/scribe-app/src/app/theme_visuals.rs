//! Theme, visuals, and motion-style application — extracted from `mod.rs` (A-01 wave 2).
#![allow(clippy::wildcard_imports)]

use super::*;

impl ScribeApp {
    /// A hash of every config input `current_visuals` reads, so `frame_tick` can
    /// rebuild the egui visuals the instant one changes (the tint colour /
    /// strength slider, opacity, translucency, background overrides, or theme) —
    /// rather than only once at startup. This is what makes the tint slider
    /// update the main window live.
    pub(super) fn visuals_signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.theme.name.hash(&mut h);
        self.config.appearance.background_override.hash(&mut h);
        self.config.appearance.note_background_override.hash(&mut h);
        self.config.appearance.link_backgrounds.hash(&mut h);
        let w = &self.config.window;
        w.transparency_enabled.hash(&mut h);
        w.tint_enabled.hash(&mut h);
        w.tint.hash(&mut h);
        // f32 has no Hash; hash its bit pattern.
        w.tint_strength.to_bits().hash(&mut h);
        w.opacity.to_bits().hash(&mut h);
        h.finish()
    }

    /// Build the egui visuals for the current theme, applying surface opacity
    /// when a translucent/glass window mode is active.
    pub(super) fn current_visuals(&self) -> egui::Visuals {
        let mut v = scribe_render::theme_to_visuals(&self.theme);
        let parse = |o: &Option<String>| {
            o.as_deref()
                .and_then(Rgba::parse_hex)
                .map(|c| Color32::from_rgb(c.r, c.g, c.b))
        };
        // #88 — app-background override (independent of the theme) repaints the
        // central panel + window backgrounds. None = follow theme.
        let app_bg = parse(&self.config.appearance.background_override);
        if let Some(c) = app_bg {
            v.panel_fill = c;
            v.window_fill = c;
        }
        // #106 — note (editor well) background. When linked it follows the app
        // background override; when unlinked it uses its own override. None at
        // the chosen source = follow the theme's editor background.
        let note_bg = if self.config.appearance.link_backgrounds {
            app_bg
        } else {
            parse(&self.config.appearance.note_background_override)
        };
        if let Some(c) = note_bg {
            v.extreme_bg_color = c;
        }
        // Tint the MAIN-APP background surfaces by the same amount the chrome
        // panels are tinted (see `render_support::apply_window_tint` /
        // `panel_fill`): the central editor panel (`panel_fill`) and the editor
        // well (`extreme_bg_color`) shift toward the tint colour while glyph/text
        // colours (a separate `foreground` path) stay untinted. Applied before the
        // translucency alpha so the tint composes with glass/mica window modes.
        //
        // `window_fill` is deliberately NOT tinted: floating windows — the
        // Settings popup (which pins its frame opaque, so a tinted window_fill
        // showed a bold tint over the whole dialog) and any other egui::Window —
        // use it, and the colour tint is a MAIN-APP-window effect only. Tinting
        // window_fill was the v0.4.48 regression where the Settings popup tinted
        // while the (translucent) main window looked comparatively untinted.
        v.panel_fill = super::render_support::apply_window_tint(v.panel_fill, &self.config.window);
        v.extreme_bg_color =
            super::render_support::apply_window_tint(v.extreme_bg_color, &self.config.window);
        if self.config.window.effective_translucent() {
            scribe_render::apply_window_opacity(&mut v, self.config.window.opacity);
        }
        v
    }

    /// Resolve which theme name to actually load, honoring
    /// `appearance.follow_os_theme`. When that is on and the OS reports its
    /// theme, the OS decides light vs dark: a light OS → the bundled light
    /// theme (`ghost-paper`); a dark OS → the user's chosen theme if it is
    /// itself dark, otherwise the default dark theme (`wired-noir`). When the
    /// toggle is off, or the OS theme is unknown, the user's chosen theme wins.
    fn effective_theme_name(&self, os_theme: egui::Theme) -> String {
        if self.config.appearance.follow_os_theme {
            match os_theme {
                egui::Theme::Light => return "ghost-paper".to_string(),
                egui::Theme::Dark => {
                    let chosen = load_theme(&self.config.appearance.theme);
                    return if matches!(chosen.appearance, scribe_core::theme::Appearance::Dark) {
                        self.config.appearance.theme.clone()
                    } else {
                        "wired-noir".to_string()
                    };
                }
            }
        }
        self.config.appearance.theme.clone()
    }

    /// Apply the current theme to the egui context (after a theme/config change).
    /// Reads the OS theme via `ctx.theme()` — egui-winit tracks the OS theme when
    /// the theme preference is `System` (set in `new`). `raw.system_theme` is
    /// unreliable/None on Windows, which is why "Follow OS theme" did nothing.
    pub(super) fn reapply_theme(&mut self, ctx: &egui::Context) {
        let os_theme = ctx.theme();
        self.last_os_theme = Some(os_theme);
        self.theme = load_theme(&self.effective_theme_name(os_theme));
        ctx.set_visuals(self.current_visuals());
        // `set_visuals` resets the caret style, so re-apply motion after it.
        self.apply_motion_style(ctx);
    }

    /// Push the `motion` preferences into egui's global style. Motion off zeroes
    /// the animation time (instant transitions, no hover fades — idle frames
    /// cost the same as plain egui) and stops the caret blinking; otherwise the
    /// intensity scales egui's default animation time. This is the whole Motion
    /// feature: only effects egui drives natively are exposed, so there are no
    /// dead per-effect toggles.
    pub(super) fn apply_motion_style(&self, ctx: &egui::Context) {
        // egui's stock animation time is 1/12 s; scale it by intensity, or zero
        // it when motion is disabled.
        const EGUI_DEFAULT_ANIMATION_TIME: f32 = 1.0 / 12.0;
        let anim = if self.config.motion.enabled {
            EGUI_DEFAULT_ANIMATION_TIME * self.config.motion.clamped_intensity()
        } else {
            0.0
        };
        let blink = self.config.motion.enabled && self.config.motion.cursor_blink;
        ctx.style_mut(|s| {
            s.animation_time = anim;
            s.visuals.text_cursor.blink = blink;
        });
    }
}
