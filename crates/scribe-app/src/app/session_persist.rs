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
    fn backup_content_sig_tracks_a_dirty_tabs_edits() {
        // The change-detection signature MUST include a dirty file-backed tab's
        // content, so a SECOND distinct edit re-triggers the backup. The 54:37
        // `|| -> &&` excludes the dirty tab from the hash, so the sig freezes and
        // the re-backup is silently missed.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("edited.txt");
        std::fs::write(&p, "on disk").unwrap();
        let mut t = EditorTab::from_path(p).expect("open");
        t.text = "edit one".into();
        t.doc_id = crate::grid::DocId(1);
        let mut app = due_backup_app(t);
        app.persist_session_and_autosave();
        let sig1 = app.last_backup_sig;
        assert_ne!(sig1, 0, "first edit backed up");
        // A second, different edit + a re-due clock: the content sig must change.
        app.tabs[0].text = "edit two is different".into();
        app.last_backup_at = Some(Instant::now().checked_sub(Duration::from_secs(5)).unwrap());
        app.persist_session_and_autosave();
        assert_ne!(app.last_backup_sig, sig1, "a changed dirty tab updates the content sig (re-backup)");
    }

    #[test]
    fn autosave_writes_a_due_dirty_file_backed_buffer() {
        // auto_save flushes DUE, dirty, file-backed buffers to disk. Kills the due
        // `elapsed >= INTERVAL -> <` (74:38) and the `!dirty.is_empty()` delete-!
        // (80:20) — both suppress the write, leaving the file at its old bytes.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("auto.txt");
        std::fs::write(&p, "old").unwrap();
        let mut cfg = Config::default();
        cfg.editor.auto_save = true;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs.clear();
        let mut t = EditorTab::from_path(p.clone()).expect("open");
        t.text = "new autosaved content".into();
        t.doc_id = crate::grid::DocId(1);
        app.tabs.push(t);
        app.active = 0;
        app.last_autosave_at = Some(Instant::now().checked_sub(Duration::from_secs(5)).unwrap());
        app.persist_session_and_autosave();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "new autosaved content",
            "a due, dirty, file-backed buffer is auto-saved to disk"
        );
    }

    /// Serializes the SCR1B3_CONFIG_DIR-mutating session-save test (belt-and-
    /// suspenders; the CI `test` job runs single-threaded, nextest per-process).
    static CFG_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn session_is_persisted_when_the_open_set_signature_changes() {
        // The open-file session is written (for restore-on-launch) only when its
        // signature changes. Kills 22:20 `sig != session_sig -> ==`: with a stale
        // session_sig, clean detects the change and updates it; == leaves it stale.
        // save_session writes into Config::config_dir(); SCR1B3_CONFIG_DIR relocates
        // that to a temp dir so the real config is never touched.
        let _g = CFG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("SCR1B3_CONFIG_DIR");
        std::env::set_var("SCR1B3_CONFIG_DIR", dir.path());
        let fp = dir.path().join("note.txt");
        std::fs::write(&fp, "x").unwrap();
        let mut app = ScribeApp::new_test(Config::default());
        // new_test force-disables restore_session; re-enable it on the built app.
        app.config.editor.restore_session = true;
        app.tabs.clear();
        let mut t = EditorTab::from_path(fp).expect("open");
        t.doc_id = crate::grid::DocId(1);
        app.tabs.push(t);
        app.active = 0;
        app.session_sig = String::new(); // stale: won't equal the real signature
        app.persist_session_and_autosave();
        let sig = app.session_sig.clone();
        match prev {
            Some(v) => std::env::set_var("SCR1B3_CONFIG_DIR", v),
            None => std::env::remove_var("SCR1B3_CONFIG_DIR"),
        }
        assert!(!sig.is_empty(), "a changed open-set updates session_sig (kills the != -> ==)");
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
