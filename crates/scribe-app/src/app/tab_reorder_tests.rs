//! Phase 2 — tab drag-reorder. The index arithmetic in
//! [`tab_index_after_move`] is the part that historically broke (the old
//! hit-test missed drop targets to the right of the dragged tab), so it is
//! pinned exhaustively here; [`ScribeApp::move_tab`] is the thin wrapper
//! that also keeps `active` pointed at the same buffer.
use super::{fuzzy_move_selection, tab_index_after_move, EditorTab, ScribeApp};
use scribe_core::Config;

#[test]
fn fuzzy_nav_down_advances_and_saturates_at_last() {
    assert_eq!(fuzzy_move_selection(0, 3, false, true), 1);
    assert_eq!(fuzzy_move_selection(2, 3, false, true), 2, "down saturates");
}

#[test]
fn fuzzy_nav_up_saturates_at_first() {
    assert_eq!(fuzzy_move_selection(1, 3, true, false), 0);
    assert_eq!(fuzzy_move_selection(0, 3, true, false), 0, "up saturates");
}

#[test]
fn fuzzy_nav_reclamps_when_results_shrank() {
    // The query just narrowed the list under a stale selection index.
    assert_eq!(fuzzy_move_selection(9, 3, false, false), 2);
    assert_eq!(fuzzy_move_selection(9, 0, false, true), 0, "empty -> 0");
}

#[test]
fn move_is_identity_when_src_equals_target() {
    for idx in 0..5 {
        assert_eq!(tab_index_after_move(2, 2, idx), idx);
    }
}

#[test]
fn rightward_move_places_dragged_at_target_slot() {
    // [0,1,2,3], drag 0 → onto 2  =>  [1,2,0,3]  (0 takes slot 2)
    assert_eq!(tab_index_after_move(0, 2, 0), 2); // dragged element → target
    assert_eq!(tab_index_after_move(0, 2, 1), 0); // 1 shifts left
    assert_eq!(tab_index_after_move(0, 2, 2), 1); // 2 shifts left
    assert_eq!(tab_index_after_move(0, 2, 3), 3); // untouched
}

#[test]
fn leftward_move_places_dragged_at_target_slot() {
    // [0,1,2,3], drag 3 → onto 1  =>  [0,3,1,2]  (3 takes slot 1)
    assert_eq!(tab_index_after_move(3, 1, 3), 1); // dragged element → target
    assert_eq!(tab_index_after_move(3, 1, 0), 0); // before target, untouched
    assert_eq!(tab_index_after_move(3, 1, 1), 2); // 1 shifts right
    assert_eq!(tab_index_after_move(3, 1, 2), 3); // 2 shifts right
}

#[test]
fn adjacent_swap_both_directions() {
    // drag 1 → onto 2 (rightward by one): [0,1,2] -> [0,2,1]
    assert_eq!(tab_index_after_move(1, 2, 1), 2);
    assert_eq!(tab_index_after_move(1, 2, 2), 1);
    // drag 2 → onto 1 (leftward by one): [0,1,2] -> [0,2,1]
    assert_eq!(tab_index_after_move(2, 1, 2), 1);
    assert_eq!(tab_index_after_move(2, 1, 1), 2);
}

/// `move_tab` must physically reorder the tabs AND keep `active` glued to
/// the buffer the user was editing, whichever tab moved.
#[test]
fn move_tab_reorders_and_tracks_active() {
    let mut app = ScribeApp::new_test(Config::default());
    // Replace whatever the constructor produced with 4 identifiable tabs.
    app.tabs.clear();
    for n in 0..4u64 {
        let mut t = EditorTab::scratch();
        t.doc_id = crate::grid::DocId(n);
        app.tabs.push(t);
    }

    // User is editing buffer 1 (tab index 1); drag tab 0 onto tab 2 so 0
    // takes slot 2 => order [1,2,0,3].
    app.active = 1;
    app.move_tab(0, 2);
    let ids: Vec<u64> = app.tabs.iter().map(|t| t.doc_id.0).collect();
    assert_eq!(ids, vec![1, 2, 0, 3], "physical order after rightward move");
    assert_eq!(app.tabs[app.active].doc_id.0, 1, "active still on buffer 1");

    // Now drag the last tab (index 3, buffer 3) onto index 0 so 3 takes
    // slot 0 => [3,1,2,0]. The user is editing buffer 1 (now at index 0);
    // it should follow to index 1.
    app.active = 0;
    let active_buf = app.tabs[app.active].doc_id.0;
    app.move_tab(3, 0);
    let ids: Vec<u64> = app.tabs.iter().map(|t| t.doc_id.0).collect();
    assert_eq!(ids, vec![3, 1, 2, 0], "physical order after leftward move");
    assert_eq!(
        app.tabs[app.active].doc_id.0, active_buf,
        "active follows its buffer across a leftward move"
    );
}

#[test]
fn move_tab_is_noop_on_bad_indices() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs.clear();
    for n in 0..3u64 {
        let mut t = EditorTab::scratch();
        t.doc_id = crate::grid::DocId(n);
        app.tabs.push(t);
    }
    app.active = 2;
    app.move_tab(0, 0); // equal
    app.move_tab(5, 1); // src OOB
    app.move_tab(1, 9); // target OOB
    let ids: Vec<u64> = app.tabs.iter().map(|t| t.doc_id.0).collect();
    assert_eq!(ids, vec![0, 1, 2], "order unchanged");
    assert_eq!(app.active, 2, "active unchanged");
}

#[test]
fn move_tab_refuses_to_move_a_pinned_tab() {
    // #R5: pinned notes are anchored — move_tab is a no-op when the source
    // tab is pinned, so a pinned note can't be drag-reordered.
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs.clear();
    for n in 0..3u64 {
        let mut t = EditorTab::scratch();
        t.doc_id = crate::grid::DocId(n);
        app.tabs.push(t);
    }
    app.tabs[0].pinned = true;
    app.active = 0;
    app.move_tab(0, 2); // try to drag the pinned tab to the end
    let ids: Vec<u64> = app.tabs.iter().map(|t| t.doc_id.0).collect();
    assert_eq!(ids, vec![0, 1, 2], "a pinned tab must not move");
}

#[test]
fn close_tab_refuses_to_close_a_pinned_tab() {
    // #R5: pinned notes can't be closed directly — close_tab is the single
    // chokepoint and refuses a pinned index, while unpinned tabs still close.
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs.clear();
    for n in 0..3u64 {
        let mut t = EditorTab::scratch();
        t.doc_id = crate::grid::DocId(n);
        app.tabs.push(t);
    }
    app.tabs[1].pinned = true;
    app.close_tab(1);
    let ids: Vec<u64> = app.tabs.iter().map(|t| t.doc_id.0).collect();
    assert_eq!(
        ids,
        vec![0, 1, 2],
        "a pinned tab must not be closed directly"
    );
    // An unpinned tab still closes normally.
    app.close_tab(0);
    let ids: Vec<u64> = app.tabs.iter().map(|t| t.doc_id.0).collect();
    assert_eq!(ids, vec![1, 2], "an unpinned tab still closes");
}

#[test]
fn find_navigate_cycles_through_matches_and_wraps() {
    // #R6 — the find bar can jump between matches (Next/Prev/F3), not just
    // count them.
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "foo bar foo baz foo".to_string(); // three "foo"
    app.find_query = "foo".to_string();
    app.find_open = true;
    assert_eq!(app.find_matches_active().len(), 3);

    app.find_match_idx = 0;
    app.find_navigate(true);
    assert_eq!(app.find_match_idx, 1);
    app.find_navigate(true);
    assert_eq!(app.find_match_idx, 2);
    app.find_navigate(true); // wraps to the first
    assert_eq!(app.find_match_idx, 0);
    app.find_navigate(false); // wraps back to the last
    assert_eq!(app.find_match_idx, 2);

    // No matches -> no-op, never panics.
    app.find_query = "this-string-is-absent".to_string();
    app.find_navigate(true);
    assert!(app.find_matches_active().is_empty());
}

/// "Close Others" (close_all_tabs_except) must FOCUS the kept tab, not merely
/// clamp `active`. With a pinned tab positioned BELOW the kept index, the
/// surviving copy of `keep` shifts left as the unpinned tabs before it are
/// removed; `active` must track to it. Regression for the prior clamp-only
/// fallback that left focus on the pinned tab.
#[test]
fn close_all_tabs_except_focuses_the_kept_tab() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs.clear();
    for n in 0..5u64 {
        let mut t = EditorTab::scratch();
        t.doc_id = crate::grid::DocId(n);
        app.tabs.push(t);
    }
    // Pin tab 1 (an index below the kept tab). Keep tab 3. The user was
    // focused elsewhere (tab 0) when invoking "Close Others".
    app.tabs[1].pinned = true;
    app.active = 0;
    app.close_all_tabs_except(3);

    // Survivors: the pinned tab (id 1) + the kept tab (id 3), order [1, 3].
    let ids: Vec<u64> = app.tabs.iter().map(|t| t.doc_id.0).collect();
    assert_eq!(ids, vec![1, 3], "kept tab + pinned tab survive");
    // active must be the kept tab (id 3 at new index 1), NOT the pinned tab.
    assert_eq!(
        app.tabs[app.active].doc_id.0, 3,
        "active focuses the kept tab, not the surviving pinned tab"
    );
}
