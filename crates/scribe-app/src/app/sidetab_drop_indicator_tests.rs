//! #82 regression — the rotate-ON side-tab drag drop-insertion indicator must
//! land in the inter-chip GAP, never inside a neighbouring tab's outline.
//!
//! The bug: `draw_rotated_side_tabs` pushed each tab's INNER rotated-label rect
//! (from `allocate_exact_size`) into the `rects` vec the indicator consumes,
//! not the FULL chip frame rect (grip · label · pin · close · margins). The
//! "gap midpoint" between two label rects therefore fell INSIDE a neighbouring
//! chip (over its grip/close glyphs), so the drop line drew inside the tab. The
//! fix pushes `chip_resp.response.rect` (the whole frame) — mirroring
//! `draw_tab_strip`. This test pins that invariant via the `TEST_ROTATED_TAB_RECTS`
//! hook so the inner-label-rect regression cannot silently return.

use super::tab_strip_render::TEST_ROTATED_TAB_RECTS;
use super::*;
use scribe_core::config::TabBarPosition;

/// Build a rotate-ON Left side-tab app with `n` tabs and render two frames so
/// the side strip lays out and records its chip rects into the test hook.
fn render_rotated_strip(n: usize) -> Vec<egui::Rect> {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    cfg.editor.tab_bar_position = TabBarPosition::Left;
    cfg.editor.side_tabs_rotated = true;
    let mut app = ScribeApp::new_test(cfg);
    // Distinct titles so the chips have real, differing heights/content.
    app.tabs.clear();
    for i in 0..n {
        let mut t = EditorTab::scratch();
        t.set_text(format!("file number {i}\nsome body text\n"));
        app.tabs.push(t);
    }
    app.active = 0;
    TEST_ROTATED_TAB_RECTS.with(|r| r.borrow_mut().clear());
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(900.0, 600.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    h.run();
    h.run();
    TEST_ROTATED_TAB_RECTS.with(|r| r.borrow().clone())
}

#[test]
fn rotated_drop_indicator_lands_in_gap_not_inside_a_chip() {
    let rects = render_rotated_strip(3);
    assert!(
        rects.len() >= 3,
        "expected >=3 recorded chip rects, got {} — the rotated side strip did \
         not render its tabs",
        rects.len()
    );

    for i in 1..rects.len() {
        let prev = rects[i - 1];
        let cur = rects[i];

        // (1) The recorded rects must be the FULL chip frames, which sit only the
        // `ui.add_space(2.0)` apart. If they were the inner rotated-LABEL rects
        // (the bug), the "gap" between a label's bottom and the next label's top
        // would also span the previous chip's pin/close + both margins + the next
        // chip's grip — tens of px. A small gap is the discriminator.
        let gap = cur.top() - prev.bottom();
        assert!(
            (0.0..8.0).contains(&gap),
            "chip {i} should nearly touch chip {} (only add_space=2px between \
             full chip frames); got gap={gap:.1}px. A large gap means the inner \
             rotated-label rect was recorded instead of the chip frame — the #82 \
             regression.",
            i - 1
        );

        // (2) The insertion line for a drop above chip `i` must sit strictly in
        // that gap and inside NO chip outline.
        let y = side_tab_insertion_y(i, cur.top(), Some(prev.bottom()));
        assert!(
            y > prev.bottom() && y < cur.top(),
            "insertion line y={y:.1} for gap {i} is not strictly between chip \
             {}.bottom={:.1} and chip {i}.top={:.1}",
            i - 1,
            prev.bottom(),
            cur.top()
        );
        for (j, r) in rects.iter().enumerate() {
            assert!(
                !(y > r.top() && y < r.bottom()),
                "insertion line y={y:.1} falls INSIDE chip {j} outline \
                 [{:.1}..{:.1}] — the drop line is drawn inside a tab (#82).",
                r.top(),
                r.bottom()
            );
        }
    }
}

/// The first-row drop (pointer above tab 0's centre) draws just above the first
/// chip's top edge — above every chip, never inside one.
#[test]
fn rotated_drop_indicator_above_first_chip_is_outside_every_chip() {
    let rects = render_rotated_strip(3);
    let first = rects[0];
    let y = side_tab_insertion_y(0, first.top(), None);
    assert!(
        y <= first.top(),
        "first-row insertion line y={y:.1} must be at/above the first chip top \
         {:.1}",
        first.top()
    );
    for (j, r) in rects.iter().enumerate() {
        assert!(
            !(y > r.top() && y < r.bottom()),
            "first-row insertion line y={y:.1} falls inside chip {j} \
             [{:.1}..{:.1}]",
            r.top(),
            r.bottom()
        );
    }
}
