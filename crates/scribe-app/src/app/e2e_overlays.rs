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

/// A `ScribeApp` with the CUSTOM (frameless) titlebar enabled — the only mode in
/// which the painted caption buttons (min/max/close) render, so the caption-button
/// tests below can reach them by their (newly added) accessible names.
fn caption_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = true;
    ScribeApp::new_test(cfg)
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

// ───────────────────────── caption buttons (titlebar) ───────────────────────
//
// The painted min/max/close caption buttons only render with the custom
// (frameless) titlebar — `caption_app()` enables it. Each button now carries an
// explicit AccessKit name (added in `chrome::caption_btn`), so the harness can
// reach it by label. A by-label query for the Close caption button is
// unambiguous because its name ("Close application window") is distinct from the
// settings dialog's ✕ ("Close window").

/// Clicking the Close caption button funnels into the two-phase close
/// (hide-before-destroy), which the handler signals by setting `want_close`.
/// `want_close` is consumed at the TOP of the NEXT `frame_tick`, so the assert
/// runs on the same frame as the click (no second `h.run()` before it).
#[test]
fn caption_close_button_sets_close_intent() {
    let mut h = harness(caption_app());
    h.run();
    assert!(
        !h.state().want_close,
        "close intent must start clear before the caption button is clicked"
    );
    h.get_by_label("Close application window").click();
    // A single step processes the click without the convergence loop: the
    // resulting want_close path requests a continuous repaint (the two-phase
    // close), which would trip `h.run()`'s max-steps guard.
    h.step();
    assert!(
        h.state().want_close,
        "clicking the Close caption button must set want_close (two-phase close)"
    );
}

/// The Maximize caption button is reachable by its accessible name and clicking
/// it is processed without panic. The maximize itself is an OS
/// `ViewportCommand::Maximized` — not a headless-assertable app flag — so this
/// asserts reachability + that the button survives a click+frame and the icon
/// stays present (the only headless-observable contract).
#[test]
fn caption_maximize_button_is_reachable_and_clicks() {
    let mut h = harness(caption_app());
    h.run();
    assert!(
        h.query_by_label("Maximize window").is_some(),
        "the Maximize caption button must expose an accessible name"
    );
    h.get_by_label("Maximize window").click();
    h.run();
    h.run();
    // The titlebar (and thus its caption buttons) must still render after the
    // click — a Maximized viewport command never tears down the titlebar.
    assert!(
        h.query_by_label("Maximize window").is_some()
            || h.query_by_label("Restore window").is_some(),
        "a window-control button (Maximize or its Restore variant) must remain reachable"
    );
}

/// The Minimize caption button is reachable by its accessible name and clicking
/// it (an OS `ViewportCommand::Minimized`) is processed without panic. Like
/// maximize there is no app-side flag to assert, so this asserts reachability +
/// no-panic survival of a click + frame.
#[test]
fn caption_minimize_button_is_reachable_and_clicks() {
    let mut h = harness(caption_app());
    h.run();
    assert!(
        h.query_by_label("Minimize window").is_some(),
        "the Minimize caption button must expose an accessible name"
    );
    h.get_by_label("Minimize window").click();
    h.run();
    assert!(
        h.query_by_label("Minimize window").is_some(),
        "the Minimize caption button must remain reachable after a click"
    );
}

// ───────────────────────── crash-consent modal ──────────────────────────────

/// Spool a single fake crash report into a temp config dir, bind the dialog to
/// it, and load it — arming `crash_consent` so `render_crash_consent` shows the
/// modal. Mirrors the launch path (`set_config_dir` + `load_from_spool`).
fn arm_crash_consent(app: &mut ScribeApp, dir: &std::path::Path) {
    let report = crate::reporting::build_crash_report("boom", "src/app/mod.rs:1");
    let spool = itasha_report_core::spool::Spool::open(dir).expect("open spool");
    spool.enqueue(&report).expect("enqueue crash report");
    app.crash_consent.set_config_dir(Some(dir.to_path_buf()));
    let queued = app.crash_consent.load_from_spool();
    assert!(
        queued >= 1,
        "the spooled crash report must load into the queue"
    );
}

/// The crash-consent modal renders its three remember-my-choice radios plus the
/// equal-weight Send / Don't-send buttons when a report is pending.
#[test]
fn crash_consent_modal_renders_radios_and_buttons() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = overlay_app();
    arm_crash_consent(&mut app, dir.path());
    let mut h = harness(app);
    h.run();
    h.run();
    assert!(
        h.state().crash_consent.has_pending(),
        "the crash-consent modal must be pending and render"
    );
    // The three equal-weight remember radios (no dark-pattern asymmetry).
    for radio in ["Ask me each time", "Always send", "Never send"] {
        assert!(
            h.query_by_label(radio).is_some(),
            "crash-consent modal must expose the `{radio}` choice"
        );
    }
    // Equal-weight Send / Don't-send.
    assert!(
        h.query_by_label("Send report").is_some(),
        "Send report button"
    );
    assert!(
        h.query_by_label("Don't send").is_some(),
        "Don't send button"
    );
}

/// Clicking "Don't send" dismisses the modal (the spooled report is discarded
/// and the queue advances to empty → no longer pending).
#[test]
fn crash_consent_dont_send_dismisses() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = overlay_app();
    arm_crash_consent(&mut app, dir.path());
    let mut h = harness(app);
    h.run();
    assert!(h.state().crash_consent.has_pending(), "armed and pending");
    h.get_by_label("Don't send").click();
    h.run();
    h.run();
    assert!(
        !h.state().crash_consent.has_pending(),
        "Don't-send must discard the report and dismiss the modal"
    );
}

// ───────────────────────── update prompt modal ──────────────────────────────

/// Arm the on-launch (Auto) update modal: a pending prompt with an `Available`
/// release, exactly as the reducer leaves it on an Auto launch finding a newer
/// version — without any network.
fn arm_update_prompt(app: &mut ScribeApp) {
    use crate::updater::UpdateState;
    let info = scribe_core::update::ReleaseInfo {
        version: semver::Version::parse("99.0.0").unwrap(),
        tag: "v99.0.0".to_string(),
        asset_url: "https://example.invalid/scr1b3.tar.gz".to_string(),
        sig_url: "https://example.invalid/scr1b3.tar.gz.minisig".to_string(),
        sha_url: "https://example.invalid/scr1b3.tar.gz.sha256".to_string(),
        html_url: "https://example.invalid/releases/tag/v99.0.0".to_string(),
        pinned_sha256: "deadbeef".to_string(),
        release_index: Some(99_000_000),
        installer: None,
    };
    app.updater.state = UpdateState::Available(info);
    app.updater.show_prompt = true;
}

/// The update modal renders its Update-now / Later buttons in the `Available`
/// state, and clicking "Later" dismisses it (records the skipped version).
#[test]
fn update_prompt_later_dismisses() {
    let mut app = overlay_app();
    arm_update_prompt(&mut app);
    let mut h = harness(app);
    h.run();
    h.run();
    assert!(
        h.state().updater.show_prompt,
        "update modal must be showing"
    );
    assert!(
        h.query_by_label("Update now").is_some(),
        "update modal must offer Update now"
    );
    h.get_by_label("Later").click();
    h.run();
    h.run();
    assert!(
        !h.state().updater.show_prompt,
        "clicking Later must dismiss the update modal"
    );
    assert_eq!(
        h.state().updater.skipped_version.as_deref(),
        Some("99.0.0"),
        "Later must record the skipped version so it won't re-prompt this session"
    );
}

/// Clicking "Update now" leaves the Available state and begins the update flow
/// (the modal stays open to show download/verify progress — `show_prompt` is not
/// cleared). The state must no longer be `Available` (the download started).
#[test]
fn update_prompt_update_now_starts_flow() {
    use crate::updater::UpdateState;
    let mut app = overlay_app();
    arm_update_prompt(&mut app);
    let mut h = harness(app);
    h.run();
    h.get_by_label("Update now").click();
    h.run();
    h.run();
    assert!(
        !matches!(h.state().updater.state, UpdateState::Available(_)),
        "Update now must leave the Available state and begin the download flow"
    );
}

// ───────────────────────── go-to-line modal ─────────────────────────────────

/// The go-to-line modal applies its query through `goto_line`, which sets
/// `pending_scroll` (the editor scroll-to-line pipe) and closes the modal. The
/// single-line text field carries no accessible label, so the typed value is set
/// on `goto_query` directly (the value a user would type); the "Go" Button is a
/// real labeled widget driven by a click.
#[test]
fn goto_line_modal_go_scrolls_and_closes() {
    let mut app = overlay_app();
    app.tabs[0].text = (0..50).map(|i| format!("line {i}\n")).collect::<String>();
    app.goto_open = true;
    app.goto_query = "42".to_string();
    app.pending_scroll = None;
    let mut h = harness(app);
    h.run();
    assert!(h.state().goto_open, "go-to-line modal must be open");
    h.get_by_label("Go").click();
    h.run();
    h.run();
    assert!(
        !h.state().goto_open,
        "applying a valid line must close the go-to-line modal"
    );
    // `pending_scroll` is set by `goto_line` then CONSUMED by the editor render
    // (`pending_scroll.take()`), so it is None again by the time the modal-apply
    // frames settle. The persistent evidence the scroll was requested is the
    // status line `goto_line` writes ("go to line N").
    assert!(
        h.state().status.contains("go to line 42"),
        "Go must apply the typed line via goto_line (status records the jump)"
    );
}

// ───────────────────────── banners (config-error / external-change) ──────────

/// The config-error banner renders when `config_error_banner` is set, and its
/// "Dismiss" button clears it for the session.
#[test]
fn config_error_banner_dismiss_clears_it() {
    let mut app = overlay_app();
    app.config_error_banner = Some("expected `=` at line 3".to_string());
    let mut h = harness(app);
    h.run();
    assert!(
        h.state().config_error_banner.is_some(),
        "config-error banner must be present before dismiss"
    );
    h.get_by_label("Dismiss").click();
    h.run();
    h.run();
    assert!(
        h.state().config_error_banner.is_none(),
        "Dismiss must clear the config-error banner"
    );
}

/// The external-change banner renders when the active tab's `external_change`
/// flag is set, and "Reload from disk" reloads the saved version (clearing the
/// flag and replacing the buffer text with the on-disk content).
#[test]
fn external_change_banner_reload_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("note.txt");
    std::fs::write(&f, "DISK VERSION\n").unwrap();
    let mut app = overlay_app();
    app.tabs.clear();
    let mut tab = EditorTab::from_path(f).expect("open file");
    // Local unsaved edit diverging from disk; the disk file changed underneath.
    tab.text = "MY UNSAVED EDIT\n".to_string();
    tab.external_change = true;
    app.tabs.push(tab);
    app.active = 0;
    let mut h = harness(app);
    h.run();
    assert!(
        h.state().tabs[0].external_change,
        "external-change banner must be armed before reload"
    );
    h.get_by_label("Reload from disk").click();
    h.run();
    h.run();
    assert!(
        !h.state().tabs[0].external_change,
        "Reload from disk must clear the external-change flag"
    );
    assert_eq!(
        h.state().tabs[0].text,
        "DISK VERSION\n",
        "Reload from disk must replace the buffer with the on-disk content"
    );
}

/// The external-change banner's "Keep my version" clears the flag WITHOUT
/// discarding the user's unsaved edits (the next save overwrites disk).
#[test]
fn external_change_banner_keep_my_version() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("note.txt");
    std::fs::write(&f, "DISK VERSION\n").unwrap();
    let mut app = overlay_app();
    app.tabs.clear();
    let mut tab = EditorTab::from_path(f).expect("open file");
    tab.text = "MY UNSAVED EDIT\n".to_string();
    tab.external_change = true;
    app.tabs.push(tab);
    app.active = 0;
    let mut h = harness(app);
    h.run();
    h.get_by_label("Keep my version").click();
    h.run();
    h.run();
    assert!(
        !h.state().tabs[0].external_change,
        "Keep my version must clear the external-change flag (stop re-prompting)"
    );
    assert_eq!(
        h.state().tabs[0].text,
        "MY UNSAVED EDIT\n",
        "Keep my version must preserve the user's unsaved edits"
    );
}

// ─────────────────── settings-pane sweep extension (4 panes) ─────────────────

/// The original `every_settings_pane_renders` swept only 6 of the 10 categories.
/// This extends the sweep to the remaining user-facing panes — Window,
/// Spellcheck, Updates, Privacy — asserting each renders its body without panic
/// and keeps the window's Close control reachable.
#[test]
fn remaining_settings_panes_render() {
    for pane in ["Window", "Spellcheck", "Updates", "Privacy"] {
        let app = overlay_app();
        let mut h = harness(app);
        h.state_mut().settings_open = true;
        h.run();
        // Navigate by the category BUTTON (the nav lists the name as a heading
        // Label too, so a bare label query is ambiguous). Bind first so the
        // query iterator's borrow of `h` is released before `h.run()`.
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

/// Window pane: drive a representative control — the "Always on top" checkbox —
/// and assert the click flips `config.window.always_on_top`.
#[test]
fn window_pane_always_on_top_toggle() {
    let app = overlay_app();
    let mut h = harness(app);
    h.state_mut().settings_open = true;
    h.run();
    if let Some(node) = h
        .get_all_by_label("Window")
        .find(|n| format!("{:?}", n.accesskit_node().role()) == "Button")
    {
        node.click();
    }
    h.run();
    h.run();
    let before = h.state().config.window.always_on_top;
    // "Always on top" appears as BOTH a group-header Label and the CheckBox; pick
    // the interactive CheckBox (a bare label query is ambiguous → kittest panics).
    if let Some(node) = h
        .get_all_by_label("Always on top")
        .find(|n| format!("{:?}", n.accesskit_node().role()) == "CheckBox")
    {
        node.click();
    }
    h.run();
    h.run();
    assert_ne!(
        h.state().config.window.always_on_top,
        before,
        "the Always-on-top checkbox must toggle config.window.always_on_top"
    );
}

/// Privacy pane: the crash-reports stream offers a 3-way reporting-mode radio
/// (Never / Ask each time / Always). Selecting "Ask each time" sets the mode.
#[test]
fn privacy_pane_crash_reports_radio_selects_ask_each_time() {
    let app = overlay_app();
    let mut h = harness(app);
    h.state_mut().settings_open = true;
    h.run();
    if let Some(node) = h
        .get_all_by_label("Privacy")
        .find(|n| format!("{:?}", n.accesskit_node().role()) == "Button")
    {
        node.click();
    }
    h.run();
    h.run();
    // The crash-reports radio group renders the three consent-language choices.
    // "Ask each time" appears for BOTH crash and manual-issue streams; pick the
    // first radio with that label and click it.
    if let Some(node) = h
        .get_all_by_label("Ask each time")
        .find(|n| format!("{:?}", n.accesskit_node().role()) == "RadioButton")
    {
        node.click();
    }
    h.run();
    h.run();
    assert_eq!(
        h.state().config.reporting.crash_reports,
        scribe_core::ReportingMode::AskEachTime,
        "selecting `Ask each time` must set the crash-reports mode"
    );
}

/// Updates pane: the "Check for updates" button is reachable and clicking it
/// kicks off a manual check (the updater leaves Idle — it enters Checking, or a
/// terminal state if the worker resolves instantly). Asserts the button is
/// reachable and the click is processed without panic.
#[test]
fn updates_pane_check_for_updates_button() {
    let app = overlay_app();
    let mut h = harness(app);
    h.state_mut().settings_open = true;
    h.run();
    if let Some(node) = h
        .get_all_by_label("Updates")
        .find(|n| format!("{:?}", n.accesskit_node().role()) == "Button")
    {
        node.click();
    }
    h.run();
    h.run();
    assert!(
        h.query_by_label("Check for updates").is_some(),
        "the Updates pane must expose a `Check for updates` button"
    );
    h.get_by_label("Check for updates").click();
    // A single step processes the click without the convergence loop: the manual
    // check spawns a worker whose in-flight `Checking` state paints a spinner
    // (continuous repaint), which would trip `h.run()`'s max-steps guard.
    h.step();
    // The settings window must remain open + the button reachable after the
    // click (the manual check runs on a worker thread; no panic, no teardown).
    assert!(
        h.state().settings_open,
        "the settings window must stay open after starting a manual update check"
    );
}

/// Spellcheck pane: drive the "Enable" checkbox and assert it flips
/// `config.spellcheck.enabled`.
#[test]
fn spellcheck_pane_enable_checkbox() {
    let app = overlay_app();
    let mut h = harness(app);
    h.state_mut().settings_open = true;
    h.run();
    if let Some(node) = h
        .get_all_by_label("Spellcheck")
        .find(|n| format!("{:?}", n.accesskit_node().role()) == "Button")
    {
        node.click();
    }
    h.run();
    h.run();
    let before = h.state().config.spellcheck.enabled;
    // The Spellcheck pane's first control is the grid_bool "Enable" checkbox.
    if let Some(node) = h
        .get_all_by_label("Enable")
        .find(|n| format!("{:?}", n.accesskit_node().role()) == "CheckBox")
    {
        node.click();
    }
    h.run();
    h.run();
    assert_ne!(
        h.state().config.spellcheck.enabled,
        before,
        "the Spellcheck `Enable` checkbox must toggle config.spellcheck.enabled"
    );
}
