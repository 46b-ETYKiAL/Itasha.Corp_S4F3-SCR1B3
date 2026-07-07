//! Motion / animation catalog and scroll-behaviour configuration
//! ([`MotionConfig`] + [`ScrollConfig`]).

use serde::{Deserialize, Serialize};

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
///
/// `drag_autoscroll` (opt-out) drives the viewport while a LEFT-drag selection
/// is in progress: rolling the wheel (or holding the pointer at the top/bottom
/// viewport edge) scrolls the editor so egui's own TextEdit extends the
/// selection past the visible region — the reported "can't select past the
/// viewport" bug. `scroll_past_end` pads blank space below the last line so it
/// can sit at a comfortable height (VS Code `scrollBeyondLastLine`).
/// `caret_scroll_off` keeps the caret at least N lines from the top/bottom edge
/// during keyboard navigation (Vim `scrolloff` / VS Code `cursorSurroundingLines`;
/// `0` disables).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ScrollConfig {
    pub speed: f32,
    pub animate_jumps: bool,
    pub autoscroll: bool,
    pub autoscroll_sensitivity: f32,
    pub autoscroll_dead_zone: f32,
    pub drag_autoscroll: bool,
    pub scroll_past_end: bool,
    pub caret_scroll_off: u8,
}

impl Default for ScrollConfig {
    fn default() -> Self {
        Self {
            speed: 75.0,
            animate_jumps: true,
            autoscroll: true,
            autoscroll_sensitivity: 6.0,
            autoscroll_dead_zone: 12.0,
            drag_autoscroll: true,
            scroll_past_end: true,
            caret_scroll_off: 3,
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
    /// Caret keep-away margin in lines, clamped so a malformed toml can't demand
    /// a margin taller than a usable viewport. `0` disables (egui's just-in-view
    /// default).
    pub fn clamped_caret_scroll_off(&self) -> u8 {
        self.caret_scroll_off.min(12)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn scroll_defaults_are_sane_and_clamp() {
        let s = ScrollConfig::default();
        assert_eq!(s.speed, 75.0);
        assert!(s.animate_jumps && s.autoscroll);
        // Drag-select scroll conveniences default ON; caret keep-away = 3 lines.
        assert!(s.drag_autoscroll && s.scroll_past_end);
        assert_eq!(s.caret_scroll_off, 3);
        // Out-of-band values clamp to the slider range.
        let wild = ScrollConfig {
            speed: 9000.0,
            autoscroll_sensitivity: -3.0,
            autoscroll_dead_zone: 999.0,
            caret_scroll_off: 200,
            ..ScrollConfig::default()
        };
        assert_eq!(wild.clamped_speed(), 200.0);
        assert_eq!(wild.clamped_sensitivity(), 2.0);
        assert_eq!(wild.clamped_dead_zone(), 40.0);
        assert_eq!(wild.clamped_caret_scroll_off(), 12);
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

    // ---- MotionConfig clamps (previously uncovered) ----

    #[test]
    fn motion_flicker_and_mesh_density_clamp_to_their_bands() {
        // A hand-edited TOML could drive these out of their design band; the
        // clamps keep flicker within the accessibility-safe 0.20 ceiling and the
        // mesh density within [0, 1], regardless of the stored value.
        let wild = MotionConfig {
            flicker_strength: 5.0,
            mesh_density: 9.0,
            ..Default::default()
        };
        assert_eq!(wild.clamped_flicker_strength(), 0.20, "flicker ceiling");
        assert_eq!(wild.clamped_mesh_density(), 1.0, "mesh density ceiling");
        let neg = MotionConfig {
            flicker_strength: -1.0,
            mesh_density: -1.0,
            ..Default::default()
        };
        assert_eq!(neg.clamped_flicker_strength(), 0.0, "flicker floor");
        assert_eq!(neg.clamped_mesh_density(), 0.0, "mesh density floor");
        // An in-band value passes through untouched.
        let ok = MotionConfig {
            flicker_strength: 0.1,
            mesh_density: 0.5,
            ..Default::default()
        };
        assert_eq!(ok.clamped_flicker_strength(), 0.1);
        assert_eq!(ok.clamped_mesh_density(), 0.5);
    }

    // ---- MUTANT-EQUIVALENT (config/motion.rs): documented, intentionally not tested ----
    //
    // The three surviving mutants are the serde-default *initial* values for
    // purely-presentational CRT post-effects. No control-flow branches on these
    // numbers — they are starting positions for UI sliders, each independently
    // clamped to its design band by a `clamped_*` method (which IS tested above,
    // killing the clamp-boundary mutants). Pinning the exact default 0.06 / 0.3 /
    // 0.4 with an equality assertion would assert a cosmetic constant for its own
    // sake (the kind of default-value test-bloat the triage brief forbids):
    //
    // MUTANT-EQUIVALENT: config/motion.rs:116 — `default_scanline_darkness -> 0.0/1.0/-1.0`.
    //   Initial scanline darkness slider value (0.3). Cosmetic; clamped at use.
    // MUTANT-EQUIVALENT: config/motion.rs:121 — `default_flicker_strength -> 0.0/1.0/-1.0`.
    //   Initial flicker-strength slider value (0.06). Cosmetic; the 0.20 ceiling
    //   is enforced by `clamped_flicker_strength` (tested), not by this default.
    // MUTANT-EQUIVALENT: config/motion.rs:126 — `default_mesh_density -> 0.0/1.0/-1.0`.
    //   Initial wired-ambient mesh-density slider value (0.4). Cosmetic; clamped
    //   to [0,1] by `clamped_mesh_density` (tested) regardless of this default.
}
