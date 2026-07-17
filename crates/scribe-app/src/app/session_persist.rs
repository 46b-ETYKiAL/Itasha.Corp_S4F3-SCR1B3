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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // A backup-enabled app whose single tab is `tab`, with the backup clock set
    // >4s ago (so it's DUE) and last_backup_sig cleared. The hot-exit branch
    // stamps `last_backup_sig` iff (due && has_unsaved && content_sig != sig);
    // snapshot_session_backups is a no-op under new_test (config_dir is None), so
    // `last_backup_sig != 0` is a clean fire/no-fire observable.
    fn due_backup_app(tab: EditorTab) -> ScribeApp {
        let mut cfg = Config::default();
        cfg.editor.session_backup = true;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs.clear();
        app.tabs.push(tab);
        app.active = 0;
        app.last_backup_at = Some(Instant::now().checked_sub(Duration::from_secs(5)).unwrap());
        app.last_backup_sig = 0;
        app
    }

    #[test]
    fn backup_fires_for_a_due_untitled_unsaved_buffer() {
        // due(true) && has_unsaved(true) && content_sig!=0 -> fires. Kills the
        // due `elapsed >= INTERVAL -> <` (41:38).
        let mut t = EditorTab::scratch();
        t.text = "unsaved".into();
        t.doc_id = crate::grid::DocId(1);
        let mut app = due_backup_app(t);
        app.persist_session_and_autosave();
        assert_ne!(app.last_backup_sig, 0, "a due, unsaved buffer triggers the hot-exit backup");
    }

    #[test]
    fn backup_skips_when_only_a_clean_saved_file_is_open() {
        // has_unsaved = is_dirty(false) || (path.is_none(false) && ...) = false ->
        // no fire. The `|| -> &&` in the 2nd disjunct (46:66 / 54:64) would make a
        // clean saved file read as unsaved and fire.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("saved.txt");
        std::fs::write(&p, "on disk").unwrap();
        let mut t = EditorTab::from_path(p).expect("open");
        t.doc_id = crate::grid::DocId(1);
        let mut app = due_backup_app(t);
        assert!(!app.tabs[0].is_dirty(), "precondition: the opened file is clean");
        app.persist_session_and_autosave();
        assert_eq!(app.last_backup_sig, 0, "a clean saved file is not 'unsaved'");
    }

    #[test]
    fn backup_skips_for_an_empty_untitled_buffer() {
        // has_unsaved = is_dirty(false) || (path.is_none(true) && !empty(false)) =
        // false -> no fire. The `delete !` in `!text.is_empty()` (46:69 / 54:67)
        // would make an empty untitled buffer read as unsaved and fire.
        let mut t = EditorTab::scratch();
        t.doc_id = crate::grid::DocId(1);
        let mut app = due_backup_app(t);
        assert!(!app.tabs[0].is_dirty(), "precondition: an empty scratch is clean");
        app.persist_session_and_autosave();
        assert_eq!(app.last_backup_sig, 0, "an empty untitled buffer has nothing to back up");
    }

    #[test]
    fn backup_fires_for_a_dirty_file_backed_buffer() {
        // has_unsaved = is_dirty(true) || ... = true -> fires. The `|| -> &&`
        // (46:39 / 54:37) makes `is_dirty() && (path.is_none() && ...)` false for a
        // file-backed tab, suppressing the fire.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("edited.txt");
        std::fs::write(&p, "on disk").unwrap();
        let mut t = EditorTab::from_path(p).expect("open");
        t.text = "edited in memory".into();
        t.doc_id = crate::grid::DocId(1);
        let mut app = due_backup_app(t);
        assert!(app.tabs[0].is_dirty(), "precondition: the edited file is dirty");
        app.persist_session_and_autosave();
        assert_ne!(app.last_backup_sig, 0, "a dirty file-backed buffer is unsaved");
    }
}
