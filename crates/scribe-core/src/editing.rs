//! Cursor + selection + edit primitives over a [`ropey::Rope`].
//!
//! This is the editing model the owned rope editor (KEYSTONE) is built on —
//! the layer egui's `TextEdit` provides internally and which we must own to
//! get multi-cursor, a real persistent undo stack, and viewport-culled
//! editing on a multi-GiB buffer.
//!
//! It is deliberately **UI-free and pure**: every operation is a function over
//! `(&mut Rope, &mut EditState)`, so the whole model is unit-testable without
//! an egui context. Indices are CHAR indices (ropey's native unit), never
//! bytes, so multi-byte UTF-8 never splits a caret.

use ropey::Rope;
use serde::{Deserialize, Serialize};

/// A single caret with an optional selection. `anchor == cursor` means no
/// selection; otherwise the selection spans `[min(anchor,cursor), max(...))`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditState {
    /// Char index of the caret (where text is inserted / deleted).
    pub cursor: usize,
    /// Char index of the selection anchor. Equal to `cursor` when there is no
    /// active selection.
    pub anchor: usize,
    /// Sticky target column for vertical movement, so moving down through a
    /// short line and back doesn't lose the original column. `None` resets to
    /// the current column on the next vertical move.
    pub goal_col: Option<usize>,
}

impl EditState {
    /// A collapsed caret at char index `cursor`.
    pub fn at(cursor: usize) -> Self {
        Self {
            cursor,
            anchor: cursor,
            goal_col: None,
        }
    }

    /// Whether a non-empty selection is active.
    pub fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    /// The selected char range `[start, end)` (empty when collapsed).
    pub fn selection(&self) -> std::ops::Range<usize> {
        let lo = self.cursor.min(self.anchor);
        let hi = self.cursor.max(self.anchor);
        lo..hi
    }

    /// Collapse caret + anchor to `c` and clear the goal column.
    fn collapse_to(&mut self, c: usize) {
        self.cursor = c;
        self.anchor = c;
        self.goal_col = None;
    }

    /// Move the caret to `c`, extending the selection iff `select` (otherwise
    /// the anchor follows the caret). Clears the goal column.
    fn place(&mut self, c: usize, select: bool) {
        self.cursor = c;
        if !select {
            self.anchor = c;
        }
        self.goal_col = None;
    }
}

/// Clamp a char index into `[0, rope.len_chars()]`.
fn clamp(rope: &Rope, c: usize) -> usize {
    c.min(rope.len_chars())
}

/// The (line, column) of char index `c`, both 0-based. Column is chars from
/// the line start.
pub fn line_col(rope: &Rope, c: usize) -> (usize, usize) {
    let c = clamp(rope, c);
    let line = rope.char_to_line(c);
    let col = c - rope.line_to_char(line);
    (line, col)
}

/// The char index at (line, column), clamping the column to the line's
/// content length (excluding the trailing newline) and the line to the rope.
pub fn char_at(rope: &Rope, line: usize, col: usize) -> usize {
    let last_line = rope.len_lines().saturating_sub(1);
    let line = line.min(last_line);
    let line_start = rope.line_to_char(line);
    // Length of the line WITHOUT a trailing '\n' so End / vertical moves land
    // before the newline, matching every editor's behaviour.
    let slice = rope.line(line);
    let mut content = slice.len_chars();
    if slice
        .get_char(content.wrapping_sub(1))
        .map(|ch| ch == '\n')
        .unwrap_or(false)
    {
        content -= 1;
    }
    line_start + col.min(content)
}

/// Delete the active selection (if any). Returns `true` when something was
/// removed. The caret collapses to the selection start.
pub fn delete_selection(rope: &mut Rope, st: &mut EditState) -> bool {
    if !st.has_selection() {
        return false;
    }
    let range = st.selection();
    rope.remove(range.clone());
    st.collapse_to(range.start);
    true
}

/// Insert `text` at the caret, replacing any active selection. The caret
/// advances to the end of the inserted text.
pub fn insert(rope: &mut Rope, st: &mut EditState, text: &str) {
    delete_selection(rope, st);
    let at = clamp(rope, st.cursor);
    rope.insert(at, text);
    st.collapse_to(at + text.chars().count());
}

/// Backspace: delete the selection if any, else the char before the caret.
pub fn backspace(rope: &mut Rope, st: &mut EditState) {
    if delete_selection(rope, st) {
        return;
    }
    if st.cursor > 0 {
        let to = clamp(rope, st.cursor);
        rope.remove(to - 1..to);
        st.collapse_to(to - 1);
    }
}

/// Delete forward: delete the selection if any, else the char at the caret.
pub fn delete_forward(rope: &mut Rope, st: &mut EditState) {
    if delete_selection(rope, st) {
        return;
    }
    let at = clamp(rope, st.cursor);
    if at < rope.len_chars() {
        rope.remove(at..at + 1);
        st.collapse_to(at);
    }
}

/// Move the caret left/right by `delta` chars. When `select` is false a
/// collapse of an existing selection lands on the appropriate edge (left →
/// selection start, right → selection end) WITHOUT moving further, matching
/// standard editor behaviour.
pub fn move_horizontal(rope: &mut Rope, st: &mut EditState, delta: isize, select: bool) {
    if !select && st.has_selection() {
        let range = st.selection();
        let edge = if delta < 0 { range.start } else { range.end };
        st.collapse_to(edge);
        return;
    }
    let cur = st.cursor as isize;
    let next = (cur + delta).clamp(0, rope.len_chars() as isize) as usize;
    st.place(next, select);
}

/// Move the caret up/down by `dir` lines (-1 = up, +1 = down), preserving the
/// sticky goal column.
pub fn move_vertical(rope: &mut Rope, st: &mut EditState, dir: isize, select: bool) {
    let (line, col) = line_col(rope, st.cursor);
    let goal = st.goal_col.unwrap_or(col);
    let last_line = rope.len_lines().saturating_sub(1);
    let target_line = (line as isize + dir).clamp(0, last_line as isize) as usize;
    let next = char_at(rope, target_line, goal);
    st.cursor = next;
    if !select {
        st.anchor = next;
    }
    st.goal_col = Some(goal);
}

/// Move to the start of the caret's line (column 0).
pub fn move_line_start(rope: &mut Rope, st: &mut EditState, select: bool) {
    let (line, _) = line_col(rope, st.cursor);
    st.place(rope.line_to_char(line), select);
}

/// Move to the end of the caret's line (before the trailing newline).
pub fn move_line_end(rope: &mut Rope, st: &mut EditState, select: bool) {
    let (line, _) = line_col(rope, st.cursor);
    st.place(char_at(rope, line, usize::MAX), select);
}

/// Select the whole buffer (anchor at start, caret at end).
pub fn select_all(rope: &Rope, st: &mut EditState) {
    st.anchor = 0;
    st.cursor = rope.len_chars();
    st.goal_col = None;
}

/// The currently-selected text, or an empty string when collapsed.
pub fn selected_text(rope: &Rope, st: &EditState) -> String {
    if !st.has_selection() {
        return String::new();
    }
    rope.slice(st.selection()).to_string()
}

/// One undo checkpoint: the full buffer text + caret char index. Snapshot-
/// based (simple and always-correct vs. operation logs); the [`History`]
/// coalesces runs of same-kind edits so a burst of typing is one undo step.
/// `Serialize`/`Deserialize` make the whole stack persistable to disk for
/// cross-session undo (F-023).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub text: String,
    pub cursor: usize,
}

impl Snapshot {
    pub fn new(text: impl Into<String>, cursor: usize) -> Self {
        Self {
            text: text.into(),
            cursor,
        }
    }
}

/// The class of an edit, used to coalesce consecutive same-kind edits into a
/// single undo checkpoint (so undo removes a typed word/run, not one char).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditKind {
    Insert,
    Delete,
    /// Anything that should always start a fresh undo group (paste, replace,
    /// reformat, structural edit).
    Other,
}

/// A bounded undo/redo history of [`Snapshot`]s. The caller records the
/// pre-edit snapshot after each edit; `undo`/`redo` swap snapshots with the
/// live state. Depth is capped (oldest dropped) to bound memory + disk size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Kind of the most recently recorded edit, for coalescing.
    last_kind: Option<EditKind>,
    cap: usize,
}

impl History {
    /// A new history holding up to `cap` undo checkpoints (`cap >= 1`).
    pub fn new(cap: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            last_kind: None,
            cap: cap.max(1),
        }
    }

    /// Record the pre-edit `before` snapshot for an edit of `kind`. Coalesces:
    /// a run of consecutive `Insert` (or consecutive `Delete`) edits keeps only
    /// the FIRST pre-edit snapshot, so undo reverts the whole run. `Other`
    /// always starts a new group. Recording clears the redo stack.
    pub fn record(&mut self, before: Snapshot, kind: EditKind) {
        self.redo.clear();
        let coalesce = matches!(kind, EditKind::Insert | EditKind::Delete)
            && self.last_kind == Some(kind)
            && !self.undo.is_empty();
        self.last_kind = Some(kind);
        if coalesce {
            return;
        }
        self.undo.push(before);
        if self.undo.len() > self.cap {
            self.undo.remove(0);
        }
    }

    /// Undo: return the snapshot to restore, pushing `current` onto the redo
    /// stack. `None` when there is nothing to undo.
    pub fn undo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let prev = self.undo.pop()?;
        self.redo.push(current);
        self.last_kind = None; // the next edit starts a fresh group
        Some(prev)
    }

    /// Redo: return the snapshot to restore, pushing `current` onto the undo
    /// stack. `None` when there is nothing to redo.
    pub fn redo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        self.last_kind = None;
        Some(next)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Force the next `record` to start a new undo group even if it is the same
    /// kind as the last (e.g. after a cursor move or a save boundary).
    pub fn break_group(&mut self) {
        self.last_kind = None;
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new(512)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    #[test]
    fn insert_advances_caret() {
        let mut r = rope("");
        let mut st = EditState::at(0);
        insert(&mut r, &mut st, "hi");
        assert_eq!(r.to_string(), "hi");
        assert_eq!(st.cursor, 2);
        assert!(!st.has_selection());
    }

    #[test]
    fn insert_replaces_selection() {
        let mut r = rope("abcd");
        let mut st = EditState {
            anchor: 1,
            cursor: 3,
            goal_col: None,
        };
        insert(&mut r, &mut st, "X");
        assert_eq!(r.to_string(), "aXd");
        assert_eq!(st.cursor, 2);
    }

    #[test]
    fn backspace_removes_prev_char() {
        let mut r = rope("abc");
        let mut st = EditState::at(2);
        backspace(&mut r, &mut st);
        assert_eq!(r.to_string(), "ac");
        assert_eq!(st.cursor, 1);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut r = rope("abc");
        let mut st = EditState::at(0);
        backspace(&mut r, &mut st);
        assert_eq!(r.to_string(), "abc");
        assert_eq!(st.cursor, 0);
    }

    #[test]
    fn backspace_deletes_selection() {
        let mut r = rope("abcd");
        let mut st = EditState {
            anchor: 1,
            cursor: 3,
            goal_col: None,
        };
        backspace(&mut r, &mut st);
        assert_eq!(r.to_string(), "ad");
        assert_eq!(st.cursor, 1);
    }

    #[test]
    fn delete_forward_removes_char_at_caret() {
        let mut r = rope("abc");
        let mut st = EditState::at(1);
        delete_forward(&mut r, &mut st);
        assert_eq!(r.to_string(), "ac");
        assert_eq!(st.cursor, 1);
    }

    #[test]
    fn horizontal_collapse_lands_on_edge() {
        let mut r = rope("abcd");
        let mut st = EditState {
            anchor: 1,
            cursor: 3,
            goal_col: None,
        };
        // Left with a selection collapses to the start, not start-1.
        move_horizontal(&mut r, &mut st, -1, false);
        assert_eq!(st.cursor, 1);
        assert!(!st.has_selection());
    }

    #[test]
    fn horizontal_clamps_at_bounds() {
        let mut r = rope("ab");
        let mut st = EditState::at(0);
        move_horizontal(&mut r, &mut st, -5, false);
        assert_eq!(st.cursor, 0);
        move_horizontal(&mut r, &mut st, 10, false);
        assert_eq!(st.cursor, 2);
    }

    #[test]
    fn horizontal_select_extends() {
        let mut r = rope("abcd");
        let mut st = EditState::at(0);
        move_horizontal(&mut r, &mut st, 2, true);
        assert_eq!(st.selection(), 0..2);
        assert!(st.has_selection());
    }

    #[test]
    fn vertical_preserves_goal_column() {
        // Line 0 is long, line 1 is short, line 2 is long again. Moving down
        // twice from col 4 should land back at col 4 on line 2 even though
        // line 1 truncated the column.
        let mut r = rope("abcdef\nxy\nuvwxyz\n");
        let mut st = EditState::at(4); // line 0, col 4
        move_vertical(&mut r, &mut st, 1, false); // → line 1, clamped to col 2
        let (l1, c1) = line_col(&r, st.cursor);
        assert_eq!((l1, c1), (1, 2));
        move_vertical(&mut r, &mut st, 1, false); // → line 2, restored col 4
        let (l2, c2) = line_col(&r, st.cursor);
        assert_eq!((l2, c2), (2, 4));
    }

    #[test]
    fn line_start_and_end() {
        let mut r = rope("hello\nworld\n");
        let mut st = EditState::at(8); // line 1, col 2
        move_line_start(&mut r, &mut st, false);
        assert_eq!(line_col(&r, st.cursor), (1, 0));
        move_line_end(&mut r, &mut st, false);
        // "world" is 5 chars → col 5, before the newline.
        assert_eq!(line_col(&r, st.cursor), (1, 5));
    }

    #[test]
    fn select_all_and_selected_text() {
        let r = rope("abc\ndef");
        let mut st = EditState::at(0);
        select_all(&r, &mut st);
        assert_eq!(st.selection(), 0..7);
        assert_eq!(selected_text(&r, &st), "abc\ndef");
    }

    #[test]
    fn multibyte_chars_are_caret_safe() {
        // Each emoji is one char but 4 bytes; caret math stays in chars.
        let mut r = rope("a😀b");
        let mut st = EditState::at(2); // after the emoji
        backspace(&mut r, &mut st); // removes the emoji, not a byte
        assert_eq!(r.to_string(), "ab");
        assert_eq!(st.cursor, 1);
    }

    #[test]
    fn selected_text_empty_when_collapsed() {
        let r = rope("abc");
        let st = EditState::at(1);
        assert_eq!(selected_text(&r, &st), "");
    }

    // ---- History (undo/redo) ----

    #[test]
    fn history_undo_redo_roundtrip() {
        let mut h = History::new(8);
        assert!(!h.can_undo());
        // Type "a" then "b": each Insert, but coalesced into one group.
        h.record(Snapshot::new("", 0), EditKind::Insert);
        h.record(Snapshot::new("a", 1), EditKind::Insert);
        assert!(h.can_undo());
        // Undo from current "ab" → restores the pre-run "" snapshot.
        let restored = h.undo(Snapshot::new("ab", 2)).unwrap();
        assert_eq!(restored, Snapshot::new("", 0));
        // Redo → back to "ab".
        let redone = h.redo(Snapshot::new("", 0)).unwrap();
        assert_eq!(redone, Snapshot::new("ab", 2));
    }

    #[test]
    fn history_coalesces_same_kind_runs() {
        let mut h = History::new(8);
        h.record(Snapshot::new("", 0), EditKind::Insert);
        h.record(Snapshot::new("a", 1), EditKind::Insert);
        h.record(Snapshot::new("ab", 2), EditKind::Insert);
        // Three inserts coalesced → ONE undo step back to "".
        assert_eq!(h.undo(Snapshot::new("abc", 3)).unwrap().text, "");
        assert!(!h.can_undo());
    }

    #[test]
    fn history_kind_change_starts_new_group() {
        let mut h = History::new(8);
        h.record(Snapshot::new("", 0), EditKind::Insert); // type
        h.record(Snapshot::new("abc", 3), EditKind::Delete); // then delete
                                                             // Two distinct groups: undo delete first, then undo insert.
        assert_eq!(h.undo(Snapshot::new("ab", 2)).unwrap().text, "abc");
        assert_eq!(h.undo(Snapshot::new("abc", 3)).unwrap().text, "");
    }

    #[test]
    fn history_record_clears_redo() {
        let mut h = History::new(8);
        h.record(Snapshot::new("", 0), EditKind::Insert);
        h.undo(Snapshot::new("a", 1));
        assert!(h.can_redo());
        // A new edit after an undo discards the redo branch.
        h.record(Snapshot::new("x", 1), EditKind::Other);
        assert!(!h.can_redo());
    }

    #[test]
    fn history_respects_cap() {
        let mut h = History::new(2);
        for i in 0..5 {
            // Distinct kinds so nothing coalesces.
            h.record(Snapshot::new(format!("{i}"), 0), EditKind::Other);
        }
        // Only the last 2 checkpoints survive.
        assert!(h.undo(Snapshot::new("cur", 0)).is_some());
        assert!(h.undo(Snapshot::new("cur", 0)).is_some());
        assert!(h.undo(Snapshot::new("cur", 0)).is_none());
    }

    #[test]
    fn history_serde_roundtrips() {
        let mut h = History::new(4);
        h.record(Snapshot::new("hello", 5), EditKind::Other);
        let json = serde_json::to_string(&h).unwrap();
        let back: History = serde_json::from_str(&json).unwrap();
        assert!(back.can_undo());
    }
}
