//! #78 — spellcheck underlines. The byte→char mapping must be correct
//! (galley cursors are char-indexed; spell spans are byte-indexed) and the
//! active-buffer misspelling scan must actually flag a bad word so the
//! painter has something to draw.
use super::{byte_to_char_index, ScribeApp};
use scribe_core::Config;

#[test]
fn byte_to_char_handles_multibyte() {
    // "café word" — 'é' is 2 bytes, so byte 6 (start of "word") is char 5.
    let s = "café word";
    assert_eq!(byte_to_char_index(s, 0), 0);
    assert_eq!(byte_to_char_index(s, 5), 4, "byte after é → char 4");
    assert_eq!(byte_to_char_index(s, 6), 5, "start of 'word' → char 5");
    assert_eq!(byte_to_char_index(s, 999), s.chars().count(), "clamps");
}

#[test]
fn active_buffer_misspelling_is_detected_when_enabled() {
    let mut cfg = Config::default();
    cfg.spellcheck.enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = "this zxqwyzz is wrong".into();
    let found = app.misspellings_for_active();
    assert!(
        found.iter().any(|m| m.word.contains("zxqwyzz")),
        "the nonsense word must be flagged (got {found:?})"
    );
}

#[test]
fn no_misspellings_when_spellcheck_disabled() {
    let mut cfg = Config::default();
    cfg.spellcheck.enabled = false;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = "zxqwyzz".into();
    assert!(app.misspellings_for_active().is_empty());
}
