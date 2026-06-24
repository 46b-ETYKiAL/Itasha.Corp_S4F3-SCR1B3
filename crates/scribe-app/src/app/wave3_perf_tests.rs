//! Wave-3 perf: the minimap + spellcheck caches now key off a per-tab
//! `edit_gen` counter instead of re-hashing the whole buffer every frame.
//! Correctness hinges on EVERY text mutation advancing the counter — these
//! tests pin the Rust-side mutation funnels (Class A + Class B). The egui /
//! rope write-back paths (Class C/D) require a live frame and are covered by
//! the `out.response.changed()` / `resp.content_changed` hooks directly.
use super::{use_rope_editor, ScribeApp};
use scribe_core::Config;

fn gen(app: &ScribeApp) -> u64 {
    app.tabs[app.active].edit_gen
}

#[test]
fn use_rope_editor_decision_matrix() {
    // The experimental opt-in forces the rope editor regardless of size.
    assert!(use_rope_editor(true, 0, 0));
    assert!(use_rope_editor(true, 10, 16 * 1024 * 1024));
    // threshold 0 disables auto-promotion no matter how big the buffer is.
    assert!(!use_rope_editor(false, usize::MAX, 0));
    // Below the threshold → the canonical egui TextEdit path.
    assert!(!use_rope_editor(false, 1024, 16 * 1024 * 1024));
    // At or above the threshold → the viewport-culled rope path.
    assert!(use_rope_editor(false, 16 * 1024 * 1024, 16 * 1024 * 1024));
    assert!(use_rope_editor(false, 32 * 1024 * 1024, 16 * 1024 * 1024));
}

#[test]
fn set_text_advances_edit_gen() {
    let mut app = ScribeApp::new_test(Config::default());
    let g0 = gen(&app);
    app.tabs[app.active].set_text("hello\nworld\n".to_string());
    assert!(
        gen(&app) > g0,
        "set_text (Class A funnel) must bump edit_gen"
    );
}

#[test]
fn direct_edit_commands_advance_edit_gen() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[app.active].text = "alpha\nbravo\ncharlie\n".to_string();
    app.last_cursor_line_col = Some((2, 1)); // 1-based line 2 (bravo)

    let g = gen(&app);
    app.duplicate_cursor_line();
    assert!(gen(&app) > g, "duplicate_cursor_line must bump edit_gen");

    let g = gen(&app);
    app.move_cursor_line(1);
    assert!(gen(&app) > g, "move_cursor_line must bump edit_gen");

    let g = gen(&app);
    app.join_cursor_line_with_next();
    assert!(
        gen(&app) > g,
        "join_cursor_line_with_next must bump edit_gen"
    );

    app.find_query = "a".to_string();
    app.replace_query = "X".to_string();
    let g = gen(&app);
    app.replace_in_active(true);
    assert!(gen(&app) > g, "replace_in_active must bump edit_gen");
}

// --- P-01 / 4-02 R2: in-buffer find cache (no per-frame rescan) -------------
//
// `find_matches_active` is called every frame the find bar is open. It is now
// memoized by `(query, active tab edit_gen, doc_id)`, so a full-document rescan
// + regex recompile happens ONLY when one of those changes. `find_recompute_count`
// is the test-only miss counter (bumped exactly when `find_all` is re-invoked).

#[test]
fn find_matches_idle_frames_do_not_recompute() {
    // THE key proof: with the query, buffer, and active tab all unchanged,
    // repeated `find_matches_active` calls (i.e. idle frames) reuse the cache —
    // `find_all` runs exactly ONCE, not once per frame.
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[app.active].set_text("foo bar foo baz foo".to_string());
    app.find_query = "foo".to_string();

    let first = app.find_matches_active();
    assert_eq!(first.len(), 3, "three 'foo' matches");
    let after_first = app.find_recompute_count.get();
    assert_eq!(
        after_first, 1,
        "the first call is a cache miss (one recompute)"
    );

    // Simulate 60 idle frames: same query, no edit, same tab.
    for _ in 0..60 {
        let again = app.find_matches_active();
        assert_eq!(again, first, "cached matches must be returned unchanged");
    }
    assert_eq!(
        app.find_recompute_count.get(),
        after_first,
        "idle frames must NOT recompute — the per-frame rescan is gone"
    );
}

#[test]
fn find_matches_query_change_invalidates_cache() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[app.active].set_text("foo bar foo".to_string());
    app.find_query = "foo".to_string();

    assert_eq!(app.find_matches_active().len(), 2);
    let n = app.find_recompute_count.get();
    // Idle reuse first.
    let _ = app.find_matches_active();
    assert_eq!(app.find_recompute_count.get(), n, "idle reuse");

    // Changing the query MUST recompute and return the new match set.
    app.find_query = "bar".to_string();
    assert_eq!(app.find_matches_active().len(), 1, "one 'bar' match");
    assert_eq!(
        app.find_recompute_count.get(),
        n + 1,
        "a query change invalidates the cache"
    );
}

#[test]
fn find_matches_edit_invalidates_cache() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[app.active].set_text("foo foo".to_string());
    app.find_query = "foo".to_string();

    assert_eq!(app.find_matches_active().len(), 2);
    let n = app.find_recompute_count.get();

    // An edit bumps edit_gen, which MUST invalidate the cache even though the
    // query string is identical — and the new match count reflects the edit.
    app.tabs[app.active].set_text("foo foo foo".to_string());
    assert_eq!(app.find_matches_active().len(), 3, "edit added a match");
    assert_eq!(
        app.find_recompute_count.get(),
        n + 1,
        "an edit (edit_gen bump) invalidates the cache"
    );
}

#[test]
fn find_matches_tab_switch_invalidates_cache() {
    // Two tabs can share an edit_gen; the doc_id keys the single-slot cache so a
    // tab switch recomputes against the newly-active buffer.
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[app.active].set_text("foo foo foo".to_string());
    app.new_tab(); // pushes a scratch tab and makes it active
    app.tabs[app.active].set_text("foo".to_string());
    app.find_query = "foo".to_string();
    // Give the two tabs DISTINCT doc_ids (production assigns these in
    // `sync_grid_state`; both scratch tabs default to the DocId(0) sentinel,
    // which would otherwise collide in the single-slot cache key).
    for t in app.tabs.iter_mut() {
        t.doc_id = app.next_doc_id.next();
    }

    // Active tab (1 match) computes first.
    assert_eq!(app.find_matches_active().len(), 1);
    let n = app.find_recompute_count.get();

    // Switch back to the first tab (3 matches) — the cache must recompute.
    app.active = 0;
    assert_eq!(
        app.find_matches_active().len(),
        3,
        "switched tab has three matches"
    );
    assert_eq!(
        app.find_recompute_count.get(),
        n + 1,
        "a tab switch (doc_id change) invalidates the cache"
    );
}
