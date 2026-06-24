//! Headless e2e (egui_kittest, no GPU) for the multi-pane **grid** header
//! controls — the per-pane "Pin note" / "Unpin note" / "Close pane" buttons
//! that `e2e.rs` only ever rendered (never clicked). Mirrors
//! `e2e.rs::split_is_unified_with_grid` for the setup (grid ON + ≥2 docs) and
//! then drives the real header buttons by their phosphor glyph labels,
//! asserting the observable state change (`tab.pinned`, `count_panes`).
//!
//! To keep each glyph uniquely queryable (kittest panics on >1 match), the
//! tests pin all-but-one pane so exactly ONE pane shows the targeted control —
//! the same isolation trick `narrow_pane_header_stacks_vertically` uses.
#![allow(clippy::wildcard_imports)]
use super::*;
use egui_kittest::kittest::Queryable as _;

/// A two-document grid app, wide enough that each pane lays its header out as a
/// single WIDE row (≥220px) so the pin + close controls render side-by-side and
/// are reachable by label.
fn grid_two_doc_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    cfg.editor.grid_enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = "fn main() {}\n".into();
    app.tabs.push(EditorTab::scratch());
    app.tabs[1].text = "second note\n".into();
    app
}

fn grid_harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        // Wide + tall: two panes each well over the 220px narrow threshold.
        .with_size(egui::Vec2::new(1280.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

#[test]
fn pin_note_button_pins_the_pane() {
    let mut app = grid_two_doc_app();
    // Pin tab 0 up front so its pane shows PUSH_PIN_SLASH; tab 1 stays unpinned
    // and is the SOLE source of a PUSH_PIN glyph — uniquely clickable.
    app.tabs[0].pinned = true;
    let mut h = grid_harness(app);
    h.run();
    h.run();
    assert_eq!(
        crate::grid::count_panes(h.state().grid_tree.as_ref().unwrap()),
        2,
        "two docs lay out as two panes"
    );
    assert!(
        !h.state().tabs[1].pinned,
        "tab 1 starts unpinned (its pane shows the Pin control)"
    );
    // Click the lone PUSH_PIN ("Pin note") button.
    h.get_by_label(egui_phosphor::thin::PUSH_PIN).click();
    h.run();
    h.run();
    assert!(
        h.state().tabs[1].pinned,
        "clicking Pin note must pin the previously-unpinned pane"
    );
}

#[test]
fn unpin_note_button_unpins_the_pane() {
    let mut app = grid_two_doc_app();
    // Exactly ONE pane pinned → exactly one PUSH_PIN_SLASH ("Unpin note") glyph;
    // the other pane shows PUSH_PIN, so the slash glyph is unique.
    app.tabs[0].pinned = true;
    let mut h = grid_harness(app);
    h.run();
    h.run();
    assert!(h.state().tabs[0].pinned, "tab 0 starts pinned");
    h.get_by_label(egui_phosphor::thin::PUSH_PIN_SLASH).click();
    h.run();
    h.run();
    assert!(
        !h.state().tabs[0].pinned,
        "clicking Unpin note must unpin the pinned pane"
    );
}

#[test]
fn close_pane_button_removes_the_pane() {
    let mut app = grid_two_doc_app();
    // Pin tab 0 so ITS close (✕) is hidden — leaving exactly ONE ✕ (tab 1's),
    // uniquely clickable. The pinned pane survives; the unpinned one closes.
    app.tabs[0].pinned = true;
    let closed_doc = app.tabs[1].doc_id;
    let mut h = grid_harness(app);
    h.run();
    h.run();
    assert_eq!(
        crate::grid::count_panes(h.state().grid_tree.as_ref().unwrap()),
        2,
        "two panes before the close"
    );
    // Click the lone ✕ ("Close pane").
    h.get_by_label(egui_phosphor::thin::X).click();
    h.run();
    h.run();
    assert_eq!(
        crate::grid::count_panes(h.state().grid_tree.as_ref().unwrap()),
        1,
        "closing a pane must decrement the pane count"
    );
    assert!(
        !h.state().tabs.iter().any(|t| t.doc_id == closed_doc),
        "the closed pane's backing tab must be removed"
    );
}
