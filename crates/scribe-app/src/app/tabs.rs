//! Tab-strip lifecycle: reorder, close (one/all/after/others), and reopen-closed. Method bodies moved verbatim from the `app` god-module (A-01 decomposition); the struct + fields stay in mod.rs, so `use super::*` re-exports every type these methods touch.
#![allow(clippy::wildcard_imports)]
use super::*;

impl ScribeApp {
    /// Move the tab at `src` so it takes original position `target`'s slot
    /// (drag-and-drop reorder), keeping [`Self::active`] pointed at the same
    /// buffer the user is editing. No-op if either index is out of range or
    /// they are equal. Index math is in [`tab_index_after_move`].
    pub(super) fn move_tab(&mut self, src: usize, target: usize) {
        if src >= self.tabs.len() || target >= self.tabs.len() || src == target {
            return;
        }
        // #R5: pinned notes are anchored — refuse to reorder a pinned tab.
        if self.tabs[src].pinned {
            return;
        }
        let new_active = tab_index_after_move(src, target, self.active);
        let tab = self.tabs.remove(src);
        // `target < original len` ⇒ `target <= new len`, so this never panics.
        self.tabs.insert(target, tab);
        self.active = new_active.min(self.tabs.len().saturating_sub(1));
    }

    /// Close every tab whose index is not `keep` AND is not pinned (F-044).
    pub(super) fn close_all_tabs_except(&mut self, keep: usize) {
        if keep >= self.tabs.len() {
            return;
        }
        // The kept tab's index AFTER removal equals the number of pinned tabs
        // that precede it: those survive and stay to its left, while every
        // other tab before `keep` is removed. Compute it before mutating so we
        // can focus the surviving copy of `keep` (not a clamped fallback).
        let new_keep = (0..keep).filter(|&i| self.tabs[i].pinned).count();
        // Walk back-to-front so swap-remove indices stay valid; never remove
        // the kept index or any pinned tab.
        let mut i = self.tabs.len();
        while i > 0 {
            i -= 1;
            if i != keep && !self.tabs[i].pinned {
                self.tabs.remove(i);
            }
        }
        // Focus the tab the user chose to keep.
        self.active = new_keep.min(self.tabs.len().saturating_sub(1));
    }

    /// Close every tab after `after` (exclusive) that is not pinned (F-044).
    pub(super) fn close_tabs_after(&mut self, after: usize) {
        let mut i = self.tabs.len();
        while i > after + 1 {
            i -= 1;
            if !self.tabs[i].pinned {
                self.tabs.remove(i);
            }
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
    }

    /// Close every tab that is not pinned (F-044). If nothing was unpinned,
    /// leave the tabs alone (don't replace them with a scratch buffer).
    pub(super) fn close_all_tabs(&mut self) {
        let any_unpinned = self.tabs.iter().any(|t| !t.pinned);
        if any_unpinned {
            self.tabs.retain(|t| t.pinned);
        }
        if self.tabs.is_empty() {
            self.tabs.push(EditorTab::scratch());
        }
        self.active = 0;
    }

    pub(super) fn close_tab(&mut self, idx: usize) {
        // #R5: pinned notes can't be closed directly — unpin first. This is the
        // single chokepoint behind the ✕ button, middle-click, and the
        // context-menu "Close" in the tab strip.
        if idx < self.tabs.len() && self.tabs[idx].pinned {
            self.status = "Note is pinned — unpin it to close".to_string();
            return;
        }
        crate::action_log::record("tab", "close");
        if idx < self.tabs.len() {
            // F-021 — capture the current scroll position so the next open
            // of the same path restores it. Uses the last-frame
            // scroll_metrics value the central panel records.
            if let Some(path) = self.tabs[idx].doc.path().map(|p| p.to_path_buf()) {
                let key = path.display().to_string();
                let y = self.scroll_metrics.0;
                scribe_core::config::record_scroll_pos(
                    &mut self.config.editor.scroll_positions,
                    &key,
                    y,
                );
                // Remember the caret too (best-effort; restored on reopen).
                if self.config.editor.restore_cursor_position {
                    let cur = self.tabs[idx]
                        .rope_state
                        .as_ref()
                        .map(|s| s.edit.cursor)
                        .unwrap_or(0);
                    if self.config.editor.cursor_positions.len()
                        >= scribe_core::config::SCROLL_POS_CAP
                    {
                        if let Some(k) = self.config.editor.cursor_positions.keys().next().cloned()
                        {
                            self.config.editor.cursor_positions.remove(&k);
                        }
                    }
                    self.config.editor.cursor_positions.insert(key.clone(), cur);
                }
                self.save_config();
            }
            // Push the closed tab onto the reopen stack (Ctrl+Shift+T-style),
            // capturing its content + caret so an accidental close is one
            // keystroke from recovery. Skip pristine empty scratch tabs.
            let tab = &self.tabs[idx];
            let cursor = tab.rope_state.as_ref().map(|s| s.edit.cursor).unwrap_or(0);
            if tab.doc.path().is_some() || !tab.text.is_empty() {
                self.closed_tabs.push(ClosedTab {
                    path: tab.doc.path().map(|p| p.to_path_buf()),
                    text: tab.text.clone(),
                    cursor,
                });
                const MAX_CLOSED: usize = 20;
                if self.closed_tabs.len() > MAX_CLOSED {
                    self.closed_tabs.remove(0);
                }
            }
            self.tabs.remove(idx);
            if self.tabs.is_empty() {
                self.tabs.push(EditorTab::scratch());
            }
            self.active = self.active.min(self.tabs.len() - 1);
        }
    }

    /// Reopen the most recently closed tab (restoring its unsaved content +
    /// caret). No-op when the reopen stack is empty.
    pub(super) fn reopen_closed_tab(&mut self) {
        let Some(closed) = self.closed_tabs.pop() else {
            self.status = "no closed tab to reopen".to_string();
            return;
        };
        let mut tab = EditorTab::from_backup(closed.path, closed.text);
        if self.config.editor.experimental_rope_editor {
            let mut st = scribe_render::RopeEditorState::new();
            st.edit = scribe_core::editing::EditState::at(closed.cursor);
            tab.rope_state = Some(st);
        }
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.status = "reopened closed tab".to_string();
    }
}
