//! Coverage for the dialog-driven file operations in `file_ops.rs`:
//! `open_dialog`, `convert_to_markdown_active`, `export_html_active`.
//!
//! These had NO tests, and the in-diff mutation gate proved what that meant:
//! all three could have their whole body replaced with `()` and the suite
//! stayed green. That is a direct consequence of the `dialogs` seam — under
//! `cfg(test)` the picker used to always return `None`, so every one of these
//! functions was a no-op and had nothing observable to break. The seam that
//! stopped rfd wedging the test runner turned three real functions into
//! equivalent-mutant factories.
//!
//! The fix is the same one `save_as_active` got: inject the answer the OS
//! dialog would have given (`dialogs::test_hooks`), so the REAL code around
//! the dialog runs and the body becomes observable again. Only the rfd call
//! itself stays untested — that is the whole of the ADR-0007 exclusion now.
//!
//! Each operation is covered twice: once driven to completion, and once
//! cancelled. The cancel case alone is NOT sufficient — a cancelled dialog is
//! indistinguishable from a deleted body, which is exactly the hole being
//! closed here.
#![allow(clippy::wildcard_imports)]
use super::*;
use std::path::PathBuf;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "scr1b3-fileops-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    // Never inherit a previous run's state: the name is unique among LIVE
    // processes, but PIDs recycle and these dirs are not cleaned up.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    ScribeApp::new_test(cfg)
}

// ---- open_dialog ----

#[test]
fn open_dialog_opens_the_picked_file_into_a_new_tab() {
    let mut app = test_app();
    let before = app.tabs.len();
    let dir = temp_dir("open");
    let file = dir.join("picked.md");
    std::fs::write(&file, "picked content").unwrap();
    super::dialogs::test_hooks::set_next_pick_file(file.clone());

    app.open_dialog();

    assert_eq!(
        app.tabs.len(),
        before + 1,
        "the picked file must open in a new tab"
    );
    assert_eq!(
        app.active,
        app.tabs.len() - 1,
        "the new tab must become active"
    );
    assert_eq!(
        app.tabs[app.active].text, "picked content",
        "the file's real content must load"
    );
    assert!(
        app.status.contains("opened"),
        "the status line must report the open, got: {}",
        app.status
    );
}

#[test]
fn open_dialog_opens_nothing_when_the_user_cancels() {
    let mut app = test_app();
    let before = app.tabs.len();

    // Nothing injected => cancelled.
    app.open_dialog();

    assert_eq!(
        app.tabs.len(),
        before,
        "a cancelled picker must not open a tab"
    );
}

#[test]
fn open_dialog_surfaces_a_readable_error_when_the_file_is_gone() {
    let mut app = test_app();
    let dir = temp_dir("open-gone");
    // Injected, but never created: the picker's answer is not a guarantee.
    super::dialogs::test_hooks::set_next_pick_file(dir.join("vanished.md"));
    let before = app.tabs.len();

    app.open_dialog();

    assert_eq!(
        app.tabs.len(),
        before,
        "a file that cannot be opened must not become a tab"
    );
    let toast = app.toast.clone().unwrap_or_default();
    assert!(
        toast.contains("Couldn't open the file"),
        "the user must be told, in plain words, got: {toast:?}"
    );
}

// ---- convert_to_markdown_active ----

#[test]
fn convert_to_markdown_active_writes_the_converted_file() {
    let mut app = test_app();
    let dir = temp_dir("md");
    let src = dir.join("page.html");
    std::fs::write(&src, "<h1>Title</h1>").unwrap();
    app.open_path(src);
    let out = dir.join("page.md");
    super::dialogs::test_hooks::set_next_save_path(out.clone());

    app.convert_to_markdown_active();

    let written = std::fs::read_to_string(&out).expect("the .md file must be written");
    assert!(
        written.contains("Title"),
        "the conversion must actually run, not write an empty file; got: {written:?}"
    );
    assert!(
        app.status.contains("Markdown"),
        "the status line must report the conversion, got: {}",
        app.status
    );
}

#[test]
fn convert_to_markdown_active_leaves_the_source_tab_untouched() {
    let mut app = test_app();
    let dir = temp_dir("md-src");
    let src = dir.join("page.html");
    std::fs::write(&src, "<h1>Title</h1>").unwrap();
    app.open_path(src.clone());
    super::dialogs::test_hooks::set_next_save_path(dir.join("page.md"));

    app.convert_to_markdown_active();

    assert_eq!(
        app.tabs[app.active].text, "<h1>Title</h1>",
        "the source buffer must not be rewritten"
    );
    assert_eq!(
        std::fs::read_to_string(&src).unwrap(),
        "<h1>Title</h1>",
        "the source FILE must not be rewritten — only the chosen .md is written"
    );
}

#[test]
fn convert_to_markdown_active_writes_nothing_when_the_user_cancels() {
    let mut app = test_app();
    let dir = temp_dir("md-cancel");
    let src = dir.join("page.html");
    std::fs::write(&src, "<h1>Title</h1>").unwrap();
    app.open_path(src);

    // Nothing injected => cancelled.
    app.convert_to_markdown_active();

    let stray: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .collect();
    assert!(stray.is_empty(), "a cancelled dialog must write nothing");
}

// ---- export_html_active ----

#[test]
fn export_html_active_writes_the_rendered_html() {
    let mut app = test_app();
    let dir = temp_dir("html");
    let src = dir.join("notes.md");
    std::fs::write(&src, "# Heading").unwrap();
    app.open_path(src);
    let out = dir.join("notes.html");
    super::dialogs::test_hooks::set_next_save_path(out.clone());

    app.export_html_active();

    let written = std::fs::read_to_string(&out).expect("the .html file must be written");
    assert!(
        written.contains("<h1>") && written.contains("Heading"),
        "the Markdown must actually be rendered to HTML; got: {written:?}"
    );
    assert!(
        app.status.contains("HTML"),
        "the status line must report the export, got: {}",
        app.status
    );
}

#[test]
fn export_html_active_writes_nothing_when_the_user_cancels() {
    let mut app = test_app();
    let dir = temp_dir("html-cancel");
    let src = dir.join("notes.md");
    std::fs::write(&src, "# Heading").unwrap();
    app.open_path(src);

    // Nothing injected => cancelled.
    app.export_html_active();

    let stray: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "html"))
        .collect();
    assert!(stray.is_empty(), "a cancelled dialog must write nothing");
}

// ---- the seam itself ----

#[test]
fn an_injected_pick_is_consumed_once_then_reads_as_cancelled() {
    // The hooks are one-shot on purpose: a leaked injection would silently
    // drive the NEXT test's dialog and couple them together.
    let dir = temp_dir("once");
    let f = dir.join("a.md");
    super::dialogs::test_hooks::set_next_pick_file(f.clone());
    assert_eq!(
        super::dialogs::pick_file(),
        Some(f),
        "first call gets the injected path"
    );
    assert_eq!(
        super::dialogs::pick_file(),
        None,
        "second call reads as cancelled"
    );
}

#[test]
fn an_injected_folder_pick_is_consumed_once_then_reads_as_cancelled() {
    let dir = temp_dir("once-folder");
    super::dialogs::test_hooks::set_next_pick_folder(dir.clone());
    assert_eq!(
        super::dialogs::pick_folder(),
        Some(dir),
        "first call gets the injected folder"
    );
    assert_eq!(
        super::dialogs::pick_folder(),
        None,
        "second call reads as cancelled"
    );
}
