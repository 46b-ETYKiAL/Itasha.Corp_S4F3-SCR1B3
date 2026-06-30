//! The app opens EVERY file passed on the command line / by the OS (a multi-
//! select, a `.desktop` `%F`, a default-app open of several files), in order,
//! with the FIRST file active and the rest as background tabs. Exercises
//! `ScribeApp::build` directly (the constructor the CLI path flows through).

use super::*;
use std::io::Write;

/// A readable temp file whose name ends in `suffix` (so the editor derives a
/// language from the extension). Kept alive by the returned handle.
fn temp_file(suffix: &str, body: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new()
        .suffix(suffix)
        .tempfile()
        .expect("create temp file");
    write!(f, "{body}").expect("write temp file");
    f
}

#[test]
fn cli_opens_all_passed_files_with_the_first_active() {
    let a = temp_file(".txt", "alpha body\n");
    let b = temp_file(".md", "bravo body\n");
    let c = temp_file(".rs", "charlie body\n");
    let paths = vec![
        a.path().to_string_lossy().into_owned(),
        b.path().to_string_lossy().into_owned(),
        c.path().to_string_lossy().into_owned(),
    ];

    // watch_config = false → no session-restore interference; the CLI files are
    // the only tabs.
    let app = ScribeApp::build(Config::default(), None, paths, false);

    assert_eq!(app.tabs.len(), 3, "all three CLI files open as tabs");
    assert_eq!(app.active, 0, "the FIRST file is the active tab");
    assert!(app.tabs[0].text.contains("alpha"), "tab 0 = first file");
    assert!(app.tabs[1].text.contains("bravo"), "tab 1 = second file");
    assert!(app.tabs[2].text.contains("charlie"), "tab 2 = third file");

    drop((a, b, c)); // keep the files alive until after build read them
}

#[test]
fn cli_with_no_files_opens_a_single_scratch_tab() {
    let app = ScribeApp::build(Config::default(), None, Vec::new(), false);
    assert_eq!(
        app.tabs.len(),
        1,
        "no CLI files → exactly one scratch buffer"
    );
}

#[test]
fn an_unreadable_cli_file_is_skipped_without_aborting_the_rest() {
    let good = temp_file(".txt", "real content\n");
    let paths = vec![
        "this/path/does/not/exist-xyz.txt".to_string(),
        good.path().to_string_lossy().into_owned(),
    ];
    let app = ScribeApp::build(Config::default(), None, paths, false);
    // The missing file is skipped (a toast is set), the readable one still opens.
    assert_eq!(app.tabs.len(), 1, "the readable file still opens");
    assert!(app.tabs[0].text.contains("real content"));
    drop(good);
}
