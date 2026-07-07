//! Drag-select scroll conveniences for the central egui `TextEdit`.
//!
//! egui's `ScrollArea` deliberately ignores the mouse wheel while ANY widget is
//! being dragged (`scroll_area.rs`: the wheel-apply block is gated behind
//! `dragged_id().is_none()`), so during a `TextEdit` drag-selection the viewport
//! never scrolls and egui — which recomputes the selection end from the pointer
//! mapped into galley space every drag frame — can never extend the selection
//! past the visible region. That is the reported "hold the left button, roll the
//! wheel, keep selecting elsewhere — doesn't work" bug.
//!
//! The fix lives entirely in the app layer and exploits egui's own recompute:
//! SCR1B3 only has to MOVE the viewport (via the existing `pending_scroll`
//! plumbing the minimap + go-to-line already use); once the galley shifts under
//! the stationary pointer, egui's TextEdit drag handler extends the selection by
//! itself — no custom galley hit-testing. Two triggers drive the move:
//!   * **P0-1** — the wheel rolled mid-drag (`smooth_scroll_delta` is still
//!     intact because the gated ScrollArea never consumed it).
//!   * **P0-2** — the pointer held near the top/bottom viewport edge, with a
//!     quadratic distance-into-margin acceleration and a self-pumped repaint
//!     (egui is reactive; a stationary edge pointer emits no events).
//!
//! [`ScribeApp::caret_scroll_off_assist`] is the keyboard-navigation companion
//! (**P1-4**): it keeps the caret at least N lines from the viewport edge on an
//! arrow / page / home / end move (Vim `scrolloff`), never fighting the wheel or
//! an active drag.
//!
//! Note on egui 0.34: this stack exposes `smooth_scroll_delta` (points, smoothed
//! over frames) — there is NO `raw_scroll_delta`. Reusing the smoothed delta is
//! also what the middle-click autoscroll and Ctrl+wheel zoom already do, so the
//! feel stays consistent.
#![allow(clippy::wildcard_imports)]

use super::*;

/// Screen-space band (px) at each viewport edge inside which an active
/// drag-selection auto-pans. ~28px ≈ two editor lines at the default size.
const EDGE_MARGIN: f32 = 28.0;
/// Peak edge-autoscroll velocity (points/sec) at the very edge, before the
/// per-frame `dt` normalisation. Tuned to feel like VS Code's drag autoscroll.
const EDGE_MAX_SPEED: f32 = 1100.0;

impl ScribeApp {
    /// Drive the editor viewport while a LEFT-drag selection is in progress so
    /// egui extends the selection past the visible region (P0-1 wheel + P0-2
    /// edge autoscroll). Call AFTER the editor's `ScrollArea` has shown and
    /// recorded [`Self::scroll_metrics`], passing the ScrollArea's screen-space
    /// `viewport` (`inner_rect`). Sets [`Self::pending_scroll`], which the editor
    /// consumes on the NEXT frame via `vertical_scroll_offset`.
    pub(super) fn drag_scroll_assist(
        &mut self,
        ctx: &egui::Context,
        editor_id: egui::Id,
        viewport: egui::Rect,
    ) {
        if !self.config.scroll.drag_autoscroll {
            return;
        }
        let (off_y, content_h, view_h) = self.scroll_metrics;
        let max_off = (content_h - view_h).max(0.0);
        if max_off <= 0.0 {
            return; // content fits — nothing to scroll into
        }
        // A drag-selection is in progress when the primary button is held AND the
        // editor owns keyboard focus. `command` is held for Ctrl+wheel font zoom
        // (handled in keyboard_input) — never hijack that as a drag-scroll.
        let (primary_down, cmd, wheel_y, ptr, dt) = ctx.input(|i| {
            (
                i.pointer.primary_down(),
                i.modifiers.command,
                i.smooth_scroll_delta.y,
                i.pointer.interact_pos(),
                i.stable_dt.clamp(1.0 / 240.0, 0.1),
            )
        });
        if !primary_down || cmd || !ctx.memory(|m| m.has_focus(editor_id)) {
            return;
        }
        let mut delta = 0.0_f32;
        // P0-1: a positive `smooth_scroll_delta.y` means the content should move
        // DOWN (view toward the top), so the scroll OFFSET moves the opposite way.
        if wheel_y != 0.0 {
            delta -= wheel_y;
        }
        // P0-2: quadratic edge autoscroll when the drag pointer nears an edge.
        if let Some(p) = ptr {
            delta += edge_autoscroll_step(p.y, viewport, dt);
        }
        if delta != 0.0 {
            self.pending_scroll = Some((off_y + delta).clamp(0.0, max_off));
            // Reactive repaint pump: a still edge-pointer emits no input events,
            // so without this the pan would stall after a single tick.
            ctx.request_repaint();
        }
    }

    /// Keep the caret at least `scroll.caret_scroll_off` lines from the viewport
    /// top/bottom on a keyboard caret move (P1-4). `caret_bottom_y` is the
    /// caret's screen-space galley baseline; `line_px` is one line's height. Runs
    /// ONLY on a navigation keypress with no button held, so it never fights the
    /// wheel or an active drag-select autoscroll.
    pub(super) fn caret_scroll_off_assist(
        &mut self,
        ctx: &egui::Context,
        caret_bottom_y: f32,
        viewport: egui::Rect,
        line_px: f32,
    ) {
        let off_lines = self.config.scroll.clamped_caret_scroll_off();
        if off_lines == 0 || line_px <= 0.0 {
            return;
        }
        let (nav, primary_down) = ctx.input(|i| {
            let pressed = i.key_pressed(egui::Key::ArrowUp)
                || i.key_pressed(egui::Key::ArrowDown)
                || i.key_pressed(egui::Key::PageUp)
                || i.key_pressed(egui::Key::PageDown)
                || i.key_pressed(egui::Key::Home)
                || i.key_pressed(egui::Key::End);
            (pressed, i.pointer.primary_down())
        });
        if !nav || primary_down {
            return;
        }
        let (off_y, content_h, view_h) = self.scroll_metrics;
        let max_off = (content_h - view_h).max(0.0);
        if max_off <= 0.0 {
            return;
        }
        // Cap the margin so it can never exceed ~40% of the viewport (a tall
        // scroll-off on a short pane would otherwise oscillate).
        let margin = (off_lines as f32 * line_px).min(view_h * 0.4);
        let nudge = caret_edge_nudge(caret_bottom_y, viewport, margin, line_px);
        if nudge != 0.0 {
            self.pending_scroll = Some((off_y + nudge).clamp(0.0, max_off));
            ctx.request_repaint();
        }
    }
}

/// Per-frame vertical autoscroll velocity (points) when the drag pointer sits
/// within [`EDGE_MARGIN`] of the viewport top/bottom, else `0.0`. Positive pans
/// toward the document end (increasing scroll offset). Acceleration is quadratic
/// in the depth into the margin and normalised by `dt` for frame-rate stability.
fn edge_autoscroll_step(py: f32, viewport: egui::Rect, dt: f32) -> f32 {
    let over_bottom = py - (viewport.bottom() - EDGE_MARGIN);
    let over_top = (viewport.top() + EDGE_MARGIN) - py;
    let (dir, depth) = if over_bottom > 0.0 {
        (1.0_f32, over_bottom)
    } else if over_top > 0.0 {
        (-1.0_f32, over_top)
    } else {
        return 0.0;
    };
    let t = (depth / EDGE_MARGIN).clamp(0.0, 1.0);
    dir * t * t * EDGE_MAX_SPEED * dt
}

/// Scroll-offset delta (points) that pulls the caret back inside the keep-away
/// band: negative scrolls the view up (caret near the top), positive scrolls it
/// down (caret near the bottom), `0.0` when the caret is comfortably framed. The
/// top limit adds one `line_px` so the caret's own row is fully clear of the
/// margin, not straddling it.
fn caret_edge_nudge(caret_bottom_y: f32, viewport: egui::Rect, margin: f32, line_px: f32) -> f32 {
    let top_limit = viewport.top() + margin + line_px;
    let bot_limit = viewport.bottom() - margin;
    if caret_bottom_y < top_limit {
        caret_bottom_y - top_limit
    } else if caret_bottom_y > bot_limit {
        caret_bottom_y - bot_limit
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(0.0, 100.0), egui::pos2(400.0, 500.0))
    }

    #[test]
    fn edge_step_zero_in_neutral_band() {
        // A pointer in the middle of the viewport does not autoscroll.
        assert_eq!(edge_autoscroll_step(300.0, vp(), 1.0 / 60.0), 0.0);
    }

    #[test]
    fn edge_step_pans_down_near_bottom_and_up_near_top() {
        let dt = 1.0 / 60.0;
        // Just inside the bottom margin -> positive (toward document end).
        let down = edge_autoscroll_step(495.0, vp(), dt);
        assert!(down > 0.0, "near bottom pans down, got {down}");
        // Just inside the top margin -> negative (toward document start).
        let up = edge_autoscroll_step(105.0, vp(), dt);
        assert!(up < 0.0, "near top pans up, got {up}");
    }

    #[test]
    fn edge_step_accelerates_with_depth_and_caps_beyond_edge() {
        let dt = 1.0 / 60.0;
        let shallow = edge_autoscroll_step(viewport_bottom_at(10.0), vp(), dt);
        let deep = edge_autoscroll_step(viewport_bottom_at(2.0), vp(), dt);
        assert!(deep > shallow, "deeper into the margin pans faster");
        // Past the very edge the velocity is clamped to the peak, not unbounded.
        let past = edge_autoscroll_step(vp().bottom() + 200.0, vp(), dt);
        let peak = EDGE_MAX_SPEED * dt;
        assert!(
            (past - peak).abs() < 1e-3,
            "clamped to peak at/over the edge"
        );
    }

    /// A y that is `inset` px above the viewport bottom (i.e. `inset` into the
    /// margin band when `inset < EDGE_MARGIN`).
    fn viewport_bottom_at(inset: f32) -> f32 {
        vp().bottom() - inset
    }

    #[test]
    fn caret_nudge_frames_caret_away_from_edges() {
        let margin = 40.0;
        let line = 16.0;
        // Caret near the bottom -> positive nudge (scroll down).
        let n = caret_edge_nudge(495.0, vp(), margin, line);
        assert!(n > 0.0, "caret near bottom nudges down, got {n}");
        // Caret near the top -> negative nudge (scroll up).
        let n = caret_edge_nudge(105.0, vp(), margin, line);
        assert!(n < 0.0, "caret near top nudges up, got {n}");
        // Caret comfortably centred -> no nudge.
        assert_eq!(caret_edge_nudge(300.0, vp(), margin, line), 0.0);
    }
}
