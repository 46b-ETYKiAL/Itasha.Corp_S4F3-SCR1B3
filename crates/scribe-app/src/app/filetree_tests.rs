//! Headless e2e (egui_kittest, no GPU) for the **file-tree / explorer**
//! sidebar — `e2e.rs`/`e2e_overlays.rs` only assert it RENDERS; these drive its
//! real interactions. A tempdir with a subdir + files is opened via
//! `open_folder_root`, then:
//!   * a directory `CollapsingHeader` is expanded and its child becomes
//!     renderable (queryable by label); collapse removes it again;
//!   * a file is opened the way the explorer actually supports it — keyboard
//!     nav (ArrowDown to focus + Enter to open) via `FileTreeState::handle_input`
//!     — and a tab opens for it;
//!   * the explorer-close "×" hides the sidebar (`file_tree_root` clears).
//!
//! The directory-expand interaction is driven by clicking the sub-directory's
//! `CollapsingHeader` by its label; egui only runs a header's body (and so only
//! renders its children) on the frame AFTER it opens, so the tests run an extra
//! frame before asserting on the child's presence in the rendered tree.
#![allow(clippy::wildcard_imports)]
use super::*;
use egui_kittest::kittest::Queryable as _;

/// Seed a temp folder: a sub-directory holding one file, plus two top-level
/// files. Neutral names throughout. Returns `(TempDir, child_basename)` — the
/// handle must outlive the harness (it owns the directory on disk).
fn seed_tree() -> (tempfile::TempDir, &'static str) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    std::fs::write(dir.path().join("nested").join("inner.txt"), "inner body\n").unwrap();
    std::fs::write(dir.path().join("top.txt"), "top body\n").unwrap();
    std::fs::write(dir.path().join("notes.md"), "# notes\n").unwrap();
    (dir, "inner.txt")
}

fn tree_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    ScribeApp::new_test(cfg)
}

fn tree_harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

#[test]
fn expanding_and_collapsing_a_directory_toggles_its_children() {
    let (dir, child) = seed_tree();
    let mut app = tree_app();
    app.open_folder_root(dir.path().to_path_buf());
    let mut h = tree_harness(app);
    h.run();
    h.run();
    // The `nested` directory header is collapsed by default, so its child is
    // NOT rendered (no label for it in the tree).
    assert!(
        h.query_by_label(child).is_none(),
        "the nested dir starts collapsed — its child is not rendered"
    );
    // Click the directory header to EXPAND it (the real CollapsingHeader click).
    h.get_by_label("nested").click();
    h.run(); // header opens
    h.run(); // body runs, the child renders
    assert!(
        h.query_by_label(child).is_some(),
        "expanding the directory must render its child in the tree"
    );
    // Click the header again to COLLAPSE — the child stops rendering.
    h.get_by_label("nested").click();
    h.run();
    h.run();
    assert!(
        h.query_by_label(child).is_none(),
        "collapsing the directory must hide its child again"
    );
}

#[test]
fn opening_a_file_via_keyboard_nav_opens_a_tab() {
    let (dir, _) = seed_tree();
    let mut app = tree_app();
    app.open_folder_root(dir.path().to_path_buf());
    let start_tabs = app.tabs.len();
    let mut h = tree_harness(app);
    h.run();
    h.run();
    // The explorer is keyboard-driven: ArrowDown focuses the next visible entry,
    // Enter opens it if it's a file. Walk focus down from the root (index 0)
    // until a top-level FILE is focused, then press Enter. `handle_input`
    // consumes one key per frame, so we send one key per `h.run()`.
    let mut opened_a_file = false;
    for _ in 0..8 {
        h.key_press(egui::Key::ArrowDown);
        h.run();
        let focused_is_file = h
            .state()
            .file_tree_state
            .focused
            .as_ref()
            .map(|p| p.is_file())
            .unwrap_or(false);
        if focused_is_file {
            h.key_press(egui::Key::Enter);
            h.run();
            h.run();
            opened_a_file = true;
            break;
        }
    }
    assert!(
        opened_a_file,
        "arrow-down nav must reach a focusable file entry in the tree"
    );
    assert!(
        h.state().tabs.len() > start_tabs,
        "pressing Enter on a focused file must open a tab for it"
    );
    // The newly-opened tab must be backed by one of the seeded files.
    let active_path = h.state().tabs[h.state().active]
        .doc
        .path()
        .map(|p| p.to_path_buf());
    assert!(
        active_path.is_some_and(|p| {
            p.ends_with("top.txt") || p.ends_with("notes.md") || p.ends_with("inner.txt")
        }),
        "the opened tab must be one of the tree's files"
    );
}

#[test]
fn explorer_close_button_hides_the_sidebar() {
    let (dir, _) = seed_tree();
    let mut app = tree_app();
    app.open_folder_root(dir.path().to_path_buf());
    let mut h = tree_harness(app);
    h.run();
    assert!(
        h.state().file_tree_root.is_some(),
        "sidebar is shown with an open folder"
    );
    // Click the explorer-close "×" small-button.
    h.get_by_label("×").click();
    h.run();
    assert!(
        h.state().file_tree_root.is_none(),
        "clicking the explorer × must hide the sidebar (clear the open folder)"
    );
}
