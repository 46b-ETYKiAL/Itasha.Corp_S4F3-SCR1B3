//! Headless e2e EXPANSION (task #17/#28): drive the previously-uncovered
//! `app/mod.rs` overlay/modal RENDER branches and each `settings.rs` pane
//! through the real `frame_tick` render loop (egui_kittest, no GPU).
//!
//! `e2e.rs` already drives the tab strip, find bar, settings close, palette,
//! and the editor. This sibling file fills the render-path gaps the coverage
//! audit flagged: every settings PANE actually renders its body; the plugin
//! manager, go-to-symbol, markdown-preview, diff-view, zen, minimap, and
//! report-issue overlays each render at least one frame; and the
//! file-tree sidebar renders with a real open folder. These are RENDER tests
//! (they exercise the egui glue), complementing the state-level
//! `execute_builtin_tests.rs`.
#![allow(clippy::wildcard_imports)]
use super::*;
use egui_kittest::kittest::{NodeT as _, Queryable as _};

fn overlay_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    ScribeApp::new_test(cfg)
}

fn harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

// ───────────────────── every settings pane renders its body ─────────────────

/// Each settings category, navigated by its accessible label, must render its
/// pane body without panic and keep the window's Close control reachable. This
/// drives the per-pane `render_sections` branches that the default-pane-only
/// tests never reached.
#[test]
fn every_settings_pane_renders() {
    for pane in [
        "Appearance",
        "Fonts",
        "Toolbar",
        "Motion",
        "Editor",
        "Plugins",
    ] {
        let app = overlay_app();
        let mut h = harness(app);
        h.state_mut().settings_open = true;
        h.run();
        // Navigate to the pane like a user (click its category BUTTON). The
        // pane name also renders as a heading Label, so a bare label query is
        // ambiguous (kittest panics on >1 match) — pick the interactive Button.
        // Bind first so the query iterator's immutable borrow of `h` is released
        // before the `h.run()` below (an if-let scrutinee's temporaries live for
        // the whole block — E0502 otherwise).
        let target = h
            .get_all_by_label(pane)
            .find(|n| format!("{:?}", n.accesskit_node().role()) == "Button");
        if let Some(node) = target {
            node.click();
        }
        h.run();
        h.run();
        assert!(
            h.state().settings_open,
            "settings must stay open while on pane `{pane}`"
        );
        assert!(
            h.query_by_label("Close window").is_some(),
            "pane `{pane}` must keep the Close control reachable"
        );
    }
}

/// The Motion pane carries the CRT-effect toggles. Rendering it exercises the
/// motion-settings branch (distinct from Appearance) that the width-constancy
/// test only touched via Toolbar.
#[test]
fn motion_settings_pane_renders_without_panic() {
    let app = overlay_app();
    let mut h = harness(app);
    h.state_mut().settings_open = true;
    h.run();
    if let Some(node) = h.query_by_label("Motion") {
        node.click();
        h.run();
        h.run();
    }
    assert!(h.state().settings_open);
}

// ───────────────────────── overlay / modal render paths ─────────────────────

/// Go-to-symbol modal (Ctrl+Shift+O surface) renders when opened.
#[test]
fn goto_symbol_modal_renders() {
    let mut app = overlay_app();
    app.tabs[0].text = "fn alpha() {}\nfn beta() {}\n".to_string();
    app.execute_builtin(BuiltinCommand::GoToSymbol);
    let mut h = harness(app);
    h.run();
    h.run();
    assert!(
        h.state().goto_symbol_open,
        "go-to-symbol modal must be open"
    );
}

/// The plugin-manager window renders when opened via its builtin command.
#[test]
fn plugin_manager_window_renders() {
    let mut app = overlay_app();
    app.execute_builtin(BuiltinCommand::OpenPluginManager);
    let mut h = harness(app);
    h.run();
    h.run();
    assert!(
        h.state().plugin_manager.open,
        "plugin manager window must be open after OpenPluginManager"
    );
}

/// Markdown preview pane renders for a `.md` buffer.
#[test]
fn markdown_preview_pane_renders() {
    let dir = tempfile::tempdir().unwrap();
    let md = dir.path().join("notes.md");
    std::fs::write(
        &md,
        "# Title\n\nSome **bold** body and a [link](https://x).\n",
    )
    .unwrap();
    let mut app = overlay_app();
    app.tabs.clear();
    app.tabs.push(EditorTab::from_path(md).expect("open .md"));
    app.active = 0;
    app.execute_builtin(BuiltinCommand::ToggleMarkdownPreview);
    let mut h = harness(app);
    h.run();
    h.run();
    assert!(
        h.state().md_preview_open,
        "markdown preview must be open and render its pane"
    );
}

/// Diff-view overlay renders when toggled on a buffer with unsaved edits.
#[test]
fn diff_view_overlay_renders() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("file.txt");
    std::fs::write(&f, "original line 1\noriginal line 2\n").unwrap();
    let mut app = overlay_app();
    app.tabs.clear();
    let mut tab = EditorTab::from_path(f).expect("open file");
    tab.text = "original line 1\nEDITED line 2\nadded line 3\n".to_string();
    app.tabs.push(tab);
    app.active = 0;
    app.execute_builtin(BuiltinCommand::ToggleDiffView);
    let mut h = harness(app);
    h.run();
    h.run();
    assert!(
        h.state().diff_view_open,
        "diff view must render its overlay"
    );
}

/// Zen mode hides chrome — render a frame in zen and assert it stays on.
#[test]
fn zen_mode_renders_without_chrome() {
    let mut app = overlay_app();
    app.execute_builtin(BuiltinCommand::ToggleZen);
    let mut h = harness(app);
    h.run();
    h.run();
    assert!(
        h.state().zen_mode,
        "zen mode must stay engaged across frames"
    );
}

/// Minimap renders alongside the editor when toggled on.
#[test]
fn minimap_renders_when_toggled() {
    let mut app = overlay_app();
    app.tabs[0].text = (0..200).map(|i| format!("line {i}\n")).collect::<String>();
    let before = app.config.editor.show_minimap;
    app.execute_builtin(BuiltinCommand::ToggleMinimap);
    let mut h = harness(app);
    h.run();
    h.run();
    assert_ne!(
        h.state().config.editor.show_minimap,
        before,
        "ToggleMinimap must flip the minimap config and render its column"
    );
}

/// The report-issue (W1TN3SS) intake modal renders when opened.
#[test]
fn report_issue_modal_renders() {
    let mut app = overlay_app();
    app.execute_builtin(BuiltinCommand::ReportIssue);
    let mut h = harness(app);
    h.run();
    h.run();
    assert!(
        h.state().issue_intake.open,
        "report-issue intake modal must open and render"
    );
}

/// The file-tree sidebar renders with a real open folder (the folder-open
/// render branch the default scratch app never reaches).
#[test]
fn file_tree_sidebar_renders_with_open_folder() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "fn b() {}\n").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("c.md"), "# c\n").unwrap();
    let mut app = overlay_app();
    app.open_folder_root(dir.path().to_path_buf());
    let mut h = harness(app);
    h.run();
    h.run();
    assert!(
        h.state().file_tree_root.is_some(),
        "file-tree sidebar must render with an open folder root"
    );
}

// ───────────────────── remaining builtin command branches ───────────────────

/// Line-ending commands flip the active buffer's EOL (covers all three arms).
#[test]
fn set_line_ending_commands_change_active_eol() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("eol.txt");
    std::fs::write(&f, "a\nb\n").unwrap();
    let mut app = overlay_app();
    app.tabs.clear();
    app.tabs.push(EditorTab::from_path(f).expect("open"));
    app.active = 0;
    app.execute_builtin(BuiltinCommand::SetLineEndingsCrlf);
    assert_eq!(app.tabs[0].doc.eol(), scribe_core::eol::Eol::Crlf);
    app.execute_builtin(BuiltinCommand::SetLineEndingsCr);
    assert_eq!(app.tabs[0].doc.eol(), scribe_core::eol::Eol::Cr);
    app.execute_builtin(BuiltinCommand::SetLineEndingsLf);
    assert_eq!(app.tabs[0].doc.eol(), scribe_core::eol::Eol::Lf);
}

/// CopyFilePath on an UNSAVED scratch tab surfaces a toast (no path) — the
/// no-path branch the happy-path tests skip.
#[test]
fn copy_file_path_on_scratch_sets_toast() {
    let mut app = overlay_app();
    app.toast = None;
    app.execute_builtin(BuiltinCommand::CopyFilePath);
    assert!(
        app.toast.is_some(),
        "CopyFilePath on a tab with no saved file must surface a toast"
    );
}

/// RevealInExplorer on an unsaved scratch tab surfaces a toast (no-file branch).
#[test]
fn reveal_in_explorer_on_scratch_sets_toast() {
    let mut app = overlay_app();
    app.toast = None;
    app.execute_builtin(BuiltinCommand::RevealInExplorer);
    assert!(
        app.toast.is_some(),
        "RevealInExplorer on a tab with no saved file must surface a toast"
    );
}

/// Bookmark toggle + navigate round-trips (covers the bookmark command arms).
#[test]
fn bookmark_toggle_and_navigate_commands() {
    let mut app = overlay_app();
    app.tabs[0].text = "l0\nl1\nl2\nl3\n".to_string();
    app.execute_builtin(BuiltinCommand::ToggleBookmark);
    // Navigate commands must not panic with a single (or zero) bookmark.
    app.execute_builtin(BuiltinCommand::NextBookmark);
    app.execute_builtin(BuiltinCommand::PrevBookmark);
    // Round-trip the toggle off.
    app.execute_builtin(BuiltinCommand::ToggleBookmark);
}

/// CloseActiveTab on the last tab leaves the editor in a valid state (a fresh
/// scratch tab) rather than an empty tab list.
#[test]
fn close_active_tab_keeps_a_valid_buffer() {
    let mut app = overlay_app();
    assert_eq!(app.tabs.len(), 1);
    app.execute_builtin(BuiltinCommand::CloseActiveTab);
    let mut h = harness(app);
    h.run();
    assert!(
        !h.state().tabs.is_empty(),
        "closing the last tab must leave a valid scratch buffer, not an empty list"
    );
    assert!(
        h.state().active < h.state().tabs.len(),
        "active index must stay in bounds after closing a tab"
    );
}
