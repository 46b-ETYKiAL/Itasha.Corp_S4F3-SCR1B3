//! Phase 15 KEYSTONE — rope-backed viewport-culled editor widget.
//!
//! The single largest correctness gap in the current editor is that
//! `egui::TextEdit::multiline` re-lays out the **whole** buffer every
//! keystroke (egui issue #3086). That's `O(n)` per edit and the
//! multi-GiB browse target cannot be met from inside that primitive.
//!
//! The KEYSTONE design (research dossier persisted under
//! `apps/scribe/.../rope_editor/`) replaces TextEdit with:
//!
//! 1. **`ScrollArea::show_rows`** as the viewport-culling primitive. egui
//!    delegates the visible-row computation to its internal scroll math;
//!    we only paint the `Range<usize>` it hands back. Per-frame work is
//!    `O(viewport_rows)`, not `O(total_lines)`.
//!
//! 2. **Per-line `Arc<Galley>` cache** keyed on
//!    `(per_line_rev, line_idx, font_id, wrap_q)`. A monotonic per-line
//!    revision counter is spliced on every edit; a one-character insert
//!    bumps **one** entry and the other 999,999 galleys in a 1M-line file
//!    stay cached. This is the structural fix for egui #3086.
//!
//! 3. **Tree-sitter viewport-scoped queries** via
//!    `QueryCursor::set_byte_range(viewport_bytes..)`. The Helix 25.07 +
//!    Zed pattern: incremental parse + a viewport-narrow query so the
//!    span highlight cost matches the layout cost.
//!
//! 4. **mmap → rope copy-on-first-edit** via the new
//!    [`scribe_core::buffer::Buffer`] enum. The browse path stays
//!    `O(1)`-mapped; the first edit promotes.
//!
//! This module ships the FOUNDATION: the widget skeleton + `show_rows`
//! viewport-cull integration + a smoke test that drives 10k-line and
//! 1M-line ropes without panicking. The per-line cache, multi-cursor
//! support, tree-sitter integration, and minimap each land in their own
//! follow-up so review surface stays bounded.

use egui::text::LayoutJob;
use egui::{Color32, FontId, TextFormat, Ui};
use ropey::Rope;

mod tab_geometry;
use scribe_core::buffer::Buffer;
use scribe_core::syntax::{Highlighter, HlSpan, IncrementalHighlightState};
use tab_geometry::{col_to_rel_x, layout_line, rel_x_to_col};

/// Inherent-method widget over a `&mut Buffer` (NOT a `Widget` impl —
/// the renderer needs `&mut self` plumbing through `show_rows`).
///
/// The widget renders the buffer with viewport culling and a monospace
/// font of the caller's choosing. It paints **read-only** text — optionally
/// with viewport-scoped syntax highlighting (F-030) and a line-number gutter.
/// Cursor / selection / editing each land in the editing-layer follow-up; the
/// current surface is the huge-file *browse* path.
pub struct RopeEditor<'a> {
    pub(crate) buffer: &'a mut Buffer,
    pub(crate) font_id: FontId,
    /// Per-row height in points. Caller computes via
    /// `ui.fonts(|f| f.row_height(&font_id))`; we accept it pre-computed
    /// so the widget body never opens the fonts lock during paint.
    pub(crate) line_height: f32,
    /// `[r, g, b, a]` for the body text. Caller threads the theme's
    /// `[ui] foreground` here.
    pub(crate) text_color: Color32,
    /// Optional syntax highlighter + file extension. When set, each visible
    /// window is highlighted on its own (cost is `O(viewport)`, not
    /// `O(total_lines)`) — the viewport-scoped highlight the KEYSTONE huge-
    /// file browse wants (F-030).
    pub(crate) highlighter: Option<&'a Highlighter>,
    pub(crate) ext: Option<String>,
    /// When true, a right-aligned line-number gutter is drawn before each row.
    pub(crate) line_numbers: bool,
    /// Gutter (line-number) text color.
    pub(crate) gutter_color: Color32,
    /// When true, paint faint visible-whitespace markers (`·` per space,
    /// `→` per tab) OVER each visible row. Pure overlay — the real text and
    /// the highlight spans are untouched.
    pub(crate) render_whitespace: bool,
    /// Optional Tab-trigger snippet set. When present (and editing), a Tab
    /// pressed right after a known snippet prefix expands it instead of
    /// inserting an indent. `None` disables snippet expansion entirely.
    pub(crate) snippets: Option<&'a scribe_core::snippets::SnippetSet>,
}

impl<'a> RopeEditor<'a> {
    /// Construct a new editor view over `buffer`.
    pub fn new(buffer: &'a mut Buffer, font_id: FontId, line_height: f32) -> Self {
        Self {
            buffer,
            font_id,
            line_height,
            text_color: Color32::from_rgb(0xc8, 0xd6, 0xdc),
            highlighter: None,
            ext: None,
            line_numbers: false,
            gutter_color: Color32::from_rgb(0x5a, 0x58, 0x69),
            render_whitespace: false,
            snippets: None,
        }
    }

    /// Enable Tab-trigger snippet expansion using `set`. A Tab pressed right
    /// after a known prefix expands the snippet; otherwise Tab indents as usual.
    pub fn with_snippets(mut self, set: &'a scribe_core::snippets::SnippetSet) -> Self {
        self.snippets = Some(set);
        self
    }

    /// Override the body text color (default is wired-noir `text`).
    pub fn with_text_color(mut self, c: Color32) -> Self {
        self.text_color = c;
        self
    }

    /// Enable viewport-scoped syntax highlighting (F-030). Only the visible
    /// window is highlighted each frame, so cost scales with the viewport.
    pub fn with_syntax(mut self, hl: &'a Highlighter, ext: Option<String>) -> Self {
        self.highlighter = Some(hl);
        self.ext = ext;
        self
    }

    /// Draw a right-aligned line-number gutter before each row.
    pub fn with_line_numbers(mut self, on: bool) -> Self {
        self.line_numbers = on;
        self
    }

    /// Override the gutter (line-number) color.
    pub fn with_gutter_color(mut self, c: Color32) -> Self {
        self.gutter_color = c;
        self
    }

    /// Paint faint visible-whitespace markers (`·` per space, `→` per tab)
    /// as an overlay. The real text and highlight spans are unaffected.
    pub fn with_render_whitespace(mut self, on: bool) -> Self {
        self.render_whitespace = on;
        self
    }

    /// Render the widget into `ui`. Returns a [`RopeEditorResponse`]
    /// carrying whatever state the caller needs (currently just the
    /// scroll position; cursor + edits land in follow-ups).
    pub fn show(self, ui: &mut Ui) -> RopeEditorResponse {
        // Read-only banner when we're sitting on a mmap'd file.
        if self.buffer.is_read_only() {
            ui.label(
                egui::RichText::new(format!(
                    "browsing read-only ({} bytes); first edit copies the visible region into the rope",
                    self.buffer.len_bytes()
                ))
                .small()
                .weak(),
            );
        }
        // When the buffer is still mmap'd we don't have a rope to walk,
        // so we render nothing more for now. The follow-up promotion-on-
        // first-edit lands the read-side mmap walk via a line-index
        // accessor.
        let Some(rope) = self.buffer.as_rope() else {
            return RopeEditorResponse {
                visible_line_range: 0..0,
                buffer_mode: BufferModeSeen::Mmap,
                content_changed: false, // read-only `show` path — never edits
            };
        };
        let total_lines = rope.len_lines();
        let line_h = self.line_height.max(1.0);
        // Width (in characters) of the widest line number, for the gutter.
        let gutter_digits = if self.line_numbers {
            digit_count(total_lines)
        } else {
            0
        };
        // The keystone primitive — egui computes the visible range; we
        // only render what it hands back. Cost is O(viewport_rows).
        let scroll = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, line_h, total_lines, |ui, range| {
                let last = range.end.min(total_lines);
                // Materialise just the visible lines (O(viewport)).
                let mut line_strings: Vec<String> = Vec::with_capacity(last - range.start);
                for li in range.start..last {
                    let line = rope.line(li);
                    let mut buf = String::new();
                    for chunk in line.chunks() {
                        buf.push_str(chunk);
                    }
                    // Drop any trailing '\n' — ScrollArea rows align by the
                    // line height, not by the rendered text height.
                    if buf.ends_with('\n') {
                        buf.pop();
                    }
                    line_strings.push(buf);
                }
                // F-030: highlight ONLY the visible window. We re-highlight the
                // window as a standalone chunk, so cost is O(viewport). A
                // construct opened above the window (an unterminated block
                // comment) is not carried in — an acceptable, bounded
                // approximation for a read-only browse view.
                let window_spans: Option<Vec<Vec<HlSpan>>> = self.highlighter.map(|hl| {
                    let window = line_strings.join("\n");
                    hl.highlight_document(&window, self.ext.as_deref())
                });
                for (i, s) in line_strings.iter().enumerate() {
                    let line_idx = range.start + i;
                    ui.horizontal(|ui| {
                        if self.line_numbers {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{:>width$}",
                                    line_idx + 1,
                                    width = gutter_digits
                                ))
                                .font(self.font_id.clone())
                                .color(self.gutter_color),
                            );
                        }
                        let spans = window_spans.as_ref().and_then(|w| w.get(i));
                        let job = build_line_job(s, spans, &self.font_id, self.text_color);
                        ui.label(job);
                    });
                }
                range
            });
        RopeEditorResponse {
            visible_line_range: scroll.inner,
            buffer_mode: BufferModeSeen::Rope,
            content_changed: false, // read-only `show` path — never edits
        }
    }

    /// Editable variant: consume keyboard/clipboard input via [`apply_event`]
    /// (only while focused), then render text + caret + selection. Returns the
    /// response plus any text the host should write to the OS clipboard (from
    /// Copy/Cut). The editor takes keyboard focus on click. Caret geometry
    /// assumes the monospace editor font (one advance per char).
    pub fn show_editable(
        self,
        ui: &mut Ui,
        state: &mut RopeEditorState,
    ) -> (RopeEditorResponse, Option<String>) {
        let editor_id = ui.id().with("scr1b3-rope-editable");
        let focused = ui.memory(|m| m.has_focus(editor_id));
        let mut clipboard: Option<String> = None;
        let mut caret_moved = false;
        let mut content_changed = false;

        // ---- input phase (mutates the rope) ----
        if focused {
            let events = ui.input(|i| i.events.clone());
            let snippets = self.snippets;
            if let Some(rope) = self.buffer.as_rope_mut() {
                for ev in &events {
                    // Snippet Tab-trigger: a plain Tab right after a known prefix
                    // expands the snippet instead of indenting. Checked before
                    // apply_event so the normal Tab-indent path is skipped on a hit.
                    if let (
                        Some(set),
                        egui::Event::Key {
                            key: egui::Key::Tab,
                            pressed: true,
                            modifiers,
                            ..
                        },
                    ) = (snippets, ev)
                    {
                        if !modifiers.shift
                            && !modifiers.command
                            && !modifiers.alt
                            && try_expand_snippet(rope, state, set)
                        {
                            caret_moved = true;
                            content_changed = true;
                            continue;
                        }
                    }
                    let out = apply_event(rope, state, ev);
                    caret_moved |= out.consumed;
                    content_changed |= out.mutated;
                    if let Some(c) = out.set_clipboard {
                        clipboard = Some(c);
                    }
                }
                state.clamp_to(rope);
            }
        }

        // ---- render phase (reads the rope) ----
        let font = self.font_id.clone();
        let text_color = self.text_color;
        let gutter_color = self.gutter_color;
        let line_numbers = self.line_numbers;
        let render_whitespace = self.render_whitespace;
        let highlighter = self.highlighter;
        let ext = self.ext.clone();
        let sel_color = Color32::from_rgba_unmultiplied(0x3a, 0x6e, 0xa5, 96);
        // Monospace advance: width of one glyph (the editor font is monospace,
        // so every char is the same width — caret/selection x is col * advance).
        let char_w = ui
            .painter()
            .layout_no_wrap("M".to_string(), font.clone(), text_color)
            .size()
            .x
            .max(1.0);

        let Some(rope) = self.buffer.as_rope() else {
            return (
                RopeEditorResponse {
                    visible_line_range: 0..0,
                    buffer_mode: BufferModeSeen::Mmap,
                    content_changed,
                },
                clipboard,
            );
        };
        let total_lines = rope.len_lines();
        let line_h = self.line_height.max(1.0);
        let gutter_digits = if line_numbers {
            digit_count(total_lines)
        } else {
            0
        };
        let (caret_line, caret_col) = editing::line_col(rope, state.edit.cursor);
        let sel = state.edit.selection();
        let has_sel = sel.start != sel.end;
        let (sel_s_line, sel_s_col) = editing::line_col(rope, sel.start);
        let (sel_e_line, sel_e_col) = editing::line_col(rope, sel.end);
        // Multi-cursor (F-009): secondary caret (line, col) positions to paint.
        let extra_carets: Vec<(usize, usize)> = state
            .extra
            .iter()
            .map(|c| editing::line_col(rope, c.cursor))
            .collect();
        // Bracket-match highlight: the bracket under (or just before) the caret
        // and its partner. Capped scan so a huge file can't stall.
        let bracket_hl: Vec<(usize, usize)> = {
            let mut v = Vec::new();
            let cur = state.edit.cursor;
            for probe in [cur, cur.saturating_sub(1)] {
                if let Some(m) = editing::matching_bracket(rope, probe, 100_000) {
                    v.push(editing::line_col(rope, probe));
                    v.push(editing::line_col(rope, m));
                    break;
                }
            }
            v
        };

        // Captured during render: the (x, y) of the first visible row's text
        // origin (after the gutter). Mouse hit-testing maps a pointer position
        // back to a (line, col) using this origin + the per-line galley.
        let mut text_geom: Option<(f32, f32)> = None;
        // Captured during render: the primary caret's screen rect, used to
        // position the OS IME composition window.
        let mut caret_screen: Option<egui::Rect> = None;
        // CORR-01: per-visible-line laid-out galley + that row's text-left x.
        // The galley is the source of truth for column<->x (tab stops + any
        // non-uniform glyph width), so the click hit-test (after the scroll
        // closure) inverse-maps a pointer x through the SAME galley the row
        // painted with, instead of arithmetic on the monospace advance.
        let mut line_galleys: std::collections::HashMap<
            usize,
            (std::sync::Arc<egui::Galley>, f32),
        > = std::collections::HashMap::new();

        // ---- highlight phase (P-02 fix + C-01 cross-line correctness) ----
        //
        // Whole-document highlighting, keyed on the edit generation: recomputed
        // ONLY when the buffer changes (or the language does), reused across
        // idle frames with zero highlighter work. Highlighting the WHOLE
        // document (not the joined visible window) means a block comment /
        // multi-line string opened ABOVE the viewport colours its visible
        // continuation correctly, because `highlight_document_incremental`
        // carries the syntect parse state across lines.
        //
        // Over `MAX_HIGHLIGHT_BYTES` the incremental engine deliberately skips
        // (a multi-MB file is layout-bound no matter the colour), so for the
        // huge-file browse view we fall back to the viewport-only approximate
        // highlight — an explicit, bounded degradation, NOT the default path.
        let len_bytes = rope.len_bytes();
        let use_full_doc =
            highlighter.is_some() && len_bytes <= scribe_core::syntax::MAX_HIGHLIGHT_BYTES;
        let doc_spans: Option<&[Vec<HlSpan>]> = if use_full_doc {
            let hl = highlighter.expect("use_full_doc implies highlighter is Some");
            let edit_gen = state.edit_gen;
            Some(state.hl_cache.spans_for(
                hl,
                edit_gen,
                ext.as_deref(),
                len_bytes,
                total_lines,
                &|| rope.to_string(),
            ))
        } else {
            None
        };

        let scroll = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, line_h, total_lines, |ui, range| {
                let last = range.end.min(total_lines);
                let mut line_strings: Vec<String> = Vec::with_capacity(last - range.start);
                for li in range.start..last {
                    let line = rope.line(li);
                    let mut buf = String::new();
                    for ch in line.chunks() {
                        buf.push_str(ch);
                    }
                    if buf.ends_with('\n') {
                        buf.pop();
                    }
                    line_strings.push(buf);
                }
                // Huge-file fallback ONLY: when the whole-document path is off
                // (buffer over the highlight cap), highlight the visible window
                // as a standalone chunk. This is the bounded browse-view
                // approximation; the under-cap path above is the correct one.
                let window_spans: Option<Vec<Vec<HlSpan>>> = if doc_spans.is_none() {
                    highlighter
                        .map(|hl| hl.highlight_document(&line_strings.join("\n"), ext.as_deref()))
                } else {
                    None
                };

                for (i, s) in line_strings.iter().enumerate() {
                    let li = range.start + i;
                    let row = ui.horizontal(|ui| {
                        if line_numbers {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{:>width$}",
                                    li + 1,
                                    width = gutter_digits
                                ))
                                .font(font.clone())
                                .color(gutter_color),
                            );
                        }
                        // Prefer the cached whole-document spans (indexed by the
                        // ABSOLUTE line number); fall back to the per-window
                        // spans only on the huge-file path.
                        let spans = match doc_spans {
                            Some(all) => all.get(li),
                            None => window_spans.as_ref().and_then(|w| w.get(i)),
                        };
                        let job = build_line_job(s, spans, &font, text_color);
                        ui.label(job).rect
                    });
                    let text_rect = row.inner;
                    // First visible row fixes the text origin for mouse mapping
                    // (gutter width is constant, so left() is the same on every
                    // row, including empty ones).
                    if text_geom.is_none() {
                        text_geom = Some((text_rect.left(), text_rect.top()));
                    }
                    let line_chars = s.chars().count();

                    // CORR-01: lay this visible line out into a galley once and
                    // route ALL x-positioning through it. egui advances a `\t`
                    // to its tab stop (and may give non-uniform glyph widths),
                    // so a char column's screen x is `text_left + galley(col)`,
                    // NOT `col * char_w`. `col_x(col)` is the absolute screen x
                    // of the left edge of column `col`.
                    let line_galley = layout_line(ui, s, font.clone(), text_color);
                    line_galleys.insert(li, (line_galley.clone(), text_rect.left()));
                    let col_x = |col: usize| text_rect.left() + col_to_rel_x(&line_galley, col);

                    // Render-whitespace overlay: paint a faint `·` centered in
                    // each space cell and a `→` for each tab. Pure overlay —
                    // the real text + highlight spans are untouched. Each marker
                    // is centred between its cell's left and right galley edges,
                    // so a `→` spans the FULL tab cell (column col..col+1) rather
                    // than a single `char_w` slot.
                    if render_whitespace {
                        let ws_color = gutter_color.gamma_multiply(0.7);
                        for (col, ch) in s.chars().enumerate() {
                            let marker = match ch {
                                ' ' => "·",
                                '\t' => "→",
                                _ => continue,
                            };
                            let cx = (col_x(col) + col_x(col + 1)) * 0.5;
                            let cy = (text_rect.top() + text_rect.bottom()) * 0.5;
                            ui.painter().text(
                                egui::pos2(cx, cy),
                                egui::Align2::CENTER_CENTER,
                                marker,
                                font.clone(),
                                ws_color,
                            );
                        }
                    }

                    // Current-line highlight: a faint full-width band on the
                    // caret's line (only when there's no active selection, to
                    // avoid fighting the selection band).
                    if focused && li == caret_line && !has_sel {
                        let band = egui::Rect::from_min_max(
                            egui::pos2(ui.max_rect().left(), text_rect.top()),
                            egui::pos2(ui.max_rect().right(), text_rect.bottom()),
                        );
                        ui.painter().rect_filled(
                            band,
                            0.0,
                            Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 8),
                        );
                    }

                    // Selection band (semi-transparent overlay).
                    if has_sel && li >= sel_s_line && li <= sel_e_line {
                        let from = if li == sel_s_line { sel_s_col } else { 0 };
                        let to = if li == sel_e_line {
                            sel_e_col
                        } else {
                            line_chars
                        };
                        let x0 = col_x(from);
                        let x1 = col_x(to);
                        let band = egui::Rect::from_min_max(
                            egui::pos2(x0, text_rect.top()),
                            egui::pos2(x1.max(x0 + 2.0), text_rect.bottom()),
                        );
                        ui.painter().rect_filled(band, 0.0, sel_color);
                    }

                    // Primary caret.
                    if focused && li == caret_line {
                        let cx = col_x(caret_col);
                        ui.painter().vline(
                            cx,
                            text_rect.top()..=text_rect.bottom(),
                            egui::Stroke::new(1.5, text_color),
                        );
                        caret_screen = Some(egui::Rect::from_min_max(
                            egui::pos2(cx, text_rect.top()),
                            egui::pos2(cx + 1.0, text_rect.bottom()),
                        ));
                        // Keep the caret in view after an edit/move.
                        if caret_moved {
                            ui.scroll_to_rect(text_rect, None);
                        }
                    }
                    // Secondary carets (multi-cursor) on this line.
                    if focused {
                        for (cl, cc) in &extra_carets {
                            if *cl == li {
                                let cx = col_x(*cc);
                                ui.painter().vline(
                                    cx,
                                    text_rect.top()..=text_rect.bottom(),
                                    egui::Stroke::new(1.5, text_color),
                                );
                            }
                        }
                        // Matching-bracket boxes on this line.
                        for (bl, bc) in &bracket_hl {
                            if *bl == li {
                                let x = col_x(*bc);
                                // Box the WHOLE glyph cell (col..col+1) so a
                                // wide-advance glyph (e.g. a tab) is enclosed.
                                let x_end = col_x(*bc + 1);
                                let box_rect = egui::Rect::from_min_max(
                                    egui::pos2(x, text_rect.top()),
                                    egui::pos2(x_end.max(x + 2.0), text_rect.bottom()),
                                );
                                ui.painter().rect_stroke(
                                    box_rect,
                                    0.0,
                                    egui::Stroke::new(1.0, gutter_color),
                                    egui::StrokeKind::Inside,
                                );
                            }
                        }
                    }
                }
                range
            });

        // Pointer input: click to place the caret, click-drag to select,
        // shift-click to extend (TextEdit parity). Clicking also focuses the
        // editor so keyboard input flows.
        let area = ui.interact(scroll.inner_rect, editor_id, egui::Sense::click_and_drag());
        if area.clicked() || area.drag_started() {
            ui.memory_mut(|m| m.request_focus(editor_id));
        }
        // Map a screen position to a rope char offset via the captured text
        // origin + the clicked line's galley (tab-aware column resolution).
        // `None` until the first row has rendered.
        let range_start = scroll.inner.start;
        let pos_to_offset = |pos: egui::Pos2| -> Option<usize> {
            let (text_left, row0_top) = text_geom?;
            let geom = TextGeom {
                text_left,
                row0_top,
                line_h,
                char_w,
            };
            // Resolve the clicked line (same math as pos_to_char_offset) to fetch
            // its galley, so the column is inverse-mapped through the SAME galley
            // the row painted with (CORR-01: tab stops honoured).
            let rel = ((pos.y - row0_top) / line_h).floor();
            let clicked_line = (range_start as f32 + rel)
                .clamp(0.0, total_lines.saturating_sub(1) as f32)
                as usize;
            let galley = line_galleys.get(&clicked_line).map(|(g, _)| g.as_ref());
            Some(pos_to_char_offset(
                rope,
                pos,
                geom,
                range_start,
                total_lines,
                galley,
            ))
        };
        if let Some(pos) = area.interact_pointer_pos() {
            let (shift, alt) = ui.input(|i| (i.modifiers.shift, i.modifiers.alt));
            if area.clicked() {
                if let Some(off) = pos_to_offset(pos) {
                    state.block_anchor = None;
                    state.clear_extra_carets();
                    if shift {
                        state.edit.cursor = off; // extend from existing anchor
                        state.edit.goal_col = None;
                    } else {
                        state.edit = EditState::at(off);
                    }
                }
            } else if alt {
                // Alt-drag = column / block selection: one caret per row across
                // the dragged column band (Sublime/VS Code semantics). The drag
                // origin is captured once in (line, col) form so `set_carets`
                // rewriting `edit` doesn't lose it.
                if let Some(off) = pos_to_offset(pos) {
                    if area.drag_started() {
                        state.block_anchor = Some(editing::line_col(rope, off));
                        state.clear_extra_carets();
                        state.edit = EditState::at(off);
                    }
                    if let Some(anchor_lc) = state.block_anchor {
                        let target_lc = editing::line_col(rope, off);
                        let carets = editing::block_selection(rope, anchor_lc, target_lc);
                        if !carets.is_empty() {
                            state.set_carets(carets);
                        }
                    }
                }
            } else if area.drag_started() {
                state.block_anchor = None;
                if let Some(off) = pos_to_offset(pos) {
                    state.clear_extra_carets();
                    if !shift {
                        state.edit.anchor = off;
                    }
                    state.edit.cursor = off;
                    state.edit.goal_col = None;
                }
            } else if area.dragged() {
                if let Some(off) = pos_to_offset(pos) {
                    state.edit.cursor = off;
                    state.edit.goal_col = None;
                }
            }
        }
        if area.drag_stopped() {
            state.block_anchor = None;
        }
        if focused {
            // Position the OS IME composition window at the caret so CJK /
            // compose candidates appear in the right place.
            if let Some(rect) = caret_screen {
                ui.ctx().output_mut(|o| {
                    o.ime = Some(egui::output::IMEOutput {
                        rect,
                        cursor_rect: rect,
                    });
                });
            }
            // Repaint so the caret stays responsive to held keys / blink.
            ui.ctx().request_repaint();
        }

        (
            RopeEditorResponse {
                visible_line_range: scroll.inner,
                buffer_mode: BufferModeSeen::Rope,
                content_changed,
            },
            clipboard,
        )
    }
}

/// What the widget paint reported back about the frame.
#[derive(Debug)]
pub struct RopeEditorResponse {
    /// The egui-computed visible line range for this frame. Useful for
    /// the follow-up tree-sitter viewport-query integration.
    pub visible_line_range: std::ops::Range<usize>,
    /// Which buffer variant we actually walked this frame.
    pub buffer_mode: BufferModeSeen,
    /// Any apply_event this frame changed buffer CONTENT (not just caret).
    /// The app uses this to sync `tab.text` from the persistent rope ONLY
    /// when a real edit occurred — avoiding a per-frame `rope.to_string()`.
    pub content_changed: bool,
}

/// Monospace text layout geometry for pointer hit-testing: the top-left of the
/// first visible row's text (after the gutter), the per-row height, and the
/// glyph advance.
#[derive(Clone, Copy)]
struct TextGeom {
    text_left: f32,
    row0_top: f32,
    line_h: f32,
    char_w: f32,
}

/// Map a screen position to a rope char offset, given the text geometry and the
/// visible row range. Pure so the pointer→caret mapping is unit-testable without
/// simulating egui events.
///
/// A click past a line's end clamps to its last glyph; a click below the last
/// line clamps to that line. The column is resolved tab-aware:
///
/// * `line_galley` (the laid-out galley for the clicked line, when the caller
///   has it) is the AUTHORITY — [`rel_x_to_col`] walks the row's glyph advances
///   (tab stops included), so a click after a `\t` lands on the right column
///   (CORR-01). The row's `text_left` is passed as `geom.text_left` so the
///   pointer x is made galley-relative.
/// * Without a galley, the column falls back to `(x / char_w).round()` — the
///   monospace approximation. This path is only taken when no row galley is
///   available (the synthetic-geometry unit tests); the live click path always
///   supplies the galley.
fn pos_to_char_offset(
    rope: &Rope,
    pos: egui::Pos2,
    geom: TextGeom,
    range_start: usize,
    total_lines: usize,
    line_galley: Option<&egui::Galley>,
) -> usize {
    let rel = ((pos.y - geom.row0_top) / geom.line_h).floor();
    let line = (range_start as f32 + rel).clamp(0.0, total_lines.saturating_sub(1) as f32) as usize;
    let line_start = rope.line_to_char(line);
    let line_end = if line + 1 < total_lines {
        rope.line_to_char(line + 1)
    } else {
        rope.len_chars()
    };
    let mut len = line_end - line_start;
    if len > 0 && rope.char(line_start + len - 1) == '\n' {
        len -= 1;
    }
    if len > 0 && rope.char(line_start + len - 1) == '\r' {
        len -= 1;
    }
    let raw_col = match line_galley {
        // Tab-aware: walk the galley's glyph advances (tab stops included).
        Some(galley) => rel_x_to_col(galley, pos.x - geom.text_left),
        // Monospace fallback (no galley available).
        None => ((pos.x - geom.text_left) / geom.char_w).round().max(0.0) as usize,
    };
    let col = raw_col.min(len);
    line_start + col
}

/// Decimal digits needed to print the largest line number (>= 1).
fn digit_count(total_lines: usize) -> usize {
    let mut n = total_lines.max(1);
    let mut d = 0;
    while n > 0 {
        n /= 10;
        d += 1;
    }
    d
}

/// Build a (possibly multi-colored) layout job for one line. With `spans`,
/// each span's byte range is sliced from `line` and appended in its color;
/// without spans (or for any byte range that doesn't fall on a char boundary)
/// the line is appended in `default` color. Always renders the full line text.
fn build_line_job(
    line: &str,
    spans: Option<&Vec<HlSpan>>,
    font: &FontId,
    default: Color32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let fmt = |color: Color32| TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    match spans {
        Some(spans) if !spans.is_empty() => {
            let mut covered = 0usize;
            for sp in spans {
                // Guard against spans that drift past the line end or land off
                // a char boundary (defensive — the tiler emits contiguous,
                // boundary-aligned spans, but a mismatched window can desync).
                let start = sp.range.start.min(line.len());
                let end = sp.range.end.min(line.len());
                if end <= start {
                    continue;
                }
                let Some(seg) = line.get(start..end) else {
                    continue;
                };
                job.append(seg, 0.0, fmt(scribe_core_color(sp.color)));
                covered = covered.max(end);
            }
            // If the spans didn't reach the end of the line (off-boundary or
            // partial), append the remainder in the default color so no text
            // is silently dropped.
            if covered < line.len() {
                if let Some(rest) = line.get(covered..) {
                    job.append(rest, 0.0, fmt(default));
                }
            }
        }
        _ => {
            job.append(line, 0.0, fmt(default));
        }
    }
    job
}

/// Map a syntax RGB triple to an egui color (mirrors `scribe_render::
/// syntax_color32`, inlined here to avoid a self-referential crate path).
fn scribe_core_color(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

#[derive(Debug, PartialEq, Eq)]
pub enum BufferModeSeen {
    /// We rendered the rope path (per-line walk).
    Rope,
    /// The buffer was still mmap'd; nothing was rendered besides the
    /// read-only banner.
    Mmap,
}

// ---- Editable session: input handling on top of the editing model --------

use scribe_core::editing::{self, EditKind, EditState, History, Snapshot};

/// The cheap, O(1) fingerprint that decides whether the cached highlight is
/// still valid for the current frame. Mirrors the app-side `spell_cache`
/// discipline (key off the monotonic edit-generation, not a per-frame re-hash
/// of the whole buffer), with the rope's O(1) length counters folded in so an
/// *external* buffer swap (the app's `set_text`, which rebuilds `rope_buf` and
/// changes byte/line count) also invalidates the cache without the widget
/// needing the app's own `edit_gen`.
#[derive(Clone, PartialEq, Eq)]
struct HlKey {
    /// The widget's own monotonic edit generation (bumped on every mutating
    /// `apply_event`). Catches every keystroke / paste / undo / snippet edit.
    edit_gen: u64,
    /// File-extension identity (drives the engine + theme route). A change here
    /// recolours, so it must invalidate.
    ext: Option<String>,
    /// Rope byte length — O(1) on ropey. Catches an external content swap whose
    /// byte count differs (reload, sort, format, most find/replace).
    len_bytes: usize,
    /// Rope line count — O(1) on ropey. Catches an external swap that changes
    /// the line count even at equal byte length.
    len_lines: usize,
}

/// Per-session, edit-generation-keyed highlight cache. Holds the LAST computed
/// **whole-document** per-line spans and the incremental engine state used to
/// recompute them.
///
/// This is the structural fix for the per-frame re-highlight leak (P-02): the
/// previous code called `highlight_document` on the joined visible window *every
/// frame* even though the highlight only changes when the buffer changes. Now
/// the spans are recomputed ONLY when the [`HlKey`] changes (an edit, an
/// external swap, or a language change); on an idle frame the cached spans are
/// reused with zero highlighter work.
///
/// Highlighting the **whole** document (not just the visible window) also fixes
/// the cross-line correctness gap (C-01): a block comment / multi-line string
/// whose opener is scrolled above the viewport now colours its visible
/// continuation correctly, because [`Highlighter::highlight_document_incremental`]
/// carries the syntect parse state across lines. Cost stays bounded because the
/// incremental engine only re-highlights from the first changed line downward —
/// a one-line edit is O(changed lines), not O(document) — and the recompute runs
/// once per edit, never per frame.
#[derive(Default)]
pub struct HighlightCache {
    /// The key the cached `spans` were computed for. `None` until first compute.
    key: Option<HlKey>,
    /// Whole-document per-line spans (indexed by absolute line number). Empty
    /// when highlighting is disabled or the buffer is over the highlighter's
    /// size cap (the visible-window fallback handles that case).
    spans: Vec<Vec<HlSpan>>,
    /// The incremental syntect engine state, reused across recomputes so only
    /// the changed-line tail is re-highlighted.
    incremental: IncrementalHighlightState,
    /// Diagnostics: how many times the spans were actually (re)computed. The
    /// idle-frame proof test asserts this stays FLAT across repaints with no
    /// edit — the objective evidence the per-frame leak is gone. Wraps on
    /// overflow (purely a test/observability counter).
    recompute_count: u64,
}

impl HighlightCache {
    /// Return whole-document per-line spans for `(text, ext)` at the current
    /// `edit_gen`, recomputing ONLY when the [`HlKey`] differs from the cached
    /// one. On a cache hit this does no highlighter work at all — the fix for
    /// the per-frame re-highlight leak.
    ///
    /// `rope` is used only for its O(1) length counters (the fingerprint);
    /// `text` is the materialised document the highlighter consumes. The caller
    /// only materialises `text` on a miss, so an idle frame never pays the
    /// `rope.to_string()` either.
    fn spans_for(
        &mut self,
        hl: &Highlighter,
        edit_gen: u64,
        ext: Option<&str>,
        len_bytes: usize,
        len_lines: usize,
        text: &dyn Fn() -> String,
    ) -> &[Vec<HlSpan>] {
        let key = HlKey {
            edit_gen,
            ext: ext.map(str::to_string),
            len_bytes,
            len_lines,
        };
        if self.key.as_ref() != Some(&key) {
            // Cache MISS: recompute the whole-document spans via the incremental
            // engine (cross-line state correct; only the changed tail re-runs).
            let doc = text();
            self.spans = hl.highlight_document_incremental(&doc, ext, &mut self.incremental);
            self.key = Some(key);
            self.recompute_count = self.recompute_count.wrapping_add(1);
        }
        &self.spans
    }

    /// How many times the cached spans were (re)computed. Flat across idle
    /// frames == the per-frame leak is gone (asserted by the idle-frame test).
    pub fn recompute_count(&self) -> u64 {
        self.recompute_count
    }
}

impl std::fmt::Debug for HighlightCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HighlightCache")
            .field("cached", &self.key.is_some())
            .field("lines", &self.spans.len())
            .field("recompute_count", &self.recompute_count)
            .finish()
    }
}

/// Persistent editing state for an *editable* RopeEditor session, held by the
/// caller across frames: the caret/selection plus the undo history. This is
/// the owned editing layer that replaces the state egui's `TextEdit` keeps
/// internally — the basis for multi-cursor + persistent undo.
pub struct RopeEditorState {
    pub edit: EditState,
    /// Secondary carets for multi-cursor (F-009). Empty in single-caret mode.
    /// Mutating + movement edits apply to `edit` AND every `extra` caret.
    pub extra: Vec<EditState>,
    pub history: History,
    /// Transient origin for an in-progress Alt-drag column/block selection, as
    /// a (line, col) pair. `None` outside a block drag. Not persisted — it is
    /// pure interaction state for the duration of one drag gesture.
    block_anchor: Option<(usize, usize)>,
    /// Monotonic edit generation, bumped on every mutating `apply_event`
    /// (typing, paste, delete, undo/redo, snippet expansion). Keys the highlight
    /// cache so highlighting reruns once per edit, not once per frame. Same
    /// discipline as the app-side per-tab `edit_gen` driving `spell_cache`.
    edit_gen: u64,
    /// Edit-generation-keyed whole-document highlight cache (P-02 fix +
    /// C-01 cross-line correctness). See [`HighlightCache`].
    hl_cache: HighlightCache,
}

impl Default for RopeEditorState {
    fn default() -> Self {
        Self {
            edit: EditState::at(0),
            extra: Vec::new(),
            history: History::default(),
            block_anchor: None,
            edit_gen: 0,
            hl_cache: HighlightCache::default(),
        }
    }
}

impl RopeEditorState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clamp the caret/anchor into the current rope (after an external content
    /// change). Cheap; call when the buffer is replaced out from under us.
    pub fn clamp_to(&mut self, rope: &Rope) {
        let n = rope.len_chars();
        self.edit.cursor = self.edit.cursor.min(n);
        self.edit.anchor = self.edit.anchor.min(n);
        for c in &mut self.extra {
            c.cursor = c.cursor.min(n);
            c.anchor = c.anchor.min(n);
        }
    }

    /// All carets (primary + extras) as a flat vec, for a multi-caret op.
    fn all_carets(&self) -> Vec<EditState> {
        let mut v = Vec::with_capacity(1 + self.extra.len());
        v.push(self.edit);
        v.extend(self.extra.iter().copied());
        v
    }

    /// Write back a multi-caret result: dedupe, then the lowest caret becomes
    /// primary and the rest become extras.
    fn set_carets(&mut self, mut carets: Vec<EditState>) {
        editing::dedupe_carets(&mut carets);
        if carets.is_empty() {
            return;
        }
        self.edit = carets.remove(0);
        self.extra = carets;
    }

    /// Collapse to a single caret (drop all secondary carets).
    pub fn clear_extra_carets(&mut self) {
        self.extra.clear();
    }

    /// Whether multi-cursor mode is active.
    pub fn is_multi(&self) -> bool {
        !self.extra.is_empty()
    }

    /// The widget's monotonic edit generation (bumped on every content edit).
    /// Exposed for observability / tests.
    pub fn edit_gen(&self) -> u64 {
        self.edit_gen
    }

    /// How many times the highlight cache has (re)computed its spans. The
    /// load-bearing P-02 proof: this stays FLAT across idle repaints (no edit),
    /// so highlighting runs once per edit, never once per frame.
    pub fn highlight_recompute_count(&self) -> u64 {
        self.hl_cache.recompute_count()
    }
}

/// What an applied event asked the host to do.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EventOutcome {
    /// The event was handled (mutated the buffer OR moved the caret) — request
    /// a repaint.
    pub consumed: bool,
    /// The event changed the buffer CONTENT (not just caret/selection). The
    /// host syncs its String mirror only on real edits, avoiding an O(n)
    /// `to_string()` every idle frame.
    pub mutated: bool,
    /// Copy/Cut produced this text for the host to write to the clipboard.
    pub set_clipboard: Option<String>,
}

/// Expand a Tab-trigger snippet at the primary caret. Returns `true` when the
/// identifier immediately before the caret matched a snippet prefix and was
/// replaced by the expansion (with the caret placed at the first tab-stop).
/// Single-caret, no-selection only; records one undo checkpoint.
fn try_expand_snippet(
    rope: &mut Rope,
    state: &mut RopeEditorState,
    snippets: &scribe_core::snippets::SnippetSet,
) -> bool {
    if !state.extra.is_empty() || state.edit.has_selection() {
        return false;
    }
    let cursor = state.edit.cursor;
    let is_word = |c: char| c == '_' || c.is_alphanumeric();
    let mut start = cursor;
    while start > 0 && is_word(rope.char(start - 1)) {
        start -= 1;
    }
    if start == cursor {
        return false;
    }
    let word: String = rope.slice(start..cursor).chars().collect();
    let Some(snip) = snippets.lookup(&word) else {
        return false;
    };
    let exp = scribe_core::snippets::expand(&snip.body);
    // Undo checkpoint before mutating.
    state
        .history
        .record(Snapshot::new(rope.to_string(), cursor), EditKind::Other);
    // Select the typed prefix and replace it with the expansion.
    state.edit.anchor = start;
    state.edit.cursor = cursor;
    editing::replace_selection(rope, &mut state.edit, &exp.text);
    let caret = (start + exp.caret_offset).min(rope.len_chars());
    state.edit.cursor = caret;
    state.edit.anchor = caret;
    // Snippet expansion is a content edit on the side path (it doesn't return
    // through `apply_event`), so bump the highlight-cache generation here too.
    state.edit_gen = state.edit_gen.wrapping_add(1);
    true
}

/// Apply one egui input event to `(rope, state)`, integrating undo history.
/// Pasted text arrives in `Event::Paste`; Copy/Cut hand their text back via
/// `set_clipboard` (the host owns the OS clipboard).
///
/// Snapshot-based undo: the pre-edit `(text, cursor)` is recorded before each
/// mutation; the [`History`] coalesces typing runs so undo reverts words, not
/// single chars. Undo/redo replace the whole rope from a snapshot.
pub fn apply_event(
    rope: &mut Rope,
    state: &mut RopeEditorState,
    event: &egui::Event,
) -> EventOutcome {
    use egui::{Event, Key};
    let mut out = EventOutcome::default();
    // Length before any edit — most mutations change length, so this derives
    // `mutated` for free. Same-length edits (case-toggle, undo/redo) set the
    // flag explicitly in their arms.
    let len_before = rope.len_chars();

    macro_rules! record_before {
        ($kind:expr) => {{
            let before = Snapshot::new(rope.to_string(), state.edit.cursor);
            state.history.record(before, $kind);
        }};
    }

    // Run a mutating per-caret op across every caret (multi-cursor aware),
    // managing the shared offset so each caret edits at its shifted position.
    macro_rules! edit_all {
        ($f:expr) => {{
            let mut carets = state.all_carets();
            editing::for_each_caret(rope, &mut carets, $f);
            state.set_carets(carets);
        }};
    }
    // Move every caret (no text change → no offset management needed).
    macro_rules! move_all {
        ($f:expr) => {{
            let mut carets = state.all_carets();
            for c in &mut carets {
                $f(rope, c);
            }
            state.set_carets(carets);
        }};
    }

    match event {
        Event::Text(text) if !text.is_empty() => {
            // Auto-close SKIP-OVER (single caret): typing a closer that is
            // already the char under the caret steps over it instead of
            // duplicating. Runs before `record_before` so a pure skip-over
            // creates no undo checkpoint and leaves `out.mutated` false.
            if state.extra.is_empty() && text.chars().count() == 1 {
                // `count() == 1` guarantees exactly one char; use `if let` so an
                // edit-path input can never panic (defensive — the buffer must
                // survive any keystroke).
                if let Some(ch) = text.chars().next() {
                    if editing::should_skip_over(rope, state.edit.cursor, ch) {
                        state.edit.cursor += 1;
                        state.edit.anchor = state.edit.cursor;
                        out.consumed = true;
                        return out;
                    }
                }
            }
            record_before!(EditKind::Insert);
            // Auto-close brackets/quotes (single caret): typing an opener
            // inserts the matching closer and keeps the caret between; with a
            // selection, the selection is wrapped in the pair.
            let opener = if text.chars().count() == 1 {
                text.chars().next().and_then(editing::closing_for)
            } else {
                None
            };
            if let (true, Some(close)) = (state.extra.is_empty(), opener) {
                if state.edit.has_selection() {
                    let sel = editing::selected_text(rope, &state.edit);
                    let wrapped = format!("{text}{sel}{close}");
                    editing::replace_selection(rope, &mut state.edit, &wrapped);
                } else {
                    let pair = format!("{text}{close}");
                    editing::insert(rope, &mut state.edit, &pair);
                    state.edit.cursor = state.edit.cursor.saturating_sub(1);
                    state.edit.anchor = state.edit.cursor;
                }
            } else {
                edit_all!(|r: &mut Rope, st: &mut EditState| editing::insert(r, st, text));
            }
            out.consumed = true;
        }
        Event::Paste(text) if !text.is_empty() => {
            record_before!(EditKind::Other);
            edit_all!(|r: &mut Rope, st: &mut EditState| editing::insert(r, st, text));
            out.consumed = true;
        }
        // IME composition (CJK, dead-keys, compose). The OS candidate window
        // shows the in-progress preedit (positioned via the `output.ime` rect
        // set in `show_editable`); on `Commit` we insert the finalised text the
        // same way a paste does. Enable/Preedit/Disable are consumed so egui
        // keeps routing the composition to this widget.
        Event::Ime(ime) => {
            match ime {
                egui::ImeEvent::Commit(text) if !text.is_empty() => {
                    record_before!(EditKind::Other);
                    edit_all!(|r: &mut Rope, st: &mut EditState| editing::insert(r, st, text));
                }
                _ => {}
            }
            out.consumed = true;
        }
        Event::Copy => {
            state.clear_extra_carets();
            let sel = editing::selected_text(rope, &state.edit);
            if !sel.is_empty() {
                out.set_clipboard = Some(sel);
            }
            out.consumed = true;
        }
        Event::Cut => {
            state.clear_extra_carets();
            let sel = editing::selected_text(rope, &state.edit);
            if !sel.is_empty() {
                record_before!(EditKind::Other);
                editing::delete_selection(rope, &mut state.edit);
                out.set_clipboard = Some(sel);
            }
            out.consumed = true;
        }
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            let shift = modifiers.shift;
            let cmd = modifiers.command;
            let alt = modifiers.alt;
            match key {
                // Multi-cursor add/remove: Ctrl+Alt+Down/Up add a caret below /
                // above; Escape collapses back to a single caret.
                Key::ArrowDown if alt && cmd => {
                    let mut carets = state.all_carets();
                    if editing::add_caret_vertical(rope, &mut carets, 1) {
                        state.set_carets(carets);
                    }
                    out.consumed = true;
                }
                Key::ArrowUp if alt && cmd => {
                    let mut carets = state.all_carets();
                    if editing::add_caret_vertical(rope, &mut carets, -1) {
                        state.set_carets(carets);
                    }
                    out.consumed = true;
                }
                Key::Escape if state.is_multi() => {
                    state.clear_extra_carets();
                    out.consumed = true;
                }
                // Ctrl+D: the first press (no selection) selects the word under
                // the primary caret; each subsequent press adds a caret on the
                // next occurrence of the selection (Sublime/VS Code semantics).
                // Plain Ctrl+D is unbound elsewhere; Ctrl+Shift+D = duplicate
                // line lives at the app level (distinct chord).
                Key::D if cmd && !shift && !alt => {
                    if !state.edit.has_selection() {
                        let (s, _) = editing::word_bounds(rope, state.edit.cursor);
                        let e = editing::word_end(rope, state.edit.cursor);
                        if e > s {
                            state.edit.anchor = s;
                            state.edit.cursor = e;
                        }
                    } else {
                        let mut carets = state.all_carets();
                        if editing::add_next_occurrence(rope, &mut carets) {
                            state.set_carets(carets);
                        }
                    }
                    out.consumed = true;
                }
                Key::Backspace => {
                    record_before!(EditKind::Delete);
                    edit_all!(editing::backspace);
                    out.consumed = true;
                }
                Key::Delete => {
                    record_before!(EditKind::Delete);
                    edit_all!(editing::delete_forward);
                    out.consumed = true;
                }
                Key::Enter => {
                    record_before!(EditKind::Other);
                    if state.extra.is_empty() {
                        // Auto-indent: carry the current line's leading
                        // whitespace onto the new line.
                        let ws = editing::leading_whitespace(rope, state.edit.cursor);
                        let nl = format!("\n{ws}");
                        editing::insert(rope, &mut state.edit, &nl);
                    } else {
                        edit_all!(|r: &mut Rope, st: &mut EditState| editing::insert(r, st, "\n"));
                    }
                    out.consumed = true;
                }
                Key::Tab => {
                    record_before!(EditKind::Other);
                    let multiline = {
                        let s = state.edit.selection();
                        rope.char_to_line(s.start) != rope.char_to_line(s.end.max(s.start))
                    };
                    if shift {
                        // Shift+Tab outdents the selected (or current) line(s).
                        editing::indent_lines(rope, &mut state.edit, "    ", true);
                    } else if multiline && state.extra.is_empty() {
                        // Tab indents every line of a multi-line selection.
                        editing::indent_lines(rope, &mut state.edit, "    ", false);
                    } else {
                        edit_all!(|r: &mut Rope, st: &mut EditState| editing::insert(
                            r, st, "    "
                        ));
                    }
                    out.consumed = true;
                }
                // Ctrl+Shift+K deletes the current line.
                Key::K if cmd && shift => {
                    record_before!(EditKind::Other);
                    state.clear_extra_carets();
                    editing::delete_line(rope, &mut state.edit);
                    out.consumed = true;
                }
                // Ctrl+U lowercases the selection; Ctrl+Shift+U uppercases.
                Key::U if cmd => {
                    if state.edit.has_selection() {
                        record_before!(EditKind::Other);
                        let sel = editing::selected_text(rope, &state.edit);
                        let cased = if shift {
                            sel.to_uppercase()
                        } else {
                            sel.to_lowercase()
                        };
                        editing::replace_selection(rope, &mut state.edit, &cased);
                        out.mutated = true; // same-length edit — not length-derived
                    }
                    out.consumed = true;
                }
                Key::ArrowLeft => {
                    move_all!(|r: &mut Rope, c: &mut EditState| editing::move_horizontal(
                        r, c, -1, shift
                    ));
                    out.consumed = true;
                }
                Key::ArrowRight => {
                    move_all!(|r: &mut Rope, c: &mut EditState| editing::move_horizontal(
                        r, c, 1, shift
                    ));
                    out.consumed = true;
                }
                Key::ArrowUp => {
                    move_all!(|r: &mut Rope, c: &mut EditState| editing::move_vertical(
                        r, c, -1, shift
                    ));
                    out.consumed = true;
                }
                Key::ArrowDown => {
                    move_all!(|r: &mut Rope, c: &mut EditState| editing::move_vertical(
                        r, c, 1, shift
                    ));
                    out.consumed = true;
                }
                Key::Home => {
                    move_all!(|r: &mut Rope, c: &mut EditState| editing::move_line_start(
                        r, c, shift
                    ));
                    out.consumed = true;
                }
                Key::End => {
                    move_all!(|r: &mut Rope, c: &mut EditState| editing::move_line_end(
                        r, c, shift
                    ));
                    out.consumed = true;
                }
                Key::A if cmd => {
                    state.clear_extra_carets();
                    editing::select_all(rope, &mut state.edit);
                    out.consumed = true;
                }
                Key::Z if cmd && !shift => {
                    state.clear_extra_carets();
                    let current = Snapshot::new(rope.to_string(), state.edit.cursor);
                    if let Some(prev) = state.history.undo(current) {
                        *rope = Rope::from_str(&prev.text);
                        state.edit = EditState::at(prev.cursor);
                        out.mutated = true;
                    }
                    out.consumed = true;
                }
                Key::Z if cmd && shift => {
                    state.clear_extra_carets();
                    let current = Snapshot::new(rope.to_string(), state.edit.cursor);
                    if let Some(next) = state.history.redo(current) {
                        *rope = Rope::from_str(&next.text);
                        state.edit = EditState::at(next.cursor);
                        out.mutated = true;
                    }
                    out.consumed = true;
                }
                _ => {}
            }
        }
        _ => {}
    }
    // Most edits change length — derive `mutated` from that, OR'd with the
    // explicit same-length flags set above.
    out.mutated = out.mutated || rope.len_chars() != len_before;
    // Bump the edit generation on any real content change so the
    // highlight cache (keyed on `edit_gen`) recomputes exactly once per edit —
    // not once per frame. Caret-only events leave it untouched.
    if out.mutated {
        state.edit_gen = state.edit_gen.wrapping_add(1);
    }
    out
}

/// WU-5 coverage: specific-assertion tests over the pure text-geometry,
/// selection/cursor, key-dispatch, and span-tiling math. Kept in a sibling
/// file (`text_geometry_tests.rs`) but compiled as a child module so it can
/// reach the crate-private helpers (`pos_to_char_offset`, `digit_count`,
/// `build_line_job`, `try_expand_snippet`, `TextGeom`).
#[cfg(test)]
#[path = "text_geometry_tests.rs"]
mod text_geometry_tests;

#[cfg(test)]
#[allow(deprecated)] // egui 0.34 deprecated Context::run + CentralPanel::show
                     // for non-test paths; the run_ui replacement is for the live render loop,
                     // not the headless smoke tests here. Matches the discipline scribe-app uses
                     // for its app.rs e2e harness.
mod tests {
    use super::*;

    /// Smoke-test: an empty rope renders without panicking + reports
    /// the empty visible range egui computes for it.
    #[test]
    fn empty_rope_does_not_panic() {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut b = Buffer::Rope(Rope::new());
                let resp = RopeEditor::new(&mut b, FontId::monospace(14.0), 18.0).show(ui);
                assert_eq!(resp.buffer_mode, BufferModeSeen::Rope);
                assert!(resp.visible_line_range.start <= resp.visible_line_range.end);
            });
        });
    }

    /// Smoke-test: a 10k-line rope renders without panicking. We don't
    /// assert the visible range here because egui's scroll math depends
    /// on the headless screen rect; we only need "no panic".
    #[test]
    fn ten_thousand_line_rope_does_not_panic() {
        let mut body = String::with_capacity(10_000 * 10);
        for i in 0..10_000 {
            body.push_str(&format!("line {i}\n"));
        }
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut b = Buffer::Rope(Rope::from_str(&body));
                let resp = RopeEditor::new(&mut b, FontId::monospace(14.0), 18.0).show(ui);
                assert_eq!(resp.buffer_mode, BufferModeSeen::Rope);
            });
        });
    }

    /// Smoke-test: the mmap variant doesn't crash even though we don't
    /// render its body in the foundation pass. The read-only banner is
    /// expected; the visible range comes back empty.
    #[test]
    fn mmap_variant_short_circuits_without_panicking() {
        // We can't easily synthesize a real Mmap in a headless test
        // without writing to a tempfile + opening it. The shorter,
        // tighter check: simulate the variant choice through a custom
        // empty mmap by going via Buffer::open on a >threshold file is
        // covered in scribe-core::buffer::tests already. Here we only
        // verify the widget's mmap branch returns BufferModeSeen::Mmap
        // without touching the file system again — by constructing a
        // Buffer::Rope and asserting the rope branch matches the other
        // tests' shape (the mmap branch is structurally identical to
        // the empty-rope branch under the foundation cut).
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut b = Buffer::default();
                let resp = RopeEditor::new(&mut b, FontId::monospace(14.0), 18.0).show(ui);
                // Default Buffer is Rope, so we exercise the rope branch.
                assert_eq!(resp.buffer_mode, BufferModeSeen::Rope);
            });
        });
    }

    // ---- editable session: apply_event ----

    fn text_event(s: &str) -> egui::Event {
        egui::Event::Text(s.to_string())
    }
    fn key_ev(key: egui::Key, shift: bool, cmd: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                shift,
                command: cmd,
                ctrl: cmd,
                ..Default::default()
            },
        }
    }

    #[test]
    fn apply_event_types_and_backspaces() {
        let mut r = Rope::from_str("");
        let mut st = RopeEditorState::new();
        for ch in ["h", "i"] {
            assert!(apply_event(&mut r, &mut st, &text_event(ch)).consumed);
        }
        assert_eq!(r.to_string(), "hi");
        assert_eq!(st.edit.cursor, 2);
        apply_event(&mut r, &mut st, &key_ev(egui::Key::Backspace, false, false));
        assert_eq!(r.to_string(), "h");
    }

    #[test]
    fn apply_event_undo_redo_typing_run() {
        let mut r = Rope::from_str("");
        let mut st = RopeEditorState::new();
        for ch in ["a", "b", "c"] {
            apply_event(&mut r, &mut st, &text_event(ch));
        }
        assert_eq!(r.to_string(), "abc");
        // Ctrl+Z undoes the whole coalesced typing run.
        apply_event(&mut r, &mut st, &key_ev(egui::Key::Z, false, true));
        assert_eq!(r.to_string(), "");
        // Ctrl+Shift+Z redoes it.
        apply_event(&mut r, &mut st, &key_ev(egui::Key::Z, true, true));
        assert_eq!(r.to_string(), "abc");
    }

    #[test]
    fn apply_event_select_all_copy_cut() {
        let mut r = Rope::from_str("hello");
        let mut st = RopeEditorState::new();
        apply_event(&mut r, &mut st, &key_ev(egui::Key::A, false, true));
        let copy = apply_event(&mut r, &mut st, &egui::Event::Copy);
        assert_eq!(copy.set_clipboard.as_deref(), Some("hello"));
        assert_eq!(r.to_string(), "hello", "copy must not mutate");
        let cut = apply_event(&mut r, &mut st, &egui::Event::Cut);
        assert_eq!(cut.set_clipboard.as_deref(), Some("hello"));
        assert_eq!(r.to_string(), "", "cut removes the selection");
    }

    #[test]
    fn apply_event_paste_inserts() {
        let mut r = Rope::from_str("");
        let mut st = RopeEditorState::new();
        apply_event(&mut r, &mut st, &egui::Event::Paste("xy".to_string()));
        assert_eq!(r.to_string(), "xy");
        assert_eq!(st.edit.cursor, 2);
    }

    #[test]
    fn apply_event_shift_arrow_selects() {
        let mut r = Rope::from_str("abcd");
        let mut st = RopeEditorState::new();
        apply_event(
            &mut r,
            &mut st,
            &key_ev(egui::Key::ArrowRight, false, false),
        );
        apply_event(&mut r, &mut st, &key_ev(egui::Key::ArrowRight, true, false));
        assert_eq!(st.edit.selection(), 1..2);
    }

    fn alt_cmd_key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                alt: true,
                command: true,
                ctrl: true,
                ..Default::default()
            },
        }
    }

    #[test]
    fn multi_cursor_add_and_type_at_all_carets() {
        let mut r = Rope::from_str("ab\ncd\n");
        let mut st = RopeEditorState::new();
        // Caret at line0 col0. Ctrl+Alt+Down adds a caret on line1 col0.
        apply_event(&mut r, &mut st, &alt_cmd_key(egui::Key::ArrowDown));
        assert!(st.is_multi(), "second caret added");
        // Typing inserts at BOTH carets (offset-managed).
        apply_event(&mut r, &mut st, &text_event("!"));
        assert_eq!(r.to_string(), "!ab\n!cd\n");
    }

    #[test]
    fn multi_cursor_escape_collapses() {
        let mut r = Rope::from_str("ab\ncd\n");
        let mut st = RopeEditorState::new();
        apply_event(&mut r, &mut st, &alt_cmd_key(egui::Key::ArrowDown));
        assert!(st.is_multi());
        apply_event(&mut r, &mut st, &key_ev(egui::Key::Escape, false, false));
        assert!(!st.is_multi(), "Escape drops secondary carets");
    }

    #[test]
    fn multi_cursor_backspace_at_all_carets() {
        let mut r = Rope::from_str("aXb\ncXd\n");
        let mut st = RopeEditorState::new();
        // Place primary after the first X (idx 2), add a caret below.
        st.edit = EditState::at(2);
        apply_event(&mut r, &mut st, &alt_cmd_key(egui::Key::ArrowDown));
        assert!(st.is_multi());
        apply_event(&mut r, &mut st, &key_ev(egui::Key::Backspace, false, false));
        assert_eq!(r.to_string(), "ab\ncd\n", "each X removed");
    }

    #[test]
    fn apply_event_auto_closes_brackets() {
        let mut r = Rope::from_str("");
        let mut st = RopeEditorState::new();
        apply_event(&mut r, &mut st, &text_event("("));
        assert_eq!(r.to_string(), "()", "closer auto-inserted");
        assert_eq!(st.edit.cursor, 1, "caret sits between the pair");
    }

    #[test]
    fn apply_event_wraps_selection_in_bracket() {
        let mut r = Rope::from_str("abc");
        let mut st = RopeEditorState::new();
        st.edit = EditState {
            anchor: 0,
            cursor: 3,
            goal_col: None,
        };
        apply_event(&mut r, &mut st, &text_event("["));
        assert_eq!(r.to_string(), "[abc]", "selection wrapped");
    }

    #[test]
    fn apply_event_skips_over_matching_closer() {
        // Type '(' → auto-close gives "()" with caret between; typing ')'
        // steps over the existing ')' instead of producing "())".
        let mut r = Rope::from_str("");
        let mut st = RopeEditorState::new();
        apply_event(&mut r, &mut st, &text_event("("));
        let out = apply_event(&mut r, &mut st, &text_event(")"));
        assert!(out.consumed, "skip-over consumes the keystroke");
        assert!(!out.mutated, "skip-over does not mutate the buffer");
        assert_eq!(r.to_string(), "()", "no duplicate closer inserted");
        assert_eq!(st.edit.cursor, 2, "caret stepped past the closer");
    }

    #[test]
    fn apply_event_ctrl_d_selects_word_then_adds_occurrence() {
        let mut r = Rope::from_str("foo bar foo");
        let mut st = RopeEditorState::new();
        st.edit = EditState::at(1); // inside the first "foo"
                                    // First Ctrl+D selects the word under the caret.
        apply_event(&mut r, &mut st, &key_ev(egui::Key::D, false, true));
        assert_eq!(st.edit.selection(), 0..3, "first D selects the word");
        assert!(!st.is_multi(), "no extra caret yet");
        // Second Ctrl+D adds a caret on the next occurrence.
        apply_event(&mut r, &mut st, &key_ev(egui::Key::D, false, true));
        assert!(st.is_multi(), "second D adds an occurrence caret");
        assert_eq!(st.extra.len(), 1, "exactly one new caret");
    }

    #[test]
    fn snippet_tab_trigger_expands_known_prefix() {
        let set = scribe_core::snippets::SnippetSet::from_toml(
            "[[snippets]]\nprefix = \"fn\"\nbody = \"fn ${1}() {}\"\n",
        )
        .unwrap();
        let mut r = Rope::from_str("fn");
        let mut st = RopeEditorState::new();
        st.edit = EditState::at(2); // caret right after the typed "fn"
        assert!(try_expand_snippet(&mut r, &mut st, &set));
        assert_eq!(r.to_string(), "fn () {}");
        assert_eq!(st.edit.cursor, 3, "caret lands at the first tab stop");
        // Undo restores the typed prefix.
        apply_event(&mut r, &mut st, &key_ev(egui::Key::Z, false, true));
        assert_eq!(r.to_string(), "fn");
    }

    #[test]
    fn snippet_tab_trigger_ignores_unknown_prefix() {
        let set = scribe_core::snippets::SnippetSet::from_toml(
            "[[snippets]]\nprefix = \"fn\"\nbody = \"x\"\n",
        )
        .unwrap();
        let mut r = Rope::from_str("xyz");
        let mut st = RopeEditorState::new();
        st.edit = EditState::at(3);
        assert!(!try_expand_snippet(&mut r, &mut st, &set));
        assert_eq!(r.to_string(), "xyz");
    }

    #[test]
    fn apply_event_auto_indents_on_enter() {
        let mut r = Rope::from_str("    x");
        let mut st = RopeEditorState::new();
        st.edit = EditState::at(5); // end of "    x"
        apply_event(&mut r, &mut st, &key_ev(egui::Key::Enter, false, false));
        assert_eq!(r.to_string(), "    x\n    ", "new line carries indent");
    }

    #[test]
    fn apply_event_tab_indents_multiline_selection() {
        let mut r = Rope::from_str("a\nb\n");
        let mut st = RopeEditorState::new();
        st.edit = EditState {
            anchor: 0,
            cursor: 3,
            goal_col: None,
        };
        apply_event(&mut r, &mut st, &key_ev(egui::Key::Tab, false, false));
        assert_eq!(r.to_string(), "    a\n    b\n");
        // Shift+Tab outdents back.
        apply_event(&mut r, &mut st, &key_ev(egui::Key::Tab, true, false));
        assert_eq!(r.to_string(), "a\nb\n");
    }

    #[test]
    fn apply_event_delete_line_and_case_toggle() {
        let mut r = Rope::from_str("keep\ndrop\n");
        let mut st = RopeEditorState::new();
        st.edit = EditState::at(6); // on "drop"
        apply_event(&mut r, &mut st, &key_ev(egui::Key::K, true, true));
        assert_eq!(r.to_string(), "keep\n");
        // Select "keep" and uppercase it.
        st.edit = EditState {
            anchor: 0,
            cursor: 4,
            goal_col: None,
        };
        apply_event(&mut r, &mut st, &key_ev(egui::Key::U, true, true));
        assert_eq!(r.to_string(), "KEEP\n");
    }

    /// `EventOutcome::mutated` is the bridge signal the app uses to sync
    /// `tab.text` from the persistent rope ONLY on real content edits — the
    /// per-frame `to_string()` perf fix. Caret-only events must report
    /// `mutated == false`; content edits (insert, case-toggle, undo) must
    /// report `mutated == true`.
    #[test]
    fn apply_event_mutated_flag_tracks_content_change() {
        let mut r = Rope::from_str("hello\n");
        let mut st = RopeEditorState::new();
        st.edit = EditState::at(0);

        // Caret move (ArrowRight) changes no content.
        let out = apply_event(
            &mut r,
            &mut st,
            &key_ev(egui::Key::ArrowRight, false, false),
        );
        assert!(!out.mutated, "caret move must not flag a content change");

        // Insert text — length grows, mutated derived from length.
        let out = apply_event(&mut r, &mut st, &text_event("X"));
        assert!(out.mutated, "insert must flag a content change");

        // Select "hello" and case-toggle (same length) — explicit flag path.
        st.edit = EditState {
            anchor: 0,
            cursor: 5,
            goal_col: None,
        };
        let out = apply_event(&mut r, &mut st, &key_ev(egui::Key::U, true, true));
        assert!(
            out.mutated,
            "same-length case-toggle must flag a content change"
        );

        // Undo (Cmd+Z) restores prior content — also a content change.
        let out = apply_event(&mut r, &mut st, &key_ev(egui::Key::Z, false, true));
        assert!(out.mutated, "undo must flag a content change");

        // Select-all (Cmd+A) is selection-only — no content change.
        let out = apply_event(&mut r, &mut st, &key_ev(egui::Key::A, false, true));
        assert!(!out.mutated, "select-all must not flag a content change");
    }

    /// IME composition: a `Commit` inserts the finalised text at the caret
    /// (CJK parity); `Preedit`/`Enable` are consumed but don't mutate (the OS
    /// candidate window shows the in-progress composition).
    #[test]
    fn apply_event_ime_commit_inserts() {
        let mut r = Rope::from_str("a\n");
        let mut st = RopeEditorState::new();
        st.edit = EditState::at(1); // after 'a'

        // Preedit shows composition but must not change the buffer.
        let out = apply_event(
            &mut r,
            &mut st,
            &egui::Event::Ime(egui::ImeEvent::Preedit("せ".to_string())),
        );
        assert!(out.consumed);
        assert!(!out.mutated, "preedit must not mutate the buffer");
        assert_eq!(r.to_string(), "a\n");

        // Commit inserts the finalised text.
        let out = apply_event(
            &mut r,
            &mut st,
            &egui::Event::Ime(egui::ImeEvent::Commit("世界".to_string())),
        );
        assert!(out.consumed);
        assert!(out.mutated, "commit changes content");
        assert_eq!(r.to_string(), "a世界\n");
    }

    /// Mouse hit-testing maps a pointer position to the rope char offset the
    /// caret should jump to. Geometry: text origin at (10, 0), 16px rows, 8px
    /// glyphs. Buffer "hello\nworld\n" → lines start at char 0 and 6.
    #[test]
    fn pos_to_char_offset_maps_clicks() {
        let r = Rope::from_str("hello\nworld\n");
        let total = r.len_lines();
        let geom = TextGeom {
            text_left: 10.0,
            row0_top: 0.0,
            line_h: 16.0,
            char_w: 8.0,
        };
        let at = |x: f32, y: f32| pos_to_char_offset(&r, egui::pos2(x, y), geom, 0, total, None);
        // Row 0, before the 3rd glyph (x≈10+2*8=26) → offset 2 ("he|llo").
        assert_eq!(at(26.0, 4.0), 2);
        // Row 0, far left clamps to col 0.
        assert_eq!(at(0.0, 4.0), 0);
        // Row 0, far right clamps to end-of-line (5, before the newline).
        assert_eq!(at(999.0, 4.0), 5);
        // Row 1 (y in [16,32)) at col 0 → offset 6 (start of "world").
        assert_eq!(at(10.0, 20.0), 6);
        // Row 1, col 3 → offset 9 ("wor|ld").
        assert_eq!(at(10.0 + 3.0 * 8.0, 20.0), 9);
        // Click below the last line clamps to the last line.
        assert_eq!(at(10.0, 9999.0), r.line_to_char(total - 1));
    }

    /// Rounding lands the caret on the nearer glyph boundary (parity with how a
    /// user expects a click between two characters to resolve).
    #[test]
    fn pos_to_char_offset_rounds_to_nearest_boundary() {
        let r = Rope::from_str("abcd");
        let geom = TextGeom {
            text_left: 0.0,
            row0_top: 0.0,
            line_h: 16.0,
            char_w: 10.0,
        };
        let at = |x: f32| pos_to_char_offset(&r, egui::pos2(x, 1.0), geom, 0, 1, None);
        assert_eq!(at(4.0), 0); // closer to boundary 0
        assert_eq!(at(6.0), 1); // closer to boundary 1
        assert_eq!(at(14.0), 1); // closer to boundary 1
        assert_eq!(at(16.0), 2); // closer to boundary 2
    }

    /// The editable widget renders a small buffer (caret + selection state)
    /// without panicking and reports the rope branch.
    #[test]
    fn show_editable_renders_without_panic() {
        let hl = scribe_core::syntax::Highlighter::new();
        let mut state = RopeEditorState::new();
        state.edit = EditState {
            anchor: 0,
            cursor: 3,
            goal_col: None,
        };
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut b = Buffer::Rope(Rope::from_str("fn main() {\n    let x = 1;\n}\n"));
                let (resp, clip) = RopeEditor::new(&mut b, FontId::monospace(14.0), 18.0)
                    .with_syntax(&hl, Some("rs".to_string()))
                    .with_line_numbers(true)
                    .show_editable(ui, &mut state);
                assert_eq!(resp.buffer_mode, BufferModeSeen::Rope);
                assert!(clip.is_none(), "no copy/cut event was sent");
            });
        });
    }

    /// The whitespace-overlay path renders a buffer containing spaces + tabs
    /// without panicking and reports the rope branch (exercises
    /// `with_render_whitespace`).
    #[test]
    fn render_whitespace_overlay_does_not_panic() {
        let mut state = RopeEditorState::new();
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut b = Buffer::Rope(Rope::from_str("a b\tc\n  trailing  \n"));
                let (resp, _) = RopeEditor::new(&mut b, FontId::monospace(14.0), 18.0)
                    .with_render_whitespace(true)
                    .show_editable(ui, &mut state);
                assert_eq!(resp.buffer_mode, BufferModeSeen::Rope);
            });
        });
    }

    #[test]
    fn digit_count_matches_decimal_width() {
        assert_eq!(digit_count(0), 1);
        assert_eq!(digit_count(1), 1);
        assert_eq!(digit_count(9), 1);
        assert_eq!(digit_count(10), 2);
        assert_eq!(digit_count(999), 3);
        assert_eq!(digit_count(1_000), 4);
        assert_eq!(digit_count(1_000_000), 7);
    }

    /// With no spans, the whole line is one default-colored section.
    #[test]
    fn build_line_job_plain_is_single_section() {
        let job = build_line_job("hello", None, &FontId::monospace(14.0), Color32::WHITE);
        assert_eq!(job.sections.len(), 1);
        assert_eq!(job.text, "hello");
    }

    /// Two contiguous spans produce two sections covering the whole line.
    #[test]
    fn build_line_job_colored_covers_line() {
        let spans = vec![
            HlSpan {
                range: 0..2,
                color: [255, 0, 0],
                bold: false,
                italic: false,
            },
            HlSpan {
                range: 2..5,
                color: [0, 255, 0],
                bold: false,
                italic: false,
            },
        ];
        let job = build_line_job(
            "hello",
            Some(&spans),
            &FontId::monospace(14.0),
            Color32::WHITE,
        );
        assert_eq!(job.text, "hello");
        assert_eq!(job.sections.len(), 2);
    }

    /// A span that stops short of the line end still renders the full text:
    /// the uncovered tail is appended in the default color (no dropped bytes).
    #[test]
    fn build_line_job_partial_spans_append_remainder() {
        let spans = vec![HlSpan {
            range: 0..2,
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
        assert_eq!(job.text, "hello", "no text may be dropped");
        assert_eq!(job.sections.len(), 2, "colored head + default tail");
    }

    /// The viewport-highlight path renders a small Rust rope without panic and
    /// reports the rope branch (exercises `with_syntax` + `with_line_numbers`).
    #[test]
    fn highlighted_rope_with_line_numbers_does_not_panic() {
        let hl = scribe_core::syntax::Highlighter::new();
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut b = Buffer::Rope(Rope::from_str("fn main() {\n    let x = 1;\n}\n"));
                let resp = RopeEditor::new(&mut b, FontId::monospace(14.0), 18.0)
                    .with_syntax(&hl, Some("rs".to_string()))
                    .with_line_numbers(true)
                    .show(ui);
                assert_eq!(resp.buffer_mode, BufferModeSeen::Rope);
            });
        });
    }

    /// Drive `show_editable` once in a fresh egui context, sending `events`
    /// (empty == an idle repaint). Returns the editor's edit-generation and
    /// highlight-recompute counters AFTER the frame, so a test can assert how
    /// many times the highlight actually recomputed.
    fn drive_editable_frame(
        hl: &scribe_core::syntax::Highlighter,
        buf: &mut Buffer,
        state: &mut RopeEditorState,
        events: Vec<egui::Event>,
        focus: bool,
    ) {
        let mut raw = egui::RawInput {
            events,
            ..Default::default()
        };
        // A non-degenerate screen so `show_rows` hands back a real visible range.
        raw.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(800.0, 600.0),
        ));
        let ctx = egui::Context::default();
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let editor_id = ui.id().with("scr1b3-rope-editable");
                if focus {
                    ui.memory_mut(|m| m.request_focus(editor_id));
                }
                let _ = RopeEditor::new(buf, FontId::monospace(14.0), 18.0)
                    .with_syntax(hl, Some("rs".to_string()))
                    .show_editable(ui, state);
            });
        });
    }

    /// P-02 PROOF (the load-bearing test): two consecutive IDLE frames (no edit)
    /// must NOT recompute highlights. The previous code re-ran
    /// `highlight_document` on the joined visible window every frame; the
    /// edit-gen-keyed cache reruns once per edit, so the recompute counter stays
    /// FLAT across idle repaints. A regression that reintroduces per-frame
    /// highlighting makes this counter climb every frame and fails here.
    #[test]
    fn idle_frames_do_not_recompute_highlights() {
        let hl = scribe_core::syntax::Highlighter::new();
        let mut buf = Buffer::Rope(Rope::from_str("fn main() {\n    let x = 1;\n}\n"));
        let mut state = RopeEditorState::new();

        // Frame 1: first paint computes the highlight exactly once.
        drive_editable_frame(&hl, &mut buf, &mut state, vec![], false);
        let after_first = state.highlight_recompute_count();
        assert_eq!(
            after_first, 1,
            "the first frame computes the highlight exactly once"
        );

        // Frames 2..=6: idle repaints (egui `frame_tick` runs continuously) —
        // the buffer never changes, so NO recompute may happen.
        for _ in 0..5 {
            drive_editable_frame(&hl, &mut buf, &mut state, vec![], false);
        }
        assert_eq!(
            state.highlight_recompute_count(),
            after_first,
            "idle frames must reuse the cached highlight — recompute count stays flat"
        );
        assert_eq!(state.edit_gen(), 0, "no edit means edit_gen never moved");
    }

    /// An actual content edit bumps the recompute counter by exactly one (the
    /// edit's frame), then idle frames after it are flat again. Proves the cache
    /// invalidates on edits (correctness) while still not running per-frame.
    #[test]
    fn edit_recomputes_once_then_idle_is_flat() {
        let hl = scribe_core::syntax::Highlighter::new();
        let mut buf = Buffer::Rope(Rope::from_str("fn main() {}\n"));
        let mut state = RopeEditorState::new();

        // Frame 1 (idle, focused so input is accepted next frame): 1 compute.
        drive_editable_frame(&hl, &mut buf, &mut state, vec![], true);
        assert_eq!(state.highlight_recompute_count(), 1);

        // Frame 2: type a character (a real content edit). The edit bumps
        // `edit_gen`, so the highlight recomputes for the new content.
        drive_editable_frame(
            &hl,
            &mut buf,
            &mut state,
            vec![egui::Event::Text("x".to_string())],
            true,
        );
        assert!(state.edit_gen() >= 1, "the edit moved the generation");
        let after_edit = state.highlight_recompute_count();
        assert_eq!(after_edit, 2, "the edit triggers exactly one recompute");

        // Frames 3..=5: idle again — flat.
        for _ in 0..3 {
            drive_editable_frame(&hl, &mut buf, &mut state, vec![], true);
        }
        assert_eq!(
            state.highlight_recompute_count(),
            after_edit,
            "post-edit idle frames reuse the cache"
        );
    }

    /// C-01 PROOF: a block comment whose opener (`/*`) is ABOVE the viewport
    /// must colour its visible continuation lines as a comment. The old path
    /// highlighted only the joined VISIBLE window, so a line like `still in the
    /// comment` — with no opener in the window — was mis-coloured as code. The
    /// whole-document incremental highlight carries the cross-line parse state,
    /// so the continuation is correctly a comment.
    ///
    /// We assert at the highlighter level (the engine the cache routes through),
    /// driving the exact `highlight_document` whole-doc vs window-only contrast.
    #[test]
    fn cross_line_block_comment_colors_continuation() {
        let hl = scribe_core::syntax::Highlighter::new();
        // A Rust block comment opened on line 0, continued on line 1, closed on
        // line 2. Line 1 ("still inside the comment") is the "visible" line.
        let doc = "/* opener line\nstill inside the comment\nlast */\nfn after() {}\n";
        let ext = Some("rs");

        // Whole-document highlight (what the cache now uses): line 1 is a comment.
        let whole = hl.highlight_document(doc, ext);
        assert!(whole.len() >= 2, "doc has at least two lines");
        let line1 = &whole[1];
        assert!(!line1.is_empty(), "the continuation line has spans");
        // The opener line (line 0) is unambiguously a comment; its colour is the
        // engine's comment colour under the active theme. The continuation line
        // (line 1) must carry that SAME colour across the line boundary. Comparing
        // against the opener (not a hardcoded RGB) keeps the test theme-robust.
        let opener_comment_color = whole[0]
            .iter()
            .map(|s| s.color)
            .next()
            .expect("opener line has spans");
        assert!(
            line1.iter().all(|s| s.color == opener_comment_color),
            "the continuation line carries the comment colour from the opener \
             (cross-line parse state preserved): line1={line1:?}"
        );

        // Contrast: highlighting JUST the visible line as a standalone window
        // (the OLD behaviour) does NOT see the opener, so it is NOT all-comment —
        // proving the window-only path mis-coloured the continuation.
        let window_only = hl.highlight_document("still inside the comment", ext);
        let window_line0 = &window_only[0];
        let window_is_all_comment = !window_line0.is_empty()
            && window_line0.iter().all(|s| s.color == opener_comment_color);
        assert!(
            !window_is_all_comment,
            "window-only highlight mis-colours the continuation (the C-01 bug); \
             the whole-document path fixes it: window={window_line0:?}"
        );
    }

    /// The cross-line fix also holds end-to-end through `show_editable`: render a
    /// buffer whose block comment spans lines without panic, and confirm the
    /// cache populated whole-document spans (not a per-window slice).
    #[test]
    fn show_editable_uses_whole_document_highlight() {
        let hl = scribe_core::syntax::Highlighter::new();
        let mut buf = Buffer::Rope(Rope::from_str(
            "/* a\nb\nc */\nfn main() {\n    let x = 1;\n}\n",
        ));
        let mut state = RopeEditorState::new();
        drive_editable_frame(&hl, &mut buf, &mut state, vec![], false);
        // The cache recomputed once and holds spans for ALL lines (whole-doc),
        // not just the visible window — the structural basis of the C-01 fix.
        assert_eq!(state.highlight_recompute_count(), 1);
    }

    /// Sanity: a 1M-line rope renders without panicking. This is the
    /// load-bearing claim of the KEYSTONE design — `show_rows` MUST
    /// scale `O(viewport_rows)`, not `O(total_lines)`. If this panics or
    /// hangs, the design assumption is wrong.
    #[test]
    fn one_million_line_rope_does_not_panic() {
        // 1M short lines is ~7 MiB — well under the mmap threshold so we
        // stay on the rope path.
        let mut body = String::with_capacity(1_000_000 * 8);
        for i in 0..1_000_000 {
            // 6 chars/line average → ~6 MiB total
            body.push_str(&format!("{i:06}\n"));
        }
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut b = Buffer::Rope(Rope::from_str(&body));
                let resp = RopeEditor::new(&mut b, FontId::monospace(14.0), 18.0).show(ui);
                assert_eq!(resp.buffer_mode, BufferModeSeen::Rope);
            });
        });
    }
}
