//! Runtime verification for the user-facing error-message rewrites.
//!
//! SCR1B3 is a native egui app, so the "exercise the affected state in a real
//! browser" step has no browser equivalent — instead each test below drives the
//! REAL app method that sets the toast and asserts (a) the new plain-language
//! copy appears and (b) NO raw OS/path/internal error text leaks into it. Only
//! states that are deterministically reproducible headless are asserted here;
//! states that need a live OS-clipboard/LSP/dialog failure are verified by the
//! source re-scan + code-path review instead (see the delivery CSV).

#![allow(clippy::wildcard_imports)]
use super::*;

fn app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    ScribeApp::new_test(cfg)
}

/// No raw OS-error / "os error N" / "No such file" text may appear in a toast.
fn assert_no_raw_leak(toast: &str) {
    for needle in [
        "os error",
        "No such file",
        "The system cannot",
        "(os ",
        "Errno",
    ] {
        assert!(
            !toast.contains(needle),
            "toast leaked raw OS error text {needle:?}: {toast:?}"
        );
    }
}

#[test]
fn e01_open_failed_is_plain_and_leak_free() {
    let mut a = app();
    let missing = std::path::PathBuf::from("definitely/not/a/real/file-xyz.txt");
    a.open_path(missing);
    let t = a.toast.clone().expect("open failure must surface a toast");
    assert!(
        t.starts_with("Couldn't open the file"),
        "expected the plain open-failure copy, got {t:?}"
    );
    assert!(t.contains("permission"), "should hint at the likely causes");
    assert_no_raw_leak(&t);
    // And no full filesystem path is echoed back as the error.
    assert!(
        !t.contains("file-xyz.txt"),
        "must not echo the raw path: {t:?}"
    );
}

#[test]
fn e03_save_failed_is_plain_and_leak_free() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let file = sub.join("note.txt");
    std::fs::write(&file, "hello\n").unwrap();

    let mut a = app();
    a.open_path(file.clone());
    // The save target's directory vanishes out from under the app.
    std::fs::remove_dir_all(&sub).unwrap();
    a.tabs[a.active].set_text("changed\n".to_string());
    a.save_active();

    let t = a.toast.clone().expect("save failure must surface a toast");
    assert!(
        t.starts_with("Couldn't save the file"),
        "expected the plain save-failure copy, got {t:?}"
    );
    assert!(
        t.contains("permission") || t.contains("disk"),
        "should offer a recovery hint"
    );
    assert_no_raw_leak(&t);
}

#[test]
fn e15_no_language_detected_is_plain_with_recovery() {
    let mut a = app();
    // Fresh scratch tab: no language hint, no path.
    a.start_lsp_for_active();
    let t = a.toast.clone().expect("must surface a toast");
    assert!(
        t.starts_with("Couldn't detect this file's language"),
        "got {t:?}"
    );
    assert!(
        t.contains("extension"),
        "should tell the user how to enable it"
    );
}

#[test]
fn e17_comment_unavailable_is_plain() {
    let mut a = app();
    a.toggle_comment_active();
    let t = a.toast.clone().expect("must surface a toast");
    assert_eq!(t, "Commenting isn't available for this file type.");
}

#[test]
fn e19_reveal_without_file_suggests_saving() {
    let mut a = app();
    a.execute_builtin(BuiltinCommand::RevealInExplorer);
    let t = a.toast.clone().expect("must surface a toast");
    assert_eq!(t, "Save this note first to show it in your file manager.");
}

#[test]
fn e20_copy_path_without_file_suggests_saving() {
    let mut a = app();
    a.execute_builtin(BuiltinCommand::CopyFilePath);
    let t = a.toast.clone().expect("must surface a toast");
    assert_eq!(t, "Save this note first to copy its file path.");
}
