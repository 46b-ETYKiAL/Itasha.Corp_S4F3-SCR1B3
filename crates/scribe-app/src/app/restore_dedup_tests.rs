//! Regression tests for the "same note opened twice on session restore"
//! duplication and its defensive guards.
//!
//! Root cause: nothing enforced the one-tab-per-file invariant. Once two tabs
//! shared a path (an un-deduped `open_path`, or a stale backup entry coexisting
//! with a clean one in the session manifest), the duplicate was persisted and
//! reappeared every restart — the two copies silently diverging (stale snapshot
//! vs current disk). These tests pin the three primitives that now uphold the
//! invariant: `open_path` focus-existing, `merge_restored_duplicate`, and the
//! vertical-strip drop-indicator geometry that the same wave fixed.

use super::{side_tab_insertion_y, EditorTab, ScribeApp};
use scribe_core::Config;

/// Opening a file that is already open must FOCUS the existing tab, never add a
/// second copy — the upstream guard that stops the duplicate from ever being
/// created (and thus persisted into the session manifest).
#[test]
fn open_path_focuses_existing_tab_instead_of_duplicating() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("note.txt");
    std::fs::write(&p, "hello").unwrap();
    let mut app = ScribeApp::new_test(Config::default());

    app.open_path(p.clone());
    let after_first = app.tabs.len();
    let first_idx = app.active;

    app.open_path(p.clone());
    assert_eq!(
        app.tabs.len(),
        after_first,
        "re-opening an already-open file must not add a tab"
    );
    assert_eq!(
        app.active, first_idx,
        "re-opening must focus the existing tab"
    );

    // Exactly one tab is bound to this canonical path.
    let canon = std::fs::canonicalize(&p).unwrap();
    let n = app
        .tabs
        .iter()
        .filter(|t| {
            t.doc
                .path()
                .map(|q| std::fs::canonicalize(q).unwrap() == canon)
                .unwrap_or(false)
        })
        .count();
    assert_eq!(n, 1, "the file must occupy exactly one tab");
}

/// A stale restored snapshot (dirty) duplicated against a clean from-disk copy
/// must collapse to ONE tab that keeps the unsaved content AND raises
/// `external_change`, so the F-022 "Reload / Keep mine" banner makes the
/// divergence explicit instead of opening a confusing second tab.
#[test]
fn merge_keeps_unsaved_copy_and_flags_divergence() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("note.txt");
    std::fs::write(&p, "DISK v2").unwrap();

    // Existing tab = clean copy of the current disk content.
    let mut existing = EditorTab::from_path(p.clone()).expect("open clean");
    assert!(!existing.is_dirty());
    // Candidate = a restored snapshot whose content differs from disk (stale).
    let candidate = EditorTab::from_backup(Some(p.clone()), "OLD snapshot".to_string());
    assert!(candidate.is_dirty(), "snapshot != disk → dirty");

    EditorTab::merge_restored_duplicate(&mut existing, candidate);

    assert_eq!(
        existing.text, "OLD snapshot",
        "the unsaved snapshot content must be preserved, not dropped"
    );
    assert!(
        existing.external_change,
        "a divergent restored copy must flag external_change so the banner fires"
    );
}

/// Two clean copies of the same (unchanged) file collapse to one tab with NO
/// banner — there is nothing for the user to resolve.
#[test]
fn merge_two_clean_copies_raises_no_banner() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("note.txt");
    std::fs::write(&p, "same").unwrap();

    let mut existing = EditorTab::from_path(p.clone()).expect("open a");
    let candidate = EditorTab::from_path(p.clone()).expect("open b");

    EditorTab::merge_restored_duplicate(&mut existing, candidate);

    assert!(!existing.is_dirty());
    assert!(
        !existing.external_change,
        "two identical clean copies need no resolution banner"
    );
}

/// The vertical side-strip drop indicator sits in the inter-row GAP (a
/// separator BETWEEN tabs), not at a chip's top edge across its inner widgets.
#[test]
fn side_tab_insertion_line_is_in_the_gap() {
    // First row: just above its top edge (no predecessor).
    assert_eq!(side_tab_insertion_y(0, 100.0, None), 99.0);
    // Middle row: midpoint of the gap between the previous row's bottom (80)
    // and this row's top (100) → 90, which is OUTSIDE both chip bodies.
    assert_eq!(side_tab_insertion_y(1, 100.0, Some(80.0)), 90.0);
    assert_eq!(side_tab_insertion_y(3, 240.0, Some(220.0)), 230.0);
    // Defensive: idx 0 ignores any stray prev_bottom (guarded by `idx > 0`).
    assert_eq!(side_tab_insertion_y(0, 50.0, Some(10.0)), 49.0);
}
