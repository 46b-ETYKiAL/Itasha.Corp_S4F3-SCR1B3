//! P2 structural multi-selection model for the central egui `TextEdit`.
//!
//! egui's `TextEdit::multiline` is a **single-selection** primitive (its
//! `TextEditState` holds exactly one `CCursorRange`), so multi-cursor,
//! select-next-occurrence, and rectangular (column) selection cannot be
//! expressed inside it. Rather than replace the shipping egui editor (and the
//! drag-scroll conveniences all wired to it), this module keeps egui's caret as
//! the **primary** and maintains an app-side list of **secondary** carets over
//! the same editor `String`. On an edit key the app intercepts the event,
//! replays it at the primary *and* every secondary against the buffer, and
//! writes the moved primary back into egui's state — so the shipping editor is
//! never replaced, only augmented.
//!
//! NB: this is a distinct surface from the *experimental* owned rope editor
//! (`scribe_render::RopeEditorState`), which has its own Ctrl+Alt+Arrow vertical
//! multi-caret. This model layers Ctrl/Cmd+click, Ctrl+D select-next, and
//! Alt+drag column selection onto the *default* egui-`TextEdit` path.
//!
//! The model is pure and char-index based (egui's `CCursor` is a char index):
//!
//! * [`Caret`] — a selection over char offsets (`anchor` fixed, `head` moving);
//!   a bare caret has `anchor == head`.
//! * [`MultiCursor::apply_edit`] — replay an [`EditOp`] (insert / backspace /
//!   delete) at the primary + all secondaries in one buffer mutation, returning
//!   the primary's new position and leaving the secondaries at their new spots.
//! * [`MultiCursor::select_next_occurrence`] — Ctrl+D: grow the match set to the
//!   next occurrence of the primary selection (reusing [`scribe_core::search`]).
//! * [`column_selection`] — Alt+drag: build one per-line caret across a
//!   rectangular region.
//!
//! All buffer mutation goes through [`apply_edits_to_string`], which applies the
//! per-caret edits **right-to-left** so the left edits' byte offsets stay valid.

use std::ops::Range;

use scribe_core::search::{find_all, Query};

/// A caret / selection over char offsets into the editor buffer. `anchor` is the
/// fixed end, `head` the moving end (where the caret blinks). A bare caret has
/// `anchor == head`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caret {
    pub anchor: usize,
    pub head: usize,
}

impl Caret {
    /// A bare (zero-width) caret at `pos`.
    pub fn at(pos: usize) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
    }

    /// A selection with the given fixed `anchor` and moving `head`.
    pub fn selection(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    /// The sorted `[min, max)` char range this caret covers (empty when bare).
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    /// The lower char offset of the selection (its start).
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// True when this is a bare caret (no selected text).
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// One editing operation replayed at every caret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditOp {
    /// Insert the given text (replacing each caret's selection). `\n` for Enter.
    Insert(String),
    /// Backspace: delete the selection, or one char before a bare caret.
    Backspace,
    /// Forward delete: delete the selection, or one char after a bare caret.
    Delete,
}

/// Result of a Ctrl+D gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtrlDOutcome {
    /// The primary caret was bare: the caller should first *select the word*
    /// under it (this range) — no secondary is added yet (VS Code's first Ctrl+D).
    SelectWord { start: usize, end: usize },
    /// The next occurrence was added as this secondary selection.
    Added(Caret),
    /// No further occurrence (or nothing to match) — the set is unchanged.
    NoMatch,
}

/// The app-side secondary-caret set layered over egui's primary caret.
#[derive(Debug, Clone, Default)]
pub struct MultiCursor {
    /// Extra carets/selections *beyond* egui's primary. Kept sorted ascending by
    /// start and de-duplicated; never overlapping in normal use.
    secondaries: Vec<Caret>,
    /// Whether multi-cursor mode is engaged (Esc / [`clear`](Self::clear) resets).
    active: bool,
}

impl MultiCursor {
    /// True while multi-cursor mode is engaged *and* at least one secondary
    /// caret exists — the app only intercepts edits in this state.
    pub fn is_active(&self) -> bool {
        self.active && !self.secondaries.is_empty()
    }

    /// The secondary carets (everything beyond egui's primary).
    pub fn secondaries(&self) -> &[Caret] {
        &self.secondaries
    }

    /// Collapse to a single caret: drop all secondaries and disengage (Esc).
    pub fn clear(&mut self) {
        self.secondaries.clear();
        self.active = false;
    }

    /// Add one secondary caret, keeping the set sorted + de-duplicated, and
    /// engage multi-cursor mode.
    pub fn add_caret(&mut self, c: Caret) {
        self.active = true;
        if self.secondaries.iter().any(|s| s == &c) {
            return;
        }
        self.secondaries.push(c);
        self.normalize();
    }

    /// Ctrl/Cmd+click entry — VS Code toggle semantics. Reconcile `c` against the
    /// current caret set (`primary` + secondaries) instead of blindly adding it:
    ///
    /// * coincident with the **primary** → no-op (never a coincident duplicate;
    ///   at least the primary always survives);
    /// * lands on an existing **secondary** (coincident bare, or inside its
    ///   selection) → remove that secondary (toggle off) — collapsing to just the
    ///   primary when it was the last one;
    /// * otherwise → add it as a new secondary.
    ///
    /// This is the caller-side half of the non-overlap contract: it keeps the
    /// visible caret set free of coincident/nested carets. [`apply_edit`] still
    /// reconciles defensively (the primary can be *navigated* onto a secondary
    /// after this runs), so corruption is impossible regardless of gesture order.
    pub fn toggle_caret(&mut self, c: Caret, primary: Caret) {
        self.active = true;
        // Dedup against the primary — clicking it never adds a duplicate.
        if caret_hits(&primary, c.head) {
            return;
        }
        // Toggle-remove a coincident / covering secondary.
        if let Some(pos) = self.secondaries.iter().position(|s| caret_hits(s, c.head)) {
            self.secondaries.remove(pos);
            if self.secondaries.is_empty() {
                // Nothing left but the primary — collapse out of multi-cursor mode.
                self.active = false;
            }
            return;
        }
        self.secondaries.push(c);
        self.normalize();
    }

    /// Replace the secondary set wholesale (used by column selection, where the
    /// caller designates one caret as egui's primary and the rest as secondaries)
    /// and engage multi-cursor mode.
    pub fn set_secondaries(&mut self, carets: Vec<Caret>) {
        self.secondaries = carets;
        self.normalize();
        self.active = !self.secondaries.is_empty();
    }

    fn normalize(&mut self) {
        self.secondaries.sort_by_key(|c| (c.start(), c.range().end));
        self.secondaries.dedup();
    }

    /// Replay `op` at the primary caret **and** every secondary, mutating `text`
    /// in one pass. Returns the primary's new position (a bare caret index) for
    /// egui to adopt; the secondaries are advanced to their new positions.
    ///
    /// All carets collapse to bare carets after an edit (their prior selection,
    /// if any, was consumed) — matching how a keystroke behaves at each cursor.
    pub fn apply_edit(&mut self, text: &mut String, primary: Caret, op: EditOp) -> usize {
        let total = text.chars().count();

        // Build the merged, tagged caret list (primary flagged so we can recover
        // its new position after the shift recompute).
        let mut merged: Vec<(Caret, bool)> = Vec::with_capacity(self.secondaries.len() + 1);
        merged.push((primary, true));
        for &s in &self.secondaries {
            merged.push((s, false));
        }
        // ENFORCE apply_edits_to_string's "must be non-overlapping" precondition:
        // reconcile the full set — drop carets coincident with or nested inside
        // another's range (keeping the primary). This closes the coincident
        // double-insert and the nested-caret overlapping-splice bugs regardless
        // of how the gesture layer assembled the carets.
        let merged = reconcile_carets(merged);

        // Per-caret (delete-range, inserted-text).
        let ins: &str = match &op {
            EditOp::Insert(s) => s.as_str(),
            _ => "",
        };
        let ins_chars = ins.chars().count();

        let mut edits: Vec<(Range<usize>, bool)> = Vec::with_capacity(merged.len());
        for (c, is_primary) in &merged {
            let r = c.range();
            let del: Range<usize> = if !r.is_empty() {
                // A non-empty selection is always deleted first (then insert).
                r
            } else {
                match op {
                    EditOp::Backspace => {
                        if c.head > 0 {
                            c.head - 1..c.head
                        } else {
                            c.head..c.head
                        }
                    }
                    EditOp::Delete => {
                        if c.head < total {
                            c.head..c.head + 1
                        } else {
                            c.head..c.head
                        }
                    }
                    EditOp::Insert(_) => c.head..c.head,
                }
            };
            edits.push((del, *is_primary));
        }

        // Skip fully no-op edits (bare caret backspace at 0 / delete at EOF with
        // no insert) so they don't spuriously shift siblings.
        let string_edits: Vec<(Range<usize>, &str)> = edits
            .iter()
            .filter(|(d, _)| !(d.is_empty() && ins.is_empty()))
            .map(|(d, _)| (d.clone(), ins))
            .collect();
        apply_edits_to_string(text, &string_edits);

        // Recompute every caret's new position left-to-right with a running shift.
        let mut shift: isize = 0;
        let mut new_primary = primary.head;
        let mut new_secondaries: Vec<Caret> = Vec::with_capacity(merged.len());
        for (del, is_primary) in &edits {
            let applied = !(del.is_empty() && ins.is_empty());
            let new_pos = (del.start as isize + shift) as usize + ins_chars;
            if applied {
                shift += ins_chars as isize - (del.end - del.start) as isize;
            }
            if *is_primary {
                new_primary = new_pos;
            } else {
                new_secondaries.push(Caret::at(new_pos));
            }
        }

        // Drop any secondary that now coincides with the primary or a sibling.
        new_secondaries.retain(|c| c.head != new_primary);
        new_secondaries.sort_by_key(|c| c.head);
        new_secondaries.dedup();
        self.secondaries = new_secondaries;

        new_primary
    }

    /// Ctrl+D. When the primary caret is bare, returns [`CtrlDOutcome::SelectWord`]
    /// (the caller selects the word first). When the primary already selects text,
    /// finds the next occurrence of that text — scanning forward from the furthest
    /// caret and wrapping — that is not already occupied, adds it as a secondary,
    /// and returns [`CtrlDOutcome::Added`]. Reuses [`scribe_core::search::find_all`].
    pub fn select_next_occurrence(&mut self, text: &str, primary: Caret) -> CtrlDOutcome {
        let chars: Vec<char> = text.chars().collect();
        // Clamp the (possibly stale/wider-than-buffer) egui selection to the
        // current char vec before slicing so a shrunk buffer cannot index OOB.
        let sel = {
            let r = primary.range();
            r.start.min(chars.len())..r.end.min(chars.len())
        };

        if sel.is_empty() {
            let (start, end) = word_bounds_chars(&chars, primary.head);
            if start == end {
                return CtrlDOutcome::NoMatch;
            }
            return CtrlDOutcome::SelectWord { start, end };
        }

        let term: String = chars[sel.clone()].iter().collect();
        // Use whole-word matching when the seed is exactly a word run (the
        // SelectWord path, or a click that selected a whole word) — VS Code
        // semantics. An explicit sub-word selection still grows by substring.
        let whole_word = {
            let (ws, we) = word_bounds_chars(&chars, sel.start);
            ws == sel.start && we == sel.end && chars[sel.clone()].iter().all(|&c| is_word_char(c))
        };
        let q = Query {
            pattern: term,
            regex: false,
            case_sensitive: true,
            whole_word,
        };
        let byte_matches = match find_all(text, &q) {
            Ok(m) => m,
            Err(_) => return CtrlDOutcome::NoMatch,
        };
        if byte_matches.is_empty() {
            return CtrlDOutcome::NoMatch;
        }

        // Convert byte spans → char spans (matches are ascending, so walk once).
        let mut char_matches: Vec<Range<usize>> = Vec::with_capacity(byte_matches.len());
        for m in &byte_matches {
            let cs = text[..m.start].chars().count();
            let ce = text[..m.end].chars().count();
            char_matches.push(cs..ce);
        }

        // Ranges already covered by a caret (primary + secondaries).
        let mut occupied: Vec<Range<usize>> = Vec::with_capacity(self.secondaries.len() + 1);
        occupied.push(sel.clone());
        for s in &self.secondaries {
            occupied.push(s.range());
        }
        let is_occupied = |r: &Range<usize>| occupied.iter().any(|o| o.start == r.start);

        // The scan cursor: just past the furthest occupied end.
        let cursor = occupied.iter().map(|o| o.end).max().unwrap_or(sel.end);

        // First unoccupied match at/after the cursor, else wrap to the first
        // unoccupied match anywhere.
        let next = char_matches
            .iter()
            .find(|r| r.start >= cursor && !is_occupied(r))
            .or_else(|| char_matches.iter().find(|r| !is_occupied(r)));

        match next {
            Some(r) => {
                let c = Caret::selection(r.start, r.end);
                self.add_caret(c);
                CtrlDOutcome::Added(c)
            }
            None => CtrlDOutcome::NoMatch,
        }
    }
}

/// True when char `c` is part of an identifier-like word (`[A-Za-z0-9_]`). Kept
/// in sync with `scribe_core::editing::word_bounds`'s definition.
fn is_word_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// `(start, end)` char indices of the identifier-like word containing `cursor`
/// over a char slice; `(cursor, cursor)` when not on a word. A char-slice twin of
/// `scribe_core::editing::word_bounds` (which operates on a `Rope`), used so this
/// model stays egui- and rope-free and purely unit-testable.
fn word_bounds_chars(chars: &[char], cursor: usize) -> (usize, usize) {
    let n = chars.len();
    let mut s = cursor.min(n);
    while s > 0 && is_word_char(chars[s - 1]) {
        s -= 1;
    }
    let mut e = cursor.min(n);
    while e < n && is_word_char(chars[e]) {
        e += 1;
    }
    (s, e)
}

/// True when char offset `pos` lands "on" caret `c`: coincident with a bare
/// caret, or anywhere within (inclusive) a selection caret's range. Used by
/// [`MultiCursor::toggle_caret`] to decide add-vs-remove.
fn caret_hits(c: &Caret, pos: usize) -> bool {
    let r = c.range();
    if r.is_empty() {
        c.head == pos
    } else {
        r.start <= pos && pos <= r.end
    }
}

/// True when caret `b` (sorted at or after `a` by start) coincides with, is
/// nested inside, or otherwise overlaps `a`. Adjacent selections (`b.start ==
/// a.range().end`, e.g. non-overlapping Ctrl+D matches like `0..2` / `2..4`) do
/// NOT conflict and are both kept.
fn carets_conflict(a: &Caret, b: &Caret) -> bool {
    let ar = a.range();
    let br = b.range();
    // b starts strictly before a ends → real overlap / nesting.
    if br.start < ar.end {
        return true;
    }
    // Two coincident bare carets (or a bare `b` on a bare `a`) share an offset.
    ar.is_empty() && br.start == ar.start
}

/// Reconcile a tagged `(caret, is_primary)` set into a sorted, non-overlapping
/// list, enforcing [`apply_edits_to_string`]'s precondition at the caller.
///
/// Carets are sorted by start (the primary ordered first among equal starts so
/// it is the one retained); any caret coincident with or nested inside the
/// previously-kept caret is dropped. When an incoming primary conflicts with a
/// kept secondary, the primary replaces it — the primary always survives.
fn reconcile_carets(mut merged: Vec<(Caret, bool)>) -> Vec<(Caret, bool)> {
    // `!is_primary` as the tiebreak puts the primary (false) first at equal start.
    merged.sort_by_key(|(c, is_primary)| (c.start(), c.range().end, !*is_primary));
    let mut kept: Vec<(Caret, bool)> = Vec::with_capacity(merged.len());
    for (c, is_primary) in merged {
        match kept.last_mut() {
            Some((prev, prev_primary)) if carets_conflict(prev, &c) => {
                // Prefer the primary: if the newcomer is primary and the kept one
                // is not, swap it in; otherwise drop the newcomer.
                if is_primary && !*prev_primary {
                    *prev = c;
                    *prev_primary = true;
                }
            }
            _ => kept.push((c, is_primary)),
        }
    }
    kept
}

/// Char offset → byte offset within `text`. `ci == char count` maps to `len()`.
fn char_to_byte(text: &str, ci: usize) -> usize {
    text.char_indices()
        .nth(ci)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

/// Apply a set of `(char-range, replacement)` edits to `text`.
///
/// Edits must be non-overlapping. They are applied **right-to-left** (largest
/// start first) so the byte offsets of the not-yet-applied left edits — computed
/// against the original string — stay valid after each splice.
pub fn apply_edits_to_string(text: &mut String, edits: &[(Range<usize>, &str)]) {
    let mut order: Vec<usize> = (0..edits.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(edits[i].0.start));
    // Fail loud in debug builds if the caller violated the non-overlap contract
    // (adjacent ranges — `end == next.start` — are fine; overlap is not).
    #[cfg(debug_assertions)]
    {
        let mut asc: Vec<Range<usize>> = edits.iter().map(|(r, _)| r.clone()).collect();
        asc.sort_by_key(|r| (r.start, r.end));
        for w in asc.windows(2) {
            debug_assert!(
                w[0].end <= w[1].start,
                "apply_edits_to_string requires non-overlapping edits: {:?} overlaps {:?}",
                w[0],
                w[1],
            );
        }
    }
    for &i in &order {
        let (r, ins) = &edits[i];
        let bs = char_to_byte(text, r.start);
        let be = char_to_byte(text, r.end);
        text.replace_range(bs..be, ins);
    }
}

/// Rectangular (block / column) selection.
///
/// Given the buffer chars and the two corner char offsets (`anchor`, `head`),
/// build one [`Caret`] per spanned line, each selecting the `[col_lo, col_hi)`
/// column band clamped to that line's content length. Lines shorter than
/// `col_lo` yield a bare caret at their end (so a later insert still lands on
/// every spanned line — matching VS Code / Sublime column-insert behaviour).
pub fn column_selection(chars: &[char], anchor: usize, head: usize) -> Vec<Caret> {
    if chars.is_empty() {
        return vec![Caret::at(0)];
    }
    let starts = line_starts(chars);
    let (la, ca) = line_col(chars, anchor, &starts);
    let (lh, ch) = line_col(chars, head, &starts);
    let (l0, l1) = (la.min(lh), la.max(lh));
    let (c0, c1) = (ca.min(ch), ca.max(ch));

    let mut out = Vec::with_capacity(l1 - l0 + 1);
    for l in l0..=l1 {
        let ls = starts[l];
        let ll = line_content_len(chars, l, &starts);
        let s = ls + c0.min(ll);
        let e = ls + c1.min(ll);
        out.push(Caret::selection(s, e));
    }
    out
}

/// Char offset of the start of each line (line 0 starts at 0; each subsequent
/// line starts one char after a `\n`).
fn line_starts(chars: &[char]) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// `(line, column)` of char offset `idx` (column is chars since the line start).
fn line_col(chars: &[char], idx: usize, starts: &[usize]) -> (usize, usize) {
    let idx = idx.min(chars.len());
    // The line whose start is the greatest start <= idx.
    let line = match starts.binary_search(&idx) {
        Ok(l) => l,
        Err(l) => l - 1,
    };
    (line, idx - starts[line])
}

/// Number of chars on line `l` before its `\n` (or EOF for the last line).
fn line_content_len(chars: &[char], l: usize, starts: &[usize]) -> usize {
    let ls = starts[l];
    let mut e = ls;
    while e < chars.len() && chars[e] != '\n' {
        e += 1;
    }
    e - ls
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cv(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn normalize_sorts_and_dedups_secondaries() {
        // set_secondaries takes an UNSORTED, DUPLICATED vec; normalize() must sort
        // ascending by start and drop the exact duplicate. With normalize a no-op
        // the vec stays [9,3,3]. Kills 182:9 (replace normalize with ()).
        let mut mc = MultiCursor::default();
        mc.set_secondaries(vec![Caret::at(9), Caret::at(3), Caret::at(3)]);
        assert_eq!(mc.secondaries(), &[Caret::at(3), Caret::at(9)]);
    }

    #[test]
    fn backspace_at_offset_zero_is_a_noop_not_an_underflow() {
        // A bare caret at offset 0 backspacing has nothing before it: the
        // `c.head > 0` guard must be strict; the `>=` mutant makes `0 - 1`
        // underflow-panic in debug. Kills 225:35.
        let mut text = "abc".to_string();
        let mut mc = MultiCursor::default();
        mc.add_caret(Caret::at(2));
        let np = mc.apply_edit(&mut text, Caret::at(0), EditOp::Backspace);
        assert_eq!(text, "ac", "offset-0 caret deletes nothing; the caret at 2 removes 'b'");
        assert_eq!(np, 0);
    }

    #[test]
    fn ctrl_d_scans_forward_from_the_seed_not_backward() {
        // "x x x x": seed the primary on the THIRD "x" (4..5). The forward scan
        // (`r.start >= cursor && !is_occupied`) must add the FOURTH "x" (6..7),
        // never wrap backward to the first. Kills 346:31, 346:41, 346:44.
        let text = "x x x x";
        let mut mc = MultiCursor::default();
        let out = mc.select_next_occurrence(text, Caret::selection(4, 5));
        assert_eq!(out, CtrlDOutcome::Added(Caret::selection(6, 7)));
    }

    #[test]
    fn toggle_caret_removes_a_secondary_when_clicking_inside_its_selection() {
        // caret_hits' SELECTION branch is only reached for a ranged caret. Add a
        // secondary selection 3..7, then Ctrl+click at offset 5 (strictly inside)
        // -> hits -> removes it. Kills 391:17 and 391:31.
        let mut mc = MultiCursor::default();
        let primary = Caret::at(0);
        mc.add_caret(Caret::selection(3, 7));
        mc.toggle_caret(Caret::at(5), primary);
        assert!(mc.secondaries().is_empty(), "clicking inside a secondary's selection toggles it off");
        assert!(!mc.is_active());
    }

    #[test]
    fn toggle_caret_just_past_a_selection_end_adds_not_removes() {
        // pos == r.end + 1 is OUTSIDE the range: start<=pos && pos<=end = true &&
        // false = false (no hit) -> the click ADDS a caret. Kills 391:24 and 391:31.
        let mut mc = MultiCursor::default();
        let primary = Caret::at(0);
        mc.add_caret(Caret::selection(3, 7));
        mc.toggle_caret(Caret::at(8), primary);
        assert_eq!(mc.secondaries(), &[Caret::selection(3, 7), Caret::at(8)]);
    }

    #[test]
    fn reconcile_keeps_the_primary_when_it_conflicts_with_an_earlier_secondary() {
        // Secondary selection 0..5 sorts first; the primary bare caret at 3 (nested)
        // must be SWAPPED IN (primary always survives) -> one insert at offset 3.
        // The `!*prev_primary` -> `*prev_primary` mutant never swaps. Kills 426:34.
        let mut text = "abcdefghij".to_string();
        let mut mc = MultiCursor::default();
        mc.add_caret(Caret::selection(0, 5));
        let np = mc.apply_edit(&mut text, Caret::at(3), EditOp::Insert("X".into()));
        assert_eq!(text, "abcXdefghij");
        assert_eq!(np, 4);
        assert!(mc.secondaries().is_empty());
    }

    #[test]
    fn backward_selection_range_and_start_use_min_max_order() {
        // A backward drag (anchor > head: shift+Home, or shift+Left dragging the
        // head left past the anchor). range() must sort to min..max and start()
        // to the lower offset. Every other test builds forward selections
        // (anchor <= head), so a mutant that drops the .min()/.max() (using
        // self.anchor / self.head directly) is byte-identical for them and only
        // observable here.
        let back = Caret::selection(7, 3);
        assert_eq!(back.range(), 3..7, "range() sorts anchor/head to min..max");
        assert_eq!(back.start(), 3, "start() is the lower offset regardless of drag direction");
        // forward direction still holds (guards against an over-correction):
        assert_eq!(Caret::selection(2, 5).range(), 2..5);
        assert_eq!(Caret::selection(2, 5).start(), 2);
    }

    #[test]
    fn caret_is_empty_reflects_zero_width_in_both_directions() {
        // is_empty() has zero call sites in the crate — pin it directly so the
        // `anchor == head` -> `!=` mutant cannot survive.
        assert!(Caret::at(3).is_empty(), "a bare caret is empty");
        assert!(!Caret::selection(1, 4).is_empty(), "a forward selection is not empty");
        assert!(!Caret::selection(4, 1).is_empty(), "a backward selection is not empty either");
    }

    #[test]
    fn char_to_byte_maps_multibyte_char_indices_to_byte_offsets() {
        // Every other fixture in this module is pure ASCII, where char index ==
        // byte offset, so any arithmetic bug in char_to_byte is invisible. Use a
        // multibyte string: 'é'=2 bytes, '中'=3 bytes, 'z'=1 byte.
        let text = "é中z";
        assert_eq!(char_to_byte(text, 0), 0);
        assert_eq!(char_to_byte(text, 1), 2, "after 'é' (2 bytes)");
        assert_eq!(char_to_byte(text, 2), 5, "after 'é中' (2+3 bytes)");
        assert_eq!(char_to_byte(text, 3), 6, "char count maps to len()");
        assert_eq!(char_to_byte(text, 99), 6, "an index past the end clamps to len()");
    }

    #[test]
    fn adjacent_touching_selections_do_not_conflict_but_overlaps_do() {
        // Exactly-touching ranges (end == next.start), e.g. non-overlapping Ctrl+D
        // matches 0..2 / 2..4, must NOT conflict (both kept) per carets_conflict's
        // documented contract. No existing fixture has touching ranges (they all
        // have a gap), so the `br.start < ar.end` -> `<=` mutant survives them.
        let a = Caret::selection(0, 2);
        let b = Caret::selection(2, 4);
        assert!(!carets_conflict(&a, &b), "touching-but-not-overlapping selections are both kept");
        // A genuine 1-char overlap DOES conflict (guards against inverting the test):
        assert!(carets_conflict(&Caret::selection(0, 3), &Caret::selection(2, 4)));
    }

    #[test]
    fn is_active_requires_both_engaged_and_a_nonempty_secondary_set() {
        let mut mc = MultiCursor::default();
        assert!(!mc.is_active(), "default: not engaged, no secondaries");
        // toggle_caret coincident with the primary engages (active=true) but adds
        // NO secondary — is_active must still be false. This is the only state in
        // the suite where active==true and secondaries is empty, so it kills the
        // `self.active && !self.secondaries.is_empty()` -> `self.active` mutant.
        mc.toggle_caret(Caret::at(5), Caret::at(5));
        assert!(!mc.is_active(), "engaged with zero secondaries is NOT active");
        mc.add_caret(Caret::at(9));
        assert!(mc.is_active(), "a real secondary makes it active");
    }

    #[test]
    fn word_bounds_include_underscores_and_digits() {
        // Every Ctrl+D fixture uses pure-alpha words, so a mutant dropping the
        // `c == '_'` disjunct or narrowing is_alphanumeric() -> is_alphabetic()
        // is invisible to them. A snake_case identifier with a digit pins both.
        let chars = cv("snake_case1 x"); // "snake_case1" is 11 chars (0..11)
        assert_eq!(
            word_bounds_chars(&chars, 3),
            (0, 11),
            "an identifier with '_' and a digit is one whole word"
        );
        assert!(is_word_char('_'), "underscore is a word char");
        assert!(is_word_char('7'), "a digit is a word char");
        assert!(!is_word_char(' '), "space is not a word char");
        assert!(!is_word_char('-'), "hyphen is not a word char");
    }

    #[test]
    fn word_bounds_left_scan_reads_the_char_before_the_cursor() {
        // A word PRECEDED by a non-word char: the left scan `chars[s - 1]` must
        // stop at the space. The "snake_case1" fixture scans to 0 either way, so
        // it missed 373:41. Here `s - 1 -> s + 1` reads chars[4] (OOB -> panic)
        // and `s - 1 -> s / 1` reads chars[s] -> stops one char late (start 1).
        assert_eq!(word_bounds_chars(&cv("a bc"), 3), (2, 4), "word 'bc' starts at index 2");
    }

    #[test]
    fn insert_at_two_carets_edits_both_spots() {
        // "aaa\naaa": primary at char 0, secondary at char 4 (start of line 2).
        let mut text = "aaa\naaa".to_string();
        let mut mc = MultiCursor::default();
        mc.add_caret(Caret::at(4));
        assert!(mc.is_active());
        let np = mc.apply_edit(&mut text, Caret::at(0), EditOp::Insert("X".into()));
        assert_eq!(text, "Xaaa\nXaaa", "both insertion points changed");
        assert_eq!(np, 1, "primary advanced past its insert");
        assert_eq!(
            mc.secondaries(),
            &[Caret::at(6)],
            "secondary advanced past its insert AND the earlier one"
        );
    }

    #[test]
    fn insert_multichar_shifts_all_following_carets() {
        let mut text = "one two three".to_string();
        let mut mc = MultiCursor::default();
        mc.add_caret(Caret::at(4)); // before "two"
        mc.add_caret(Caret::at(8)); // before "three"
        let np = mc.apply_edit(&mut text, Caret::at(0), EditOp::Insert(">>".into()));
        assert_eq!(text, ">>one >>two >>three");
        assert_eq!(np, 2);
        assert_eq!(mc.secondaries(), &[Caret::at(8), Caret::at(14)]);
    }

    #[test]
    fn backspace_at_all_carets_deletes_char_before_each() {
        // Carets after the first char of each line: delete it at every caret.
        let mut text = "ab\nab\nab".to_string();
        let mut mc = MultiCursor::default();
        mc.add_caret(Caret::at(4)); // after 'a' on line 2 (index 3='a',4='b')
        mc.add_caret(Caret::at(7)); // after 'a' on line 3 (index 6='a',7='b')
        let np = mc.apply_edit(&mut text, Caret::at(1), EditOp::Backspace);
        assert_eq!(text, "b\nb\nb", "the 'a' before each caret was removed");
        assert_eq!(np, 0);
        assert_eq!(mc.secondaries(), &[Caret::at(2), Caret::at(4)]);
    }

    #[test]
    fn delete_forward_at_all_carets() {
        let mut text = "ab\nab".to_string();
        let mut mc = MultiCursor::default();
        mc.add_caret(Caret::at(3)); // start of line 2, forward-deletes 'a'
        let np = mc.apply_edit(&mut text, Caret::at(0), EditOp::Delete);
        assert_eq!(text, "b\nb");
        assert_eq!(np, 0);
        assert_eq!(mc.secondaries(), &[Caret::at(2)]);
    }

    #[test]
    fn enter_inserts_newline_at_all_carets() {
        let mut text = "aa bb".to_string();
        let mut mc = MultiCursor::default();
        mc.add_caret(Caret::at(3)); // before "bb"
        let np = mc.apply_edit(&mut text, Caret::at(0), EditOp::Insert("\n".into()));
        assert_eq!(text, "\naa \nbb");
        assert_eq!(np, 1);
        assert_eq!(mc.secondaries(), &[Caret::at(5)]);
    }

    #[test]
    fn typing_replaces_selection_at_every_caret() {
        // Primary selects "aa" (0..2), secondary selects "aa" (3..5); type "Z".
        let mut text = "aa aa".to_string();
        let mut mc = MultiCursor::default();
        mc.add_caret(Caret::selection(3, 5));
        let np = mc.apply_edit(
            &mut text,
            Caret::selection(0, 2),
            EditOp::Insert("Z".into()),
        );
        assert_eq!(text, "Z Z", "each selection replaced by the typed char");
        assert_eq!(np, 1);
        assert_eq!(mc.secondaries(), &[Caret::at(3)]);
    }

    #[test]
    fn clear_collapses_to_single_caret() {
        let mut mc = MultiCursor::default();
        mc.add_caret(Caret::at(5));
        assert!(mc.is_active());
        mc.clear();
        assert!(!mc.is_active());
        assert!(mc.secondaries().is_empty());
    }

    #[test]
    fn ctrl_d_bare_caret_asks_to_select_word() {
        let text = "foo foo foo";
        let mut mc = MultiCursor::default();
        // Caret sits inside the first "foo".
        let out = mc.select_next_occurrence(text, Caret::at(1));
        assert_eq!(out, CtrlDOutcome::SelectWord { start: 0, end: 3 });
        assert!(
            mc.secondaries().is_empty(),
            "no secondary added on the first Ctrl+D"
        );
    }

    #[test]
    fn ctrl_d_selection_adds_next_occurrence() {
        let text = "foo foo foo";
        let mut mc = MultiCursor::default();
        // Primary already selects the first "foo" (0..3).
        let out = mc.select_next_occurrence(text, Caret::selection(0, 3));
        assert_eq!(out, CtrlDOutcome::Added(Caret::selection(4, 7)));
        // A second Ctrl+D grows to the third "foo".
        let out2 = mc.select_next_occurrence(text, Caret::selection(0, 3));
        assert_eq!(out2, CtrlDOutcome::Added(Caret::selection(8, 11)));
        assert_eq!(
            mc.secondaries().len(),
            2,
            "two secondaries: the 2nd and 3rd occurrence"
        );
    }

    #[test]
    fn ctrl_d_edit_applies_to_all_matches_rename_like() {
        // Select-next twice, then type — every "foo" becomes "bar".
        let mut text = "foo foo foo".to_string();
        let mut mc = MultiCursor::default();
        let primary = Caret::selection(0, 3);
        assert!(matches!(
            mc.select_next_occurrence(&text, primary),
            CtrlDOutcome::Added(_)
        ));
        assert!(matches!(
            mc.select_next_occurrence(&text, primary),
            CtrlDOutcome::Added(_)
        ));
        let np = mc.apply_edit(&mut text, primary, EditOp::Insert("bar".into()));
        assert_eq!(text, "bar bar bar", "rename-like multi-edit");
        assert_eq!(np, 3);
    }

    #[test]
    fn ctrl_d_wraps_and_stops_when_all_occupied() {
        let text = "x x";
        let mut mc = MultiCursor::default();
        let primary = Caret::selection(0, 1); // first "x"
        assert_eq!(
            mc.select_next_occurrence(text, primary),
            CtrlDOutcome::Added(Caret::selection(2, 3))
        );
        // Both occurrences occupied now -> no more matches.
        assert_eq!(
            mc.select_next_occurrence(text, primary),
            CtrlDOutcome::NoMatch
        );
    }

    #[test]
    fn column_selection_spans_lines_with_clamped_columns() {
        // "abcd\nefgh\nijkl": rectangle from (line0,col1) to (line2,col3).
        let c = cv("abcd\nefgh\nijkl");
        let carets = column_selection(&c, 1, 13);
        assert_eq!(
            carets,
            vec![
                Caret::selection(1, 3),   // line0 "bc"
                Caret::selection(6, 8),   // line1 "fg"
                Caret::selection(11, 13), // line2 "jk"
            ]
        );
    }

    #[test]
    fn column_selection_clamps_short_lines() {
        // Middle line is shorter than the column band; its caret clamps to EOL.
        let c = cv("aaaa\nbb\ncccc");
        // (line0,col1)..(line2,col3): line1 has only 2 chars, so col3 clamps to
        // its end (index 7) and col1 is index 6.
        let carets = column_selection(&c, 1, 11);
        assert_eq!(
            carets,
            vec![
                Caret::selection(1, 3),  // line0 "aa" (col1..col3)
                Caret::selection(6, 7),  // line1 "b"  (col1..clamped col2)
                Caret::selection(9, 11)  // line2 "cc" (col1..col3)
            ]
        );
    }

    #[test]
    fn column_selection_then_insert_hits_every_line() {
        let mut text = "abcd\nefgh\nijkl".to_string();
        let chars: Vec<char> = text.chars().collect();
        // col0 (char 0) down to col0 of line 2 (char 10) => bare col-0 carets.
        let mut carets = column_selection(&chars, 0, 10);
        // Designate the first as egui's primary, the rest as secondaries.
        let primary = carets.remove(0);
        let mut mc = MultiCursor::default();
        mc.set_secondaries(carets);
        let _ = mc.apply_edit(&mut text, primary, EditOp::Insert("#".into()));
        assert_eq!(text, "#abcd\n#efgh\n#ijkl", "column insert on every line");
    }

    #[test]
    fn apply_edits_right_to_left_keeps_offsets_valid() {
        let mut text = "0123456789".to_string();
        apply_edits_to_string(&mut text, &[(1..2, "A"), (5..7, "BBB"), (9..9, "C")]);
        // 1..2 '1'->'A'; 5..7 '56'->'BBB'; 9..9 insert 'C' before '9'.
        assert_eq!(text, "0A234BBB78C9");
    }

    // ---- FIX-1 — caret reconciliation + toggle-remove (no double-edit / splice) ----

    #[test]
    fn ctrl_click_on_primary_offset_adds_no_coincident_caret() {
        // Repro 1: primary at offset 5, Ctrl+click the SAME char (offset 5).
        // toggle_caret must dedup against the primary — no phantom secondary.
        let mut text = "aaaaaaaaaa".to_string(); // 10 chars
        let mut mc = MultiCursor::default();
        let primary = Caret::at(5);
        mc.toggle_caret(Caret::at(5), primary);
        assert!(
            mc.secondaries().is_empty(),
            "clicking the primary's own offset must not add a coincident caret"
        );
        // Typing inserts EXACTLY once (single primary edit, no doubled "XX").
        let np = mc.apply_edit(&mut text, primary, EditOp::Insert("X".into()));
        assert_eq!(text, "aaaaaXaaaaa", "exactly one X inserted at offset 5");
        assert_eq!(np, 6);
        assert!(mc.secondaries().is_empty(), "no phantom secondary caret");
    }

    #[test]
    fn ctrl_click_existing_secondary_toggles_it_off() {
        // Ctrl+click an existing secondary caret REMOVES it (VS Code toggle).
        let mut mc = MultiCursor::default();
        let primary = Caret::at(0);
        mc.toggle_caret(Caret::at(3), primary);
        assert_eq!(mc.secondaries(), &[Caret::at(3)], "first click adds it");
        mc.toggle_caret(Caret::at(3), primary);
        assert!(
            mc.secondaries().is_empty(),
            "second click on the same offset removes it"
        );
        assert!(
            !mc.is_active(),
            "removing the last secondary collapses back to the primary"
        );
    }

    #[test]
    fn primary_navigated_onto_secondary_reconciles_to_one_caret() {
        // Repro 2: a secondary exists at offset 6, then the primary is moved onto
        // offset 6 (Right arrow / click). apply_edit must reconcile to ONE caret
        // there — no double insert, no phantom caret.
        let mut text = "aaaaaaaaaa".to_string();
        let mut mc = MultiCursor::default();
        mc.add_caret(Caret::at(6));
        let np = mc.apply_edit(&mut text, Caret::at(6), EditOp::Insert("X".into()));
        assert_eq!(
            text, "aaaaaaXaaaa",
            "exactly one X — the coincident caret merged"
        );
        assert_eq!(np, 7);
        assert!(mc.secondaries().is_empty());
    }

    #[test]
    fn bare_caret_inside_selection_is_reconciled_no_garbage() {
        // "foo foo": primary selects the first "foo" (0..3), a secondary selects
        // the second (4..7), and a BARE caret sits at offset 5 INSIDE that 4..7
        // selection. Typing must edit each region at most once — no overlapping
        // splice, no garbage ("fooX Xo" was the corrupted result).
        let mut text = "foo foo".to_string();
        let mut mc = MultiCursor::default();
        mc.add_caret(Caret::selection(4, 7));
        mc.add_caret(Caret::at(5)); // bare caret nested inside 4..7
        let primary = Caret::selection(0, 3);
        let _ = mc.apply_edit(&mut text, primary, EditOp::Insert("X".into()));
        assert_eq!(
            text, "X X",
            "coherent: each foo replaced once, nested caret dropped"
        );
    }

    // ---- FIX-2 — clamp a stale selection wider than the buffer ----

    #[test]
    fn stale_selection_wider_than_buffer_does_not_panic() {
        // A stale egui selection wider than the (shrunk) buffer must clamp, not
        // index past the char vec.
        let text = "foo";
        let mut mc = MultiCursor::default();
        let stale_primary = Caret::selection(0, 99);
        // Must not panic; the single "foo" is already occupied → NoMatch.
        let out = mc.select_next_occurrence(text, stale_primary);
        assert_eq!(out, CtrlDOutcome::NoMatch);
    }

    // ---- FIX-3 — whole-word select-next when seeded by a word ----

    #[test]
    fn ctrl_d_from_word_matches_whole_words_only() {
        // "foo foobar foo": Ctrl+D from the first whole word "foo" grows to the
        // standalone "foo" (11..14), NOT the "foo" inside "foobar" (4..7).
        let text = "foo foobar foo";
        let mut mc = MultiCursor::default();
        let out = mc.select_next_occurrence(text, Caret::selection(0, 3));
        assert_eq!(
            out,
            CtrlDOutcome::Added(Caret::selection(11, 14)),
            "whole-word: skips the 'foo' inside 'foobar'"
        );
    }

    #[test]
    fn ctrl_d_from_subword_selection_still_grows_substrings() {
        // Explicit sub-word selection "oo" (1..3) is NOT a whole word, so growth
        // uses substring matching and reaches the "oo" inside "foobar" (5..7).
        let text = "foo foobar";
        let mut mc = MultiCursor::default();
        let out = mc.select_next_occurrence(text, Caret::selection(1, 3));
        assert_eq!(out, CtrlDOutcome::Added(Caret::selection(5, 7)));
    }

    // ---- FIX-4 — single-line Alt+drag → a valid single-line selection ----

    #[test]
    fn single_line_column_selection_is_one_ranged_caret() {
        // A single-line Alt+drag yields one ranged caret on that line (a normal
        // selection), not an empty/ignored gesture.
        let c = cv("abcdef");
        let carets = column_selection(&c, 1, 4);
        assert_eq!(carets, vec![Caret::selection(1, 4)]);
    }
}
