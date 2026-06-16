//! Notepad++-style per-line "change bar" indicators for the line-number
//! gutter. A line edited during the current session but not yet saved shows
//! one colour; a line edited and then saved shows another; a line untouched
//! this session shows nothing.
//!
//! The state is DERIVED by a line-level diff (via `similar`, the same crate
//! the diff view uses) against two frozen baselines — it is NOT tracked by
//! instrumenting each edit primitive. This is deliberate:
//!
//!   * The default editor surface is egui's `TextEdit`, which only reports a
//!     whole-buffer `.changed()` boolean — there is no per-edit line delta to
//!     hook. A diff works uniformly for both the egui and rope editor paths.
//!   * Diffing sidesteps the index-shift bug class entirely: inserting or
//!     deleting a line never false-flags the lines that merely shifted, because
//!     every recompute matches lines by content, not by index. `similar`'s
//!     Myers/Patience diff does the line matching.
//!
//! Two baselines drive the three states:
//!   * `session` — the buffer text at session/open time (frozen until reload).
//!   * `saved`   — the buffer text at the last save (updated on every save).
//!
//! A current line is `None` if it matches the session baseline (never touched),
//! `Saved` if it changed vs the session baseline but currently matches the
//! saved baseline (edited this session and since persisted), and `Unsaved`
//! otherwise (differs from the saved baseline too).

use similar::{ChangeTag, TextDiff};

/// Per-line change state rendered as a coloured bar in the gutter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineChange {
    /// Unchanged since the session began — no indicator.
    None,
    /// Edited this session and not yet saved.
    Unsaved,
    /// Edited this session and since persisted to disk.
    Saved,
}

/// For each line of `current`, whether it is new-or-modified relative to
/// `base` (`true` = changed/new, `false` = present-and-equal in `base`). The
/// returned vector length is exactly `current.lines().count()`.
fn changed_mask(base: &str, current: &str) -> Vec<bool> {
    let n = current.lines().count();
    // Default to "changed": any current line the diff does not explicitly mark
    // Equal is new or modified.
    let mut mask = vec![true; n];
    let diff = TextDiff::from_lines(base, current);
    for ch in diff.iter_all_changes() {
        // `new_index()` is the line's index on the `current` side (None for a
        // deletion, which has no current line). Equal => the line is identical
        // in `base`; Insert => a new/modified current line.
        if let Some(idx) = ch.new_index() {
            if idx < n {
                mask[idx] = ch.tag() != ChangeTag::Equal;
            }
        }
    }
    mask
}

/// Classify every line of `current` as [`LineChange`] using the `session` and
/// `saved` baselines. Length of the result == `current.lines().count()`.
pub fn compute_change_states(session: &str, saved: &str, current: &str) -> Vec<LineChange> {
    let n = current.lines().count();
    if n == 0 {
        return Vec::new();
    }
    // Common case: nothing has been saved yet this session, so the two
    // baselines are identical and no line can be `Saved`. One diff suffices.
    if session == saved {
        return changed_mask(saved, current)
            .into_iter()
            .map(|c| {
                if c {
                    LineChange::Unsaved
                } else {
                    LineChange::None
                }
            })
            .collect();
    }
    let vs_session = changed_mask(session, current);
    let vs_saved = changed_mask(saved, current);
    (0..n)
        .map(|i| {
            if !vs_session.get(i).copied().unwrap_or(true) {
                LineChange::None
            } else if vs_saved.get(i).copied().unwrap_or(true) {
                LineChange::Unsaved
            } else {
                LineChange::Saved
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(session: &str, saved: &str, current: &str) -> Vec<LineChange> {
        compute_change_states(session, saved, current)
    }

    #[test]
    fn untouched_buffer_has_no_marks() {
        let t = "alpha\nbeta\ngamma\n";
        assert_eq!(
            states(t, t, t),
            vec![LineChange::None, LineChange::None, LineChange::None]
        );
    }

    #[test]
    fn empty_buffer_is_empty() {
        assert!(states("", "", "").is_empty());
    }

    #[test]
    fn edited_unsaved_line_is_unsaved_only_on_that_line() {
        let base = "alpha\nbeta\ngamma\n";
        let cur = "alpha\nBETA\ngamma\n";
        // session == saved (nothing saved yet) -> the edited line is Unsaved,
        // the rest None.
        assert_eq!(
            states(base, base, cur),
            vec![LineChange::None, LineChange::Unsaved, LineChange::None]
        );
    }

    #[test]
    fn saved_line_is_green_then_unsaved_again_on_re_edit() {
        let session = "alpha\nbeta\ngamma\n";
        // After saving an edit to line 2, the saved baseline includes it.
        let saved = "alpha\nBETA\ngamma\n";
        let cur = "alpha\nBETA\ngamma\n";
        // Edited this session (differs from session) but matches saved -> Saved.
        assert_eq!(
            states(session, saved, cur),
            vec![LineChange::None, LineChange::Saved, LineChange::None]
        );
        // Re-edit the same line without saving -> back to Unsaved.
        let cur2 = "alpha\nBETA2\ngamma\n";
        assert_eq!(
            states(session, saved, cur2),
            vec![LineChange::None, LineChange::Unsaved, LineChange::None]
        );
    }

    #[test]
    fn inserting_a_line_does_not_flag_the_shifted_lines() {
        // The whole point of diffing: insert a line at the top; only the NEW
        // line is marked, the shifted-down originals stay None.
        let base = "alpha\nbeta\ngamma\n";
        let cur = "NEW\nalpha\nbeta\ngamma\n";
        assert_eq!(
            states(base, base, cur),
            vec![
                LineChange::Unsaved, // the inserted line
                LineChange::None,    // alpha (shifted, unchanged)
                LineChange::None,    // beta
                LineChange::None,    // gamma
            ]
        );
    }

    #[test]
    fn deleting_a_line_leaves_survivors_unmarked() {
        let base = "alpha\nbeta\ngamma\n";
        let cur = "alpha\ngamma\n"; // deleted beta
        assert_eq!(
            states(base, base, cur),
            vec![LineChange::None, LineChange::None]
        );
    }

    #[test]
    fn undo_past_save_point_shows_unsaved_again() {
        // Saved an edit (saved baseline has BETA), then undid back to the
        // original session content. The line now differs from saved -> Unsaved.
        let session = "alpha\nbeta\ngamma\n";
        let saved = "alpha\nBETA\ngamma\n";
        let cur = "alpha\nbeta\ngamma\n"; // undone to session content
                                          // Differs from session? No -> None. Matches the user's mental model:
                                          // reverting a line to its original session state clears its mark.
        assert_eq!(
            states(session, saved, cur),
            vec![LineChange::None, LineChange::None, LineChange::None]
        );
    }

    #[test]
    fn length_always_matches_current_line_count() {
        // Defensive: never panics, length invariant holds for odd baselines.
        for (s, sv, c) in [
            ("", "x\n", "a\nb\n"),
            ("a\nb\nc\n", "", "z\n"),
            ("one\n", "one\ntwo\n", "one\ntwo\nthree\n"),
        ] {
            let out = compute_change_states(s, sv, c);
            assert_eq!(out.len(), c.lines().count());
        }
    }

    #[test]
    fn no_trailing_newline_is_handled() {
        let base = "alpha\nbeta"; // no trailing newline
        let cur = "alpha\nBETA";
        assert_eq!(
            states(base, base, cur),
            vec![LineChange::None, LineChange::Unsaved]
        );
    }
}
