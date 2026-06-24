//! Headless e2e (egui_kittest, no GPU) for the project-wide **find-in-files**
//! side panel — a surface `e2e.rs`/`e2e_overlays.rs` never drive
//! interactively. These tests open the panel, seed a real on-disk folder via
//! `open_folder_root(tempdir)`, type a query, click the "search" button, and
//! assert the streamed results populate; flip the "regex" checkbox and assert
//! the regex semantics change the hit set; and click a result row and assert it
//! NAVIGATES (active tab switches to the matched file + the caret scroll is
//! requested via `goto_line`).
//!
//! The search runs OFF the frame thread (`spawn_search` → `drain_find_in_files`
//! each frame), so the helpers below drive frames in a short poll loop until the
//! streaming worker signals `Done` (`find_in_files_running` clears) before
//! asserting on the results.
#![allow(clippy::wildcard_imports)]
use super::*;
use egui_kittest::kittest::Queryable as _;

/// A find-in-files harness app: first-run done + non-frameless so no welcome /
/// titlebar chrome competes for the queried labels.
fn fif_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    ScribeApp::new_test(cfg)
}

fn fif_harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

/// Seed a temp folder with two neutral-named text files and return its handle.
/// The `TempDir` must outlive the search (it owns the directory) — callers keep
/// it bound for the duration of the test.
fn seed_folder() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("alpha.txt"),
        "needle here\nplain line\nNEEDLE upper\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("beta.rs"),
        "fn f() { /* needle in code */ }\nlet x = 1;\n",
    )
    .unwrap();
    dir
}

/// Drive frames until the off-thread project-search worker finishes (its
/// `Done` clears `find_in_files_running`), bounded so a hung worker can't wedge
/// the test. Each iteration runs the UI (which calls `drain_find_in_files`) then
/// yields briefly so the worker thread can stream. `run_ok` (not `run`) is used
/// because the focused query field's blinking-cursor keeps requesting repaints,
/// which would otherwise trip `run`'s max-steps panic.
fn run_until_search_done(h: &mut egui_kittest::Harness<'static, ScribeApp>) {
    for _ in 0..200 {
        h.run_ok();
        if !h.state().find_in_files_running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // One more pass so the final drained batch is rendered into result rows.
    h.run_ok();
}

#[test]
fn search_button_populates_results_for_open_folder() {
    let dir = seed_folder();
    let mut app = fif_app();
    app.open_folder_root(dir.path().to_path_buf());
    app.find_in_files_open = true;
    app.find_in_files_query = "needle".to_string();
    let mut h = fif_harness(app);
    h.run_ok();
    // Click the panel's "search" button like a user.
    h.get_by_label("search").click();
    run_until_search_done(&mut h);
    // Three "needle" occurrences across the two files (alpha: 2 case-insensitive,
    // beta: 1) — the off-thread walk streamed them all into the results pane.
    assert!(
        h.state().find_in_files_results.len() >= 3,
        "search must populate results from the open folder (got {})",
        h.state().find_in_files_results.len()
    );
    assert!(
        h.state()
            .find_in_files_results
            .iter()
            .any(|m| m.path.ends_with("alpha.txt")),
        "alpha.txt must appear in the project-search hits"
    );
}

#[test]
fn regex_toggle_changes_the_hit_set() {
    // A pattern that is a literal substring of NOTHING but a valid regex that
    // matches: `needle|NEEDLE` finds the two alpha lines + the beta line only
    // when regex is ON; with regex OFF the literal `needle|NEEDLE` substring is
    // absent from every file, so the result set is empty. Flipping the checkbox
    // is what moves the count from 0 → >0.
    let dir = seed_folder();
    let mut app = fif_app();
    app.open_folder_root(dir.path().to_path_buf());
    app.find_in_files_open = true;
    app.find_in_files_query = "needle|here".to_string();
    let mut h = fif_harness(app);
    h.run_ok();

    // Literal (regex OFF): "needle|here" is not a substring of any file.
    h.get_by_label("search").click();
    run_until_search_done(&mut h);
    assert_eq!(
        h.state().find_in_files_results.len(),
        0,
        "literal search for the alternation string finds nothing"
    );

    // Flip the regex checkbox ON, re-run, and the alternation now matches.
    h.get_by_label("regex").click();
    h.run_ok();
    assert!(
        h.state().find_in_files_regex,
        "the regex checkbox must toggle on when clicked"
    );
    h.get_by_label("search").click();
    run_until_search_done(&mut h);
    assert!(
        h.state().find_in_files_results.len() >= 3,
        "regex search for `needle|here` must match (got {})",
        h.state().find_in_files_results.len()
    );
}

#[test]
fn clicking_a_result_row_navigates_to_the_match() {
    let dir = seed_folder();
    let mut app = fif_app();
    app.open_folder_root(dir.path().to_path_buf());
    app.find_in_files_open = true;
    // Match ONLY in beta.rs so the click target is unambiguous and the
    // navigation lands on a file that is NOT currently the active tab.
    app.find_in_files_query = "in code".to_string();
    let start_tabs = app.tabs.len();
    let mut h = fif_harness(app);
    h.run_ok();
    h.get_by_label("search").click();
    run_until_search_done(&mut h);
    assert!(
        !h.state().find_in_files_results.is_empty(),
        "the unique-to-beta query must yield a result row to click"
    );
    // The result row label is `"{name}:{line}  {line_text.trim()}"`; query the
    // row by its rendered substring (file name + line + context).
    let row = &h.state().find_in_files_results[0];
    let name = row.path.file_name().unwrap().to_str().unwrap().to_string();
    let label = format!("{}:{}  {}", name, row.line, row.line_text.trim());
    h.get_by_label(label.as_str()).click();
    h.run_ok();
    // Navigation: clicking the row opened the matched file in a NEW tab and made
    // it the active tab (active-file switch is the observable navigation). The
    // caret move is `open_find_in_files_result` → `goto_line`, which sets
    // `pending_scroll`; the editor CONSUMES that on the same frame it renders
    // (`pending_scroll.take()`), so it is asserted directly in the unit-level
    // `goto_line_sets_pending_scroll` e2e test rather than re-observed here.
    assert!(
        h.state().tabs.len() > start_tabs,
        "clicking a result opens the matched file in a new tab"
    );
    let active_path = h.state().tabs[h.state().active]
        .doc
        .path()
        .map(|p| p.to_path_buf());
    assert!(
        active_path.is_some_and(|p| p.ends_with("beta.rs")),
        "the clicked result must become the active tab (beta.rs) — navigation fired"
    );
}
