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
    /// Flicker cadence multiplier (0.25 = quarter-speed .. 3.0 = triple-speed,
    /// clamped). Scales the flicker's time input so the default `1.0` reproduces
    /// the current shipped cadence EXACTLY and higher values flicker faster.
    /// Independent of [`flicker_strength`], which tunes only the depth. Inert
    /// until [`flicker`](Self::flicker) is enabled (default OFF).
    #[serde(default = "default_flicker_speed")]
    pub flicker_speed: f32,
    /// VHS-style horizontal tracking lines that drift down the window. Off by
    /// default.
    #[serde(default)]
    pub vhs_tracking: bool,
    /// VHS tracking-line drift multiplier (0.25 .. 3.0, clamped). Scales BOTH
    /// tracking-band drift speeds proportionally so the default `1.0` reproduces
    /// the current shipped drift EXACTLY and higher values sweep faster. Inert
    /// until [`vhs_tracking`](Self::vhs_tracking) is enabled (default OFF).
    #[serde(default = "default_vhs_speed")]
    pub vhs_speed: f32,
    /// Animated wired node-mesh ambient background (Lain-inspired). Off by
    /// default; drawn at Background order behind the editor.
    #[serde(default)]
    pub wired_ambient: bool,
    /// Node-mesh density (0.0 = sparse .. 2.0 = dense, clamped). Drives the
    /// node count of the wired-ambient background. (M3: widened 0..1 -> 0..2 for
    /// C0PL4ND parity, with area-aware node scaling in the painter.)
    #[serde(default = "default_mesh_density")]
    pub mesh_density: f32,
    /// Wired-mesh brightness multiplier (0.0 = invisible .. 3.0 = bold, clamped).
    /// Scales the mesh link/dot alphas; the default `1.0` reproduces the current
    /// shipped look exactly (link alpha 16, dot alpha 40). (M1, C0PL4ND parity.)
    #[serde(default = "default_mesh_brightness")]
    pub mesh_brightness: f32,
    /// Wired-mesh node-drift multiplier (0.25 .. 3.0, clamped). Scales the mesh
    /// node drift rate so the default `1.0` reproduces the current shipped drift
    /// EXACTLY and higher values let the lattice breathe faster. Independent of
    /// [`mesh_density`]/[`mesh_brightness`], which tune the node count and alpha.
    /// Inert until [`wired_ambient`](Self::wired_ambient) is enabled (default OFF).
    #[serde(default = "default_mesh_drift_speed")]
    pub mesh_drift_speed: f32,
    /// Custom wired-mesh colour override, as sRGB `[r, g, b]`. `None` (the
    /// default) means the mesh FOLLOWS the active theme's accent colour. `Some`
    /// pins the mesh to a chosen colour; the Settings picker then shows a
    /// "Reset to theme" button that clears this back to `None`. Stored as a
    /// fixed byte array (not a hex `String`) so `MotionConfig` stays `Copy`.
    /// Inert until [`wired_ambient`](Self::wired_ambient) is enabled.
    #[serde(default)]
    pub mesh_color: Option<[u8; 3]>,
    /// Caret ghost-trail: a fading echo follows the caret as it moves. Off by
    /// default.
    #[serde(default)]
    pub caret_trail: bool,
    /// Caret-trail intensity (0.0 = faint short flick .. 2.0 = bold long comet
    /// tail, clamped). Scales BOTH the echo lifetime (via [`caret_trail_life`])
    /// and its peak opacity so the slider tunes the trail's reach. Inert until
    /// [`caret_trail`](Self::caret_trail) is enabled (default OFF). (M2, C0PL4ND
    /// parity.)
    #[serde(default = "default_caret_trail_intensity")]
    pub caret_trail_intensity: f32,
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

/// Default wired-mesh brightness — `1.0` reproduces the current shipped alphas
/// (link 16, dot 40) exactly, so enabling the field changes nothing by default.
fn default_mesh_brightness() -> f32 {
    1.0
}

/// Default caret-trail intensity — matches C0PL4ND's `0.6` (a ~0.65s tail). Inert
/// until `caret_trail` is enabled, so this default never changes the resting look.
fn default_caret_trail_intensity() -> f32 {
    0.6
}

/// Default flicker cadence multiplier — `1.0` reproduces the current shipped
/// flicker rate exactly, so enabling the field changes nothing by default.
fn default_flicker_speed() -> f32 {
    1.0
}

/// Default VHS tracking-drift multiplier — `1.0` reproduces the current shipped
/// drift speeds exactly, so enabling the field changes nothing by default.
fn default_vhs_speed() -> f32 {
    1.0
}

/// Default wired-mesh node-drift multiplier — `1.0` reproduces the current
/// shipped drift rate exactly, so enabling the field changes nothing by default.
fn default_mesh_drift_speed() -> f32 {
    1.0
}

/// Base wired-mesh link-line alpha at brightness `1.0` (the current shipped look).
const MESH_LINK_BASE_ALPHA: f32 = 16.0;
/// Base wired-mesh node-dot alpha at brightness `1.0` (the current shipped look).
const MESH_DOT_BASE_ALPHA: f32 = 40.0;

/// Lifetime (seconds) of a single caret-trail echo for a given `intensity`
/// (0..=2). A higher intensity lets each echo linger longer, so the trail reads
/// as a longer comet tail. Shared by the painter's fade math AND the caller's
/// deque-prune so the two never disagree about when an echo is dead — pure and
/// unit-testable. Ported verbatim from C0PL4ND's `cursor_trail_life`.
///
/// `0.35s` at zero intensity (a short flick) .. `1.35s` at max (a long tail); the
/// default config intensity (`0.6`) lands at `~0.65s`.
pub fn caret_trail_life(intensity: f32) -> f64 {
    (0.35 + 0.5 * intensity.clamp(0.0, 2.0)) as f64
}

impl MotionConfig {
    /// Clamped intensity so a malformed user config can't drive an animation
    /// outside its design band. (M4: widened 0..1 -> 0..2 for C0PL4ND parity.)
    pub fn clamped_intensity(&self) -> f32 {
        self.intensity.clamp(0.0, 2.0)
    }

    /// Flicker strength clamped to a calm, accessibility-safe ceiling.
    pub fn clamped_flicker_strength(&self) -> f32 {
        self.flicker_strength.clamp(0.0, 0.20)
    }

    /// Node-mesh density clamped to its design band. (M3: widened 0..1 -> 0..2.)
    pub fn clamped_mesh_density(&self) -> f32 {
        self.mesh_density.clamp(0.0, 2.0)
    }

    /// Wired-mesh brightness clamped to its design band (0..3). (M1.)
    pub fn clamped_mesh_brightness(&self) -> f32 {
        self.mesh_brightness.clamp(0.0, 3.0)
    }

    /// Wired-mesh link-line alpha, scaled by [`clamped_mesh_brightness`]. At the
    /// default brightness `1.0` this returns `16` — the current shipped look. (M1.)
    pub fn mesh_link_alpha(&self) -> u8 {
        (MESH_LINK_BASE_ALPHA * self.clamped_mesh_brightness())
            .round()
            .clamp(0.0, 255.0) as u8
    }

    /// Wired-mesh node-dot alpha, scaled by [`clamped_mesh_brightness`]. At the
    /// default brightness `1.0` this returns `40` — the current shipped look. (M1.)
    pub fn mesh_dot_alpha(&self) -> u8 {
        (MESH_DOT_BASE_ALPHA * self.clamped_mesh_brightness())
            .round()
            .clamp(0.0, 255.0) as u8
    }

    /// Caret-trail intensity clamped to its design band (0..2). (M2.)
    pub fn clamped_caret_trail_intensity(&self) -> f32 {
        self.caret_trail_intensity.clamp(0.0, 2.0)
    }

    /// Flicker cadence multiplier clamped to its design band (0.25..=3.0). At the
    /// default `1.0` the flicker runs at the current shipped cadence exactly.
    pub fn clamped_flicker_speed(&self) -> f32 {
        self.flicker_speed.clamp(0.25, 3.0)
    }

    /// VHS tracking-drift multiplier clamped to its design band (0.25..=3.0). At
    /// the default `1.0` the tracking bands drift at the current shipped speeds.
    pub fn clamped_vhs_speed(&self) -> f32 {
        self.vhs_speed.clamp(0.25, 3.0)
    }

    /// Wired-mesh node-drift multiplier clamped to its design band (0.25..=3.0).
    /// At the default `1.0` the lattice drifts at the current shipped rate.
    pub fn clamped_mesh_drift_speed(&self) -> f32 {
        self.mesh_drift_speed.clamp(0.25, 3.0)
    }

    /// Lifetime (seconds) of a caret-trail echo at the configured intensity. (M2.)
    pub fn caret_trail_life(&self) -> f64 {
        caret_trail_life(self.caret_trail_intensity)
    }

    /// Resolve the wired-mesh colour as sRGB `[r, g, b]`: the user override when
    /// one is pinned ([`mesh_color`](Self::mesh_color) is `Some`), else the
    /// supplied theme accent (the default "follow the theme" behaviour). Pure so
    /// the follow-theme-vs.-pinned choice is unit-testable without egui.
    pub fn resolved_mesh_color(&self, theme_accent: [u8; 3]) -> [u8; 3] {
        self.mesh_color.unwrap_or(theme_accent)
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
            flicker_speed: default_flicker_speed(),
            vhs_tracking: false,
            vhs_speed: default_vhs_speed(),
            wired_ambient: false,
            mesh_density: default_mesh_density(),
            mesh_brightness: default_mesh_brightness(),
            mesh_drift_speed: default_mesh_drift_speed(),
            mesh_color: None,
            caret_trail: false,
            caret_trail_intensity: default_caret_trail_intensity(),
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
    fn motion_intensity_clamps_to_two_band() {
        // M4: the animation-speed band widened 0..1 -> 0..2 for C0PL4ND parity.
        // A value above the new ceiling clamps to 2.0 (not 1.0), an in-band 1.5
        // passes through, and the default (0.6) is unchanged.
        let lo = MotionConfig {
            intensity: -5.0,
            ..MotionConfig::default()
        };
        let hi = MotionConfig {
            intensity: 42.0,
            ..MotionConfig::default()
        };
        let mid = MotionConfig {
            intensity: 1.5,
            ..MotionConfig::default()
        };
        assert_eq!(lo.clamped_intensity(), 0.0);
        assert_eq!(hi.clamped_intensity(), 2.0, "widened ceiling is 2.0");
        assert_eq!(mid.clamped_intensity(), 1.5, "1.5 is now in-band");
        assert_eq!(MotionConfig::default().clamped_intensity(), 0.6);
    }

    #[test]
    fn mesh_brightness_defaults_to_one_and_reproduces_current_alphas() {
        // M1: the new brightness field defaults to 1.0 and, at that default,
        // reproduces the CURRENT shipped mesh alphas exactly (link 16, dot 40) so
        // enabling the field changes nothing by default.
        let d = MotionConfig::default();
        assert_eq!(d.mesh_brightness, 1.0, "default brightness is 1.0");
        assert_eq!(d.mesh_link_alpha(), 16, "default reproduces link alpha 16");
        assert_eq!(d.mesh_dot_alpha(), 40, "default reproduces dot alpha 40");
        // Clamp band is [0, 3].
        let hi = MotionConfig {
            mesh_brightness: 9.0,
            ..MotionConfig::default()
        };
        let lo = MotionConfig {
            mesh_brightness: -1.0,
            ..MotionConfig::default()
        };
        assert_eq!(hi.clamped_mesh_brightness(), 3.0, "brightness ceiling 3.0");
        assert_eq!(lo.clamped_mesh_brightness(), 0.0, "brightness floor 0.0");
        // Brightness scales the alphas linearly (and saturates at 255).
        let bright = MotionConfig {
            mesh_brightness: 2.0,
            ..MotionConfig::default()
        };
        assert_eq!(bright.mesh_link_alpha(), 32, "2x brightness doubles link");
        assert_eq!(bright.mesh_dot_alpha(), 80, "2x brightness doubles dot");
        assert_eq!(lo.mesh_link_alpha(), 0, "zero brightness => invisible mesh");
        assert_eq!(hi.mesh_dot_alpha(), 120, "3x brightness triples dot");
        // Serde backfill: a config that predates the field loads with the 1.0
        // default (no visual change on upgrade).
        let cfg = Config::from_toml_str("[motion]\nwired_ambient = true\n").unwrap();
        assert_eq!(
            cfg.motion.mesh_brightness, 1.0,
            "absent field backfills to 1.0"
        );
    }

    #[test]
    fn cursor_trail_life_matches_c0pl4nd_curve() {
        // M2: the caret-trail life curve ported verbatim from C0PL4ND — a linear
        // 0.35s..1.35s ramp over intensity 0..2, with the config default (0.6)
        // landing at ~0.65s.
        assert!((caret_trail_life(0.0) - 0.35).abs() < 1e-6, "0.0 -> 0.35s");
        assert!((caret_trail_life(0.6) - 0.65).abs() < 1e-6, "0.6 -> 0.65s");
        assert!((caret_trail_life(2.0) - 1.35).abs() < 1e-6, "2.0 -> 1.35s");
        // Out-of-band intensity is clamped before the curve is applied.
        assert!((caret_trail_life(9.0) - 1.35).abs() < 1e-6, "clamps at 2.0");
        assert!(
            (caret_trail_life(-1.0) - 0.35).abs() < 1e-6,
            "clamps at 0.0"
        );
        // The method reads the configured intensity.
        assert!((MotionConfig::default().caret_trail_life() - 0.65).abs() < 1e-6);
    }

    #[test]
    fn caret_trail_default_off_so_intensity_is_inert() {
        // M2: the caret-trail feature is OFF by default, so its intensity (0.6)
        // never affects the resting surface — it only tunes the trail once the
        // user opts in. This is the "default-off so inert until enabled" contract.
        let d = MotionConfig::default();
        assert!(!d.caret_trail, "caret-trail is OFF by default");
        assert_eq!(d.caret_trail_intensity, 0.6, "intensity default is 0.6");
        // Intensity clamps to its 0..2 band.
        let hi = MotionConfig {
            caret_trail_intensity: 42.0,
            ..MotionConfig::default()
        };
        let lo = MotionConfig {
            caret_trail_intensity: -5.0,
            ..MotionConfig::default()
        };
        assert_eq!(hi.clamped_caret_trail_intensity(), 2.0);
        assert_eq!(lo.clamped_caret_trail_intensity(), 0.0);
        // Serde backfill leaves the trail off with the default intensity.
        let cfg = Config::from_toml_str("[motion]\nenabled = true\n").unwrap();
        assert!(!cfg.motion.caret_trail, "absent field keeps the trail off");
        assert_eq!(cfg.motion.caret_trail_intensity, 0.6);
    }

    #[test]
    fn per_effect_speeds_default_to_one_and_clamp_to_their_band() {
        // Per-effect speed multipliers default to 1.0 so the resting look is
        // byte-for-byte unchanged (1.0 reproduces the current shipped cadence for
        // flicker, VHS drift, and mesh drift). Each clamps to the 0.25..=3.0 band
        // so a hand-edited TOML can't drive a seizure-fast or frozen animation.
        let d = MotionConfig::default();
        assert_eq!(d.flicker_speed, 1.0, "flicker_speed default is 1.0");
        assert_eq!(d.vhs_speed, 1.0, "vhs_speed default is 1.0");
        assert_eq!(d.mesh_drift_speed, 1.0, "mesh_drift_speed default is 1.0");
        assert_eq!(d.clamped_flicker_speed(), 1.0);
        assert_eq!(d.clamped_vhs_speed(), 1.0);
        assert_eq!(d.clamped_mesh_drift_speed(), 1.0);
        // Above-band values clamp to the 3.0 ceiling.
        let hi = MotionConfig {
            flicker_speed: 42.0,
            vhs_speed: 9.0,
            mesh_drift_speed: 5.0,
            ..MotionConfig::default()
        };
        assert_eq!(hi.clamped_flicker_speed(), 3.0, "flicker ceiling 3.0");
        assert_eq!(hi.clamped_vhs_speed(), 3.0, "vhs ceiling 3.0");
        assert_eq!(hi.clamped_mesh_drift_speed(), 3.0, "mesh-drift ceiling 3.0");
        // Below-band (incl. zero/negative) values clamp UP to the 0.25 floor so a
        // frozen animation is impossible.
        let lo = MotionConfig {
            flicker_speed: 0.0,
            vhs_speed: -3.0,
            mesh_drift_speed: 0.1,
            ..MotionConfig::default()
        };
        assert_eq!(lo.clamped_flicker_speed(), 0.25, "flicker floor 0.25");
        assert_eq!(lo.clamped_vhs_speed(), 0.25, "vhs floor 0.25");
        assert_eq!(lo.clamped_mesh_drift_speed(), 0.25, "mesh-drift floor 0.25");
        // An in-band value passes through untouched.
        let mid = MotionConfig {
            flicker_speed: 1.5,
            vhs_speed: 2.0,
            mesh_drift_speed: 0.5,
            ..MotionConfig::default()
        };
        assert_eq!(mid.clamped_flicker_speed(), 1.5);
        assert_eq!(mid.clamped_vhs_speed(), 2.0);
        assert_eq!(mid.clamped_mesh_drift_speed(), 0.5);
        // Serde backfill: a config that predates these fields loads with the 1.0
        // default (no visual change on upgrade).
        let cfg = Config::from_toml_str("[motion]\nflicker = true\n").unwrap();
        assert_eq!(
            cfg.motion.flicker_speed, 1.0,
            "absent field backfills to 1.0"
        );
        assert_eq!(cfg.motion.vhs_speed, 1.0, "absent field backfills to 1.0");
        assert_eq!(
            cfg.motion.mesh_drift_speed, 1.0,
            "absent field backfills to 1.0"
        );
    }

    #[test]
    fn mesh_color_defaults_to_follow_theme_and_round_trips() {
        // The mesh colour override defaults to None => the mesh follows the theme
        // accent. `resolved_mesh_color` returns the theme accent when unset and
        // the pinned colour when set. A config predating the field backfills to
        // None (no visual change on upgrade). The override round-trips through
        // TOML.
        let d = MotionConfig::default();
        assert_eq!(d.mesh_color, None, "default follows the theme");
        let accent = [0x00, 0xe5, 0xff];
        assert_eq!(
            d.resolved_mesh_color(accent),
            accent,
            "unset => follow the theme accent"
        );
        let pinned = MotionConfig {
            mesh_color: Some([0x12, 0x34, 0x56]),
            ..MotionConfig::default()
        };
        assert_eq!(
            pinned.resolved_mesh_color(accent),
            [0x12, 0x34, 0x56],
            "set => the pinned colour wins over the theme accent"
        );
        // Serde backfill: absent field loads as None.
        let cfg = Config::from_toml_str("[motion]\nwired_ambient = true\n").unwrap();
        assert_eq!(
            cfg.motion.mesh_color, None,
            "absent field backfills to None"
        );
        // Round-trip a pinned colour through the config TOML.
        let mut c2 = Config::default();
        c2.motion.mesh_color = Some([10, 20, 30]);
        let s = c2.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert_eq!(back.motion.mesh_color, Some([10, 20, 30]));
    }

    // ---- MotionConfig clamps (previously uncovered) ----

    #[test]
    fn motion_flicker_and_mesh_density_clamp_to_their_bands() {
        // A hand-edited TOML could drive these out of their design band; the
        // clamps keep flicker within the accessibility-safe 0.20 ceiling and the
        // mesh density within [0, 2] (M3: widened from [0, 1]), regardless of the
        // stored value.
        let wild = MotionConfig {
            flicker_strength: 5.0,
            mesh_density: 9.0,
            ..Default::default()
        };
        assert_eq!(wild.clamped_flicker_strength(), 0.20, "flicker ceiling");
        assert_eq!(
            wild.clamped_mesh_density(),
            2.0,
            "mesh density ceiling (M3)"
        );
        let neg = MotionConfig {
            flicker_strength: -1.0,
            mesh_density: -1.0,
            ..Default::default()
        };
        assert_eq!(neg.clamped_flicker_strength(), 0.0, "flicker floor");
        assert_eq!(neg.clamped_mesh_density(), 0.0, "mesh density floor");
        // An in-band value passes through untouched — including a value (1.5) that
        // is only reachable AFTER the M3 widening.
        let ok = MotionConfig {
            flicker_strength: 0.1,
            mesh_density: 1.5,
            ..Default::default()
        };
        assert_eq!(ok.clamped_flicker_strength(), 0.1);
        assert_eq!(ok.clamped_mesh_density(), 1.5, "1.5 is in-band after M3");
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
