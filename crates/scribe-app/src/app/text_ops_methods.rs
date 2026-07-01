//! Editor text operations (indent, auto-indent, bracket-jump, datetime, duplicate, comment, line ops) — extracted from `mod.rs` (A-01 wave 2).
#![allow(clippy::wildcard_imports)]

use super::*;

/// P3-3 — built-in "new note from template" seeds (ride the plain-buffer path,
/// no new subsystem). Bodies are checklist-first so the task features are
/// discoverable immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoteTemplate {
    Checklist,
    Meeting,
    Daily,
}

impl NoteTemplate {
    fn label(self) -> &'static str {
        match self {
            NoteTemplate::Checklist => "checklist",
            NoteTemplate::Meeting => "meeting",
            NoteTemplate::Daily => "daily",
        }
    }

    fn body(self) -> &'static str {
        match self {
            NoteTemplate::Checklist => "# Checklist\n\n- [ ] \n- [ ] \n- [ ] \n",
            NoteTemplate::Meeting => {
                "# Meeting notes\n\n**Date:** \n**Attendees:** \n\n\
                 ## Agenda\n\n- \n\n## Decisions\n\n- \n\n## Action items\n\n- [ ] \n"
            }
            NoteTemplate::Daily => {
                "# Daily note\n\n## Focus\n\n- [ ] \n\n## Notes\n\n- \n\n## Done\n\n- [x] \n"
            }
        }
    }
}

impl ScribeApp {
    /// Replace the active editor's selection (or insert at the caret) with
    /// `tab_width` spaces, then advance the caret — the Tab-key handler when
    /// `insert_spaces` is enabled. Operates directly on the TextEdit state for
    /// `id` so the caret tracks the edit.
    pub(super) fn indent_with_spaces(&mut self, ctx: &egui::Context, id: egui::Id, active: usize) {
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let lo = range.primary.index.min(range.secondary.index);
        let hi = range.primary.index.max(range.secondary.index);
        let (new_text, new_idx) = apply_indent(
            &self.tabs[active].text,
            lo,
            hi,
            self.config.editor.tab_width,
        );
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(new_idx),
            )));
        state.store(ctx, id);
    }

    /// Auto-indent on Enter (#107): insert a newline that keeps the current
    /// line's leading whitespace, so indentation carries to the next line. Only
    /// acts on a single caret (no selection); returns false so the caller lets
    /// egui handle Enter normally otherwise.
    pub(super) fn auto_indent_newline(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) -> bool {
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return false;
        };
        let Some(range) = state.cursor.char_range() else {
            return false;
        };
        // Only a collapsed caret — a selection+Enter should replace, which we
        // leave to egui.
        if range.primary.index != range.secondary.index {
            return false;
        }
        let cursor = range.primary.index;

        // P0-2 — smart list continuation: for note files with smart-lists on,
        // continue the list marker (or terminate on an empty item) when Enter is
        // pressed at the end of a list line.
        if self.config.editor.smart_lists && self.note_file_active(active) {
            if let Some((new_text, new_idx)) = self.smart_list_newline(active, cursor) {
                self.tabs[active].set_text(new_text);
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(
                        egui::text::CCursor::new(new_idx),
                    )));
                state.store(ctx, id);
                return true;
            }
        }

        let (new_text, new_idx) = newline_with_indent(&self.tabs[active].text, cursor);
        // No indent to carry → let egui insert the plain newline (cheaper, and
        // keeps egui's own undo grouping for the common case).
        if new_idx == cursor + 1 {
            return false;
        }
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(new_idx),
            )));
        state.store(ctx, id);
        true
    }

    /// P0-2 helper — compute the `(new_text, new_caret)` for a smart list
    /// continuation, or `None` when the current line is not a list item, or the
    /// caret is not at the end of the line's content (fall back to plain Enter).
    fn smart_list_newline(&self, active: usize, cursor: usize) -> Option<(String, usize)> {
        let text = &self.tabs.get(active)?.text;
        let bcur = char_to_byte(text, cursor);
        let line_start_b = text[..bcur].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end_b = text[bcur..]
            .find('\n')
            .map(|i| bcur + i)
            .unwrap_or(text.len());
        // Only continue when the caret is at the end of the line's content — a
        // mid-line Enter should split normally.
        if bcur != line_end_b {
            return None;
        }
        let line = &text[line_start_b..line_end_b];
        let cont = scribe_core::md_ops::continue_list_marker(line);
        if cont.clear_current_line {
            // Empty item → drop the dangling marker (exit the list). The line
            // becomes blank and the caret lands at its start.
            let mut new_text = String::with_capacity(text.len());
            new_text.push_str(&text[..line_start_b]);
            new_text.push_str(&text[line_end_b..]);
            let new_caret = text[..line_start_b].chars().count();
            return Some((new_text, new_caret));
        }
        let marker = cont.marker_to_insert?;
        let insert = format!("\n{marker}");
        let mut new_text = text.to_string();
        new_text.insert_str(bcur, &insert);
        Some((new_text, cursor + insert.chars().count()))
    }

    /// Move the caret to the bracket paired with the one at/next to the caret
    /// (Ctrl+M). No-op when the caret is not on a bracket pair. Bounded to the
    /// same buffer size as the bracket-match highlight.
    pub(super) fn jump_matching_bracket(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) {
        if self.tabs[active].text.len() > 500_000 {
            return;
        }
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let caret = range.primary.index;
        let Some((open_ci, close_ci)) =
            matching_bracket_char_indices(&self.tabs[active].text, caret)
        else {
            return;
        };
        // The caret sits on (or just past) one bracket of the pair; jump to the
        // other end. Pick whichever end the caret is NOT adjacent to.
        let target = if caret.abs_diff(open_ci) <= caret.abs_diff(close_ci) {
            close_ci
        } else {
            open_ci
        };
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(target),
            )));
        state.store(ctx, id);
    }

    /// Insert a UTC ISO-8601 timestamp at the caret, replacing any selection.
    pub(super) fn insert_datetime_at_caret(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) {
        let ts = crate::datetime::now_iso8601_utc();
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let lo = range.primary.index.min(range.secondary.index);
        let hi = range.primary.index.max(range.secondary.index);
        let text = &self.tabs[active].text;
        let lo_b = char_to_byte(text, lo);
        let hi_b = char_to_byte(text, hi);
        let mut new_text = String::with_capacity(text.len() + ts.len());
        new_text.push_str(&text[..lo_b]);
        new_text.push_str(&ts);
        new_text.push_str(&text[hi_b..]);
        self.tabs[active].set_text(new_text);
        let new_caret = lo + ts.chars().count();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(new_caret),
            )));
        state.store(ctx, id);
        self.status = format!("inserted {ts}");
    }

    /// Duplicate the current selection (or the caret's line when there is no
    /// selection), inserting the copy immediately after and moving the caret
    /// onto the copy.
    pub(super) fn duplicate_selection(&mut self, ctx: &egui::Context, id: egui::Id, active: usize) {
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let (prim, sec) = (range.primary.index, range.secondary.index);
        let text = &self.tabs[active].text;
        let (insert_at_b, copy, new_caret) = if prim != sec {
            // Selection: insert a copy of [lo,hi) right after hi; caret ends
            // after the inserted copy.
            let lo = prim.min(sec);
            let hi = prim.max(sec);
            let lo_b = char_to_byte(text, lo);
            let hi_b = char_to_byte(text, hi);
            (hi_b, text[lo_b..hi_b].to_string(), hi + (hi - lo))
        } else {
            // Collapsed caret: duplicate the whole line below, keeping the
            // caret's column on the new copy.
            let caret_b = char_to_byte(text, prim);
            let start_b = text[..caret_b].rfind('\n').map_or(0, |i| i + 1);
            let end_b = text[caret_b..]
                .find('\n')
                .map_or(text.len(), |i| caret_b + i);
            let line = text[start_b..end_b].to_string();
            // New caret = same column, one line down. Column in chars:
            let col = text[start_b..caret_b].chars().count();
            let dup_line_start_chars = prim + (line.chars().count() - col) + 1;
            (end_b, format!("\n{line}"), dup_line_start_chars + col)
        };
        let mut new_text = String::with_capacity(text.len() + copy.len());
        new_text.push_str(&text[..insert_at_b]);
        new_text.push_str(&copy);
        new_text.push_str(&text[insert_at_b..]);
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(new_caret),
            )));
        state.store(ctx, id);
    }
    /// F-016 — Toggle the line-comment prefix on every line touched by the
    /// active selection (or the cursor line if no selection). The prefix is
    /// picked from `comment_prefix_for_extension` based on the active doc's
    /// language hint; unknown languages fall back to no-op + status toast.
    ///
    /// Behaviour: if EVERY non-blank touched line already starts with the
    /// prefix, strip one prefix occurrence per line; otherwise prepend the
    /// prefix to every non-blank line.
    pub(super) fn toggle_comment_active(&mut self) {
        if self.active >= self.tabs.len() {
            return;
        }
        let lang = self.tabs[self.active].doc.language_hint();
        let prefix = lang
            .as_deref()
            .and_then(comment_prefix_for_extension)
            .unwrap_or("");
        if prefix.is_empty() {
            self.toast = Some("Commenting isn't available for this file type.".to_string());
            return;
        }
        let text = &mut self.tabs[self.active].text;
        // Cheap full-buffer rewrite: split, decide direction by ALL-vs-ANY,
        // toggle, rejoin. The user's "selection" surface is the whole
        // buffer until we wire egui's selection range through to the rope
        // helpers (Phase 15 KEYSTONE follow-up F-009).
        let lines: Vec<&str> = text.lines().collect();
        let non_blank = lines.iter().any(|l| !l.trim().is_empty());
        if !non_blank {
            return;
        }
        let all_commented = lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .all(|l| l.trim_start().starts_with(prefix));
        let pfx_with_space = format!("{prefix} ");
        let new_lines: Vec<String> = lines
            .iter()
            .map(|l| {
                if l.trim().is_empty() {
                    (*l).to_string()
                } else if all_commented {
                    // Strip the prefix (and one trailing space if present).
                    let trimmed = l.trim_start();
                    let leading_ws_len = l.len() - trimmed.len();
                    let after_pfx = trimmed
                        .strip_prefix(&pfx_with_space)
                        .or_else(|| trimmed.strip_prefix(prefix))
                        .unwrap_or(trimmed);
                    format!("{}{}", &l[..leading_ws_len], after_pfx)
                } else {
                    let trimmed = l.trim_start();
                    let leading_ws_len = l.len() - trimmed.len();
                    format!("{}{pfx_with_space}{trimmed}", &l[..leading_ws_len])
                }
            })
            .collect();
        // Preserve a trailing newline if the original buffer had one.
        let trailing_nl = text.ends_with('\n');
        *text = new_lines.join("\n");
        if trailing_nl {
            text.push('\n');
        }
        let i = self.active;
        self.tabs[i].edit_gen = self.tabs[i].edit_gen.wrapping_add(1);
    }

    /// F-017 — Swap the cursor line with the neighbour `dir` rows away (-1 =
    /// up, +1 = down). No-op at the buffer's first/last line. The cursor
    /// "line" is read from `last_cursor_line_col`; if absent, defaults to
    /// line 0 (start of buffer) so the action is still observable on a
    /// fresh buffer.
    pub(super) fn move_cursor_line(&mut self, dir: i32) {
        if self.active >= self.tabs.len() {
            return;
        }
        let ln = self
            .last_cursor_line_col
            .map(|(l, _)| l.saturating_sub(1))
            .unwrap_or(0);
        let text = &mut self.tabs[self.active].text;
        let trailing_nl = text.ends_with('\n');
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        // split('\n') with a trailing newline produces a trailing "" — drop it.
        if trailing_nl && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if lines.is_empty() {
            return;
        }
        // Guard `ln` too, not just `target`: the cursor line (from
        // `last_cursor_line_col`, a 1-based line count that can point AT the
        // post-final-newline empty line) can equal `lines.len()` after the
        // trailing-"" pop above, while `target = ln - 1` is still in range — so
        // `lines.swap(ln, target)` would index `ln` out of bounds and abort
        // (`panic = "abort"`). The sibling line-ops (`duplicate_cursor_line`,
        // `join_cursor_line_with_next`) already guard `ln` this way.
        if ln >= lines.len() {
            return;
        }
        let target = (ln as i32) + dir;
        if target < 0 || (target as usize) >= lines.len() {
            return;
        }
        lines.swap(ln, target as usize);
        // Track the cursor to the moved line.
        let new_ln = target as usize + 1;
        let new_col = self.last_cursor_line_col.map(|(_, c)| c).unwrap_or(1);
        self.last_cursor_line_col = Some((new_ln, new_col));
        *text = lines.join("\n");
        if trailing_nl {
            text.push('\n');
        }
        let i = self.active;
        self.tabs[i].edit_gen = self.tabs[i].edit_gen.wrapping_add(1);
    }

    /// F-017 — Duplicate the cursor line in-place: the new copy lands on the
    /// row immediately below.
    pub(super) fn duplicate_cursor_line(&mut self) {
        if self.active >= self.tabs.len() {
            return;
        }
        let ln = self
            .last_cursor_line_col
            .map(|(l, _)| l.saturating_sub(1))
            .unwrap_or(0);
        let text = &mut self.tabs[self.active].text;
        let trailing_nl = text.ends_with('\n');
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        if trailing_nl && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if ln >= lines.len() {
            return;
        }
        let copy = lines[ln].clone();
        lines.insert(ln + 1, copy);
        *text = lines.join("\n");
        if trailing_nl {
            text.push('\n');
        }
        let i = self.active;
        self.tabs[i].edit_gen = self.tabs[i].edit_gen.wrapping_add(1);
    }

    // -------------------------------------------------------------------
    // Note-usability caret operations (P0–P2). Each loads the live
    // `TextEditState`, calls a pure `scribe_core::md_ops` transform, writes the
    // result back, and restores a sensible caret — mirroring the existing
    // caret-command methods above.
    // -------------------------------------------------------------------

    /// True when the active document is a note-shaped file (markdown / plain
    /// text / untitled scratch) — the surface where smart-lists, list-aware
    /// indent, and smart link-paste apply. Code files are excluded so their
    /// indentation/behaviour is unchanged.
    pub(super) fn note_file_active(&self, active: usize) -> bool {
        match self.tabs.get(active).and_then(|t| t.doc.language_hint()) {
            None => true,
            Some(l) => matches!(l.as_str(), "md" | "markdown" | "txt" | "text"),
        }
    }

    /// The active editor's selection substring, or `None` when there is no
    /// selection (collapsed caret) or no live state.
    pub(super) fn active_selection_text(
        &self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) -> Option<String> {
        let state = egui::TextEdit::load_state(ctx, id)?;
        let range = state.cursor.char_range()?;
        let (lo, hi) = (
            range.primary.index.min(range.secondary.index),
            range.primary.index.max(range.secondary.index),
        );
        if lo == hi {
            return None;
        }
        let text = &self.tabs.get(active)?.text;
        let lo_b = char_to_byte(text, lo);
        let hi_b = char_to_byte(text, hi);
        Some(text[lo_b..hi_b].to_string())
    }

    /// The 0-based `(lo_line, hi_line)` line span touched by the current
    /// caret/selection, plus the primary caret char index. `None` on no state.
    fn active_line_span(
        &self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) -> Option<(usize, usize, usize)> {
        let state = egui::TextEdit::load_state(ctx, id)?;
        let range = state.cursor.char_range()?;
        let (lo, hi) = (
            range.primary.index.min(range.secondary.index),
            range.primary.index.max(range.secondary.index),
        );
        let text = &self.tabs.get(active)?.text;
        let line_of = |ci: usize| -> usize {
            let b = char_to_byte(text, ci);
            text[..b].bytes().filter(|&c| c == b'\n').count()
        };
        let lo_line = line_of(lo);
        let mut hi_line = line_of(hi);
        // A selection ending exactly at a line start should not pull in the next
        // line (its char just before `hi` is the newline).
        if hi > lo {
            let hb = char_to_byte(text, hi);
            if text[..hb].ends_with('\n') {
                hi_line = hi_line.saturating_sub(1);
            }
        }
        Some((lo_line, hi_line, range.primary.index))
    }

    /// True when any line the caret/selection touches is a list item (bullet /
    /// ordered / task) AND the active file is note-shaped with smart-lists on.
    pub(super) fn active_selection_on_list(
        &self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) -> bool {
        if !self.config.editor.smart_lists || !self.note_file_active(active) {
            return false;
        }
        let Some((lo, hi, _)) = self.active_line_span(ctx, id, active) else {
            return false;
        };
        let Some(text) = self.tabs.get(active).map(|t| &t.text) else {
            return false;
        };
        let lines: Vec<&str> = text.split('\n').collect();
        (lo..=hi).any(|idx| {
            lines
                .get(idx)
                .is_some_and(|l| scribe_core::md_ops::parse_list_marker(l).is_some())
        })
    }

    /// P0-3 — list-aware indent (`dir > 0`) / outdent (`dir < 0`) of the
    /// touched list lines, with ordered renumber. Returns true when it changed
    /// the buffer (so a Tab handler knows not to fall back to space-indent).
    pub(super) fn indent_list_lines_active(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
        dir: i32,
    ) -> bool {
        let Some((lo, hi, caret)) = self.active_line_span(ctx, id, active) else {
            return false;
        };
        let width = self.config.editor.tab_width;
        let Some(new_text) =
            scribe_core::md_ops::indent_list_lines(&self.tabs[active].text, lo, hi, width, dir)
        else {
            return false;
        };
        let new_len = new_text.chars().count();
        self.tabs[active].set_text(new_text);
        self.store_caret(ctx, id, caret.min(new_len));
        true
    }

    /// P0-1 — toggle / insert the GFM task checkbox on the caret / selection
    /// lines. Surfaces a toast when no list item was touched.
    pub(super) fn toggle_task_checkbox_active(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) {
        let Some((lo, hi, caret)) = self.active_line_span(ctx, id, active) else {
            return;
        };
        match scribe_core::md_ops::toggle_task_on_lines(&self.tabs[active].text, lo, hi) {
            Some(new_text) => {
                let new_len = new_text.chars().count();
                self.tabs[active].set_text(new_text);
                self.tabs[active].doc.mark_dirty();
                self.store_caret(ctx, id, caret.min(new_len));
            }
            None => {
                self.toast = Some("No list item on this line to make a checkbox.".to_string());
            }
        }
    }

    /// P0-4 — wrap-toggle the selection with `marker` (`**`/`*`/`` ` ``/`~~`).
    pub(super) fn wrap_selection_active(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
        marker: &str,
    ) {
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let (lo, hi) = (
            range.primary.index.min(range.secondary.index),
            range.primary.index.max(range.secondary.index),
        );
        let (new_text, new_lo, new_hi) =
            scribe_core::md_ops::toggle_wrap(&self.tabs[active].text, lo, hi, marker);
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(new_lo),
                egui::text::CCursor::new(new_hi),
            )));
        state.store(ctx, id);
    }

    /// P1-4 — case-convert the selection: 0 = lower, 1 = upper, 2 = title.
    pub(super) fn case_selection_active(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
        op: u8,
    ) {
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let (lo, hi) = (
            range.primary.index.min(range.secondary.index),
            range.primary.index.max(range.secondary.index),
        );
        if lo == hi {
            self.toast = Some("Select some text first to change its case.".to_string());
            return;
        }
        let text = &self.tabs[active].text;
        let lo_b = char_to_byte(text, lo);
        let hi_b = char_to_byte(text, hi);
        let sel = &text[lo_b..hi_b];
        let converted = match op {
            1 => scribe_core::text_ops::to_case(sel, true),
            2 => scribe_core::md_ops::to_title_case(sel),
            _ => scribe_core::text_ops::to_case(sel, false),
        };
        let mut new_text = String::with_capacity(text.len());
        new_text.push_str(&text[..lo_b]);
        new_text.push_str(&converted);
        new_text.push_str(&text[hi_b..]);
        let new_hi = lo + converted.chars().count();
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(lo),
                egui::text::CCursor::new(new_hi),
            )));
        state.store(ctx, id);
    }

    /// P2-1 — format the markdown pipe table under the caret (align columns).
    pub(super) fn format_table_active(&mut self, ctx: &egui::Context, id: egui::Id, active: usize) {
        let Some((caret_line, _, caret)) = self.active_line_span(ctx, id, active) else {
            return;
        };
        let text = self.tabs[active].text.clone();
        let Some((lo, hi)) = scribe_core::md_ops::table_block_bounds(&text, caret_line) else {
            self.toast = Some("Put the caret inside a markdown table first.".to_string());
            return;
        };
        let lines: Vec<&str> = text.split('\n').collect();
        let block = lines[lo..=hi].join("\n");
        let formatted = scribe_core::md_ops::format_markdown_table(&block);
        if formatted == block {
            return;
        }
        let mut new_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        new_lines.splice(lo..=hi, formatted.split('\n').map(str::to_string));
        let new_text = new_lines.join("\n");
        let new_len = new_text.chars().count();
        self.tabs[active].set_text(new_text);
        self.tabs[active].doc.mark_dirty();
        self.store_caret(ctx, id, caret.min(new_len));
        self.status = "formatted table".to_string();
    }

    /// P3-3 — open a fresh note tab seeded from a built-in template.
    pub(super) fn new_note_from_template(&mut self, kind: NoteTemplate) {
        self.new_tab();
        let active = self.active;
        self.tabs[active].set_text(kind.body().to_string());
        self.status = format!("new note: {}", kind.label());
    }

    /// P2-2 — auto-pair (default-OFF `editor.auto_pair`). When an opening or
    /// closing bracket/quote/backtick is typed, apply the pure
    /// [`scribe_core::md_ops::auto_pair_action`] decision: wrap a selection,
    /// insert the pair (caret between), or type over an existing closing char.
    /// Consumes the originating `Event::Text` so egui does not also insert it.
    pub(super) fn handle_auto_pair(&mut self, ctx: &egui::Context, id: egui::Id, active: usize) {
        if !self.config.editor.auto_pair {
            return;
        }
        // A single-char Text event that is a pair-relevant char.
        let typed: Option<char> = ctx.input(|i| {
            i.events.iter().find_map(|e| {
                let egui::Event::Text(s) = e else { return None };
                let mut it = s.chars();
                let c = it.next()?;
                if it.next().is_some() {
                    return None;
                }
                if scribe_core::md_ops::auto_pair_close(c).is_some() || matches!(c, ')' | ']' | '}')
                {
                    Some(c)
                } else {
                    None
                }
            })
        });
        let Some(c) = typed else {
            return;
        };
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let (lo, hi) = (
            range.primary.index.min(range.secondary.index),
            range.primary.index.max(range.secondary.index),
        );
        let text = &self.tabs[active].text;
        let char_after = text.chars().nth(hi);
        let action = scribe_core::md_ops::auto_pair_action(c, lo != hi, char_after);
        if matches!(action, scribe_core::md_ops::AutoPairAction::Passthrough) {
            return;
        }
        // Consume the char so egui's TextEdit does not ALSO insert it.
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                !matches!(e, egui::Event::Text(s)
                    if s.chars().count() == 1 && s.starts_with(c))
            });
        });
        let lo_b = char_to_byte(text, lo);
        let hi_b = char_to_byte(text, hi);
        use scribe_core::md_ops::AutoPairAction as A;
        let (new_text, sel_lo, sel_hi) = match action {
            A::Wrap { open, close } => {
                let mut s = String::with_capacity(text.len() + 2);
                s.push_str(&text[..lo_b]);
                s.push(open);
                s.push_str(&text[lo_b..hi_b]);
                s.push(close);
                s.push_str(&text[hi_b..]);
                (s, lo + 1, hi + 1)
            }
            A::InsertPair { open, close } => {
                let mut s = String::with_capacity(text.len() + 2);
                s.push_str(&text[..lo_b]);
                s.push(open);
                s.push(close);
                s.push_str(&text[lo_b..]);
                (s, lo + 1, lo + 1)
            }
            A::TypeOver => {
                // No text change; step the caret over the existing closing char.
                (text.clone(), lo + 1, lo + 1)
            }
            A::Passthrough => return,
        };
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(sel_lo),
                egui::text::CCursor::new(sel_hi),
            )));
        state.store(ctx, id);
    }

    /// Store a single collapsed caret at `char_idx` in the editor state for `id`.
    fn store_caret(&self, ctx: &egui::Context, id: egui::Id, char_idx: usize) {
        if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(char_idx),
                )));
            state.store(ctx, id);
        }
    }

    /// F-017 — Join the cursor line with the next: trims the trailing
    /// whitespace of the cursor line + the leading whitespace of the next,
    /// joins them with a single space (the standard editor convention).
    pub(super) fn join_cursor_line_with_next(&mut self) {
        if self.active >= self.tabs.len() {
            return;
        }
        let ln = self
            .last_cursor_line_col
            .map(|(l, _)| l.saturating_sub(1))
            .unwrap_or(0);
        let text = &mut self.tabs[self.active].text;
        let trailing_nl = text.ends_with('\n');
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        if trailing_nl && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if ln + 1 >= lines.len() {
            return;
        }
        let next = lines.remove(ln + 1);
        let cur = lines[ln].trim_end().to_string();
        let nxt = next.trim_start();
        lines[ln] = if cur.is_empty() || nxt.is_empty() {
            format!("{cur}{nxt}")
        } else {
            format!("{cur} {nxt}")
        };
        *text = lines.join("\n");
        if trailing_nl {
            text.push('\n');
        }
        let i = self.active;
        self.tabs[i].edit_gen = self.tabs[i].edit_gen.wrapping_add(1);
    }
}
