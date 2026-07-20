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
    /// Whether the multi-cursor handler should STEAL the Escape key this frame.
    /// True only when multi-cursor is genuinely active (≥1 secondary caret or a
    /// live column selection — [`MultiCursor::is_active`]) AND the editor is the
    /// active surface (no find bar / palette / settings overlay is open). When an
    /// overlay is open it needs Escape itself (close find, dismiss the palette),
    /// and stealing it here would starve that consumer or double-handle the key
    /// (P2-E). Pure predicate so the gating is unit-testable without a frame.
    pub(super) fn mc_should_consume_escape(&self, overlay_open: bool) -> bool {
        self.multi_cursor.is_active() && !overlay_open
    }

    /// Collapse multi-cursor to a single caret when Escape is pressed. Consumes
    /// the Escape (so egui's TextEdit does not also surrender focus on it) ONLY
    /// when [`Self::mc_should_consume_escape`] holds — so Escape still closes
    /// find / zen / the palette / the completion popup in the common
    /// single-caret case AND whenever an overlay is open. Focus-independent
    /// otherwise (egui can transiently drop editor focus between an intercepted
    /// edit and the next key).
    pub(super) fn mc_collapse_on_escape(&mut self, ctx: &egui::Context, overlay_open: bool) {
        if !self.mc_should_consume_escape(overlay_open) {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.mc_clear_carets();
            ctx.request_repaint();
        }
    }

    /// Drop all multi-cursor state (secondaries + column anchor + the owning
    /// `doc_id`). The single choke point for invalidating carets — used by the
    /// Esc collapse, the tab-scope reconcile, and every out-of-band buffer
    /// mutation (palette transform / external reload) so stale carets can never
    /// edit at wrong offsets (P1-A / P2-C).
    pub(super) fn mc_clear_carets(&mut self) {
        self.multi_cursor.clear();
        self.column_anchor = None;
        self.mc_owner_doc = None;
    }

    /// Bind the app-global multi-cursor state to the tab it was built on. The
    /// secondaries index into ONE specific document's buffer; the editor is keyed
    /// per tab, and switching tabs auto-focuses the new editor "no click required"
    /// — so without this the next keystroke would replay tab A's carets against
    /// tab B and silently corrupt the wrong document (P1-A). Called at the TOP of
    /// the editor render: when the carets' owning `doc_id` no longer matches the
    /// active tab, drop them before any edit/paint this frame. A `None` owner
    /// (freshly created this frame, not yet recorded) is never treated as a
    /// mismatch.
    pub(super) fn mc_reconcile_owner(&mut self, active: usize) {
        if active >= self.tabs.len() {
            return;
        }
        let active_doc = self.tabs[active].doc_id;
        let dirty = self.multi_cursor.is_active()
            || !self.multi_cursor.secondaries().is_empty()
            || self.column_anchor.is_some();
        if dirty && self.mc_owner_doc.is_some() && self.mc_owner_doc != Some(active_doc) {
            self.mc_clear_carets();
        }
    }

    /// Record the active tab's `doc_id` as the owner of the current multi-cursor
    /// state (or clear the owner when no state is live). Called at the END of the
    /// editor frame so gestures created this frame (Ctrl+D / Ctrl+click / Alt+drag)
    /// are attributed to the tab they were built on; the next frame's
    /// [`Self::mc_reconcile_owner`] then invalidates them on a tab switch.
    pub(super) fn mc_record_owner(&mut self, active: usize) {
        if active >= self.tabs.len() {
            return;
        }
        let dirty = self.multi_cursor.is_active()
            || !self.multi_cursor.secondaries().is_empty()
            || self.column_anchor.is_some();
        self.mc_owner_doc = if dirty {
            Some(self.tabs[active].doc_id)
        } else {
            None
        };
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
        // Defensive backstop to the top-of-frame owner reconcile: NEVER replay
        // carets built against a different document (a mid-frame tab switch must
        // not slip an edit onto the wrong buffer). P1-A.
        if self.mc_owner_doc.is_some() && self.mc_owner_doc != Some(self.tabs[active].doc_id) {
            self.mc_clear_carets();
            return;
        }
        let ops = ctx.input_mut(|i| mc_collect_edit_ops(&mut i.events));
        if ops.is_empty() {
            return;
        }
        let Some(mut primary) = mc_load_primary(ctx, editor_id) else {
            return;
        };
        // FIX-4 (P2-D): snapshot the PRE-edit (cursor, text) into egui's own
        // TextEdit undoer so a subsequent Ctrl+Z reverts the ENTIRE multi-caret
        // edit as ONE whole-text step. The app-side splice never flows through
        // egui, so without this checkpoint Ctrl+Z would restore whatever egui
        // last auto-snapshotted, desyncing the text from the caret set.
        // GRANULARITY LIMIT: undo is whole-buffer per multi-caret batch, NOT
        // per-caret — egui 0.34's `Undoer<(CCursorRange, String)>` cannot express
        // N independent caret ranges in a single undo step.
        let undo_checkpoint = (
            egui::text::CCursorRange::two(
                egui::text::CCursor::new(primary.anchor),
                egui::text::CCursor::new(primary.head),
            ),
            self.tabs[active].text.clone(),
        );
        for op in ops {
            let np = self
                .multi_cursor
                .apply_edit(&mut self.tabs[active].text, primary, op);
            primary = Caret::at(np);
        }
        mc_push_undo_checkpoint(ctx, editor_id, undo_checkpoint);
        // Parity with the TextEdit edit path: mark the doc dirty and bump the gen
        // counter so the minimap / spell / change-bar caches refresh.
        self.tabs[active].doc.mark_dirty();
        self.tabs[active].edit_gen = self.tabs[active].edit_gen.wrapping_add(1);
        mc_set_primary(ctx, editor_id, primary.head, primary.head);
        // The carets still index into THIS tab's (now-mutated) buffer; refresh the
        // owner so the tab-scope reconcile keeps them alive next frame.
        self.mc_owner_doc = Some(self.tabs[active].doc_id);
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

/// Push a pre-edit `(cursor, text)` checkpoint onto egui's TextEdit undoer for
/// `editor_id` so a following Ctrl+Z reverts the app-side multi-caret splice as
/// ONE whole-text step (FIX-4 / P2-D). `add_undo` is a no-op when the checkpoint
/// already equals the undoer's latest point, so repeated multi-cursor keystrokes
/// don't stack duplicate states. Uses only the public `undoer()` / `set_undoer()`
/// seam (the `undoer` field itself is `pub(crate)` in egui).
pub(super) fn mc_push_undo_checkpoint(
    ctx: &egui::Context,
    editor_id: egui::Id,
    checkpoint: (egui::text::CCursorRange, String),
) {
    let mut state = egui::TextEdit::load_state(ctx, editor_id).unwrap_or_default();
    let mut undoer = state.undoer();
    undoer.add_undo(&checkpoint);
    state.set_undoer(undoer);
    state.store(ctx, editor_id);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::DocId;

    #[test]
    fn collect_edit_ops_ignores_empty_text_events() {
        // An empty Text event must NOT become a spurious Insert(""). The
        // `if !t.is_empty() -> if true` mutant captures it. Kills 229:33.
        let mut events = vec![egui::Event::Text(String::new())];
        let ops = mc_collect_edit_ops(&mut events);
        assert!(ops.is_empty(), "empty text produces no edit op");
        assert_eq!(events.len(), 1, "the empty text event is left for egui");
    }

    #[test]
    fn collect_edit_ops_respects_the_modifier_gate_for_edit_keys() {
        // Plain edit key -> captured + removed; modified combo -> left for egui.
        // The `modifiers.is_none() -> false` drops the plain key; `-> true`
        // wrongly captures a Ctrl+combo. Kills 238:14, 247:14, 256:14.
        fn key(k: egui::Key, m: egui::Modifiers) -> egui::Event {
            egui::Event::Key {
                key: k,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: m,
            }
        }
        for (k, op) in [
            (egui::Key::Backspace, EditOp::Backspace),
            (egui::Key::Delete, EditOp::Delete),
            (egui::Key::Enter, EditOp::Insert("\n".to_string())),
        ] {
            let mut plain = vec![key(k, egui::Modifiers::NONE)];
            let ops = mc_collect_edit_ops(&mut plain);
            assert_eq!(ops, vec![op.clone()], "plain {k:?} is captured");
            assert!(plain.is_empty(), "captured event removed from the queue");
            let mut ctrl = vec![key(k, egui::Modifiers::CTRL)];
            let ops2 = mc_collect_edit_ops(&mut ctrl);
            assert!(ops2.is_empty(), "modified {k:?} is NOT captured");
            assert_eq!(ctrl.len(), 1, "modified event left for egui");
        }
    }

    #[test]
    fn mc_reconcile_owner_keeps_owner_when_no_carets_are_live() {
        // Empty multi-cursor state is NOT dirty -> reconcile must NOT clear the
        // owner even on a doc mismatch. The `!secondaries.is_empty() ->
        // secondaries.is_empty()` mutant reads empty as dirty. Kills 77:16.
        let mut app = ScribeApp::new_test(Config::default());
        let active_doc = app.tabs[0].doc_id;
        let other = DocId(active_doc.0 + 1);
        app.multi_cursor.clear();
        app.column_anchor = None;
        app.mc_owner_doc = Some(other);
        app.mc_reconcile_owner(0);
        assert_eq!(
            app.mc_owner_doc,
            Some(other),
            "an empty multi-cursor state is not dirty; owner untouched"
        );
    }

    #[test]
    fn mc_reconcile_owner_clears_a_bare_column_anchor_on_doc_mismatch() {
        // A live column_anchor with no secondaries is still dirty and MUST be
        // dropped on a doc mismatch. The second `|| -> &&` makes a bare anchor
        // read as not-dirty. Kills 78:13.
        let mut app = ScribeApp::new_test(Config::default());
        let active_doc = app.tabs[0].doc_id;
        let other = DocId(active_doc.0 + 1);
        app.multi_cursor.clear();
        app.column_anchor = Some(3);
        app.mc_owner_doc = Some(other);
        app.mc_reconcile_owner(0);
        assert!(
            app.column_anchor.is_none(),
            "the stale column anchor was dropped"
        );
        assert_eq!(app.mc_owner_doc, None, "and the owner was reset");
    }

    #[test]
    fn mc_record_owner_marks_dirty_for_a_bare_column_anchor() {
        // A column_anchor alone (no secondaries) is dirty state that must be
        // attributed to the active tab. The second `|| -> &&` makes a bare anchor
        // read as not-dirty -> owner wrongly None. Kills 95:13.
        let mut app = ScribeApp::new_test(Config::default());
        app.multi_cursor.clear();
        app.column_anchor = Some(2);
        app.mc_record_owner(0);
        assert_eq!(
            app.mc_owner_doc,
            Some(app.tabs[0].doc_id),
            "a column_anchor alone marks the state as owned"
        );
    }
}
