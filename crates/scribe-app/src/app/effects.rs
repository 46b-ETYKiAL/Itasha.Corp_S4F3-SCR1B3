//! CRT / motion visual effects: ctx-layer painters for the tint,
//! scanline, flicker, VHS, wired-mesh, caret-trail, and boot-glitch
//! overlays. Extracted from the `app` module root.
#![allow(clippy::wildcard_imports)]
use super::*;

/// Paint a translucent color tint over the whole window (portable; works in
/// every mode and on every OS). Strength scales the alpha.
pub(super) fn paint_tint_overlay(ctx: &egui::Context, tint_hex: &str, strength: f32) {
    let Some(c) = Rgba::parse_hex(tint_hex) else {
        return;
    };
    let a = (strength.clamp(0.0, 1.0) * 90.0).round() as u8;
    // #77 — paint the tint at the BACKMOST layer so it washes the app
    // background only. At Order::Foreground it painted OVER every window
    // (including the Settings window), which is exactly the "transparency
    // applies to the settings window" bug the user reported. Behind the panels,
    // it shows through translucent (glass-mode) chrome but never over a window.
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("tint-overlay"),
    ));
    painter.rect_filled(
        // egui 0.34: screen_rect -> content_rect (same window-content footprint).
        ctx.content_rect(),
        0.0,
        Color32::from_rgba_unmultiplied(c.r, c.g, c.b, a),
    );
}

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
pub(super) fn paint_flicker(ctx: &egui::Context, strength: f32, t: f64) {
    let s = strength.clamp(0.0, 0.20);
    if s <= 0.0 {
        return;
    }
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
/// window at two different speeds, like analogue tape tracking error.
pub(super) fn paint_vhs_tracking(ctx: &egui::Context, t: f64) {
    let rect = ctx.content_rect();
    if rect.height() < 1.0 {
        return;
    }
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

/// Animated wired node-mesh ambient background (Lain "Wired" feel). `density`
/// (0..1) drives the node count (8..64); nodes drift slowly and near neighbours
/// are linked with faint accent lines. `Order::Background` so it sits BEHIND the
/// editor like the tint overlay. O(n²) over n ≤ 64 — bounded per frame.
pub(super) fn paint_wired_mesh(ctx: &egui::Context, density: f32, color: Color32, t: f64) {
    let rect = ctx.content_rect();
    if rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("wired-mesh"),
    ));
    let n = (8.0 + density.clamp(0.0, 1.0) * 56.0) as usize;
    let mut pts: Vec<egui::Pos2> = Vec::with_capacity(n);
    for i in 0..n {
        let fi = i as f64;
        let bx = (fi * 0.732).fract() as f32;
        let by = (fi * 0.387 + 0.13).fract() as f32;
        let dx = ((t * 0.07 + fi * 1.3).sin() * 0.5 + 0.5) as f32;
        let dy = ((t * 0.05 + fi * 0.7).cos() * 0.5 + 0.5) as f32;
        let x = rect.left() + (bx * 0.85 + dx * 0.1) * rect.width();
        let y = rect.top() + (by * 0.85 + dy * 0.1) * rect.height();
        pts.push(egui::pos2(x, y));
    }
    let link = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 16);
    let dot = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 40);
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
/// `trail` (rect + birth-time) as the caret moves; each echo fades over ~0.45s.
pub(super) fn paint_caret_trail(
    ctx: &egui::Context,
    trail: &std::collections::VecDeque<(egui::Rect, f64)>,
    color: Color32,
    now: f64,
) {
    if trail.is_empty() {
        return;
    }
    const LIFE: f64 = 0.45;
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("caret-trail"),
    ));
    for (rect, born) in trail.iter() {
        let age = (now - born).clamp(0.0, LIFE);
        let f = 1.0 - (age / LIFE);
        if f <= 0.0 {
            continue;
        }
        let a = (f * 90.0) as u8;
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
