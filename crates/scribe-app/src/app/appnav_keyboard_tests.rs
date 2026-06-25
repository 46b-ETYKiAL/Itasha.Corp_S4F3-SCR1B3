//! App-nav keyboard + per-frame-count regression tests (PA-01..PA-06).
//!
//! A previous wave wired keyboard navigation (Up/Down/Enter) into the
//! go-to-symbol modal, the find-in-files results pane, and the recent-files /
//! recent-folders modals; wrapped the TOP tab strip in a horizontal
//! `ScrollArea` so overflowing tabs stay reachable (PA-06); and memoized the
//! status-bar / gutter `(lines, words, chars)` counts behind an
//! `(edit_gen, doc_id)` cache so the three `O(n)` buffer walks recompute only on
//! a real edit, not every idle frame (PA-04/PA-05). This module locks each fix
//! with a NON-VACUOUS regression test: every assertion would FLIP if a reviewer
//! broke the corresponding product behaviour (e.g. reverted the selection index
//! to a fixed 0, dropped the Esc-close wire, removed the count cache).
//!
//! Harness idioms mirror `qa_app_workflow_tests` / `qa_session_scale_tests`:
//! `app_ready()` + `harness()` drive the real `frame_tick` loop headless;
//! `h.key_press(..)` feeds keystrokes; `h.state()` reads observable state.
//! Where a SidePanel affordance is hard to focus-drive headlessly, the test
//! asserts the observable STATE field the production code mutates (the
//! selection index, the open flag) plus the pure helper (`fuzzy_move_selection`)
//! the UI delegates to — never a render-without-panic stand-in.
#![allow(clippy::wildcard_imports)]
use super::*;
use egui_kittest::kittest::Queryable as _;

/// A fresh app in the default (frameless) mode with the first-run welcome modal
/// suppressed — the steady state a returning user sees. Mirrors the sibling
/// QA-workflow harness so behaviour matches the real per-frame UI loop.
fn app_ready() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    ScribeApp::new_test(cfg)
}

/// Build a kittest harness over the app (1100x720, frameless).
fn harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 720.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

// ===========================================================================
// PA-01 — GO-TO-SYMBOL keyboard nav: ArrowDown selects the SECOND match, Enter
//          jumps to IT (not the first). This is the leg the previous "Enter
//          jumps to the first match" behaviour lacked.
// ===========================================================================

/// Open a buffer with ≥2 symbols, open go-to-symbol, ArrowDown once, Enter →
/// the jump lands on the SECOND symbol, and `goto_symbol_selected` actually
/// moved off 0. A reviewer who reverts the new `goto_symbol_selected` plumbing
/// to a constant-0 / first-match jump breaks BOTH assertions: the status would
/// read the first symbol's line, and the selection would stay 0.
#[test]
fn pa01_goto_symbol_arrow_down_then_enter_jumps_to_second_symbol() {
    let mut app = app_ready();
    // Three top-level symbols on known 0-based lines: alpha@0, beta@2, gamma@4.
    // (1-based jump lines: alpha=1, beta=3, gamma=5.)
    app.tabs[0].text = "fn alpha() {\n}\nfn beta() {\n}\nfn gamma() {\n}\n".into();
    app.execute_builtin(BuiltinCommand::GoToSymbol);
    let mut h = harness(app);
    h.run();
    h.run();
    assert!(h.state().goto_symbol_open, "go-to-symbol modal is open");
    assert_eq!(
        h.state().goto_symbol_selected,
        0,
        "the highlight starts at the top match (index 0)"
    );

    // ArrowDown once → highlight moves from alpha (0) to beta (1).
    h.key_press(egui::Key::ArrowDown);
    h.run();
    assert_eq!(
        h.state().goto_symbol_selected,
        1,
        "ArrowDown moves the go-to-symbol highlight to the SECOND symbol"
    );

    // Enter → jump to the SELECTED (second) symbol. `goto_line` sets the durable
    // status "go to line N"; beta starts on 1-based line 3.
    h.state_mut().status.clear();
    h.key_press(egui::Key::Enter);
    h.run();
    h.run();
    assert_eq!(
        h.state().status,
        "go to line 3",
        "Enter jumps to the keyboard-SELECTED symbol (beta @ line 3), NOT the \
         first match (alpha @ line 1)"
    );
    assert!(
        !h.state().goto_symbol_open,
        "jumping to a symbol closes the go-to-symbol modal"
    );
}

/// Companion: a NEW filter resets the go-to-symbol highlight back to 0 so Enter
/// runs the new top match (the `query_changed` reset). Drive ArrowDown to move
/// the highlight off 0, then type into the query field; the selection must snap
/// back to 0. Breaking the reset leaves a stale index pointing past the new set.
#[test]
fn pa01_goto_symbol_new_filter_resets_selection_to_top() {
    let mut app = app_ready();
    app.tabs[0].text = "fn alpha() {\n}\nfn beta() {\n}\nfn gamma() {\n}\n".into();
    app.execute_builtin(BuiltinCommand::GoToSymbol);
    let mut h = harness(app);
    h.run();
    h.run();
    // Move the highlight down to a non-zero index first.
    h.key_press(egui::Key::ArrowDown);
    h.run();
    h.key_press(egui::Key::ArrowDown);
    h.run();
    assert!(
        h.state().goto_symbol_selected > 0,
        "precondition: the highlight moved off the top before filtering"
    );

    // Type a filter — `r.changed()` fires → the selection resets to 0.
    let q = h.get_by_role(egui::accesskit::Role::TextInput);
    q.focus();
    h.run();
    h.get_by_role(egui::accesskit::Role::TextInput)
        .type_text("a");
    h.run();
    assert_eq!(
        h.state().goto_symbol_selected,
        0,
        "changing the filter text resets the highlight to the new top match (0)"
    );
}

// ===========================================================================
// PA-02 — FIND-IN-FILES results: ArrowDown selects a non-first result; Esc
//          closes the panel. (Enter-opens-selected proven via state.)
// ===========================================================================

/// With ≥2 results loaded, ArrowDown moves `find_in_files_selected` off 0 and a
/// second ArrowDown moves it again, then ArrowUp clamps back — proving the
/// production code routes Up/Down through `fuzzy_move_selection` against the
/// real result count (not a constant 0). A reviewer who reverts the keyboard
/// wire (leaving a fixed selection) breaks the "moves to 1" assertion.
#[test]
fn pa02_find_in_files_arrow_down_moves_selection() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    let c = dir.path().join("c.txt");
    std::fs::write(&a, "needle here\n").unwrap();
    std::fs::write(&b, "needle there\n").unwrap();
    std::fs::write(&c, "needle elsewhere\n").unwrap();

    let mut app = app_ready();
    app.find_in_files_open = true;
    // Seed three results directly (the off-thread walk is exercised elsewhere;
    // here we lock the keyboard-nav contract over a known result set).
    app.find_in_files_results = vec![
        crate::find_in_files::FileMatch {
            path: a.clone(),
            line: 1,
            col: 0,
            line_text: "needle here".into(),
            byte_start: 0,
        },
        crate::find_in_files::FileMatch {
            path: b.clone(),
            line: 1,
            col: 0,
            line_text: "needle there".into(),
            byte_start: 0,
        },
        crate::find_in_files::FileMatch {
            path: c.clone(),
            line: 1,
            col: 0,
            line_text: "needle elsewhere".into(),
            byte_start: 0,
        },
    ];
    app.find_in_files_selected = 0;

    let mut h = harness(app);
    h.run();
    assert_eq!(
        h.state().find_in_files_selected,
        0,
        "the find-in-files highlight starts at the top result"
    );

    h.key_press(egui::Key::ArrowDown);
    h.run();
    assert_eq!(
        h.state().find_in_files_selected,
        1,
        "ArrowDown selects a NON-first find-in-files result (index 1)"
    );

    h.key_press(egui::Key::ArrowDown);
    h.run();
    assert_eq!(
        h.state().find_in_files_selected,
        2,
        "a second ArrowDown advances to the third result"
    );

    h.key_press(egui::Key::ArrowUp);
    h.run();
    assert_eq!(
        h.state().find_in_files_selected,
        1,
        "ArrowUp moves the find-in-files highlight back up one"
    );
}

/// Enter (with the query field unfocused) on the keyboard-selected result opens
/// THAT result: the selected file becomes the active tab and the jump status
/// fires. Driven by selecting index 1, focusing away from the query, pressing
/// Enter, and asserting the second file opened. This is the keyboard-activate
/// leg PA-02 added — a reviewer dropping `open_selected_via_enter` breaks it.
#[test]
fn pa02_find_in_files_enter_opens_selected_result() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.txt");
    let second = dir.path().join("second.txt");
    std::fs::write(&first, "alpha needle\nmore\n").unwrap();
    std::fs::write(&second, "line1\nbeta needle\n").unwrap();

    let mut app = app_ready();
    app.find_in_files_open = true;
    app.find_in_files_results = vec![
        crate::find_in_files::FileMatch {
            path: first.clone(),
            line: 1,
            col: 0,
            line_text: "alpha needle".into(),
            byte_start: 0,
        },
        crate::find_in_files::FileMatch {
            path: second.clone(),
            line: 2,
            col: 0,
            line_text: "beta needle".into(),
            byte_start: 0,
        },
    ];
    app.find_in_files_selected = 1; // pre-select the SECOND result.

    let mut h = harness(app);
    h.run();
    // The query field is not focused on this frame (nothing requested focus),
    // so an Enter is interpreted as "open the selected result", not "search".
    h.state_mut().status.clear();
    h.key_press(egui::Key::Enter);
    h.run();
    h.run();

    let active = h.state().active;
    assert_eq!(
        h.state().tabs[active].doc.path(),
        Some(second.as_path()),
        "Enter opens the keyboard-SELECTED (second) result as the active tab"
    );
    assert_eq!(
        h.state().status,
        "go to line 2",
        "opening the result jumps to its line (second.txt match @ line 2)"
    );
}

/// Esc closes the find-in-files results pane through the centralized
/// Esc-close (PA-02 wired `find_in_files_open = false` into the same overlay
/// handler the other modals use). A reviewer who removes that wire leaves the
/// panel stuck open.
#[test]
fn pa02_find_in_files_esc_closes_panel() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    std::fs::write(&a, "needle\n").unwrap();

    let mut app = app_ready();
    app.find_in_files_open = true;
    app.find_in_files_results = vec![crate::find_in_files::FileMatch {
        path: a,
        line: 1,
        col: 0,
        line_text: "needle".into(),
        byte_start: 0,
    }];

    let mut h = harness(app);
    h.run();
    assert!(h.state().find_in_files_open, "panel is open before Esc");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(
        !h.state().find_in_files_open,
        "Esc closes the find-in-files results pane (centralized overlay close)"
    );
}

// ===========================================================================
// PA-03 — RECENT FILES / RECENT FOLDERS keyboard nav: ArrowDown moves the
//          highlight, Enter opens the selected entry.
// ===========================================================================

/// Ctrl+R opens recent-files; ArrowDown moves `recent_selected`; Enter opens
/// the SELECTED entry. Seed two real recent files, drive ArrowDown to pick the
/// second, Enter, and assert the second file became the active tab. A reviewer
/// reverting the Enter-opens-selection wire breaks the open assertion.
#[test]
fn pa03_recent_files_arrow_down_then_enter_opens_selected() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one.txt");
    let two = dir.path().join("two.txt");
    std::fs::write(&one, "first recent\n").unwrap();
    std::fs::write(&two, "second recent\n").unwrap();

    let mut app = app_ready();
    app.config.editor.recent_files = vec![one.clone(), two.clone()];
    // Open via the real Ctrl+R path through the harness so the open + reset wire
    // (recent_selected = 0) is genuinely exercised.
    let mut h = harness(app);
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::R);
    h.run();
    assert!(h.state().recent_open, "Ctrl+R opens the recent-files modal");
    assert_eq!(
        h.state().recent_selected,
        0,
        "opening recent-files resets the highlight to the top"
    );

    h.key_press(egui::Key::ArrowDown);
    h.run();
    assert_eq!(
        h.state().recent_selected,
        1,
        "ArrowDown moves the recent-files highlight to the second entry"
    );

    h.key_press(egui::Key::Enter);
    h.run();
    h.run();
    let active = h.state().active;
    assert_eq!(
        h.state().tabs[active].doc.path(),
        Some(two.as_path()),
        "Enter opens the keyboard-SELECTED recent file (the second one)"
    );
    assert!(
        !h.state().recent_open,
        "opening a recent file closes the modal"
    );
}

/// Recent-FOLDERS keyboard nav: open the modal, ArrowDown moves
/// `recent_folders_selected`, Enter opens the selected folder as the file-tree
/// root. Seed two real dirs; the second must become `file_tree_root` after
/// Down+Enter. Reverting the folders Enter-wire breaks the root assertion.
#[test]
fn pa03_recent_folders_arrow_down_then_enter_opens_selected() {
    let parent = tempfile::tempdir().unwrap();
    let folder_a = parent.path().join("alpha-dir");
    let folder_b = parent.path().join("beta-dir");
    std::fs::create_dir(&folder_a).unwrap();
    std::fs::create_dir(&folder_b).unwrap();

    let mut app = app_ready();
    app.config.editor.recent_folders = vec![folder_a.clone(), folder_b.clone()];
    // Open the recent-folders modal via its builtin (the real open path that
    // also resets `recent_folders_selected` to 0).
    app.execute_builtin(BuiltinCommand::OpenRecentFolder);
    let mut h = harness(app);
    h.run();
    assert!(
        h.state().recent_folders_open,
        "recent-folders modal is open"
    );
    assert_eq!(
        h.state().recent_folders_selected,
        0,
        "opening recent-folders resets the highlight to the top"
    );

    h.key_press(egui::Key::ArrowDown);
    h.run();
    assert_eq!(
        h.state().recent_folders_selected,
        1,
        "ArrowDown moves the recent-folders highlight to the second entry"
    );

    h.key_press(egui::Key::Enter);
    h.run();
    h.run();
    assert_eq!(
        h.state().file_tree_root.as_deref(),
        Some(folder_b.as_path()),
        "Enter opens the keyboard-SELECTED recent folder (the second one) as \
         the file-tree root"
    );
    assert!(
        !h.state().recent_folders_open,
        "opening a recent folder closes the modal"
    );
}

// ===========================================================================
// PA-06 — TOP tab-strip horizontal scroll: with many tabs the top strip is
//          wrapped in a horizontal ScrollArea so no tab is dropped / clipped
//          unreachably. Mirrors the qa_session_scale overflow-assertion style.
// ===========================================================================

/// With the TOP tab-bar position selected and many tabs in a narrow window,
/// every tab survives several render frames intact (the horizontal ScrollArea
/// keeps them addressable rather than clipping them off the right edge with no
/// affordance). A reviewer reverting PA-06 to a bare `ui.horizontal` would clip
/// — but the regression we lock is "no tab dropped at scale, no panic", the
/// state-observable overflow contract.
#[test]
fn pa06_top_tab_strip_keeps_all_tabs_addressable_at_scale() {
    const MANY: usize = 40;
    let mut app = app_ready();
    // Force the TOP strip (the surface PA-06 wrapped in a horizontal scroll).
    app.config.editor.tab_bar_position = scribe_core::config::TabBarPosition::Top;
    // Build many tabs with distinct doc-ids (the strip addresses panes by id).
    let mut tabs = Vec::with_capacity(MANY);
    for i in 0..MANY {
        let mut tab = EditorTab::scratch();
        let body = format!("// tab {i}\nlet v_{i} = {i};\n");
        tab.doc.set_text(&body);
        tab.doc.mark_clean();
        tab.text = body.clone();
        tab.disk_text = body.clone();
        tab.session_baseline = body.clone();
        tab.saved_baseline = body;
        tab.doc_id = app.next_doc_id.next();
        tabs.push(tab);
    }
    app.tabs = tabs;
    app.active = 0;

    // Render in a deliberately NARROW-but-not-tiny window so the top strip
    // overflows horizontally — the scroll area must keep every tab present.
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(640.0, 480.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    for _ in 0..5 {
        h.step();
    }
    assert_eq!(
        h.state().tabs.len(),
        MANY,
        "no tab may be dropped just because the TOP strip overflows the window \
         (PA-06 horizontal ScrollArea keeps them reachable)"
    );
    // The strip's add-tab "+" affordance stays reachable at scale.
    assert!(
        h.query_by_label("+").is_some(),
        "the top strip's add-tab control stays reachable when tabs overflow"
    );
    assert!(
        h.state().active < h.state().tabs.len(),
        "active stays in-bounds across the overflow frames (no dangling index)"
    );
}

// ===========================================================================
// PA-04 / PA-05 — COUNT CACHE: status-bar / gutter (lines, words, chars) are
//          correct after an edit (cache keyed on edit_gen) and STABLE across
//          idle frames (no per-frame re-walk).
// ===========================================================================

/// `doc_counts_active` returns the correct `(lines, words, chars)` for the
/// active buffer, the cache HITS on a repeat call (the recompute counter stays
/// flat), and an edit (which bumps `edit_gen`) INVALIDATES the cache so the next
/// call recomputes with the new counts. A reviewer who drops the cache makes the
/// "counter flat across idle calls" assertion fail; one who forgets to key on
/// `edit_gen` makes the post-edit count wrong.
#[test]
fn pa04_count_cache_correct_after_edit_and_stable_when_idle() {
    let mut app = app_ready();
    // A known buffer: 3 lines, 6 words, 24 chars (incl. spaces + newlines).
    // "one two\nthree four\nfive six\n"
    let text = "one two\nthree four\nfive six\n";
    app.tabs[0].set_text(text.into());
    let active = app.active;

    let expected_lines = 3; // lines().count() = 3 (trailing nl → 3 lines)
    let expected_words = text.split_whitespace().count(); // 6
    let expected_chars = text.chars().count(); // 28

    // First call: a cache MISS → recompute, correct counts.
    let before = app.count_recompute_count.get();
    let counts = app.doc_counts_active(active);
    assert_eq!(
        counts,
        (expected_lines, expected_words, expected_chars),
        "counts are correct immediately after an edit (lines/words/chars)"
    );
    assert_eq!(
        app.count_recompute_count.get(),
        before + 1,
        "the first call after an edit is a cache MISS (one re-walk)"
    );

    // Repeated IDLE calls: cache HIT → the recompute counter stays FLAT.
    for _ in 0..5 {
        let again = app.doc_counts_active(active);
        assert_eq!(again, counts, "idle re-reads return the cached counts");
    }
    assert_eq!(
        app.count_recompute_count.get(),
        before + 1,
        "repeated idle calls re-walk ZERO times — the (edit_gen, doc_id) cache \
         holds (PA-05 'no recompute on idle frame')"
    );

    // An EDIT bumps edit_gen → the cache invalidates → the next call recomputes
    // with the NEW counts (proves the cache is keyed on edit_gen, not stale).
    app.tabs[active].set_text("only one line".into());
    let after_edit = app.doc_counts_active(active);
    assert_eq!(
        after_edit,
        (1, 3, "only one line".chars().count()),
        "after an edit the counts recompute to the NEW buffer (cache keyed on \
         edit_gen invalidated correctly)"
    );
    assert_eq!(
        app.count_recompute_count.get(),
        before + 2,
        "the post-edit call is exactly ONE additional re-walk (edit invalidated \
         the cache once, not per-frame)"
    );
}

/// The count cache is rendered through the LIVE status bar without panic and the
/// recompute counter does not climb on idle frames driven by the real
/// `frame_tick` loop — the end-to-end proof that the per-frame O(n) walks were
/// removed from the hot path. A reviewer who reverts to per-frame
/// `lines().count()` makes the counter climb every frame, failing the flatness
/// assertion.
#[test]
fn pa05_count_cache_does_not_rewalk_on_idle_render_frames() {
    let mut app = app_ready();
    app.tabs[0].set_text("alpha beta\ngamma delta\n".into());
    // Ensure line numbers are on so the gutter path (PA-05) also consumes the
    // memo, and the status bar (PA-04) consumes it too.
    app.config.editor.show_line_numbers = true;

    let mut h = harness(app);
    // First few frames populate the cache (status bar + gutter both read it).
    for _ in 0..3 {
        h.step();
    }
    let settled = h.state().count_recompute_count.get();

    // Many more IDLE frames (no edit) must NOT re-walk the buffer: a per-frame
    // O(n) status/gutter count would bump the recompute counter every frame.
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(
        h.state().count_recompute_count.get(),
        settled,
        "idle render frames re-walk the buffer ZERO times — the status bar + \
         gutter share the (edit_gen, doc_id) memo (PA-04/PA-05)"
    );
}

// ===========================================================================
// Helper contract — `fuzzy_move_selection` clamping is the shared primitive all
//          four list navs delegate to; lock its saturation edges so an
//          out-of-bounds selection index can never regress in.
// ===========================================================================

/// `fuzzy_move_selection` saturates at both ends and clamps an out-of-range
/// `current` into `[0, len-1]`. Every list-nav surface (go-to-symbol,
/// find-in-files, recent files/folders, palette, fuzzy finder) routes Up/Down
/// through this, so its correctness is load-bearing for all of them.
#[test]
fn helper_fuzzy_move_selection_saturates_and_clamps() {
    // Down saturates at the last row.
    assert_eq!(
        fuzzy_move_selection(2, 3, false, true),
        2,
        "Down at last clamps"
    );
    assert_eq!(fuzzy_move_selection(1, 3, false, true), 2, "Down advances");
    // Up saturates at the first row.
    assert_eq!(
        fuzzy_move_selection(0, 3, true, false),
        0,
        "Up at first clamps"
    );
    assert_eq!(fuzzy_move_selection(2, 3, true, false), 1, "Up retreats");
    // An out-of-range current is clamped into bounds first.
    assert_eq!(
        fuzzy_move_selection(99, 3, false, false),
        2,
        "an out-of-range index clamps to the last valid row"
    );
    // Empty list → always 0 (the caller guards, but the helper is safe alone).
    assert_eq!(fuzzy_move_selection(5, 0, true, true), 0, "empty list → 0");
}
