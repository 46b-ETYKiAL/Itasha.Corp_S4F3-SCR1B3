//! Headless interaction tests for the v0.4.58 note-tab-bar wave: the LEFT/RIGHT
//! HORIZONTAL side bar must (1) shrink BELOW the longest note title once titles
//! truncate, and (2) honour the "Wrap note titles to 2 lines" config end-to-end.
//! These drive the REAL `frame_tick` render through an `egui_kittest` harness
//! (no GPU needed — layout + accesskit + panel-resize input all run headless),
//! so the behaviour is pinned against a regression in every CI build.
#![allow(clippy::wildcard_imports)]
use super::*;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;

/// A very long note title so a single line overflows the fit-to-content bar.
const LONG_TITLE: &str = "a-very-long-note-title-that-overflows-the-side-bar.md";

/// Build a LEFT, non-rotated (horizontal-label) side-bar app with the given
/// 2-line option and a single long-titled real-file tab. Returns the app plus
/// the temp dir kept alive for the test's lifetime (dropping it deletes the
/// backing file).
fn left_bar_app(two_line: bool) -> (ScribeApp, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join(LONG_TITLE);
    std::fs::write(&path, "// long title note\nbody\n").expect("write note");
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    cfg.editor.tab_bar_position = scribe_core::config::TabBarPosition::Left;
    cfg.editor.side_tabs_rotated = false;
    cfg.editor.side_tabs_wrap_two_lines = two_line;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::from_path(path).expect("open note");
    t.doc_id = crate::grid::DocId(1);
    app.tabs.push(t);
    app.active = 0;
    (app, dir)
}

fn panel_width(h: &Harness<'_, ScribeApp>) -> f32 {
    egui::PanelState::load(&h.ctx, egui::Id::new("tabs-left"))
        .map(|s| s.size().x)
        .expect("left tab panel must have a stored width")
}

/// Truncation-allows-shrink: the resizable side bar can be dragged NARROWER than
/// the longest title, because the title truncates (…) instead of forcing the
/// panel wide. Drives the actual panel-resize separator, then asserts the stored
/// panel width collapsed to hug the MINIMAL tab (grip + truncated title + close,
/// ~96px) — far below the >280px the full title would otherwise demand. Before
/// the fix, the un-truncated label floored the panel at the full title width, so
/// this drag couldn't shrink it at all.
#[test]
fn side_bar_shrinks_below_longest_title_via_resize() {
    let (app, _dir) = left_bar_app(false);
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 600.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    h.run();
    h.run();

    let start_w = panel_width(&h);
    // The separator sits at the panel's right edge (x == width). Press there,
    // drag the pointer hard left, holding the button down across several frames
    // so the resize fully converges to the content-driven minimum.
    let sep_x = start_w;
    let mid_y = 300.0;
    h.drag_at(egui::pos2(sep_x, mid_y));
    h.run();
    for _ in 0..8 {
        h.hover_at(egui::pos2(20.0, mid_y));
        h.run();
    }
    h.drop_at(egui::pos2(20.0, mid_y));
    h.run();
    h.run();

    let end_w = panel_width(&h);
    assert!(
        end_w < start_w - 100.0,
        "the side bar must shrink well below its fit-to-content start once titles \
         truncate: start={start_w:.1} end={end_w:.1}"
    );
    // The title's natural width is well over 280px; a truncating bar collapses to
    // hug the minimal tab affordances (~96px). Assert it landed far below the
    // title width — the behaviour that was impossible before truncation.
    assert!(
        end_w < 140.0,
        "a truncating side bar must collapse below the full-title width (it can't \
         before the fix): end={end_w:.1}"
    );
}

/// 2-line config wiring, end-to-end: turning ON "Wrap note titles to 2 lines"
/// makes a long-titled side tab render TALLER (the title wraps to a 2nd line),
/// which pushes the "+" add button — laid out below the tab column — lower.
/// Proves the `side_tabs_wrap_two_lines` flag actually reaches the renderer.
#[test]
fn two_line_wrap_makes_side_tabs_taller() {
    fn plus_top(two_line: bool) -> f32 {
        let (app, _dir) = left_bar_app(two_line);
        let mut h = Harness::builder()
            // Narrow enough that the long title can't fit one line → it wraps
            // when the option is ON.
            .with_size(egui::vec2(360.0, 600.0))
            .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
        h.run();
        h.run();
        h.get_by_label(egui_phosphor::thin::PLUS).rect().top()
    }

    let one_line = plus_top(false);
    let two_line = plus_top(true);
    assert!(
        two_line > one_line + 8.0,
        "2-line wrap must make the tab column taller (the + button moves down): \
         one_line_plus_top={one_line:.1} two_line_plus_top={two_line:.1}"
    );
}
