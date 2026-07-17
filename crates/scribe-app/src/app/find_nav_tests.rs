//! Mutation-survivor kills for `app/find_nav.rs`. The existing find-nav tests
//! assert only `is_some()` / relative ordering, so the exact scroll-offset
//! arithmetic (line-height fallback, newline count) and the stale-index clamp
//! survived. These pin the exact values. Under `new_test` the `line_gutter` is
//! empty, so `goto_line`/`scroll_to_offset` take the `line0 * (size*line_height)`
//! fallback.
use super::*;

#[test]
fn goto_line_sets_exact_pending_scroll_via_line_height_fallback() {
    // line_1based=3 -> line0=2; pending = 2 * (size*line_height). Kills 27:57
    // (size*lh) and 28:55 (line0*lh) offset mutants.
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "a\nb\nc\nd\ne\n".into();
    let size = app.config.fonts.clamped_editor_size();
    let lh = app.config.fonts.clamped_line_height();
    let expected = 2.0_f32 * (size * lh);
    app.pending_scroll = None;
    app.goto_line(3);
    assert_eq!(app.pending_scroll, Some(expected));
}

#[test]
fn find_matches_active_guards_out_of_range_active() {
    // Non-empty query + out-of-range active: clean short-circuits via `||`; the
    // `|| -> &&` mutant (45:39) falls through to `&self.tabs[active]` and panics.
    let mut app = ScribeApp::new_test(Config::default());
    app.find_query = "x".into();
    app.active = 99;
    assert!(app.find_matches_active().is_empty());
}

#[test]
fn find_navigate_clamps_a_stale_match_index_before_wrapping() {
    // A STALE idx >= n must clamp to n-1 BEFORE wrapping. n=3, idx=3, forward:
    // clean min(3,2)=2 -> (2+1)%3=0; a +/÷ mutant on `n - 1` lands on 1. Kills 105:57(x2).
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "foo bar foo baz foo".into();
    app.find_query = "foo".into();
    app.find_open = true;
    assert_eq!(app.find_matches_active().len(), 3);
    app.find_match_idx = 3;
    app.find_navigate(true);
    assert_eq!(app.find_match_idx, 0);
}

#[test]
fn find_navigate_sets_exact_pending_scroll_from_newline_count() {
    // One match on line index 1; pending = 1 * (size*line_height). Exercises
    // scroll_to_offset (private) through find_navigate. Kills 76:9 (stub),
    // 76:24 (guard flip), 84:33 (newline filter !=), 91:57 + 92:55 (offset math).
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "aaaa\nfoo".into();
    app.find_query = "foo".into();
    app.find_open = true;
    assert_eq!(app.find_matches_active().len(), 1);
    let size = app.config.fonts.clamped_editor_size();
    let lh = app.config.fonts.clamped_line_height();
    let expected = 1.0_f32 * (size * lh);
    app.find_match_idx = 0;
    app.pending_scroll = None;
    app.find_navigate(true);
    assert_eq!(app.pending_scroll, Some(expected));
}
