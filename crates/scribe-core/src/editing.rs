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
    // Guard the CLAMPED index, not the raw cursor: a stale `st.cursor > 0`
    // restored from a session manifest over a now-empty rope would clamp to
    // `to == 0`, and `to - 1` then underflows → `rope.remove(usize::MAX..0)`
    // panics → `panic = "abort"` crashes the editor. `delete_forward` already
    // guards its clamped index this way.
    let to = clamp(rope, st.cursor);
    if to > 0 {
        rope.remove(to - 1..to);
        st.collapse_to(to - 1);
    } else {
        st.collapse_to(0);
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

// ---- Multi-cursor (F-009) ------------------------------------------------

/// Apply a per-caret edit to every caret in `carets`, managing the running
/// text-length offset so each caret edits at its position SHIFTED by the edits
/// the earlier carets performed. `carets` is sorted ascending and de-duplicated
/// (carets that collapse onto the same position after editing are merged).
///
/// `f` performs the op on ONE `(rope, caret)` and the function derives each
/// edit's char-length delta from the rope length, so callers reuse the existing
/// single-caret primitives ([`insert`], [`backspace`], …) unchanged. This is
/// the load-bearing correctness primitive for multi-cursor edits.
pub fn for_each_caret<F>(rope: &mut Rope, carets: &mut Vec<EditState>, mut f: F)
where
    F: FnMut(&mut Rope, &mut EditState),
{
    if carets.is_empty() {
        return;
    }
    // C-02 / R1 root cause: dispatching to every caret double-edits any region
    // two carets' selections share. Merge overlapping/adjacent selection
    // intervals into ONE caret per merged region BEFORE dispatch so the shared
    // region is edited exactly once. Disjoint carets are untouched, so the
    // offset bookkeeping below is unchanged for the non-overlapping case.
    normalize_carets(carets);
    carets.sort_by_key(|c| c.selection().start.min(c.cursor));
    let mut offset: isize = 0;
    for caret in carets.iter_mut() {
        // Shift this caret by the net length change earlier carets caused, then
        // clamp BOTH ends to the live char-length. The low `.max(0)` alone is not
        // enough: a delete-class op (`delete_selection`/`replace_selection`/…)
        // does not re-clamp the selection's high end, so a stale caret whose
        // index exceeds the current length would hand `rope.remove` an
        // out-of-range range → panic → `panic = "abort"`. Insert/backspace launder
        // their index through `clamp`, but the delete-class primitives do not.
        let len = rope.len_chars();
        caret.cursor = ((caret.cursor as isize + offset).max(0) as usize).min(len);
        caret.anchor = ((caret.anchor as isize + offset).max(0) as usize).min(len);
        let before = rope.len_chars() as isize;
        f(rope, caret);
        offset += rope.len_chars() as isize - before;
    }
    dedupe_carets(carets);
}

/// Merge carets whose selections OVERLAP or are ADJACENT (touching) into a
/// single caret per merged region, leaving disjoint carets untouched. This is
/// the load-bearing invariant that prevents the multi-caret double-edit bug
/// (C-02 / R1): if two carets' selections share any chars, dispatching the edit
/// to both applies it twice to the shared region, corrupting the text. Merging
/// to one caret per union region guarantees each region is edited exactly once.
///
/// Semantics:
/// - Carets are sorted by selection start (then end). Walking left-to-right, a
///   caret is absorbed into the current run when its selection start is `<=` the
///   running merged end — i.e. overlapping *or* exactly touching. The merged
///   region is the interval union `[min(starts), max(ends))`.
/// - Two collapsed carets at the *same* position merge (they coincide); two
///   collapsed carets at *different* positions do NOT merge (a zero-width
///   selection at `p` only touches another zero-width selection also at `p`),
///   so independent multi-cursor typing is preserved exactly as before.
/// - Direction is preserved sensibly: the merged caret keeps the orientation of
///   the run's PRIMARY caret (the first caret of the run in start-sorted order).
///   A reversed primary (anchor > cursor) yields a reversed merged selection
///   (anchor at the union end, cursor at the union start); a forward primary
///   yields a forward merged selection. The goal column is carried from the
///   primary. The selected char range (`selection()`) is identical either way.
pub fn normalize_carets(carets: &mut Vec<EditState>) {
    if carets.len() < 2 {
        return;
    }
    // Sort by selection start, then end — the canonical interval-merge order.
    carets.sort_by_key(|c| {
        let r = c.selection();
        (r.start, r.end)
    });

    let mut merged: Vec<EditState> = Vec::with_capacity(carets.len());
    for &caret in carets.iter() {
        let cur = caret.selection();
        match merged.last_mut() {
            // Absorb into the current run when this caret's start touches or
            // overlaps the running merged end.
            Some(run) if cur.start <= run.selection().end => {
                let run_sel = run.selection();
                let new_start = run_sel.start.min(cur.start);
                let new_end = run_sel.end.max(cur.end);
                // Preserve the run primary's orientation. The primary is the
                // first caret of the run, which already lives in `run`.
                let reversed = run.anchor > run.cursor;
                if reversed {
                    run.anchor = new_end;
                    run.cursor = new_start;
                } else {
                    run.anchor = new_start;
                    run.cursor = new_end;
                }
            }
            // Disjoint from the previous run (or first caret) → start a new run.
            _ => merged.push(caret),
        }
    }
    *carets = merged;
}

/// Merge carets that occupy the same collapsed position (keep one), preserving
/// ascending order. Selections are compared by `(start, end)`.
///
/// `normalize_carets` is the stronger primitive (it also merges overlapping and
/// adjacent *selections*); `dedupe_carets` only collapses exact coincidence and
/// is kept for callers that post-process carets after an op has already
/// collapsed them onto shared positions.
pub fn dedupe_carets(carets: &mut Vec<EditState>) {
    carets.sort_by_key(|c| (c.selection().start, c.cursor));
    carets.dedup_by_key(|c| (c.cursor, c.anchor));
}

/// Add a collapsed caret one line below the lowest caret (`dir = 1`) or above
/// the highest (`dir = -1`), at that reference caret's column. No-op when the
/// new line would fall outside the buffer. Returns `true` when a caret was
/// added.
pub fn add_caret_vertical(rope: &Rope, carets: &mut Vec<EditState>, dir: isize) -> bool {
    let reference = if dir >= 0 {
        carets.iter().map(|c| c.cursor).max()
    } else {
        carets.iter().map(|c| c.cursor).min()
    };
    let Some(reference) = reference else {
        return false;
    };
    let (line, col) = line_col(rope, reference);
    let last_line = rope.len_lines().saturating_sub(1);
    let target = line as isize + dir;
    if target < 0 || target as usize > last_line {
        return false;
    }
    let at = char_at(rope, target as usize, col);
    if carets.iter().any(|c| c.cursor == at && !c.has_selection()) {
        return false;
    }
    carets.push(EditState::at(at));
    dedupe_carets(carets);
    true
}

// ---- Editing power-features: auto-close, auto-indent, line ops -----------

/// The auto-close partner for an opening bracket/quote, or `None`.
pub fn closing_for(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' => Some('\''),
        '`' => Some('`'),
        _ => None,
    }
}

/// True when `ch` is a closing bracket/quote that auto-close skip-over applies
/// to AND the char at the caret in `rope` is exactly that closer. Used so that
/// typing a closer immediately before an identical auto-inserted closer steps
/// over it instead of inserting a duplicate (the universal editor behaviour).
pub fn should_skip_over(rope: &Rope, cursor: usize, ch: char) -> bool {
    if !matches!(ch, ')' | ']' | '}' | '"' | '\'' | '`') {
        return false;
    }
    cursor < rope.len_chars() && rope.char(cursor) == ch
}

/// (start, end) char indices of the identifier-like word containing `cursor`.
/// A word is `[A-Za-z0-9_]+`; returns `(cursor, cursor)` when not on a word.
pub fn word_bounds(rope: &Rope, cursor: usize) -> (usize, usize) {
    let is_word = |c: char| c == '_' || c.is_alphanumeric();
    let n = rope.len_chars();
    let mut s = cursor.min(n);
    while s > 0 && is_word(rope.char(s - 1)) {
        s -= 1;
    }
    let mut e = cursor.min(n);
    while e < n && is_word(rope.char(e)) {
        e += 1;
    }
    (s, e)
}

/// End char index of the word at `cursor` (companion to `word_bounds`).
pub fn word_end(rope: &Rope, cursor: usize) -> usize {
    word_bounds(rope, cursor).1
}

/// Add a new caret selecting the next occurrence of the PRIMARY caret's
/// selected text, searching forward from the end of the furthest existing
/// selection and wrapping once. Returns `true` when a caret was added.
///
/// Mirrors Sublime/VS Code "Ctrl+D": the first press (no selection yet) is the
/// caller's job (select the word under the caret); each subsequent press lands
/// here and grows the caret set. Literal, case-sensitive match — matching the
/// "expand to next exact occurrence" semantics users expect from Ctrl+D.
///
/// Bounded by a 5M-char cap (it materialises a char vector of the whole buffer
/// per call); above that, Ctrl+D is a no-op to keep the editor fast.
pub fn add_next_occurrence(rope: &Rope, carets: &mut Vec<EditState>) -> bool {
    if rope.len_chars() > 5_000_000 {
        return false;
    }
    // The "seed" is the primary (lowest) caret's selection.
    let Some(primary) = carets.first().copied() else {
        return false;
    };
    if !primary.has_selection() {
        return false;
    }
    let sel = primary.selection();
    let needle: String = rope.slice(sel.clone()).chars().collect();
    if needle.is_empty() {
        return false;
    }
    let nlen = needle.chars().count();
    // Search starts after the END of the furthest selection so repeated presses
    // walk forward through the document.
    let search_from = carets
        .iter()
        .map(|c| c.selection().end)
        .max()
        .unwrap_or(sel.end);
    let hay: String = rope.chars().collect();
    // Char-index search (ropey is char-indexed; build a char vec once — fine for
    // the typical buffer, and Ctrl+D is an interactive, bounded operation).
    let chars: Vec<char> = hay.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    let find_at = |start: usize| -> Option<usize> {
        if chars.len() < nlen {
            return None;
        }
        (start..=chars.len() - nlen).find(|&i| chars[i..i + nlen] == needle_chars[..])
    };
    // Forward from search_from, else wrap to start (but never re-add an existing
    // selection range).
    let existing: std::collections::HashSet<(usize, usize)> = carets
        .iter()
        .map(|c| (c.selection().start, c.selection().end))
        .collect();
    let candidate = find_at(search_from)
        .or_else(|| find_at(0))
        .filter(|&at| !existing.contains(&(at, at + nlen)));
    let Some(at) = candidate else {
        return false;
    };
    let mut st = EditState::at(at + nlen); // cursor at end of match
    st.anchor = at; // anchor at start → the match is selected
    carets.push(st);
    dedupe_carets(carets);
    true
}

/// Build a block (column) selection: one caret/selection per row from
/// `anchor` to `target` (inclusive), each spanning columns `[col_a, col_b]`
/// clamped to that row's length. Rows shorter than `col_a` get a collapsed
/// caret at end-of-line (Sublime behaviour). Returns carets ordered top→bottom.
pub fn block_selection(
    rope: &Rope,
    anchor: (usize, usize),
    target: (usize, usize),
) -> Vec<EditState> {
    let (l0, l1) = (anchor.0.min(target.0), anchor.0.max(target.0));
    let (ca, cb) = (anchor.1.min(target.1), anchor.1.max(target.1));
    let last_line = rope.len_lines().saturating_sub(1);
    let mut out = Vec::new();
    for line in l0..=l1.min(last_line) {
        let line_start = rope.line_to_char(line);
        let line_len = {
            let s = rope.line(line);
            // exclude the trailing '\n' from the selectable width
            let mut len = s.len_chars();
            if s.len_chars() > 0 && s.char(s.len_chars() - 1) == '\n' {
                len -= 1;
            }
            len
        };
        let a = line_start + ca.min(line_len);
        let b = line_start + cb.min(line_len);
        let mut st = EditState::at(b);
        st.anchor = a;
        out.push(st);
    }
    dedupe_carets(&mut out);
    out
}

/// The leading whitespace (spaces/tabs) of the line containing char index `c`.
pub fn leading_whitespace(rope: &Rope, c: usize) -> String {
    let (line, _) = line_col(rope, c);
    let start = rope.line_to_char(line);
    let slice = rope.line(line);
    let mut ws = String::new();
    for ch in slice.chars() {
        if ch == ' ' || ch == '\t' {
            ws.push(ch);
        } else {
            break;
        }
    }
    let _ = start;
    ws
}

/// Indent (or `outdent`) every line touched by the caret's selection (or the
/// caret's own line when collapsed) by one `unit` of whitespace. The selection
/// is extended to cover the affected lines. Returns the net char delta.
pub fn indent_lines(rope: &mut Rope, st: &mut EditState, unit: &str, outdent: bool) {
    let sel = st.selection();
    let first_line = rope.char_to_line(sel.start);
    let last_line = rope.char_to_line(sel.end.max(sel.start));
    let unit_len = unit.chars().count();
    // Walk lines bottom-up so earlier edits don't shift later line offsets.
    for line in (first_line..=last_line).rev() {
        let line_start = rope.line_to_char(line);
        if outdent {
            // Remove up to `unit_len` leading whitespace chars.
            let slice = rope.line(line);
            let mut removable = 0usize;
            for ch in slice.chars().take(unit_len) {
                if ch == ' ' || ch == '\t' {
                    removable += 1;
                } else {
                    break;
                }
            }
            if removable > 0 {
                rope.remove(line_start..line_start + removable);
            }
        } else {
            // Don't indent a completely empty last line of a selection.
            if rope.line(line).len_chars() == 0 {
                continue;
            }
            rope.insert(line_start, unit);
        }
    }
    // Re-anchor the caret/selection to span the affected lines.
    let new_first = rope.line_to_char(first_line);
    let new_last_end = char_at(rope, last_line, usize::MAX);
    st.anchor = new_first;
    st.cursor = new_last_end;
    st.goal_col = None;
}

/// Delete the entire line(s) the selection touches (including the trailing
/// newline), collapsing the caret to the start of the following line.
pub fn delete_line(rope: &mut Rope, st: &mut EditState) {
    let sel = st.selection();
    let first_line = rope.char_to_line(sel.start);
    let last_line = rope.char_to_line(sel.end.max(sel.start));
    let start = rope.line_to_char(first_line);
    let end = if last_line + 1 < rope.len_lines() {
        rope.line_to_char(last_line + 1)
    } else {
        rope.len_chars()
    };
    rope.remove(start..end);
    st.collapse_to(start.min(rope.len_chars()));
}

/// Replace the selection with `replacement` (same char length expected for a
/// case toggle), keeping it selected. No-op when collapsed.
pub fn replace_selection(rope: &mut Rope, st: &mut EditState, replacement: &str) {
    if !st.has_selection() {
        return;
    }
    let range = st.selection();
    rope.remove(range.clone());
    rope.insert(range.start, replacement);
    st.anchor = range.start;
    st.cursor = range.start + replacement.chars().count();
    st.goal_col = None;
}

/// Find the position of the bracket matching the `(`/`)`/`[`/`]`/`{`/`}` at
/// char index `pos`, or `None` when `pos` isn't on a bracket or no match
/// exists. Scans in the bracket's direction tracking nesting, capped at
/// `max_scan` chars so a huge file with an unmatched bracket can't stall.
pub fn matching_bracket(rope: &Rope, pos: usize, max_scan: usize) -> Option<usize> {
    if pos >= rope.len_chars() {
        return None;
    }
    let ch = rope.char(pos);
    const PAIRS: &[(char, char)] = &[('(', ')'), ('[', ']'), ('{', '}')];
    let (open, close, forward) = if let Some(p) = PAIRS.iter().find(|(o, _)| *o == ch) {
        (p.0, p.1, true)
    } else if let Some(p) = PAIRS.iter().find(|(_, c)| *c == ch) {
        (p.0, p.1, false)
    } else {
        return None;
    };
    let mut depth = 0i32;
    let mut steps = 0usize;
    if forward {
        let mut i = pos;
        let n = rope.len_chars();
        while i < n && steps < max_scan {
            let c = rope.char(i);
            if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            i += 1;
            steps += 1;
        }
    } else {
        let mut i = pos as isize;
        while i >= 0 && steps < max_scan {
            let c = rope.char(i as usize);
            if c == close {
                depth += 1;
            } else if c == open {
                depth -= 1;
                if depth == 0 {
                    return Some(i as usize);
                }
            }
            i -= 1;
            steps += 1;
        }
    }
    None
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

/// Default byte budget for the combined undo+redo snapshot text. A snapshot
/// holds the WHOLE buffer, so a count-only cap lets a 512-deep stack of a
/// multi-GiB buffer pin tens of GiB resident. The byte budget bounds that:
/// once the summed snapshot text exceeds it, the oldest checkpoints are evicted
/// even if the count cap is not yet reached. 64 MiB comfortably holds a deep
/// stack of ordinary source files while capping the pathological large-rope
/// case.
pub const DEFAULT_HISTORY_BYTE_BUDGET: usize = 64 * 1024 * 1024;

/// A bounded undo/redo history of [`Snapshot`]s. The caller records the
/// pre-edit snapshot after each edit; `undo`/`redo` swap snapshots with the
/// live state. Depth is bounded by BOTH a count cap (oldest dropped) AND a byte
/// budget over the summed snapshot text (oldest dropped) so editing a large
/// rope can't blow up resident memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Kind of the most recently recorded edit, for coalescing.
    last_kind: Option<EditKind>,
    cap: usize,
    /// Max summed bytes of `undo` + `redo` snapshot text. Older snapshots are
    /// evicted when this is exceeded. `#[serde(default)]` so histories
    /// persisted before this field existed still deserialize (defaulting to 0,
    /// which `byte_budget()` promotes to the named default).
    #[serde(default)]
    byte_budget: usize,
}

impl History {
    /// A new history holding up to `cap` undo checkpoints (`cap >= 1`) within
    /// the [`DEFAULT_HISTORY_BYTE_BUDGET`] byte budget.
    pub fn new(cap: usize) -> Self {
        Self::with_byte_budget(cap, DEFAULT_HISTORY_BYTE_BUDGET)
    }

    /// A new history with an explicit count cap AND byte budget (both `>= 1`).
    pub fn with_byte_budget(cap: usize, byte_budget: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            last_kind: None,
            cap: cap.max(1),
            byte_budget: byte_budget.max(1),
        }
    }

    /// The effective byte budget, promoting a 0 (legacy-deserialized) value to
    /// the named default.
    fn byte_budget(&self) -> usize {
        if self.byte_budget == 0 {
            DEFAULT_HISTORY_BYTE_BUDGET
        } else {
            self.byte_budget
        }
    }

    /// Summed bytes of every retained `undo` + `redo` snapshot's text.
    pub fn retained_bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(self.redo.iter())
            .map(|s| s.text.len())
            .sum()
    }

    /// Drop the oldest `undo` checkpoints until BOTH the count cap and byte
    /// budget hold. The `undo` front is the oldest history; the `redo` stack
    /// is short-lived (cleared on every `record`) so it is left intact and the
    /// most-recent checkpoint is always preserved.
    fn enforce_limits(&mut self) {
        while self.undo.len() > self.cap {
            self.undo.remove(0);
        }
        let budget = self.byte_budget();
        // Never evict the single most-recent undo checkpoint, even if it alone
        // exceeds the budget — losing it would silently drop the user's last
        // undo step. Evict only while there is more than one to trim.
        while self.undo.len() > 1 && self.retained_bytes() > budget {
            self.undo.remove(0);
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
        self.enforce_limits();
    }

    /// Undo: return the snapshot to restore, pushing `current` onto the redo
    /// stack. `None` when there is nothing to undo.
    pub fn undo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let prev = self.undo.pop()?;
        self.redo.push(current);
        self.last_kind = None; // the next edit starts a fresh group
        self.enforce_limits();
        Some(prev)
    }

    /// Redo: return the snapshot to restore, pushing `current` onto the undo
    /// stack. `None` when there is nothing to redo.
    pub fn redo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        self.last_kind = None;
        self.enforce_limits();
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
    fn skip_over_detects_matching_closer_at_caret() {
        let r = Rope::from_str("()");
        // caret between the two parens, typing ')' should skip over the ')'.
        assert!(should_skip_over(&r, 1, ')'));
        // caret at 0 (over '(') — not a closer position.
        assert!(!should_skip_over(&r, 0, ')'));
        // a non-closer char never skips.
        assert!(!should_skip_over(&r, 1, 'x'));
        // EOF guard.
        assert!(!should_skip_over(&r, 2, ')'));
    }

    #[test]
    fn word_bounds_spans_identifier() {
        let r = Rope::from_str("let foo_bar = 1");
        let (s, e) = word_bounds(&r, 6); // inside foo_bar
        assert_eq!((s, e), (4, 11));
    }

    #[test]
    fn add_next_occurrence_grows_caret_set_and_wraps() {
        let r = Rope::from_str("foo bar foo baz foo");
        // primary selects the first "foo" (0..3).
        let mut p = EditState::at(3);
        p.anchor = 0;
        let mut carets = vec![p];
        assert!(add_next_occurrence(&r, &mut carets)); // second foo @ 8..11
        assert_eq!(carets.len(), 2);
        assert!(add_next_occurrence(&r, &mut carets)); // third foo @ 16..19
        assert_eq!(carets.len(), 3);
        // Fourth press wraps but every occurrence is taken → no-op.
        assert!(!add_next_occurrence(&r, &mut carets));
        assert_eq!(carets.len(), 3);
    }

    #[test]
    fn add_next_occurrence_requires_a_selection() {
        let r = Rope::from_str("foo foo");
        let mut carets = vec![EditState::at(0)]; // collapsed caret
        assert!(!add_next_occurrence(&r, &mut carets));
    }

    #[test]
    fn block_selection_one_caret_per_row_clamped() {
        let r = Rope::from_str("hello\nhi\nworld\n");
        // columns 2..4 across rows 0..2.
        let carets = block_selection(&r, (0, 2), (2, 4));
        assert_eq!(carets.len(), 3);
        // row "hi" (len 2) clamps both ends to 2 → collapsed caret at col 2.
        let hi = carets[1].selection();
        assert_eq!(hi.start, hi.end);
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
    fn backspace_with_stale_cursor_over_empty_rope_is_noop() {
        // A `cursor > 0` restored from a session manifest over a now-empty rope:
        // the old guard checked the RAW cursor, so `to = clamp(..) = 0` then
        // `to - 1` underflowed → `rope.remove(usize::MAX..0)` aborted the editor.
        let mut r = rope("");
        let mut st = EditState::at(5);
        backspace(&mut r, &mut st); // must not panic
        assert_eq!(r.to_string(), "");
        assert_eq!(st.cursor, 0);
    }

    #[test]
    fn for_each_caret_clamps_stale_caret_past_end() {
        // A stale multi-cursor selection whose range exceeds the live length:
        // the low-only `.max(0)` clamp left the high end past `len_chars`, so a
        // delete-class op handed `rope.remove` an out-of-range range → abort.
        let mut r = rope("ab");
        let mut carets = vec![EditState {
            anchor: 5,
            cursor: 9,
            goal_col: None,
        }];
        for_each_caret(&mut r, &mut carets, |rope, st| {
            delete_selection(rope, st);
        }); // must not panic
        assert_eq!(r.to_string(), "ab");
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
    fn history_respects_byte_budget() {
        // A 1 KiB budget with a generous count cap: pushing many large
        // snapshots must evict oldest so retained bytes stay under budget,
        // while the most-recent checkpoint is preserved.
        let budget = 1024;
        let mut h = History::with_byte_budget(1000, budget);
        let big = "x".repeat(400); // 400 bytes each
        for i in 0..20 {
            // Distinct kinds so nothing coalesces — every push is a checkpoint.
            h.record(Snapshot::new(format!("{big}{i}"), 0), EditKind::Other);
        }
        // Count cap (1000) never reached, but the byte budget bounds retention.
        assert!(
            h.retained_bytes() <= budget,
            "retained {} > budget {budget}",
            h.retained_bytes()
        );
        // The most-recent checkpoint survives: undoing returns the latest text.
        let restored = h.undo(Snapshot::new("cur", 0)).unwrap();
        assert!(
            restored.text.ends_with("19"),
            "most-recent checkpoint must be preserved, got {:?}",
            restored.text
        );
    }

    #[test]
    fn history_byte_budget_enforced_on_redo_push() {
        // The redo() path pushes `current` onto the undo stack; that push must
        // also honour the budget (the bug was that only record() trimmed).
        let budget = 1024;
        let mut h = History::with_byte_budget(1000, budget);
        let big = "y".repeat(400);
        // Build a few checkpoints, then undo once to populate redo.
        for i in 0..3 {
            h.record(Snapshot::new(format!("{big}{i}"), 0), EditKind::Other);
        }
        h.undo(Snapshot::new(format!("{big}cur"), 0));
        // Redo pushes a 400+ byte `current` back onto undo; budget still holds.
        h.redo(Snapshot::new(format!("{big}redoing"), 0));
        assert!(
            h.retained_bytes() <= budget,
            "redo push must respect the byte budget: {} > {budget}",
            h.retained_bytes()
        );
    }

    #[test]
    fn history_default_byte_budget_is_named_const() {
        let h = History::default();
        // The default-constructed history uses the named 64 MiB budget.
        assert_eq!(super::DEFAULT_HISTORY_BYTE_BUDGET, 64 * 1024 * 1024);
        // Empty history retains zero bytes.
        assert_eq!(h.retained_bytes(), 0);
    }

    #[test]
    fn history_serde_roundtrips() {
        let mut h = History::new(4);
        h.record(Snapshot::new("hello", 5), EditKind::Other);
        let json = serde_json::to_string(&h).unwrap();
        let back: History = serde_json::from_str(&json).unwrap();
        assert!(back.can_undo());
    }

    // ---- Multi-cursor (F-009) ----

    #[test]
    fn for_each_caret_inserts_at_every_caret_with_offset() {
        // Two carets in "abXcdX" style: insert "!" at col 0 of each line.
        let mut r = rope("ab\ncd\n");
        // caret at line0 col0 (idx 0) and line1 col0 (idx 3).
        let mut carets = vec![EditState::at(0), EditState::at(3)];
        for_each_caret(&mut r, &mut carets, |rope, st| {
            insert(rope, st, "!");
        });
        // Both lines gain a leading '!'. The second caret's index shifted by
        // the first insert (offset management).
        assert_eq!(r.to_string(), "!ab\n!cd\n");
        // Carets sit just after each inserted '!'.
        assert_eq!(
            carets.iter().map(|c| c.cursor).collect::<Vec<_>>(),
            vec![1, 5]
        );
    }

    #[test]
    fn for_each_caret_backspace_offsets_correctly() {
        let mut r = rope("aXbX");
        // Carets after each 'X' (idx 2 and idx 4).
        let mut carets = vec![EditState::at(2), EditState::at(4)];
        for_each_caret(&mut r, &mut carets, |rope, st| {
            backspace(rope, st);
        });
        // Each backspace removes the preceding 'X'.
        assert_eq!(r.to_string(), "ab");
        assert_eq!(
            carets.iter().map(|c| c.cursor).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn dedupe_carets_merges_coincident() {
        let mut carets = vec![EditState::at(3), EditState::at(1), EditState::at(3)];
        dedupe_carets(&mut carets);
        assert_eq!(
            carets.iter().map(|c| c.cursor).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    /// Build a caret whose selection is `[start, end)` with the cursor on the
    /// high end (forward selection).
    fn sel(start: usize, end: usize) -> EditState {
        EditState {
            anchor: start,
            cursor: end,
            goal_col: None,
        }
    }

    /// Reference: apply `f` once to a SINGLE caret covering `[start, end)` and
    /// return the resulting rope string. This is the ground truth a set of
    /// overlapping carets covering the same union region must reproduce.
    fn single_pass(text: &str, start: usize, end: usize, replacement: &str) -> String {
        let mut r = rope(text);
        let mut st = sel(start, end);
        replace_selection(&mut r, &mut st, replacement);
        r.to_string()
    }

    #[test]
    fn for_each_caret_overlapping_selections_edit_once_not_twice() {
        // C-02 / R1 root-cause proof. Two carets whose selections OVERLAP but
        // are not identical: A = [0,4) "abcd", B = [3,7) "defg", overlap [3,4).
        // The union of the two selections is [0,7) "abcdefg". A single-caret
        // replace of that union with "<<<" yields "<<<h". Because the carets
        // overlap, the correct multi-caret behaviour is to merge them into one
        // edit of the union region and apply the replacement exactly ONCE.
        //
        // Before the fix `for_each_caret` dispatched to BOTH carets: caret A
        // edits, the offset drags caret B's clamped range over text caret A
        // already wrote, and the replacement is applied a SECOND time to the
        // shared region — producing "<<<<<<h" (the "<<<" doubled).
        let union_once = single_pass("abcdefgh", 0, 7, "<<<");
        assert_eq!(union_once, "<<<h", "single-pass oracle");

        let mut r = rope("abcdefgh");
        let mut carets = vec![sel(0, 4), sel(3, 7)];
        for_each_caret(&mut r, &mut carets, |rope, st| {
            replace_selection(rope, st, "<<<");
        });
        // The edit must be applied exactly once over the merged union region.
        assert_eq!(
            r.to_string(),
            union_once,
            "overlapping carets must edit the union region exactly once, not twice"
        );
        // The overlapping pair collapses to a single caret.
        assert_eq!(carets.len(), 1, "overlapping carets merge to one");
    }

    #[test]
    fn normalize_carets_merges_nested_selection() {
        // B = [2,4) is fully nested inside A = [0,6). Union is just A.
        let mut carets = vec![sel(0, 6), sel(2, 4)];
        normalize_carets(&mut carets);
        assert_eq!(carets.len(), 1);
        assert_eq!(carets[0].selection(), 0..6);
    }

    #[test]
    fn normalize_carets_merges_adjacent_touching_selections() {
        // A = [0,3), B = [3,6): touching at index 3. Merge to one [0,6) so the
        // boundary char is never double-claimed.
        let mut carets = vec![sel(0, 3), sel(3, 6)];
        normalize_carets(&mut carets);
        assert_eq!(carets.len(), 1);
        assert_eq!(carets[0].selection(), 0..6);
    }

    #[test]
    fn normalize_carets_keeps_disjoint_selections_separate() {
        // A = [0,2), B = [4,6): a one-char gap at [2,4). They must NOT merge.
        let mut carets = vec![sel(0, 2), sel(4, 6)];
        normalize_carets(&mut carets);
        assert_eq!(carets.len(), 2);
        assert_eq!(carets[0].selection(), 0..2);
        assert_eq!(carets[1].selection(), 4..6);
    }

    #[test]
    fn normalize_carets_distinct_collapsed_carets_stay_separate() {
        // Two zero-width carets at different positions are independent
        // multi-cursors and must survive normalization unchanged.
        let mut carets = vec![EditState::at(1), EditState::at(5)];
        normalize_carets(&mut carets);
        assert_eq!(carets.len(), 2);
        assert_eq!(
            carets.iter().map(|c| c.cursor).collect::<Vec<_>>(),
            vec![1, 5]
        );
    }

    #[test]
    fn normalize_carets_coincident_collapsed_carets_merge() {
        let mut carets = vec![EditState::at(3), EditState::at(3)];
        normalize_carets(&mut carets);
        assert_eq!(carets.len(), 1);
        assert_eq!(carets[0].cursor, 3);
    }

    #[test]
    fn normalize_carets_preserves_reversed_primary_direction() {
        // Primary (start-sorted first) is reversed: anchor=4 > cursor=0,
        // selecting [0,4). It overlaps a forward B = [2,6). The merged caret
        // keeps the primary's reversed orientation: anchor at union end (6),
        // cursor at union start (0).
        let mut carets = vec![
            EditState {
                anchor: 4,
                cursor: 0,
                goal_col: None,
            },
            sel(2, 6),
        ];
        normalize_carets(&mut carets);
        assert_eq!(carets.len(), 1);
        assert_eq!(carets[0].selection(), 0..6);
        assert!(
            carets[0].anchor > carets[0].cursor,
            "reversed orientation kept"
        );
        assert_eq!(carets[0].anchor, 6);
        assert_eq!(carets[0].cursor, 0);
    }

    #[test]
    fn for_each_caret_many_overlapping_carets_edit_once() {
        // A chain of mutually-overlapping selections over "abcdefghij" (len 10):
        // [0,3) [2,5) [4,7) [6,9) — each overlaps the next. Union is [0,9).
        let mut r = rope("abcdefghij");
        let mut carets = vec![sel(0, 3), sel(2, 5), sel(4, 7), sel(6, 9)];
        for_each_caret(&mut r, &mut carets, |rope, st| {
            replace_selection(rope, st, "#");
        });
        assert_eq!(r.to_string(), single_pass("abcdefghij", 0, 9, "#"));
        assert_eq!(r.to_string(), "#j");
        assert_eq!(carets.len(), 1);
    }

    #[test]
    fn for_each_caret_disjoint_carets_unchanged_by_normalize() {
        // Two disjoint selections each get the edit applied independently — the
        // exact pre-fix behaviour for non-overlapping carets, proving the
        // offset bookkeeping is untouched for the disjoint case.
        // "aXXbYYc": A = [1,3) "XX", B = [4,6) "YY".
        let mut r = rope("aXXbYYc");
        let mut carets = vec![sel(1, 3), sel(4, 6)];
        for_each_caret(&mut r, &mut carets, |rope, st| {
            replace_selection(rope, st, "_");
        });
        assert_eq!(r.to_string(), "a_b_c");
        assert_eq!(carets.len(), 2);
    }

    #[test]
    fn for_each_caret_reversed_overlapping_selections_edit_once() {
        // Both carets reversed (anchor > cursor) but overlapping. Result must
        // still equal the single-pass union edit, applied once.
        let mut r = rope("abcdefgh");
        let mut carets = vec![
            EditState {
                anchor: 4,
                cursor: 0,
                goal_col: None,
            },
            EditState {
                anchor: 7,
                cursor: 3,
                goal_col: None,
            },
        ];
        for_each_caret(&mut r, &mut carets, |rope, st| {
            replace_selection(rope, st, "<<<");
        });
        assert_eq!(r.to_string(), single_pass("abcdefgh", 0, 7, "<<<"));
        assert_eq!(carets.len(), 1);
    }

    #[test]
    fn add_caret_vertical_below_keeps_column() {
        let r = rope("abcd\nefgh\nijkl\n");
        let mut carets = vec![EditState::at(2)]; // line 0, col 2
        assert!(add_caret_vertical(&r, &mut carets, 1));
        // New caret on line 1 col 2 → idx 7.
        assert!(carets.iter().any(|c| line_col(&r, c.cursor) == (1, 2)));
        assert_eq!(carets.len(), 2);
    }

    #[test]
    fn add_caret_vertical_clamps_at_buffer_edge() {
        let r = rope("only\n");
        let mut carets = vec![EditState::at(0)]; // line 0
                                                 // No line above → no caret added.
        assert!(!add_caret_vertical(&r, &mut carets, -1));
        assert_eq!(carets.len(), 1);
    }

    // ---- editing power-features ----

    #[test]
    fn closing_for_pairs() {
        assert_eq!(closing_for('('), Some(')'));
        assert_eq!(closing_for('['), Some(']'));
        assert_eq!(closing_for('{'), Some('}'));
        assert_eq!(closing_for('"'), Some('"'));
        assert_eq!(closing_for('x'), None);
    }

    #[test]
    fn leading_whitespace_of_line() {
        let r = rope("    indented\nflush\n");
        assert_eq!(leading_whitespace(&r, 6), "    "); // inside line 0
        assert_eq!(leading_whitespace(&r, 13), ""); // line 1 (flush)
    }

    #[test]
    fn indent_lines_indents_selection() {
        let mut r = rope("a\nb\nc\n");
        // Select lines 0..2 (chars 0..4 spans line0+line1).
        let mut st = EditState {
            anchor: 0,
            cursor: 3,
            goal_col: None,
        };
        indent_lines(&mut r, &mut st, "  ", false);
        assert_eq!(r.to_string(), "  a\n  b\nc\n");
    }

    #[test]
    fn indent_lines_outdents() {
        let mut r = rope("    a\n    b\n");
        let mut st = EditState {
            anchor: 0,
            cursor: 6,
            goal_col: None,
        };
        indent_lines(&mut r, &mut st, "  ", true);
        assert_eq!(r.to_string(), "  a\n  b\n");
    }

    #[test]
    fn delete_line_removes_whole_line() {
        let mut r = rope("a\nb\nc\n");
        let mut st = EditState::at(2); // on line 1 ("b")
        delete_line(&mut r, &mut st);
        assert_eq!(r.to_string(), "a\nc\n");
    }

    #[test]
    fn matching_bracket_forward_and_back() {
        let r = rope("a(b[c]d)e");
        // '(' at idx 1 matches ')' at idx 7.
        assert_eq!(matching_bracket(&r, 1, 1000), Some(7));
        // ')' at idx 7 matches '(' at idx 1.
        assert_eq!(matching_bracket(&r, 7, 1000), Some(1));
        // '[' at idx 3 matches ']' at idx 5.
        assert_eq!(matching_bracket(&r, 3, 1000), Some(5));
        // 'a' at idx 0 is not a bracket.
        assert_eq!(matching_bracket(&r, 0, 1000), None);
    }

    #[test]
    fn matching_bracket_unmatched_is_none() {
        let r = rope("(((");
        assert_eq!(matching_bracket(&r, 0, 1000), None);
    }

    #[test]
    fn replace_selection_swaps_and_reselects() {
        let mut r = rope("abcd");
        let mut st = EditState {
            anchor: 1,
            cursor: 3,
            goal_col: None,
        };
        replace_selection(&mut r, &mut st, "XY");
        assert_eq!(r.to_string(), "aXYd");
        assert_eq!(st.selection(), 1..3);
    }
}

#[cfg(test)]
mod proptests {
    //! Property-based invariants for the pure caret/selection transforms — the
    //! correctness backbone of the editor. These assert the laws that must hold
    //! for ANY input, complementing the example-based tests above.
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// `word_bounds` always returns a range that contains the caret and
        /// stays within the buffer.
        #[test]
        fn word_bounds_contains_caret_and_in_bounds(s in ".*", c in 0usize..80) {
            let r = Rope::from_str(&s);
            let n = r.len_chars();
            let c = c.min(n);
            let (lo, hi) = word_bounds(&r, c);
            prop_assert!(lo <= c, "lo {lo} <= c {c}");
            prop_assert!(c <= hi, "c {c} <= hi {hi}");
            prop_assert!(hi <= n, "hi {hi} <= n {n}");
        }

        /// `should_skip_over` never panics and only ever returns true for a
        /// closer sitting under the caret.
        #[test]
        fn should_skip_over_is_total(s in ".*", c in 0usize..80, ch in any::<char>()) {
            let r = Rope::from_str(&s);
            let c = c.min(r.len_chars());
            if should_skip_over(&r, c, ch) {
                prop_assert!(c < r.len_chars());
                prop_assert_eq!(r.char(c), ch);
            }
        }

        /// Block selection produces carets that stay within the buffer.
        #[test]
        fn block_selection_carets_in_bounds(
            s in ".*",
            l0 in 0usize..12, c0 in 0usize..30,
            l1 in 0usize..12, c1 in 0usize..30,
        ) {
            let r = Rope::from_str(&s);
            let n = r.len_chars();
            for cur in block_selection(&r, (l0, c0), (l1, c1)) {
                prop_assert!(cur.cursor <= n);
                prop_assert!(cur.anchor <= n);
            }
        }

        /// `dedupe_carets` is idempotent — running it twice equals running once.
        #[test]
        fn dedupe_carets_idempotent(positions in prop::collection::vec(0usize..120, 0..24)) {
            let mut carets: Vec<EditState> = positions.iter().map(|&p| EditState::at(p)).collect();
            dedupe_carets(&mut carets);
            let once = carets.clone();
            dedupe_carets(&mut carets);
            prop_assert_eq!(once, carets);
        }

        /// `add_next_occurrence` never moves a caret outside the buffer and never
        /// shrinks the caret set.
        #[test]
        fn add_next_occurrence_keeps_carets_in_bounds(s in ".*", a in 0usize..80, b in 0usize..80) {
            let r = Rope::from_str(&s);
            let n = r.len_chars();
            let (lo, hi) = (a.min(n), b.min(n));
            let mut p = EditState::at(hi.max(lo));
            p.anchor = lo.min(hi);
            let before = vec![p];
            let mut carets = before.clone();
            let _ = add_next_occurrence(&r, &mut carets);
            prop_assert!(carets.len() >= before.len());
            for cur in &carets {
                prop_assert!(cur.cursor <= n && cur.anchor <= n);
            }
        }

        /// After `normalize_carets`, the resulting selection intervals are
        /// pairwise NON-touching (each next.start strictly greater than the
        /// previous end) and their union (set of covered char indices) equals
        /// the union of the input selections. This is the interval-merge law
        /// that guarantees no two carets can claim a shared char.
        #[test]
        fn normalize_carets_yields_disjoint_intervals_covering_same_union(
            spans in prop::collection::vec((0usize..40, 0usize..40), 0..16),
        ) {
            let input: Vec<EditState> = spans
                .iter()
                .map(|&(a, b)| EditState { anchor: a.min(b), cursor: a.max(b), goal_col: None })
                .collect();
            // Reference union as a set of covered indices.
            let mut covered = std::collections::BTreeSet::new();
            for c in &input {
                let r = c.selection();
                covered.extend(r.start..r.end);
            }

            let mut carets = input.clone();
            normalize_carets(&mut carets);

            // Merged intervals are pairwise disjoint AND non-touching.
            let mut sels: Vec<std::ops::Range<usize>> =
                carets.iter().map(|c| c.selection()).collect();
            sels.sort_by_key(|r| (r.start, r.end));
            for w in sels.windows(2) {
                // Non-empty selections must not touch; collapsed carets at
                // distinct positions are allowed (they cover no chars).
                if !w[0].is_empty() && !w[1].is_empty() {
                    prop_assert!(w[1].start > w[0].end,
                        "intervals {:?} and {:?} touch/overlap", w[0], w[1]);
                }
            }

            // Union of merged intervals equals the input union.
            let mut merged_covered = std::collections::BTreeSet::new();
            for r in &sels {
                merged_covered.extend(r.start..r.end);
            }
            prop_assert_eq!(merged_covered, covered);
        }

        /// THE C-02 INVARIANT: applying a destructive edit through
        /// `for_each_caret` over ANY set of carets equals deleting the merged
        /// union intervals exactly once (single-pass oracle). Overlapping
        /// carets can never double-edit a shared region.
        #[test]
        fn for_each_caret_delete_equals_single_pass_over_merged_intervals(
            // 12-char fixed buffer so spans are always in range.
            spans in prop::collection::vec((0usize..12, 0usize..12), 0..8),
        ) {
            const TEXT: &str = "abcdefghijkl";

            // Subject: dispatch delete_selection through for_each_caret.
            let mut subject = Rope::from_str(TEXT);
            let mut carets: Vec<EditState> = spans
                .iter()
                .map(|&(a, b)| EditState { anchor: a.min(b), cursor: a.max(b), goal_col: None })
                .collect();
            for_each_caret(&mut subject, &mut carets, |rope, st| {
                delete_selection(rope, st);
            });

            // Oracle: compute the merged intervals independently, then delete
            // them from the original buffer from RIGHT to LEFT (so earlier
            // deletions don't shift later indices) — each region deleted once.
            let mut intervals: Vec<(usize, usize)> = spans
                .iter()
                .map(|&(a, b)| (a.min(b), a.max(b)))
                .filter(|(s, e)| e > s)
                .collect();
            intervals.sort();
            let mut merged: Vec<(usize, usize)> = Vec::new();
            for (s, e) in intervals {
                match merged.last_mut() {
                    Some(last) if s <= last.1 => last.1 = last.1.max(e),
                    _ => merged.push((s, e)),
                }
            }
            let mut oracle = Rope::from_str(TEXT);
            for (s, e) in merged.into_iter().rev() {
                oracle.remove(s..e);
            }

            prop_assert_eq!(subject.to_string(), oracle.to_string());
        }
    }
}
