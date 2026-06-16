//! #91 — perf + security smoke coverage. The perf test guards against a
//! pathological-slowdown regression on a large buffer; the security tests
//! assert untrusted / hostile buffer content and traversal-shaped paths are
//! handled without panicking.
use super::{byte_to_char_index, ScribeApp};
use scribe_core::Config;

#[test]
fn large_buffer_spell_scan_stays_bounded() {
    let mut cfg = Config::default();
    cfg.spellcheck.enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = "word ".repeat(20_000); // ~100 KB / 20k words
    let t = std::time::Instant::now();
    let _ = app.misspellings_for_active();
    // Memoized + linear; a generous ceiling that still catches an O(n^2)
    // regression without being flaky on slow CI.
    assert!(
        t.elapsed().as_secs() < 10,
        "spell scan on a 100 KB buffer must stay well under 10s"
    );
}

#[test]
fn hostile_buffer_content_does_not_panic() {
    let mut app = ScribeApp::new_test(Config::default());
    // NUL + control chars + a very long line + an RTL-override + combining.
    app.tabs[0].text = format!("\0\u{1}\u{7f}{}\u{202e}rtl\u{0301}", "x".repeat(50_000));
    let _ = app.misspellings_for_active();
    let _ = app.spell_count();
    // Byte→char mapping must clamp, never index mid-codepoint or overflow.
    let _ = byte_to_char_index(&app.tabs[0].text, 49_999);
    let _ = byte_to_char_index(&app.tabs[0].text, usize::MAX);
    assert_eq!(app.tabs.len(), 1);
}

#[test]
fn traversal_shaped_open_path_is_handled_gracefully() {
    let mut app = ScribeApp::new_test(Config::default());
    let before = app.tabs.len();
    // A nonexistent, traversal-shaped path must not panic or open a phantom
    // tab; from_path fails and is surfaced as a toast.
    app.open_path(std::path::PathBuf::from("../../../../nonexistent/passwd"));
    assert_eq!(
        app.tabs.len(),
        before,
        "no tab opened for an unreadable path"
    );
}
