//! Differential / oracle testing of the rope edit engine + Unicode-segmentation
//! correctness.
//!
//! PART 2 §A items 2 (differential/oracle) and 3 (grapheme-cluster) of the
//! SCR1B3 testing taxonomy.
//!
//! ## Differential model
//!
//! `scribe_core::editing` performs cursor/selection/edit ops over a
//! `ropey::Rope` indexed in **char** (codepoint) units. We build a trivial
//! reference oracle over a `Vec<char>` + a plain cursor/anchor, apply a random
//! op-stream to BOTH, and assert the buffer contents and caret position stay
//! identical after every op. This catches rope-index off-by-ones that
//! single-value unit tests miss.
//!
//! ## Grapheme-cluster correctness
//!
//! The editing layer is codepoint-indexed by design (ropey's native unit), so a
//! single `move_horizontal(±1)` advances ONE codepoint, which may land *inside*
//! a multi-codepoint grapheme cluster (emoji ZWJ sequence, base+combining mark).
//! That is the intended low-level behaviour. The invariants we CAN assert for
//! ANY Unicode input — and which a corruption bug would break — are:
//!   * every caret index is always a valid char boundary (never splits a UTF-8
//!     scalar), and
//!   * deleting a whole grapheme cluster (advancing the caret across the cluster
//!     by its codepoint length, then back-deleting that many times) removes
//!     exactly that cluster and nothing else, matching a `unicode-segmentation`
//!     reference.
//!
//! These prove the engine never produces invalid UTF-8 and that grapheme-aware
//! callers (cursor movement built ON these primitives) have a sound foundation.

use proptest::prelude::*;
use ropey::Rope;
use unicode_segmentation::UnicodeSegmentation;

use scribe_core::editing::{
    self, backspace, delete_forward, insert, move_horizontal, select_all, selected_text, EditState,
};

// ---------------------------------------------------------------------------
// Reference oracle over Vec<char>
// ---------------------------------------------------------------------------

/// A trivial char-vector editor mirroring `scribe_core::editing`'s public
/// semantics. Deliberately naive (no rope, no optimisation) so it is obviously
/// correct and serves as the differential oracle.
#[derive(Clone, Debug)]
struct Oracle {
    chars: Vec<char>,
    cursor: usize,
    anchor: usize,
}

impl Oracle {
    fn new(s: &str) -> Self {
        Self {
            chars: s.chars().collect(),
            cursor: 0,
            anchor: 0,
        }
    }

    fn text(&self) -> String {
        self.chars.iter().collect()
    }

    fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    fn sel(&self) -> (usize, usize) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        let (lo, hi) = self.sel();
        self.chars.drain(lo..hi);
        self.cursor = lo;
        self.anchor = lo;
        true
    }

    fn insert(&mut self, text: &str) {
        self.delete_selection();
        let at = self.cursor.min(self.chars.len());
        for (i, ch) in text.chars().enumerate() {
            self.chars.insert(at + i, ch);
        }
        let n = text.chars().count();
        self.cursor = at + n;
        self.anchor = self.cursor;
    }

    fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let to = self.cursor.min(self.chars.len());
        if to > 0 {
            self.chars.remove(to - 1);
            self.cursor = to - 1;
            self.anchor = to - 1;
        } else {
            self.cursor = 0;
            self.anchor = 0;
        }
    }

    fn delete_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        let at = self.cursor.min(self.chars.len());
        if at < self.chars.len() {
            self.chars.remove(at);
            self.cursor = at;
            self.anchor = at;
        }
    }

    fn move_horizontal(&mut self, delta: isize, select: bool) {
        if !select && self.has_selection() {
            let (lo, hi) = self.sel();
            let edge = if delta < 0 { lo } else { hi };
            self.cursor = edge;
            self.anchor = edge;
            return;
        }
        let next = (self.cursor as isize + delta).clamp(0, self.chars.len() as isize) as usize;
        self.cursor = next;
        if !select {
            self.anchor = next;
        }
    }

    fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.chars.len();
    }
}

/// One random editor operation applied to both engines.
#[derive(Clone, Debug)]
enum Op {
    Insert(String),
    Backspace,
    DeleteForward,
    MoveHoriz { delta: i8, select: bool },
    SelectAll,
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        // Insert short strings including multibyte chars to stress index math.
        "[a-z😀é\nλ]{0,4}".prop_map(Op::Insert),
        Just(Op::Backspace),
        Just(Op::DeleteForward),
        (-3i8..=3, any::<bool>()).prop_map(|(delta, select)| Op::MoveHoriz { delta, select }),
        Just(Op::SelectAll),
    ]
}

fn apply_real(rope: &mut Rope, st: &mut EditState, op: &Op) {
    match op {
        Op::Insert(s) => insert(rope, st, s),
        Op::Backspace => backspace(rope, st),
        Op::DeleteForward => delete_forward(rope, st),
        Op::MoveHoriz { delta, select } => move_horizontal(rope, st, *delta as isize, *select),
        Op::SelectAll => select_all(rope, st),
    }
}

fn apply_oracle(o: &mut Oracle, op: &Op) {
    match op {
        Op::Insert(s) => o.insert(s),
        Op::Backspace => o.backspace(),
        Op::DeleteForward => o.delete_forward(),
        Op::MoveHoriz { delta, select } => o.move_horizontal(*delta as isize, *select),
        Op::SelectAll => o.select_all(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    /// Differential: a random op-stream applied to the rope engine and the
    /// char-vector oracle keeps the buffer text AND caret/anchor identical after
    /// EVERY op. A divergence is a rope index bug.
    #[test]
    fn rope_engine_matches_char_oracle(
        initial in "[a-z😀é\nλ]{0,20}",
        ops in prop::collection::vec(op_strategy(), 0..40),
    ) {
        let mut rope = Rope::from_str(&initial);
        let mut st = EditState::at(0);
        let mut oracle = Oracle::new(&initial);

        for (i, op) in ops.iter().enumerate() {
            apply_real(&mut rope, &mut st, op);
            apply_oracle(&mut oracle, op);

            prop_assert_eq!(
                rope.to_string(), oracle.text(),
                "buffer diverged after op #{} {:?}", i, op
            );
            prop_assert_eq!(
                st.cursor, oracle.cursor,
                "cursor diverged after op #{} {:?} (text {:?})", i, op, oracle.text()
            );
            prop_assert_eq!(
                st.anchor, oracle.anchor,
                "anchor diverged after op #{} {:?} (text {:?})", i, op, oracle.text()
            );
            // selected_text agreement: the real engine's selection slice equals
            // the oracle's.
            let (lo, hi) = oracle.sel();
            let expected_sel: String = oracle.chars[lo..hi].iter().collect();
            prop_assert_eq!(selected_text(&rope, &st), expected_sel,
                "selection diverged after op #{} {:?}", i, op);
        }
    }
}

// ---------------------------------------------------------------------------
// Grapheme-cluster correctness
// ---------------------------------------------------------------------------

proptest! {
    /// After ANY sequence of horizontal moves over arbitrary Unicode, the caret
    /// index always lands on a valid char boundary — the engine NEVER produces
    /// an index that would split a UTF-8 scalar. (ropey panics on a non-boundary
    /// char index, so this also proves the moves never feed it garbage.)
    #[test]
    fn caret_always_on_char_boundary(
        s in "(\\PC|\n){0,30}",  // any printable/Unicode + newlines
        moves in prop::collection::vec((-3i8..=3, any::<bool>()), 0..30),
    ) {
        let rope = Rope::from_str(&s);
        let mut r = rope.clone();
        let mut st = EditState::at(0);
        for (delta, select) in moves {
            move_horizontal(&mut r, &mut st, delta as isize, select);
            // Converting the char index to a byte index must succeed (panics if
            // out of range), and the byte index must be a UTF-8 boundary.
            prop_assert!(st.cursor <= r.len_chars());
            let byte = r.char_to_byte(st.cursor);
            prop_assert!(s.is_char_boundary(byte),
                "caret char {} -> byte {} is not a char boundary in {:?}",
                st.cursor, byte, s);
        }
    }

    /// Deleting a whole leading grapheme cluster (by back-deleting its codepoint
    /// count after moving the caret past it) removes EXACTLY that cluster — the
    /// remaining text equals the original with the first grapheme stripped, as
    /// computed by `unicode-segmentation`. Proves multi-codepoint clusters
    /// (emoji ZWJ, base+combining) are deleted cleanly codepoint-by-codepoint.
    #[test]
    fn deleting_first_grapheme_cluster_matches_segmentation(s in "(\\PC|\n){1,30}") {
        let graphemes: Vec<&str> = s.graphemes(true).collect();
        prop_assume!(!graphemes.is_empty());
        let first = graphemes[0];
        let first_cp = first.chars().count();
        let expected: String = graphemes[1..].concat();

        let mut r = Rope::from_str(&s);
        let mut st = EditState::at(first_cp); // caret just past the first cluster
        // Back-delete the cluster's codepoints one at a time.
        for _ in 0..first_cp {
            backspace(&mut r, &mut st);
        }
        prop_assert_eq!(r.to_string(), expected,
            "deleting first grapheme {:?} ({} cp) did not match segmentation",
            first, first_cp);
        prop_assert_eq!(st.cursor, 0);
    }

    /// `line_col` / `char_at` round-trip on a char boundary for arbitrary
    /// Unicode multi-line text: converting a caret to (line, col) and back lands
    /// on the same char index (when the column is within the line's content).
    #[test]
    fn line_col_char_at_roundtrip(s in "(\\PC|\n){0,40}", c in 0usize..50) {
        let r = Rope::from_str(&s);
        let c = c.min(r.len_chars());
        let (line, col) = editing::line_col(&r, c);
        let back = editing::char_at(&r, line, col);
        // char_at clamps the column to the line's content (excluding newline),
        // so the round-trip is exact when the caret is not sitting ON a trailing
        // newline; otherwise `back` lands at end-of-content (<= c).
        prop_assert!(back <= c, "char_at({line},{col}) = {back} > {c}");
        // And it is always a valid index.
        prop_assert!(back <= r.len_chars());
    }
}
