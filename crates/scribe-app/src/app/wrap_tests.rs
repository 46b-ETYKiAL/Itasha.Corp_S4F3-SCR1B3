//! #75 — word-wrap toggle. egui's TextEdit passes the scroll viewport's
//! available width to the layouter as the wrap width regardless of
//! `desired_width`, so the editor wrapped even with word-wrap OFF. The fix
//! lives in `effective_wrap_width`: infinite width when wrap is off (galley
//! lays out on one line, the ScrollArea scrolls horizontally), the given
//! viewport width when on.
use super::render_support::effective_wrap_width;

#[test]
fn rotated_tab_geometry_swaps_axes_and_anchors_top_right() {
    use super::{rotated_tab_size, rotated_tab_text_pos};
    // A 100x16 horizontal label with (8,10) padding → a 24-wide, 110-tall
    // cell (height+pad.x wide, width+pad.y tall).
    let g = egui::vec2(100.0, 16.0);
    let pad = egui::vec2(8.0, 10.0);
    let size = rotated_tab_size(g, pad);
    assert_eq!(size, egui::vec2(24.0, 110.0));
    // Anchor sits at the top-right of the padded inner area so a +90° spin
    // drops the text into the cell.
    let rect = egui::Rect::from_min_size(egui::pos2(5.0, 7.0), size);
    let pos = rotated_tab_text_pos(rect, g, pad);
    assert_eq!(pos, egui::pos2(5.0 + 4.0 + 16.0, 7.0 + 5.0));
}

#[test]
fn wrap_off_forces_infinite_width() {
    assert_eq!(effective_wrap_width(false, 800.0), f32::INFINITY);
    assert_eq!(effective_wrap_width(false, 1.0), f32::INFINITY);
}

#[test]
fn wrap_on_uses_the_viewport_width() {
    assert_eq!(effective_wrap_width(true, 800.0), 800.0);
    assert_eq!(effective_wrap_width(true, 123.5), 123.5);
}
