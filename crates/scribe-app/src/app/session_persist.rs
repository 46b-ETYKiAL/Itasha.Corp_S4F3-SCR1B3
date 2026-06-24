//! Session persistence + hot-exit backup + auto-save tail of `frame_tick`,
//! extracted from `mod.rs` (A-01 wave 3).
//!
//! Behavior-neutral split: the end-of-frame persistence steps (paths-only
//! session save, throttled hot-exit content backup, opt-in auto-save of dirty
//! file-backed buffers) are moved verbatim out of `frame_tick`. The block reads
//! and writes only `self`; nothing else changes.
#![allow(clippy::wildcard_imports)]

use super::*;

impl ScribeApp {
    /// Persist the open-file session, flush hot-exit content backups, and run
    /// opt-in auto-save — the end-of-frame persistence tail of `frame_tick`.
    ///
    /// Moved verbatim from the inline tail; each step's throttle, change-detection
    /// signature, and condition is identical.
    pub(super) fn persist_session_and_autosave(&mut self) {
        // Persist the open-file session when it changes (for restore-on-launch).
        if self.config.editor.restore_session {
            let sig = session_signature(&self.tabs);
            if sig != self.session_sig {
                let paths: Vec<PathBuf> = self
                    .tabs
                    .iter()
                    .filter_map(|t| t.doc.path().map(|p| p.to_path_buf()))
                    .collect();
                save_session(&paths);
                self.session_sig = sig;
            }
        }

        // Hot-exit: periodically flush unsaved buffer CONTENT to the backup
        // store so an unsaved note survives a crash/restart. Throttled so we
        // don't rewrite content every keystroke, and only when something is
        // actually unsaved.
        if self.config.editor.session_backup {
            const BACKUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(4);
            let due = self
                .last_backup_at
                .map(|t| t.elapsed() >= BACKUP_INTERVAL)
                .unwrap_or(true);
            let has_unsaved = self
                .tabs
                .iter()
                .any(|t| t.is_dirty() || (t.doc.path().is_none() && !t.text.is_empty()));
            // Audit fix F1: only rewrite backups when the unsaved CONTENT
            // actually changed since the last flush — avoids re-writing the
            // same bytes every interval while a buffer sits dirty but idle.
            let content_sig = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                for t in &self.tabs {
                    if t.is_dirty() || (t.doc.path().is_none() && !t.text.is_empty()) {
                        t.text.hash(&mut h);
                        t.doc.path().map(|p| p.to_path_buf()).hash(&mut h);
                    }
                }
                h.finish()
            };
            if due && has_unsaved && content_sig != self.last_backup_sig {
                self.snapshot_session_backups();
                self.last_backup_sig = content_sig;
            }
        }

        // Auto-save (opt-in, default OFF): after a quiet interval, write any
        // dirty file-backed buffer to disk. Untitled buffers are never
        // auto-saved (no path → would pop a dialog). Throttled like backups.
        if self.config.editor.auto_save {
            const AUTOSAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);
            let due = self
                .last_autosave_at
                .map(|t| t.elapsed() >= AUTOSAVE_INTERVAL)
                .unwrap_or(true);
            if due {
                let dirty: Vec<usize> = (0..self.tabs.len())
                    .filter(|&i| self.tabs[i].doc.path().is_some() && self.tabs[i].is_dirty())
                    .collect();
                if !dirty.is_empty() {
                    let prev_active = self.active;
                    for i in dirty {
                        self.active = i;
                        self.save_active();
                    }
                    self.active = prev_active.min(self.tabs.len().saturating_sub(1));
                }
                self.last_autosave_at = Some(std::time::Instant::now());
            }
        }
    }
}
