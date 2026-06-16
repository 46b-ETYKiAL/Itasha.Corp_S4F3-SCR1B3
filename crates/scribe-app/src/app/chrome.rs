//! Custom-titlebar chrome: caption buttons + frameless-window
//! resize handling. Free functions extracted from the `app`
//! module root; `use super::*` pulls in egui + app-local types.
#![allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn caption_btn(
    ui: &mut egui::Ui,
    icon: CaptionIcon,
    base: Color32,
    hover_fill: Color32,
    height: f32,
) -> egui::Response {
    // 46px wide is the standard Windows caption-button width; the height tracks
    // the titlebar so the buttons stay consistent with the in-titlebar toolbar
    // buttons when the user picks a large toolbar button size (the default 28px
    // is preserved — see the call site's `.max(28.0)`).
    let size = egui::vec2(46.0, height);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, 2.0, hover_fill);
    }
    let col = if resp.hovered() { Color32::WHITE } else { base };
    let c = rect.center();
    let s = 4.5_f32;
    let stroke = egui::Stroke::new(1.4, col);
    match icon {
        CaptionIcon::Minimize => {
            painter.line_segment([egui::pos2(c.x - s, c.y), egui::pos2(c.x + s, c.y)], stroke);
        }
        CaptionIcon::Maximize => {
            // egui 0.34: rect_stroke gained a 4th StrokeKind arg.
            painter.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(2.0 * s, 2.0 * s)),
                1.0,
                stroke,
                egui::StrokeKind::Outside,
            );
        }
        CaptionIcon::Restore => {
            // Full front square (lower-left) + an L of the back square peeking
            // out upper-right — reads as "restore" with no overlap masking.
            let front = egui::Rect::from_center_size(
                egui::pos2(c.x - 1.5, c.y + 1.5),
                egui::vec2(2.0 * s, 2.0 * s),
            );
            painter.rect_stroke(front, 1.0, stroke, egui::StrokeKind::Outside);
            let top = front.top() - 3.0;
            let right = front.right() + 3.0;
            painter.line_segment(
                [egui::pos2(front.left() + 3.0, top), egui::pos2(right, top)],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(right, top),
                    egui::pos2(right, front.bottom() - 3.0),
                ],
                stroke,
            );
        }
        CaptionIcon::Close => {
            painter.line_segment(
                [egui::pos2(c.x - s, c.y - s), egui::pos2(c.x + s, c.y + s)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x - s, c.y + s), egui::pos2(c.x + s, c.y - s)],
                stroke,
            );
        }
    }
    resp
}

/// Width of the 4 edge resize zones, in logical px. Slim so they only intercept
/// pointer events right at the window border.
const RESIZE_EDGE_PX: f32 = 8.0;
/// Side length of the 4 corner resize zones, in logical px. Slightly larger than
/// the edges so diagonal grabs are forgiving.
const RESIZE_CORNER_PX: f32 = 12.0;

/// Which window-edge resize direction (if any) the pointer `p` is over, given
/// the window `rect` and the edge/corner band widths. Corners (within `corner`
/// of two sides) take priority over straight edges; the interior returns `None`.
/// Pure + unit-tested so the frameless-resize hit-testing can't silently regress.
pub(super) fn resize_dir_at(
    p: egui::Pos2,
    rect: egui::Rect,
    edge: f32,
    corner: f32,
) -> Option<egui::ResizeDirection> {
    use egui::ResizeDirection as D;
    let (l, r, t, b) = (
        p.x - rect.left(),
        rect.right() - p.x,
        p.y - rect.top(),
        rect.bottom() - p.y,
    );
    // Outside the window → not a resize zone.
    if l < 0.0 || r < 0.0 || t < 0.0 || b < 0.0 {
        return None;
    }
    let (w, e, n, s) = (l <= edge, r <= edge, t <= edge, b <= edge);
    let (nw, ne, nn, ns) = (l <= corner, r <= corner, t <= corner, b <= corner);
    if (n && nw) || (w && nn) {
        Some(D::NorthWest)
    } else if (n && ne) || (e && nn) {
        Some(D::NorthEast)
    } else if (s && nw) || (w && ns) {
        Some(D::SouthWest)
    } else if (s && ne) || (e && ns) {
        Some(D::SouthEast)
    } else if n {
        Some(D::North)
    } else if s {
        Some(D::South)
    } else if w {
        Some(D::West)
    } else if e {
        Some(D::East)
    } else {
        None
    }
}

/// Frameless window edge-resize, the no-Area way. Each frame: if the pointer is
/// over an edge band, hint the matching resize cursor; on a primary press there
/// — and only when egui isn't already using the pointer for a widget — start an
/// OS resize via `ViewportCommand::BeginResize`. No persistent `Order::Foreground`
/// Areas, so it never swallows clicks meant for tabs / the settings ✕ / panels,
/// and it works on every resize, not just the first.
pub(super) fn handle_frameless_resize(ctx: &egui::Context) {
    use egui::{CursorIcon as C, ResizeDirection as D, ViewportCommand};
    let Some(p) = ctx.pointer_latest_pos() else {
        return;
    };
    // Hit-test against the FULL window surface (screen_rect), not content_rect —
    // content_rect can exclude the top titlebar / bottom status panels, which
    // would push the resize bands inward off the real window edges so the user
    // can't grab them. screen_rect is the whole inner window area.
    let Some(dir) = resize_dir_at(p, ctx.screen_rect(), RESIZE_EDGE_PX, RESIZE_CORNER_PX) else {
        return;
    };
    ctx.set_cursor_icon(match dir {
        D::North => C::ResizeNorth,
        D::South => C::ResizeSouth,
        D::West => C::ResizeWest,
        D::East => C::ResizeEast,
        D::NorthWest => C::ResizeNorthWest,
        D::NorthEast => C::ResizeNorthEast,
        D::SouthWest => C::ResizeSouthWest,
        D::SouthEast => C::ResizeSouthEast,
    });
    // Start the OS resize on a FRESH press anywhere in the (thin) edge/corner
    // band. The previous `&& !ctx.wants_pointer_input()` guard is why resize
    // silently did nothing: the editor TextEdit + the status/side panels cover
    // every window edge, so `wants_pointer_input()` is true at the edges and the
    // BeginResize was always skipped (the cursor still changed — that part is
    // unconditional — which is exactly the "cursor changes but it doesn't
    // resize" report). The band is only 8px (12px at corners), so a press that
    // lands in it is an intentional resize; handing the drag to the OS is the
    // right call even if a widget also sits under the very edge. `primary_pressed`
    // is the rising edge, so no widget drag is in progress yet.
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        // The OS now owns the drag. winit's modal resize loop swallows the
        // button-up, so egui can be left believing a drag is still in progress —
        // which makes `wants_pointer_input()` return true forever and blocks
        // EVERY subsequent resize (the "works once, then never" bug). Clearing
        // egui's drag bookkeeping here unsticks that state so resize re-arms.
        ctx.stop_dragging();
    }
    // Belt-and-suspenders: with no button held there can be no legitimate drag,
    // so proactively clear any phantom drag the OS resize loop may have orphaned.
    if !ctx.input(|i| i.pointer.any_down()) {
        ctx.stop_dragging();
    }
}
