//! WU-5 coverage: specific-assertion tests over the rope editor's PURE
//! text-geometry, selection/cursor, and key-dispatch math.
//!
//! This sibling module is `#[path]`-included from `mod.rs` under `#[cfg(test)]`
//! so it can reach the crate-private helpers (`pos_to_char_offset`,
//! `digit_count`, `build_line_job`, `try_expand_snippet`, `TextGeom`) and the
//! public `apply_event` / `RopeEditorState` surface. It targets the
//! selection/cursor/index-conversion/word-navigation/hit-testing branches that
//! the egui paint glue calls but never exercised under the existing smoke tests
//! — NOT the GPU paint itself (that is WU-0's justified exclusion).

use super::*;
use scribe_core::editing::EditState;
use scribe_core::snippets::SnippetSet;

// ---- test event constructors (mirror the inline tests' helpers) ----

fn text_ev(s: &str) -> egui::Event {
    egui::Event::Text(s.to_string())
}

fn key(key: egui::Key, shift: bool, cmd: bool, alt: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            shift,
            command: cmd,
            ctrl: cmd,
            alt,
            ..Default::default()
        },
    }
}

/// A plain (no-modifier) keypress.
fn plain(k: egui::Key) -> egui::Event {
    key(k, false, false, false)
}

// =====================================================================
// pos_to_char_offset — click-to-position hit-testing math
// =====================================================================

/// CRLF lines: a click past the end of a `\r\n`-terminated line must clamp to
/// the last *visible* glyph, stripping BOTH the trailing `\n` and `\r` (line
/// 660-665 in mod.rs). Without the `\r` strip, the caret would land one column
/// too far right on a Windows-EOL buffer.
#[test]
fn pos_to_char_offset_strips_crlf_when_clamping_to_eol() {
    let r = Rope::from_str("ab\r\ncd\r\n");
    let total = r.len_lines();
    let geom = TextGeom {
        text_left: 0.0,
        row0_top: 0.0,
        line_h: 16.0,
        char_w: 8.0,
    };
    // Row 0 ("ab\r\n"): far-right click clamps to col 2 (after 'b', before \r\n),
    // i.e. char offset 2 — NOT 3 (which would sit on the '\r').
    let off = pos_to_char_offset(&r, egui::pos2(999.0, 4.0), geom, 0, total, None);
    assert_eq!(off, 2, "CRLF: clamp lands before \\r, not on it");
    // Sanity: a click at col 1 still lands at offset 1.
    let off1 = pos_to_char_offset(&r, egui::pos2(8.0, 4.0), geom, 0, total, None);
    assert_eq!(off1, 1);
}

/// Bare `\r` (old-Mac EOL, no following `\n`): the lone-`\r` strip arm
/// (line 663-664) must also engage. ropey treats a lone `\r` as a line break,
/// so "x\ry" is two lines; clamping row 0 to EOL must exclude the `\r`.
#[test]
fn pos_to_char_offset_strips_lone_cr() {
    let r = Rope::from_str("x\ry\r");
    let total = r.len_lines();
    let geom = TextGeom {
        text_left: 0.0,
        row0_top: 0.0,
        line_h: 16.0,
        char_w: 8.0,
    };
    // Far-right on row 0 ("x\r") clamps after 'x' (offset 1), excluding the \r.
    let off = pos_to_char_offset(&r, egui::pos2(999.0, 4.0), geom, 0, total, None);
    assert_eq!(off, 1, "lone \\r excluded from clamp");
}

/// A non-zero `range_start` (scrolled viewport) offsets the row mapping: the
/// first painted row is `range_start`, so a click on the top visible row maps
/// to line `range_start`, not line 0.
#[test]
fn pos_to_char_offset_honours_scrolled_range_start() {
    let r = Rope::from_str("l0\nl1\nl2\nl3\nl4\n");
    let total = r.len_lines();
    let geom = TextGeom {
        text_left: 0.0,
        row0_top: 0.0,
        line_h: 16.0,
        char_w: 8.0,
    };
    // Viewport scrolled so row0 == line 2. A click at y in [0,16) maps to line 2.
    let off = pos_to_char_offset(&r, egui::pos2(0.0, 4.0), geom, 2, total, None);
    assert_eq!(
        off,
        r.line_to_char(2),
        "top visible row maps to range_start"
    );
    // Second visible row (y in [16,32)) → line 3.
    let off3 = pos_to_char_offset(&r, egui::pos2(0.0, 20.0), geom, 2, total, None);
    assert_eq!(off3, r.line_to_char(3));
}

/// A click ABOVE the first visible row (negative relative y) must clamp to the
/// first visible line, never underflow into a negative line index.
#[test]
fn pos_to_char_offset_click_above_clamps_to_first_line() {
    let r = Rope::from_str("aaa\nbbb\n");
    let total = r.len_lines();
    let geom = TextGeom {
        text_left: 0.0,
        row0_top: 100.0, // first row painted at y=100
        line_h: 16.0,
        char_w: 8.0,
    };
    // Click well above the first row (y=0) → rel is negative → clamps to line 0.
    let off = pos_to_char_offset(&r, egui::pos2(0.0, 0.0), geom, 0, total, None);
    assert_eq!(off, 0, "click above first row clamps to line 0 col 0");
}

/// An empty rope (single empty line) maps any click to offset 0 without
/// panicking on the `len == 0` line.
#[test]
fn pos_to_char_offset_empty_rope_is_zero() {
    let r = Rope::from_str("");
    let total = r.len_lines();
    let geom = TextGeom {
        text_left: 0.0,
        row0_top: 0.0,
        line_h: 16.0,
        char_w: 8.0,
    };
    assert_eq!(
        pos_to_char_offset(&r, egui::pos2(50.0, 4.0), geom, 0, total, None),
        0
    );
}

/// Multi-byte (non-ASCII) line: the column is measured in CHARS, not bytes, so
/// a click at column 2 of "éàü" lands at char offset 2 even though those chars
/// are 2 bytes each in UTF-8.
#[test]
fn pos_to_char_offset_multibyte_columns_are_char_indexed() {
    let r = Rope::from_str("éàü\n");
    let total = r.len_lines();
    let geom = TextGeom {
        text_left: 0.0,
        row0_top: 0.0,
        line_h: 16.0,
        char_w: 8.0,
    };
    // Column 2 (x ≈ 2*8 = 16) → char offset 2 (before 'ü').
    let off = pos_to_char_offset(&r, egui::pos2(16.0, 4.0), geom, 0, total, None);
    assert_eq!(off, 2, "column counts chars, not bytes");
    // Far right clamps to 3 (the three chars), not the byte length 6.
    let end = pos_to_char_offset(&r, egui::pos2(999.0, 4.0), geom, 0, total, None);
    assert_eq!(end, 3);
}

// =====================================================================
// digit_count — gutter-width math
// =====================================================================

#[test]
fn digit_count_zero_and_boundaries() {
    // 0 lines still needs one digit (the `.max(1)` floor).
    assert_eq!(digit_count(0), 1);
    assert_eq!(digit_count(99), 2);
    assert_eq!(digit_count(100), 3);
    assert_eq!(digit_count(usize::from(u16::MAX)), 5); // 65535
}

// =====================================================================
// build_line_job — span-tiling / colour-section math
// =====================================================================

/// An empty span list falls through to the single-default-section path
/// (the `Some(spans) if !spans.is_empty()` guard is false).
#[test]
fn build_line_job_empty_span_list_is_single_default_section() {
    let spans: Vec<HlSpan> = Vec::new();
    let job = build_line_job(
        "abc",
        Some(&spans),
        &FontId::monospace(14.0),
        Color32::WHITE,
    );
    assert_eq!(job.text, "abc");
    assert_eq!(job.sections.len(), 1, "empty spans → one default section");
}

/// A span whose `end <= start` (degenerate / out-of-order) is skipped without
/// dropping text — the remainder is appended in the default colour
/// (line 706-707 `continue`, then the covered<len remainder at 718-722).
#[test]
fn build_line_job_degenerate_span_is_skipped_and_remainder_kept() {
    let spans = vec![HlSpan {
        range: 3..3, // zero-width → end <= start → skipped
        color: [255, 0, 0],
        bold: false,
        italic: false,
    }];
    let job = build_line_job(
        "hello",
        Some(&spans),
        &FontId::monospace(14.0),
        Color32::WHITE,
    );
    assert_eq!(job.text, "hello", "no text dropped by degenerate span");
    // Whole line is the default tail (the degenerate span contributed nothing).
    assert_eq!(job.sections.len(), 1);
}

/// A span that runs PAST the line end is clamped to `line.len()` (the
/// `.min(line.len())` guards at 704-705) and still renders only the real text.
#[test]
fn build_line_job_overlong_span_clamps_to_line_len() {
    let spans = vec![HlSpan {
        range: 0..999, // far past the 2-byte line
        color: [0, 0, 255],
        bold: false,
        italic: false,
    }];
    let job = build_line_job("hi", Some(&spans), &FontId::monospace(14.0), Color32::WHITE);
    assert_eq!(job.text, "hi");
    // One colored section covering the whole (clamped) line; no default tail
    // because covered reached line.len().
    assert_eq!(job.sections.len(), 1);
}

/// A span starting OFF a char boundary inside a multi-byte glyph: `line.get`
/// returns `None` for a non-boundary slice, so that span is skipped (line 709
/// `continue`) but the full text is still emitted via the default fallback.
#[test]
fn build_line_job_off_boundary_span_is_skipped_text_preserved() {
    // "é" is 2 bytes (0xC3 0xA9). A span 1..2 lands mid-char → line.get(1..2) None.
    let spans = vec![HlSpan {
        range: 1..2,
        color: [255, 0, 0],
        bold: false,
        italic: false,
    }];
    let job = build_line_job("éx", Some(&spans), &FontId::monospace(14.0), Color32::WHITE);
    assert_eq!(job.text, "éx", "off-boundary span must not drop text");
}

// =====================================================================
// RopeEditorState — caret/selection bookkeeping
// =====================================================================

/// `clamp_to` pulls the primary AND every secondary caret back into range when
/// the rope shrinks under them (line 785-793 — the extra-caret loop).
#[test]
fn clamp_to_clamps_primary_and_extra_carets() {
    let mut st = RopeEditorState::new();
    st.edit = EditState {
        anchor: 50,
        cursor: 60,
        goal_col: None,
    };
    st.extra = vec![EditState {
        anchor: 40,
        cursor: 70,
        goal_col: None,
    }];
    let r = Rope::from_str("short"); // 5 chars
    st.clamp_to(&r);
    assert_eq!(st.edit.cursor, 5, "primary cursor clamped to len");
    assert_eq!(st.edit.anchor, 5, "primary anchor clamped to len");
    assert_eq!(st.extra[0].cursor, 5, "extra cursor clamped");
    assert_eq!(st.extra[0].anchor, 5, "extra anchor clamped");
}

/// `clear_extra_carets` + `is_multi` reflect the secondary-caret set.
#[test]
fn clear_extra_carets_collapses_to_single() {
    let mut st = RopeEditorState::new();
    st.extra = vec![EditState::at(3), EditState::at(7)];
    assert!(st.is_multi());
    st.clear_extra_carets();
    assert!(!st.is_multi());
    assert!(st.extra.is_empty());
}

// =====================================================================
// try_expand_snippet — Tab-trigger word-boundary math
// =====================================================================

fn fn_snippet() -> SnippetSet {
    SnippetSet::from_toml("[[snippets]]\nprefix = \"fn\"\nbody = \"fn ${1}()\"\n").unwrap()
}

/// A selection active at the caret blocks snippet expansion (line 848-849 guard).
#[test]
fn try_expand_snippet_blocked_by_selection() {
    let set = fn_snippet();
    let mut r = Rope::from_str("fn");
    let mut st = RopeEditorState::new();
    st.edit = EditState {
        anchor: 0,
        cursor: 2, // a real selection over "fn"
        goal_col: None,
    };
    assert!(!try_expand_snippet(&mut r, &mut st, &set));
    assert_eq!(r.to_string(), "fn", "no expansion under a selection");
}

/// Multi-cursor mode blocks snippet expansion (line 848 `!state.extra.is_empty()`).
#[test]
fn try_expand_snippet_blocked_by_multi_cursor() {
    let set = fn_snippet();
    let mut r = Rope::from_str("fn");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(2);
    st.extra = vec![EditState::at(2)];
    assert!(!try_expand_snippet(&mut r, &mut st, &set));
}

/// Caret at column 0 (no word before it) → `start == cursor` early-out
/// (line 857-859), no expansion.
#[test]
fn try_expand_snippet_no_word_before_caret() {
    let set = fn_snippet();
    let mut r = Rope::from_str("fn");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(0); // caret before the word
    assert!(!try_expand_snippet(&mut r, &mut st, &set));
    assert_eq!(r.to_string(), "fn");
}

// =====================================================================
// apply_event — key-dispatch branches the smoke tests skipped
// =====================================================================

/// ArrowRight with no modifiers (no shift) collapses any selection and moves
/// the caret right (line 1124-1128 single-caret move path).
#[test]
fn apply_event_arrow_right_moves_caret() {
    let mut r = Rope::from_str("abc");
    let mut st = RopeEditorState::new();
    let out = apply_event(&mut r, &mut st, &plain(egui::Key::ArrowRight));
    assert!(out.consumed);
    assert!(!out.mutated, "movement does not mutate");
    assert_eq!(st.edit.cursor, 1);
}

/// ArrowLeft at the start of the buffer stays at 0 (clamped move).
#[test]
fn apply_event_arrow_left_at_start_clamps() {
    let mut r = Rope::from_str("abc");
    let mut st = RopeEditorState::new();
    apply_event(&mut r, &mut st, &plain(egui::Key::ArrowLeft));
    assert_eq!(st.edit.cursor, 0, "left at start stays at 0");
}

/// ArrowUp / ArrowDown move between lines preserving the goal column.
#[test]
fn apply_event_arrow_up_down_change_line() {
    let mut r = Rope::from_str("abcd\nefgh\nijkl\n");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(7); // line1 col2 ("ef|gh")
    apply_event(&mut r, &mut st, &plain(egui::Key::ArrowDown));
    let (l, _c) = editing::line_col(&r, st.edit.cursor);
    assert_eq!(l, 2, "ArrowDown moves to the next line");
    apply_event(&mut r, &mut st, &plain(egui::Key::ArrowUp));
    let (l2, _) = editing::line_col(&r, st.edit.cursor);
    assert_eq!(l2, 1, "ArrowUp moves back up");
}

/// Home / End move to the line's first / last column.
#[test]
fn apply_event_home_and_end() {
    let mut r = Rope::from_str("  abc\n");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(4); // mid-line
    apply_event(&mut r, &mut st, &plain(egui::Key::End));
    assert_eq!(st.edit.cursor, 5, "End → last column (before newline)");
    apply_event(&mut r, &mut st, &plain(egui::Key::Home));
    // Home goes to the line start (col 0 of the line, char offset 0 here).
    let (_l, c) = editing::line_col(&r, st.edit.cursor);
    assert_eq!(c, 0, "Home → column 0");
}

/// Delete (forward) at mid-buffer removes the char to the RIGHT of the caret
/// (line 1059-1062), distinct from Backspace.
#[test]
fn apply_event_delete_forward_removes_right_char() {
    let mut r = Rope::from_str("abc");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(1); // between 'a' and 'b'
    let out = apply_event(&mut r, &mut st, &plain(egui::Key::Delete));
    assert!(out.mutated);
    assert_eq!(r.to_string(), "ac", "Delete removes the char to the right");
    assert_eq!(st.edit.cursor, 1, "caret stays put on forward-delete");
}

/// Delete at end-of-buffer is a no-op (nothing to the right).
#[test]
fn apply_event_delete_at_eof_is_noop() {
    let mut r = Rope::from_str("ab");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(2);
    let out = apply_event(&mut r, &mut st, &plain(egui::Key::Delete));
    assert!(!out.mutated, "delete at EOF mutates nothing");
    assert_eq!(r.to_string(), "ab");
}

/// Ctrl+U with NO selection is consumed but mutates nothing (line 1104-1116:
/// the `has_selection()` guard is false, so only `out.consumed` is set).
#[test]
fn apply_event_ctrl_u_without_selection_is_noop_mutation() {
    let mut r = Rope::from_str("hello");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(0); // no selection
    let out = apply_event(&mut r, &mut st, &key(egui::Key::U, false, true, false));
    assert!(out.consumed, "Ctrl+U is always consumed");
    assert!(!out.mutated, "no selection → no case change");
    assert_eq!(r.to_string(), "hello");
}

/// Ctrl+U lowercases an already-uppercase selection (the non-shift case arm).
#[test]
fn apply_event_ctrl_u_lowercases_selection() {
    let mut r = Rope::from_str("HELLO");
    let mut st = RopeEditorState::new();
    st.edit = EditState {
        anchor: 0,
        cursor: 5,
        goal_col: None,
    };
    let out = apply_event(&mut r, &mut st, &key(egui::Key::U, false, true, false));
    assert!(out.mutated);
    assert_eq!(r.to_string(), "hello");
}

/// Undo with an EMPTY history is consumed but is a no-op (line 1159-1167: the
/// `if let Some(prev)` is None, so `out.mutated` stays false).
#[test]
fn apply_event_undo_empty_history_is_noop() {
    let mut r = Rope::from_str("x");
    let mut st = RopeEditorState::new();
    let out = apply_event(&mut r, &mut st, &key(egui::Key::Z, false, true, false));
    assert!(out.consumed, "Ctrl+Z is consumed even with nothing to undo");
    assert!(!out.mutated, "empty history → no content change");
    assert_eq!(r.to_string(), "x");
}

/// Redo with nothing to redo is consumed but a no-op (line 1169-1178).
#[test]
fn apply_event_redo_with_nothing_is_noop() {
    let mut r = Rope::from_str("x");
    let mut st = RopeEditorState::new();
    let out = apply_event(&mut r, &mut st, &key(egui::Key::Z, true, true, false));
    assert!(out.consumed);
    assert!(!out.mutated, "nothing to redo → no content change");
    assert_eq!(r.to_string(), "x");
}

/// Copy with NO selection produces no clipboard text (line 985-991: `sel` is
/// empty, so `set_clipboard` stays None) but is still consumed.
#[test]
fn apply_event_copy_without_selection_yields_no_clipboard() {
    let mut r = Rope::from_str("abc");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(1); // caret, no selection
    let out = apply_event(&mut r, &mut st, &egui::Event::Copy);
    assert!(out.consumed);
    assert!(out.set_clipboard.is_none(), "no selection → nothing copied");
}

/// Cut with NO selection produces no clipboard text and no mutation
/// (line 993-1001: the empty-selection branch records nothing).
#[test]
fn apply_event_cut_without_selection_is_noop() {
    let mut r = Rope::from_str("abc");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(1);
    let out = apply_event(&mut r, &mut st, &egui::Event::Cut);
    assert!(out.consumed);
    assert!(out.set_clipboard.is_none());
    assert!(!out.mutated);
    assert_eq!(r.to_string(), "abc");
}

/// Tab on a SINGLE-LINE selection (not multi-line, not multi-cursor) inserts a
/// 4-space indent at the caret via the `edit_all` fallthrough (line 1089-1093),
/// rather than the multi-line indent path.
#[test]
fn apply_event_tab_single_line_inserts_spaces() {
    let mut r = Rope::from_str("abc");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(0);
    let out = apply_event(&mut r, &mut st, &plain(egui::Key::Tab));
    assert!(out.mutated);
    assert_eq!(
        r.to_string(),
        "    abc",
        "single-line Tab inserts four spaces"
    );
}

/// Enter in multi-cursor mode inserts a plain newline at every caret (the
/// `else` arm at line 1072-1074), distinct from the single-caret auto-indent.
#[test]
fn apply_event_enter_multi_cursor_inserts_plain_newlines() {
    let mut r = Rope::from_str("  ab\n  cd\n");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(4); // end of "  ab"
    apply_event(&mut r, &mut st, &alt_cmd_down());
    assert!(st.is_multi(), "second caret added below");
    apply_event(&mut r, &mut st, &plain(egui::Key::Enter));
    // Both carets get a bare '\n' (no indent carried) in the multi-cursor arm.
    assert_eq!(r.to_string(), "  ab\n\n  cd\n\n");
}

/// ArrowUp in multi-cursor mode (alt+cmd) adds a caret ABOVE (line 1022-1027),
/// the mirror of the smoke test's ArrowDown.
#[test]
fn apply_event_alt_cmd_up_adds_caret_above() {
    let mut r = Rope::from_str("ab\ncd\n");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(5); // on line1 ("cd")
    let out = apply_event(&mut r, &mut st, &alt_cmd_up());
    assert!(out.consumed);
    assert!(st.is_multi(), "Ctrl+Alt+Up adds a caret above");
}

/// Ctrl+D twice: first selects the word, second adds the next-occurrence caret —
/// asserting the rope CONTENT is unchanged (pure selection op, no mutation).
#[test]
fn apply_event_ctrl_d_is_non_mutating() {
    let mut r = Rope::from_str("foo foo");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(1);
    let out1 = apply_event(&mut r, &mut st, &key(egui::Key::D, false, true, false));
    assert!(!out1.mutated, "Ctrl+D selects, never mutates");
    assert_eq!(st.edit.selection(), 0..3);
    let out2 = apply_event(&mut r, &mut st, &key(egui::Key::D, false, true, false));
    assert!(!out2.mutated);
    assert!(st.is_multi());
    assert_eq!(r.to_string(), "foo foo", "content untouched by Ctrl+D");
}

/// Backspace at the very start of the buffer is a no-op (nothing to the left).
#[test]
fn apply_event_backspace_at_start_is_noop() {
    let mut r = Rope::from_str("abc");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(0);
    let out = apply_event(&mut r, &mut st, &plain(egui::Key::Backspace));
    assert!(!out.mutated, "backspace at start mutates nothing");
    assert_eq!(r.to_string(), "abc");
}

/// An empty `Event::Text("")` is ignored (the `!text.is_empty()` arm guard),
/// falling through to the no-op `_ => {}` — nothing consumed, nothing mutated.
#[test]
fn apply_event_empty_text_is_ignored() {
    let mut r = Rope::from_str("a");
    let mut st = RopeEditorState::new();
    let out = apply_event(&mut r, &mut st, &text_ev(""));
    assert!(!out.consumed, "empty text event is not consumed");
    assert!(!out.mutated);
    assert_eq!(r.to_string(), "a");
}

/// An IME Preedit (in-progress composition) is consumed but never mutates
/// (line 975-984: only `Commit` inserts; other Ime arms fall to `_ => {}`).
#[test]
fn apply_event_ime_preedit_consumed_but_not_mutating() {
    let mut r = Rope::from_str("a");
    let mut st = RopeEditorState::new();
    st.edit = EditState::at(1);
    let out = apply_event(&mut r, &mut st, &egui::Event::Ime(egui::ImeEvent::Enabled));
    assert!(out.consumed, "Ime events are consumed to keep routing");
    assert!(!out.mutated);
    assert_eq!(r.to_string(), "a");
}

/// A released key (`pressed: false`) is ignored entirely — the `Event::Key`
/// arm only matches `pressed: true`, so a key-up falls to `_ => {}`.
#[test]
fn apply_event_key_release_is_ignored() {
    let mut r = Rope::from_str("a");
    let mut st = RopeEditorState::new();
    let release = egui::Event::Key {
        key: egui::Key::ArrowRight,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    };
    let out = apply_event(&mut r, &mut st, &release);
    assert!(!out.consumed, "key-release is not consumed");
    assert_eq!(st.edit.cursor, 0, "caret unmoved by key-release");
}

/// Auto-close opener WITH an active selection wraps the selection in the pair
/// (line 949-959 `has_selection()` branch of the opener path), distinct from
/// the empty-caret auto-close.
#[test]
fn apply_event_opener_wraps_active_selection() {
    let mut r = Rope::from_str("word");
    let mut st = RopeEditorState::new();
    st.edit = EditState {
        anchor: 0,
        cursor: 4,
        goal_col: None,
    };
    apply_event(&mut r, &mut st, &text_ev("{"));
    assert_eq!(r.to_string(), "{word}", "opener wraps the selection");
}

// ---- multi-cursor alt+cmd event helpers ----

fn alt_cmd_down() -> egui::Event {
    key(egui::Key::ArrowDown, false, true, true)
}
fn alt_cmd_up() -> egui::Event {
    key(egui::Key::ArrowUp, false, true, true)
}
