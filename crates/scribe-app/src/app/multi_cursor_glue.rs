//! egui glue for the P2 multi-cursor model (kept out of `frame_tick` and out of
//! `crate::multi_cursor` so the model stays egui-free and purely unit-testable).
//!
//! The keyboard seam ([`ScribeApp::handle_multi_cursor_keys`]) runs BEFORE the
//! central `TextEdit` shows this frame: it handles Esc-collapse, Ctrl/Cmd+D
//! select-next, and — while multi-cursor mode is engaged — pulls the plain edit
//! events (text / Backspace / Delete / Enter) out of the queue and replays them
//! at the primary + every secondary in one buffer mutation, so egui does not
//! ALSO apply them to the primary. The pointer seams (Ctrl+click add/toggle,
//! Alt+drag column build) need the laid-out galley for a pos→char hit-test and
//! so live inline in `frame_tick`, calling back into the model here.
#![allow(clippy::wildcard_imports)]

use super::*;

use crate::multi_cursor::{Caret, CtrlDOutcome, EditOp};

impl ScribeApp {
    /// Intercept the multi-cursor keyboard gestures (Esc collapse, Ctrl/Cmd+D
    /// select-next, and the edit keys while multi-cursor mode is engaged) BEFORE
    /// the central `TextEdit` consumes this frame's events. Assumes the central
    /// editor holds focus (the caller gates on it).
    /// Collapse multi-cursor to a single caret when Escape is pressed. Consumes
    /// the Escape (so egui's TextEdit does not also surrender focus on it) ONLY
    /// while multi-cursor is engaged, so Escape still closes find / zen / the
    /// completion popup in the common single-caret case. Focus-independent — the
    /// caller gates on [`crate::multi_cursor::MultiCursor::is_active`].
    pub(super) fn mc_collapse_on_escape(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.multi_cursor.clear();
            ctx.request_repaint();
        }
    }

    pub(super) fn handle_multi_cursor_keys(
        &mut self,
        ctx: &egui::Context,
        editor_id: egui::Id,
        active: usize,
    ) {
        // Ctrl/Cmd+D — select the word under the caret, then add each next match.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::D)) {
            if let Some(primary) = mc_load_primary(ctx, editor_id) {
                let text = self.tabs[active].text.clone();
                match self.multi_cursor.select_next_occurrence(&text, primary) {
                    CtrlDOutcome::SelectWord { start, end } => {
                        // First Ctrl+D just selects the word (egui adopts it).
                        mc_set_primary(ctx, editor_id, start, end);
                        ctx.request_repaint();
                    }
                    CtrlDOutcome::Added(_) => ctx.request_repaint(),
                    CtrlDOutcome::NoMatch => {}
                }
            }
            return;
        }

        // Edit replay — only while multi-cursor mode is engaged.
        if !self.multi_cursor.is_active() {
            return;
        }
        let ops = ctx.input_mut(|i| mc_collect_edit_ops(&mut i.events));
        if ops.is_empty() {
            return;
        }
        let Some(mut primary) = mc_load_primary(ctx, editor_id) else {
            return;
        };
        for op in ops {
            let np = self
                .multi_cursor
                .apply_edit(&mut self.tabs[active].text, primary, op);
            primary = Caret::at(np);
        }
        // Parity with the TextEdit edit path: mark the doc dirty and bump the gen
        // counter so the minimap / spell / change-bar caches refresh.
        self.tabs[active].doc.mark_dirty();
        self.tabs[active].edit_gen = self.tabs[active].edit_gen.wrapping_add(1);
        mc_set_primary(ctx, editor_id, primary.head, primary.head);
        ctx.request_repaint();
    }
}

/// Load egui's primary caret for `editor_id` as a [`Caret`] (anchor = egui's
/// `secondary`, head = egui's `primary`).
pub(super) fn mc_load_primary(ctx: &egui::Context, editor_id: egui::Id) -> Option<Caret> {
    let state = egui::TextEdit::load_state(ctx, editor_id)?;
    let range = state.cursor.char_range()?;
    Some(Caret::selection(range.secondary.index, range.primary.index))
}

/// The primary caret's head (moving-end) char index, if any.
pub(super) fn mc_load_primary_head(ctx: &egui::Context, editor_id: egui::Id) -> Option<usize> {
    mc_load_primary(ctx, editor_id).map(|c| c.head)
}

/// Write egui's primary caret to the `[anchor, head)` char range so the TextEdit
/// adopts it on its next `show()`.
pub(super) fn mc_set_primary(ctx: &egui::Context, editor_id: egui::Id, anchor: usize, head: usize) {
    let mut state = egui::TextEdit::load_state(ctx, editor_id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange {
        primary: egui::text::CCursor::new(head),
        secondary: egui::text::CCursor::new(anchor),
        h_pos: None,
    }));
    state.store(ctx, editor_id);
}

/// Pull the multi-cursor edit events (plain text / Backspace / Delete / Enter,
/// all un-modified) out of this frame's queue, in order, so egui's TextEdit does
/// not ALSO apply them to the primary caret — the app replays them at every
/// caret. Modified combos (Ctrl+Backspace word-delete, Shift+Enter, Tab) are
/// left for egui (primary only).
pub(super) fn mc_collect_edit_ops(events: &mut Vec<egui::Event>) -> Vec<EditOp> {
    let mut ops = Vec::new();
    events.retain(|ev| match ev {
        egui::Event::Text(t) if !t.is_empty() => {
            ops.push(EditOp::Insert(t.clone()));
            false
        }
        egui::Event::Key {
            key: egui::Key::Backspace,
            pressed: true,
            modifiers,
            ..
        } if modifiers.is_none() => {
            ops.push(EditOp::Backspace);
            false
        }
        egui::Event::Key {
            key: egui::Key::Delete,
            pressed: true,
            modifiers,
            ..
        } if modifiers.is_none() => {
            ops.push(EditOp::Delete);
            false
        }
        egui::Event::Key {
            key: egui::Key::Enter,
            pressed: true,
            modifiers,
            ..
        } if modifiers.is_none() => {
            ops.push(EditOp::Insert("\n".to_string()));
            false
        }
        _ => true,
    });
    ops
}
