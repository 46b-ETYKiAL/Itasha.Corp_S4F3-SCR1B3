//! Coverage for the SELECTION-DRIVEN text operations in `text_ops_methods.rs`.
//!
//! These are the markdown/editing conveniences reachable from the toolbar, the
//! palette, and the keymap — bold/italic wrapping, case conversion, task
//! checkboxes, list indent/outdent, smart list continuation, table formatting,
//! bracket jump, auto-pairing. Each carries real string/index logic whose bugs
//! are directly user-visible (a mangled table, a lost selection, a checkbox that
//! toggles the wrong line).
//!
//! They were uncovered for a structural reason, not an intentional one: each
//! reads its caret/selection from live `egui::TextEdit` state, so a plain unit
//! test sees `load_state -> None` and every method early-returns without doing
//! anything. `select()` below stores a real `TextEditState` at the same id the
//! method loads, which is the whole unlock — no render loop needed.
//!
//! `text_ops_tests.rs` covers the PURE helpers next to these (`char_to_byte`,
//! bracket-index math); this file covers the `&mut self` methods that mutate a
//! buffer through a selection.
#![allow(clippy::wildcard_imports)]
use super::text_ops_methods::NoteTemplate;
use super::*;

/// The editor id these tests store caret state under. Any stable id works — the
/// methods take the id as a parameter; production passes the live editor's.
fn edit_id() -> egui::Id {
    egui::Id::new("text-ops-selection-tests")
}

/// An app with one tab holding `text`, named `name` (the extension drives
/// `note_file_active`, which gates the markdown-only ops).
///
/// Each call gets its OWN directory: the file NAME is load-bearing (the
/// extension is the gate input) and several tests legitimately reuse one, so a
/// shared directory means two parallel tests write different fixtures to the
/// same path and read each other's. That is not hypothetical — it raced under
/// the coverage job's parallel run while passing under `--test-threads=1`.
fn app_with(name: &str, text: &str) -> (ScribeApp, egui::Context) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);

    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    let mut app = ScribeApp::new_test(cfg);
    let dir = std::env::temp_dir().join(format!(
        "scr1b3-text-ops-tests/{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    app.open_path(path);
    // `open_path` is the production entry, so the tab carries a real language
    // hint — that is what the note-shaped gate keys off.
    assert_eq!(app.tabs[app.active].text, text, "fixture loaded verbatim");
    (app, egui::Context::default())
}

/// Store a real selection (`lo..hi` in CHARS) at `edit_id`, as a live editor
/// would. A collapsed caret is `lo == hi`.
fn select(ctx: &egui::Context, lo: usize, hi: usize) {
    let mut state = egui::text_edit::TextEditState::default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(lo),
            egui::text::CCursor::new(hi),
        )));
    state.store(ctx, edit_id());
}

/// The current selection range (chars), for asserting the caret follows an edit.
fn selection(ctx: &egui::Context) -> (usize, usize) {
    let state = egui::TextEdit::load_state(ctx, edit_id()).expect("state was stored");
    let r = state.cursor.char_range().expect("range was set");
    (
        r.primary.index.min(r.secondary.index),
        r.primary.index.max(r.secondary.index),
    )
}

// ---- note_file_active (the gate every markdown-only op sits behind) ----

#[test]
fn note_file_active_is_true_for_note_shaped_files_only() {
    // md/txt (and an unknown/hint-less buffer) are note-shaped; source is not.
    for name in ["a.md", "a.txt"] {
        let (app, _) = app_with(name, "x");
        assert!(
            app.note_file_active(app.active),
            "{name} must be note-shaped"
        );
    }
    let (app, _) = app_with("a.rs", "fn x() {}");
    assert!(
        !app.note_file_active(app.active),
        "a source file must NOT be note-shaped — the markdown ops must stay off"
    );
    // Out-of-range index must not panic (it is `tabs.get(active)`).
    assert!(
        app.note_file_active(999),
        "no tab => no language hint => treated as note-shaped"
    );
}

// ---- active_selection_text ----

#[test]
fn active_selection_text_returns_the_selected_substring_or_none() {
    let (app, ctx) = app_with("sel.md", "hello world");
    // No stored state at all → None (the early-return every op shares).
    assert_eq!(app.active_selection_text(&ctx, edit_id(), app.active), None);

    select(&ctx, 6, 11);
    assert_eq!(
        app.active_selection_text(&ctx, edit_id(), app.active)
            .as_deref(),
        Some("world")
    );
    // A collapsed caret is not a selection.
    select(&ctx, 3, 3);
    assert_eq!(app.active_selection_text(&ctx, edit_id(), app.active), None);
}

#[test]
fn active_selection_text_is_char_indexed_not_byte_indexed() {
    // "é" is 2 bytes / 1 char; a byte-indexed slice here would panic or cut a
    // codepoint in half. Selecting chars 0..2 must yield "é中".
    let (app, ctx) = app_with("uni.md", "é中z");
    select(&ctx, 0, 2);
    assert_eq!(
        app.active_selection_text(&ctx, edit_id(), app.active)
            .as_deref(),
        Some("é中")
    );
}

#[test]
fn active_selection_text_is_direction_agnostic() {
    // A backwards drag (primary < secondary) selects the same span.
    let (app, ctx) = app_with("sel.md", "hello world");
    select(&ctx, 11, 6);
    assert_eq!(
        app.active_selection_text(&ctx, edit_id(), app.active)
            .as_deref(),
        Some("world"),
        "a right-to-left selection must yield the same text"
    );
}

// ---- wrap_selection_active (Ctrl+B / Ctrl+I / inline code) ----

#[test]
fn wrap_selection_wraps_then_unwraps_and_keeps_the_selection() {
    let (mut app, ctx) = app_with("w.md", "make me bold");
    let active = app.active;
    select(&ctx, 8, 12); // "bold"

    app.wrap_selection_active(&ctx, edit_id(), active, "**");
    assert_eq!(app.tabs[active].text, "make me **bold**");
    assert_eq!(
        app.active_selection_text(&ctx, edit_id(), active)
            .as_deref(),
        Some("bold"),
        "the selection must still cover the same WORD, not the markers"
    );

    // Toggling with the same marker round-trips exactly.
    app.wrap_selection_active(&ctx, edit_id(), active, "**");
    assert_eq!(
        app.tabs[active].text, "make me bold",
        "a second wrap with the same marker unwraps (exact round-trip)"
    );
}

#[test]
fn wrap_selection_without_stored_state_is_a_noop() {
    // The load_state early-return: no editor state => the buffer is untouched.
    let (mut app, ctx) = app_with("w.md", "unchanged");
    let active = app.active;
    app.wrap_selection_active(&ctx, edit_id(), active, "**");
    assert_eq!(app.tabs[active].text, "unchanged");
}

// ---- case_selection_active (0 = lower, 1 = upper, 2 = title) ----

#[test]
fn case_selection_converts_only_the_selection() {
    let (mut app, ctx) = app_with("c.md", "keep MiXeD keep");
    let active = app.active;
    select(&ctx, 5, 10); // "MiXeD"

    app.case_selection_active(&ctx, edit_id(), active, 1);
    assert_eq!(app.tabs[active].text, "keep MIXED keep", "1 = upper");

    app.case_selection_active(&ctx, edit_id(), active, 0);
    assert_eq!(app.tabs[active].text, "keep mixed keep", "0 = lower");

    app.case_selection_active(&ctx, edit_id(), active, 2);
    assert_eq!(app.tabs[active].text, "keep Mixed keep", "2 = title");
}

#[test]
fn case_selection_reselects_the_converted_span() {
    // The converted text can change LENGTH (e.g. 'ß' -> "SS"), so the new
    // selection must be recomputed from the converted string, not reused.
    let (mut app, ctx) = app_with("c.md", "straße x");
    let active = app.active;
    select(&ctx, 0, 6); // "straße"
    app.case_selection_active(&ctx, edit_id(), active, 1);
    assert_eq!(app.tabs[active].text, "STRASSE x");
    assert_eq!(
        selection(&ctx),
        (0, 7),
        "the selection must cover the LONGER uppercased span"
    );
}

#[test]
fn case_selection_with_a_collapsed_caret_toasts_instead_of_editing() {
    let (mut app, ctx) = app_with("c.md", "nothing selected");
    let active = app.active;
    select(&ctx, 3, 3);
    app.case_selection_active(&ctx, edit_id(), active, 1);
    assert_eq!(
        app.tabs[active].text, "nothing selected",
        "a collapsed caret must not edit the buffer"
    );
    assert!(
        app.toast
            .as_deref()
            .is_some_and(|t| t.contains("Select some text")),
        "the user must be told WHY nothing happened, got: {:?}",
        app.toast
    );
}

// ---- active_selection_on_list ----

#[test]
fn active_selection_on_list_detects_list_lines_and_respects_its_gates() {
    let (mut app, ctx) = app_with("l.md", "- one\nplain\n- two\n");
    let active = app.active;

    select(&ctx, 0, 0); // caret on "- one"
    assert!(app.active_selection_on_list(&ctx, edit_id(), active));

    select(&ctx, 6, 6); // caret on "plain"
    assert!(!app.active_selection_on_list(&ctx, edit_id(), active));

    // A selection spanning a plain line AND a list line counts (any-of).
    select(&ctx, 6, 17);
    assert!(app.active_selection_on_list(&ctx, edit_id(), active));

    // Gate 1: smart_lists off.
    select(&ctx, 0, 0);
    app.config.editor.smart_lists = false;
    assert!(
        !app.active_selection_on_list(&ctx, edit_id(), active),
        "smart_lists=false must disable list awareness"
    );
    app.config.editor.smart_lists = true;

    // Gate 2: not a note-shaped file.
    let (mut src, sctx) = app_with("l.rs", "- one\n");
    let sactive = src.active;
    select(&sctx, 0, 0);
    assert!(
        !src.active_selection_on_list(&sctx, edit_id(), sactive),
        "a .rs buffer must not get markdown list behaviour"
    );
    let _ = &mut src;
}

#[test]
fn active_selection_on_list_ending_at_a_line_start_excludes_the_next_line() {
    // Selecting "plain\n" exactly (chars 6..12) ends at the start of "- two",
    // which must NOT be pulled in — otherwise a Tab press would indent a list
    // line the user never selected.
    let (app, ctx) = app_with("l.md", "- one\nplain\n- two\n");
    select(&ctx, 6, 12);
    assert!(
        !app.active_selection_on_list(&ctx, edit_id(), app.active),
        "a selection ending at a line start must not include that line"
    );
}

// ---- indent_list_lines_active ----

#[test]
fn indent_list_lines_indents_and_outdents_the_touched_lines() {
    let (mut app, ctx) = app_with("i.md", "- one\n- two\n");
    let active = app.active;
    let width = app.config.editor.tab_width;
    let pad = " ".repeat(width);

    select(&ctx, 0, 0);
    assert!(app.indent_list_lines_active(&ctx, edit_id(), active, 1));
    assert_eq!(
        app.tabs[active].text,
        format!("{pad}- one\n- two\n"),
        "indent adds one tab_width to the caret's list line only"
    );

    assert!(app.indent_list_lines_active(&ctx, edit_id(), active, -1));
    assert_eq!(
        app.tabs[active].text, "- one\n- two\n",
        "outdent is the exact inverse"
    );
}

#[test]
fn indent_list_lines_reports_false_when_it_changes_nothing() {
    // The bool is load-bearing: the Tab handler falls back to space-indent when
    // this returns false. Outdenting an already-flush list must not claim a
    // change, or Tab would silently do nothing at all.
    let (mut app, ctx) = app_with("i.md", "- one\n");
    let active = app.active;
    select(&ctx, 0, 0);
    assert!(
        !app.indent_list_lines_active(&ctx, edit_id(), active, -1),
        "outdent at column 0 changes nothing => must report false"
    );
    assert_eq!(app.tabs[active].text, "- one\n");
    // And with no editor state at all.
    let (mut app2, ctx2) = app_with("i2.md", "- one\n");
    let a2 = app2.active;
    assert!(!app2.indent_list_lines_active(&ctx2, edit_id(), a2, 1));
}

// ---- toggle_task_checkbox_active ----

#[test]
fn toggle_task_checkbox_cycles_unchecked_and_checked() {
    let (mut app, ctx) = app_with("t.md", "- [ ] todo\n");
    let active = app.active;
    select(&ctx, 0, 0);

    app.toggle_task_checkbox_active(&ctx, edit_id(), active);
    assert_eq!(
        app.tabs[active].text, "- [x] todo\n",
        "unchecked -> checked"
    );

    app.toggle_task_checkbox_active(&ctx, edit_id(), active);
    assert_eq!(
        app.tabs[active].text, "- [ ] todo\n",
        "checked -> unchecked"
    );
}

#[test]
fn toggle_task_checkbox_spans_a_multi_line_selection() {
    let (mut app, ctx) = app_with("t.md", "- [ ] a\n- [ ] b\n");
    let active = app.active;
    select(&ctx, 0, 15); // both lines
    app.toggle_task_checkbox_active(&ctx, edit_id(), active);
    assert_eq!(
        app.tabs[active].text, "- [x] a\n- [x] b\n",
        "every selected task line toggles, not just the first"
    );
}

// ---- auto_indent_newline (Enter: smart list continuation + carried indent) ----
//
// Driven through the public entry rather than its private `smart_list_newline`
// helper: that is the path Enter actually takes, and it exercises the gates
// (collapsed-caret-only, smart_lists, note-shaped) along with the helper.
// The returned bool is load-bearing — false means "let egui insert the plain
// newline itself", so a wrong `true` silently swallows the keystroke.

#[test]
fn auto_indent_newline_continues_a_list_marker() {
    let (mut app, ctx) = app_with("s.md", "- one");
    let active = app.active;
    select(&ctx, 5, 5); // caret at end of the item
    assert!(app.auto_indent_newline(&ctx, edit_id(), active));
    assert_eq!(app.tabs[active].text, "- one\n- ");
    assert_eq!(selection(&ctx), (8, 8), "caret lands after the new marker");
}

#[test]
fn auto_indent_newline_on_an_empty_item_exits_the_list() {
    // Enter on a dangling "- " must CLEAR the marker (exit the list), not add
    // another one — otherwise the list is impossible to end with the keyboard.
    let (mut app, ctx) = app_with("s.md", "- one\n- ");
    let active = app.active;
    select(&ctx, 8, 8);
    assert!(app.auto_indent_newline(&ctx, edit_id(), active));
    assert_eq!(
        app.tabs[active].text, "- one\n",
        "the dangling marker is dropped"
    );
    assert_eq!(
        selection(&ctx),
        (6, 6),
        "caret lands at the blank line start"
    );
}

#[test]
fn auto_indent_newline_renumbers_an_ordered_list() {
    let (mut app, ctx) = app_with("s.md", "1. one");
    let active = app.active;
    select(&ctx, 6, 6);
    assert!(app.auto_indent_newline(&ctx, edit_id(), active));
    assert_eq!(
        app.tabs[active].text, "1. one\n2. ",
        "the NEXT ordinal is inserted, not a repeat of 1."
    );
}

#[test]
fn auto_indent_newline_mid_line_defers_to_egui() {
    // A mid-line Enter must split normally: returning false hands the keystroke
    // back to egui (which also keeps egui's undo grouping for the common case).
    let (mut app, ctx) = app_with("s.md", "- one");
    let active = app.active;
    select(&ctx, 3, 3);
    assert!(
        !app.auto_indent_newline(&ctx, edit_id(), active),
        "a mid-line Enter must not be handled here"
    );
    assert_eq!(
        app.tabs[active].text, "- one",
        "and must not edit the buffer"
    );
}

#[test]
fn auto_indent_newline_declines_for_a_selection_or_no_state() {
    // Enter with a SELECTION should replace it (egui's job), and with no editor
    // state there is nothing to do.
    let (mut app, ctx) = app_with("s.md", "- one");
    let active = app.active;
    assert!(
        !app.auto_indent_newline(&ctx, edit_id(), active),
        "no stored state => false"
    );
    select(&ctx, 0, 5);
    assert!(
        !app.auto_indent_newline(&ctx, edit_id(), active),
        "a non-collapsed selection must be left to egui"
    );
    assert_eq!(app.tabs[active].text, "- one");
}

#[test]
fn auto_indent_newline_list_continuation_is_gated_to_note_files() {
    // A .rs buffer whose line happens to start with "- " must NOT get list
    // continuation. It carries the plain indent instead.
    let (mut app, ctx) = app_with("s.rs", "- one");
    let active = app.active;
    select(&ctx, 5, 5);
    app.auto_indent_newline(&ctx, edit_id(), active);
    assert!(
        !app.tabs[active].text.contains("\n- "),
        "a source file must not continue markdown list markers, got: {:?}",
        app.tabs[active].text
    );
}

#[test]
fn auto_indent_newline_carries_the_leading_indent() {
    // Non-list path: Enter at the end of an indented line repeats that indent.
    let (mut app, ctx) = app_with("s.rs", "    indented");
    let active = app.active;
    select(&ctx, 12, 12);
    assert!(app.auto_indent_newline(&ctx, edit_id(), active));
    assert_eq!(
        app.tabs[active].text, "    indented\n    ",
        "the new line must carry the same leading indent"
    );
}

// ---- jump_matching_bracket ----

#[test]
fn jump_matching_bracket_moves_the_caret_to_the_pair() {
    //             0123456789.....
    //             fn f() { body }
    //                    ^7      ^14
    let (mut app, ctx) = app_with("b.rs", "fn f() { body }");
    let active = app.active;
    select(&ctx, 7, 7); // caret at the '{'
    app.jump_matching_bracket(&ctx, edit_id(), active);
    let (lo, hi) = selection(&ctx);
    assert_eq!(lo, hi, "the jump collapses to a caret");
    assert_eq!(
        lo, 14,
        "the caret lands ON the matching '}}', not past it — the jump is \
         symmetric, so a second press must return to the '{{'"
    );

    // The symmetry that index is chosen for: jump back.
    app.jump_matching_bracket(&ctx, edit_id(), active);
    assert_eq!(
        selection(&ctx),
        (7, 7),
        "jumping from the close bracket must return to the open one"
    );
}

#[test]
fn jump_matching_bracket_off_a_bracket_is_a_noop() {
    let (mut app, ctx) = app_with("b.rs", "fn f() { body }");
    let active = app.active;
    select(&ctx, 11, 11); // inside "body", not on a bracket
    app.jump_matching_bracket(&ctx, edit_id(), active);
    assert_eq!(
        selection(&ctx),
        (11, 11),
        "no bracket at the caret => the caret must not move"
    );
}

// ---- format_table_active ----

#[test]
fn format_table_aligns_the_pipe_table_under_the_caret() {
    let (mut app, ctx) = app_with("tab.md", "|a|bb|\n|-|-|\n|ccc|d|\n");
    let active = app.active;
    select(&ctx, 0, 0);
    app.format_table_active(&ctx, edit_id(), active);
    let out = &app.tabs[active].text;
    assert!(
        out.lines().all(|l| l.starts_with('|')),
        "every row stays a pipe row: {out:?}"
    );
    // Alignment means every row is the same rendered width.
    let widths: Vec<usize> = out.lines().map(str::len).collect();
    assert!(
        widths.windows(2).all(|w| w[0] == w[1]),
        "an aligned table has equal-width rows, got {widths:?} in {out:?}"
    );
}

#[test]
fn format_table_leaves_a_non_table_buffer_alone() {
    let (mut app, ctx) = app_with("tab.md", "just prose\n");
    let active = app.active;
    select(&ctx, 0, 0);
    app.format_table_active(&ctx, edit_id(), active);
    assert_eq!(app.tabs[active].text, "just prose\n");
}

// ---- new_note_from_template ----

#[test]
fn new_note_from_template_opens_a_seeded_tab_per_kind() {
    let mut app = {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        ScribeApp::new_test(cfg)
    };
    for (kind, needle) in [
        (NoteTemplate::Checklist, "# Checklist"),
        (NoteTemplate::Meeting, "# Meeting notes"),
        (NoteTemplate::Daily, "- [ ] "),
    ] {
        let before = app.tabs.len();
        app.new_note_from_template(kind);
        assert_eq!(app.tabs.len(), before + 1, "{kind:?} opens a new tab");
        let text = &app.tabs[app.active].text;
        assert!(
            text.contains(needle),
            "{kind:?} must seed its body (looking for {needle:?}), got: {text:?}"
        );
    }
}
