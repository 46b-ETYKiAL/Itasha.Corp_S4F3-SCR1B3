//! CRT / motion visual effects: ctx-layer painters for the tint,
//! scanline, flicker, VHS, wired-mesh, caret-trail, and boot-glitch
//! overlays. Extracted from the `app` module root.
#![allow(clippy::wildcard_imports)]
use super::*;

/// Window colour-tint — now an inert no-op.
///
/// The tint used to be painted here as a translucent full-window rect at
/// `Order::Background`. That was an OVERLAY layer whose visibility depended on
/// the panel-fill alpha: in an opaque window it did nothing, and in a
/// translucent/glass window it washed the ENTIRE content area — the background
/// behind AND around every glyph — so the user perceived the text itself as
/// tinted ("it tints the ENTIRE app including the text").
///
/// The tint is now applied as COLOUR MATH on the background surfaces instead:
/// `render_support::apply_window_tint` blends the tint into the chrome panel
/// fill (`panel_fill`) and into the editor visuals (`current_visuals`). That
/// shifts only background colours; glyphs are painted on top with their own
/// untinted theme colours, so text is never affected. This function is kept as
/// a no-op purely so its existing call site stays valid without change.
pub(super) fn paint_tint_overlay(_ctx: &egui::Context, _tint_hex: &str, _strength: f32) {}

/// Paint translucent horizontal CRT scanlines over the window — a calm retro
/// post-effect ported from C0PL4ND. Dark bands a few points apart at `darkness`
/// strength, drifting slowly with `t` (seconds) so they shimmer like a live CRT
/// rather than reading as a static grid. `Order::Foreground` so they sit OVER
/// the text; the caller gates this behind `motion.enabled && crt_scanlines`.
pub(super) fn paint_crt_scanlines(ctx: &egui::Context, darkness: f32, t: f64) {
    let alpha = (darkness.clamp(0.0, 1.0) * 200.0).round() as u8;
    if alpha == 0 {
        return;
    }
    let rect = ctx.content_rect();
    let band = Color32::from_rgba_unmultiplied(0, 0, 0, alpha);
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("crt-scanlines"),
    ));
    const PERIOD: f32 = 3.0; // points between band tops
    const BAND_H: f32 = 1.0; // points of dark per band
                             // Slow vertical drift (~6 pt/s) so the lines visibly shimmer.
    let drift = (t * 6.0).rem_euclid(PERIOD as f64) as f32;
    let mut y = rect.top() - PERIOD + drift;
    while y < rect.bottom() {
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), y),
                egui::pos2(rect.right(), y + BAND_H),
            ),
            0.0,
            band,
        );
        y += PERIOD;
    }
}

/// Subtle full-window brightness flicker (CRT-style). A translucent black wash
/// whose alpha wanders via layered sines of `t` (deterministic — no RNG, so the
/// reduced-motion resting frame is stable). `strength` is capped at 0.20 for
/// accessibility. `Order::Foreground` so it modulates the whole composited view.
/// `speed` is a cadence multiplier (clamped upstream); at `1.0` the flicker runs
/// at the shipped rate and higher values flicker faster.
pub(super) fn paint_flicker(ctx: &egui::Context, strength: f32, t: f64, speed: f32) {
    let s = strength.clamp(0.0, 0.20);
    if s <= 0.0 {
        return;
    }
    // Per-effect speed: scale the time input so 1.0 reproduces the shipped cadence.
    let t = t * speed as f64;
    let n = ((t * 17.0).sin() * 0.5 + (t * 53.0).sin() * 0.3 + (t * 97.0).sin() * 0.2).abs();
    let a = (s * n as f32 * 90.0).round() as u8;
    if a == 0 {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("crt-flicker"),
    ));
    painter.rect_filled(
        ctx.content_rect(),
        0.0,
        Color32::from_rgba_unmultiplied(0, 0, 0, a),
    );
}

/// VHS-style tracking lines: faint bright horizontal bands sweeping down the
/// window at two different speeds, like analogue tape tracking error. `speed` is
/// a drift multiplier (clamped upstream); at `1.0` the bands sweep at the shipped
/// rate and higher values sweep faster (both bands scale proportionally).
pub(super) fn paint_vhs_tracking(ctx: &egui::Context, t: f64, speed: f32) {
    let rect = ctx.content_rect();
    if rect.height() < 1.0 {
        return;
    }
    // Per-effect speed: scale the time input so 1.0 reproduces the shipped drift.
    let t = t * speed as f64;
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("vhs-tracking"),
    ));
    for (i, speed) in [(0u32, 0.13f64), (1, 0.071)].iter() {
        let phase = (t * speed + *i as f64 * 0.5).rem_euclid(1.0) as f32;
        let y = rect.top() + phase * rect.height();
        let band_h = 16.0;
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), y),
                egui::pos2(rect.right(), y + band_h),
            ),
            0.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 9),
        );
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), y + band_h * 0.4),
                egui::pos2(rect.right(), y + band_h * 0.6),
            ),
            0.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 7),
        );
    }
}

/// AREA-AWARE wired-mesh node count for a `width`×`height` content rect at the
/// given `density` (0..2). Ported from C0PL4ND (M3): a fixed 8..64 count is
/// invisibly sparse on a large (e.g. 4K) display, so the count scales with the
/// window area — capped at 160 so the O(n²) neighbour pass stays bounded per
/// frame — and density interpolates from a calm field (12 nodes) to a busy one.
/// Pure so the scaling + cap are unit-testable.
pub(super) fn wired_mesh_node_count(width: f32, height: f32, density: f32) -> usize {
    let d = density.clamp(0.0, 2.0);
    let area_cap = (width * height / 26_000.0).clamp(24.0, 160.0);
    (12.0 + d * (area_cap - 12.0)).max(12.0) as usize
}

/// Animated wired node-mesh ambient background (Lain "Wired" feel). `density`
/// (0..2) drives an area-aware node count (12..160, M3); nodes drift slowly and
/// near neighbours are linked with faint accent lines. `link_alpha` / `dot_alpha`
/// are the brightness-scaled alphas (M1) — at the default brightness `1.0` they
/// are the shipped `16` / `40`. `Order::Background` so it sits BEHIND the editor
/// like the tint overlay (SCR1B3 keeps Background, NOT C0PL4ND's Foreground).
/// O(n²) over n ≤ 160 — bounded per frame. `drift_speed` is a node-drift
/// multiplier (clamped upstream); at `1.0` the lattice drifts at the shipped rate
/// and higher values let it breathe faster (only the drift, not the layout).
pub(super) fn paint_wired_mesh(
    ctx: &egui::Context,
    density: f32,
    link_alpha: u8,
    dot_alpha: u8,
    color: Color32,
    t: f64,
    drift_speed: f32,
) {
    let rect = ctx.content_rect();
    if rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("wired-mesh"),
    ));
    let n = wired_mesh_node_count(rect.width(), rect.height(), density);
    // Per-effect speed: scale the drift-time input so 1.0 reproduces the shipped
    // drift rate. The static node layout (bx/by) is left untouched.
    let td = t * drift_speed as f64;
    let mut pts: Vec<egui::Pos2> = Vec::with_capacity(n);
    for i in 0..n {
        let fi = i as f64;
        let bx = (fi * 0.732).fract() as f32;
        let by = (fi * 0.387 + 0.13).fract() as f32;
        let dx = ((td * 0.07 + fi * 1.3).sin() * 0.5 + 0.5) as f32;
        let dy = ((td * 0.05 + fi * 0.7).cos() * 0.5 + 0.5) as f32;
        let x = rect.left() + (bx * 0.85 + dx * 0.1) * rect.width();
        let y = rect.top() + (by * 0.85 + dy * 0.1) * rect.height();
        pts.push(egui::pos2(x, y));
    }
    let link = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), link_alpha);
    let dot = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), dot_alpha);
    // A fully-dimmed mesh (brightness 0) has nothing to paint — skip the O(n²) pass.
    if link_alpha == 0 && dot_alpha == 0 {
        return;
    }
    let max_d = rect.width().min(rect.height()) * 0.18;
    for i in 0..n {
        for j in (i + 1)..n {
            if pts[i].distance(pts[j]) < max_d {
                painter.line_segment([pts[i], pts[j]], egui::Stroke::new(1.0, link));
            }
        }
        painter.circle_filled(pts[i], 1.5, dot);
    }
}

/// Caret ghost-trail: fading echoes of recent caret rectangles. The caller feeds
/// `trail` (rect + birth-time) as the caret moves; `intensity` (0..2, M2) scales
/// BOTH the echo lifetime (via `caret_trail_life`) and its peak opacity so the
/// Motion → Caret-trail-intensity slider tunes the trail from a faint flick to a
/// bold comet tail. Ported from C0PL4ND's `paint_cursor_trail`.
pub(super) fn paint_caret_trail(
    ctx: &egui::Context,
    trail: &std::collections::VecDeque<(egui::Rect, f64)>,
    color: Color32,
    now: f64,
    intensity: f32,
) {
    if trail.is_empty() {
        return;
    }
    let life = scribe_core::config::caret_trail_life(intensity);
    // Peak echo alpha scales from 110 (faint) up to 310 (saturates at 255) with
    // intensity, so a pronounced trail is unmistakable while a low setting stays
    // subtle (C0PL4ND parity — above the old fixed 90).
    let peak = 110.0 + 100.0 * intensity.clamp(0.0, 2.0);
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("caret-trail"),
    ));
    for (rect, born) in trail.iter() {
        let age = (now - born).clamp(0.0, life);
        let f = 1.0 - (age / life) as f32;
        if f <= 0.0 {
            continue;
        }
        let a = (f * peak).clamp(0.0, 255.0) as u8;
        painter.rect_filled(
            *rect,
            1.0,
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a),
        );
    }
}

/// One-shot boot "glitch" sweep over the first ~0.55s after launch: a bright
/// scan line descends while a few dark offset bands flicker, all fading out.
/// `elapsed` is seconds since the first frame; outside `[0, DUR]` it no-ops.
pub(super) fn paint_boot_glitch(ctx: &egui::Context, elapsed: f64) {
    const DUR: f64 = 0.55;
    if !(0.0..=DUR).contains(&elapsed) {
        return;
    }
    let rect = ctx.content_rect();
    if rect.width() < 160.0 {
        return; // first-frame 0-width content_rect guard
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("boot-glitch"),
    ));
    let p = (elapsed / DUR) as f32;
    let fade = 1.0 - p;
    let y = rect.top() + p * rect.height();
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(rect.left(), y - 2.0),
            egui::pos2(rect.right(), y + 2.0),
        ),
        0.0,
        Color32::from_rgba_unmultiplied(255, 255, 255, (fade * 120.0) as u8),
    );
    for i in 0..3u32 {
        let fi = i as f32;
        let gy = rect.top() + ((p * 2.0 + fi * 0.27).fract()) * rect.height();
        let gh = 6.0 + fi * 4.0;
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), gy),
                egui::pos2(rect.right(), gy + gh),
            ),
            0.0,
            Color32::from_rgba_unmultiplied(0, 0, 0, (fade * 60.0) as u8),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::wired_mesh_node_count;

    #[test]
    fn wired_mesh_node_count_scales_with_area_and_caps() {
        // M3: the node count is area-aware — a small window stays sparse, a large
        // window fills a denser web, and density interpolates from the 12-node
        // floor up through the per-area ceiling. The per-area ceiling is CAPPED at
        // 160 (so density 1.0 on any surface never exceeds 160 nodes), and the
        // absolute count is bounded at 308 (density 2.0 on a max-area surface) so
        // the O(n²) neighbour pass stays bounded per frame.

        // Density 0 always lands on the 12-node floor, regardless of area.
        assert_eq!(wired_mesh_node_count(1920.0, 1080.0, 0.0), 12);
        assert_eq!(wired_mesh_node_count(400.0, 300.0, 0.0), 12);

        // The per-area ceiling caps at 160: on a huge 4K surface, density 1.0 lands
        // exactly on the 160-node ceiling (12 + 1.0*(160-12)).
        assert_eq!(
            wired_mesh_node_count(3840.0, 2160.0, 1.0),
            160,
            "density 1.0 on a huge surface saturates the 160 area-ceiling"
        );
        // A tiny surface hits the 24-node area-floor at density 1.0 (12 + (24-12)).
        assert_eq!(
            wired_mesh_node_count(400.0, 300.0, 1.0),
            24,
            "density 1.0 on a tiny surface saturates the 24-node area-floor"
        );

        // The absolute maximum (density 2.0 on a max-area surface) is bounded at
        // 308 = 12 + 2.0*(160-12), keeping the per-frame O(n²) pass bounded.
        assert_eq!(wired_mesh_node_count(3840.0, 2160.0, 2.0), 308);
        assert!(
            wired_mesh_node_count(7680.0, 4320.0, 2.0) <= 308,
            "even an 8K surface stays within the 308-node bound"
        );

        // A larger area yields a higher node count at the same density (monotone
        // in area up to the ceiling).
        let small = wired_mesh_node_count(800.0, 600.0, 1.0);
        let large = wired_mesh_node_count(2560.0, 1440.0, 1.0);
        assert!(
            large > small,
            "more area => more nodes at equal density ({large} > {small})"
        );

        // Out-of-band density is clamped to the 0..2 span before interpolation, so
        // a garbage value can never blow past the 308 bound.
        assert_eq!(
            wired_mesh_node_count(3840.0, 2160.0, 99.0),
            308,
            "density clamps to 2.0"
        );
        assert_eq!(wired_mesh_node_count(3840.0, 2160.0, -1.0), 12);
    }
}
