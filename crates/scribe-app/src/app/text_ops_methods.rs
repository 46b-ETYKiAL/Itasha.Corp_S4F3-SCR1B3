//! Editor text operations (indent, auto-indent, bracket-jump, datetime, duplicate, comment, line ops) — extracted from `mod.rs` (A-01 wave 2).
#![allow(clippy::wildcard_imports)]

use super::*;

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
            self.toast = Some("no comment prefix for this language".to_string());
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
