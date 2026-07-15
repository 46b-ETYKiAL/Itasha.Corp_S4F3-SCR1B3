//! Session + save IO persistence: save-active/save-as, the rolling session backup snapshot, restore-from-manifest, external-disk change polling, and save-hook firing. Bodies moved verbatim from the `app` god-module (A-01 decomposition); `use super::*` re-exports the types these methods touch.
#![allow(clippy::wildcard_imports)]
use super::*;

impl ScribeApp {
    pub(super) fn save_active(&mut self) {
        let active = self.active;
        if active >= self.tabs.len() {
            return;
        }
        // Save-time hygiene (opt-in): trim trailing whitespace + ensure a
        // final newline. Cleaned text is reflected back into the live buffer.
        let mut text = self.tabs[active].text.clone();
        if self.config.editor.trim_trailing_whitespace_on_save {
            text = scribe_core::text_ops::trim_trailing_whitespace(&text);
        }
        if self.config.editor.final_newline_on_save {
            text = scribe_core::text_ops::ensure_final_newline(&text);
        }
        if text != self.tabs[active].text {
            self.tabs[active].set_text(text.clone());
        }
        // Sync editable text into the document model, then persist.
        self.tabs[active].doc.set_text(&text);
        if self.tabs[active].doc.path().is_none() {
            self.save_as_active();
            return;
        }
        match self.tabs[active].doc.save() {
            Ok(lossy) => {
                self.status = format!("saved {}", self.tabs[active].doc.file_name());
                // #R6 — the file's encoding can't represent every character;
                // those were replaced (data lost). Warn loudly + offer the fix.
                if lossy {
                    self.toast = Some(format!(
                        "Saved, but some characters can't be written as {} — they were \
                         replaced. Convert the file to UTF-8 to keep them.",
                        self.tabs[active].doc.encoding().name
                    ));
                }
                // F-022 — refresh the disk fingerprint after a successful
                // save so the next poll doesn't false-positive.
                self.tabs[active].disk_text = self.tabs[active].text.clone();
                if let Some(p) = self.tabs[active].doc.path() {
                    if let Some(m) = file_mtime(p) {
                        self.tabs[active].disk_mtime = Some(m);
                    }
                }
                // Change-bar: lines edited this session flip from unsaved to
                // saved (the saved baseline now includes them).
                self.tabs[active].mark_change_saved();
                self.fire_save_hooks(active);
            }
            Err(e) => {
                tracing::warn!("save failed: {e}");
                self.toast = Some(
                    "Couldn't save the file. Check that you have permission to write here \
                     and that the disk isn't full, then try again."
                        .into(),
                );
            }
        }
    }

    /// Hot-exit snapshot: flush every unsaved buffer's content to the backup
    /// store + write the session manifest, so unsaved work (incl. untitled
    /// scratch notes) survives a restart or crash. Each dirty file tab and each
    /// non-empty untitled tab gets an atomic content backup; clean tabs are
    /// recorded by path only; orphan backups are pruned. Best-effort.
    pub(super) fn snapshot_session_backups(&mut self) {
        use scribe_core::session;
        // Instance config dir (test-isolated in `new_test`) — NOT the global
        // `Config::config_dir()`. A test's periodic snapshot must never write its
        // unsaved-buffer fixture into the real user session backup.
        let Some(dir) = self.config_dir.clone() else {
            return;
        };
        let bdir = session::backup_dir(&dir);
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        let mut snapshots = Vec::with_capacity(self.tabs.len());
        for (i, tab) in self.tabs.iter().enumerate() {
            let path = tab.doc.path().map(|p| p.display().to_string());
            let dirty = tab.is_dirty();
            let untitled_with_content = path.is_none() && !tab.text.is_empty();
            let cursor = tab.rope_state.as_ref().map(|s| s.edit.cursor).unwrap_or(0);
            let backup = if dirty || untitled_with_content {
                let name = session::backup_name(path.as_deref(), i);
                match session::write_backup(&bdir, &name, &tab.text) {
                    Ok(()) => Some(name),
                    Err(e) => {
                        // Data-loss path: the manifest will then record this tab
                        // as having no recoverable content, so unsaved work is
                        // lost on the next crash. Make it visible in the log.
                        tracing::error!(
                            "hot-exit backup write failed for tab {i}; unsaved content \
                             will NOT be recoverable: {e}"
                        );
                        None
                    }
                }
            } else {
                None
            };
            snapshots.push(session::TabSnapshot {
                path,
                dirty,
                backup,
                cursor,
            });
        }
        let manifest = session::SessionManifest::new(snapshots, active);
        // E-02: best-effort hot-exit manifest write. A persistently-failing
        // save means unsaved-buffer recovery silently breaks on the next crash
        // -- log it (non-fatal: the periodic flush keeps trying next interval).
        if let Err(e) = session::save_manifest(&dir, &manifest) {
            tracing::warn!("session backup manifest write failed (non-fatal): {e}");
        }
        session::prune_orphan_backups(&bdir, &manifest);
        self.last_backup_at = Some(std::time::Instant::now());
    }

    /// Restore tabs from the session manifest + content backups (hot exit).
    /// Returns `(tabs, active_index)` or `None` when there is no usable
    /// manifest. A tab with a backup restores its unsaved content (marked
    /// dirty); a clean tab opens from disk.
    /// Restore tabs from the hot-exit manifest. The two session features are
    /// distinct and gated separately:
    ///   • unsaved CONTENT (a tab with a `backup`) is always restored here —
    ///     that is the "Restore unsaved notes" / `session_backup` feature.
    ///   • a clean, file-backed tab (a `path` with no `backup`) is reopened ONLY
    ///     when `restore_session` is on — that is the "Restore session" /
    ///     reopen-previous-tabs feature.
    /// So with "Restore session" UNCHECKED, previously-open clean files are NOT
    /// reopened, while unsaved scratch content is still recovered (if its own
    /// toggle is on). This is what makes the "Restore session" toggle authoritative
    /// instead of a no-op (the backup path used to reopen every clean file too).
    pub(super) fn restore_tabs_from_manifest(
        restore_session: bool,
    ) -> Option<(Vec<EditorTab>, usize)> {
        use scribe_core::session;
        let dir = Config::config_dir()?;
        let manifest = session::load_manifest(&dir)?;
        let bdir = session::backup_dir(&dir);
        let mut tabs: Vec<EditorTab> = Vec::new();

        // R6 / S-04 — a tampered `session.json` can point at a
        // `\\attacker\share\…` UNC path, and restore AUTO-opens it with no
        // user interaction, which makes Windows authenticate to the attacker's
        // SMB host and hand over a NetNTLMv2 response. That — a path that
        // reaches off this machine — is the one thing the guard blocks. See
        // `session_path_guard` for why it does not also try to fence the
        // restore inside a "prior working set".
        // Enforce the one-tab-per-file invariant on restore: a file must NEVER be
        // reopened into two tabs. The manifest can legitimately carry two entries
        // for the same path — a stale unsaved-backup entry coexisting with a
        // clean one — once a prior session opened the file twice (the un-deduped
        // `open_path` allowed that). Without this guard the duplicate persists and
        // COMPOUNDS every restart, the two copies silently diverging (restored
        // snapshot vs current disk) — exactly the "same note opened twice, the
        // second a newer saved version" report. Key by the host-FS identity of
        // the canonical path (falling back to the raw path when the file has
        // vanished) so two casings/separators of one file on Windows collapse to
        // a single tab; POSIX stays case-sensitive (see `scribe_core::path_norm`).
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        // Map each manifest entry index → the tab index it resolved to, so the
        // active-tab pointer stays coherent after dedup collapses entries.
        let mut snap_to_tab: std::collections::HashMap<usize, usize> =
            std::collections::HashMap::new();
        for (si, snap) in manifest.tabs.iter().enumerate() {
            let raw_path = snap.path.as_ref().map(PathBuf::from);
            // R6 / S-04 — classify this entry's declared path. A path that
            // FAILS the restore guard (reaches a remote host, or has vanished)
            // is NEVER auto-opened from disk and NEVER carried as the tab's
            // save target. `path` below is the SAFE path used for disk
            // re-open / save-target; an unsafe path is dropped to `None`.
            let path: Option<PathBuf> = match &raw_path {
                Some(p) if crate::session_path_guard::is_safe_restore_path(p) => Some(p.clone()),
                Some(p) => {
                    // debug!, not warn!: the path itself is untrusted content,
                    // and `session.rs` keeps it out of higher log levels for
                    // that reason. A skipped restore is not an alarm.
                    tracing::debug!(
                        "session restore: skipping {} (resolves off this machine, or is gone) \
                         — not auto-opening",
                        p.display()
                    );
                    None
                }
                None => None,
            };
            // Build the candidate tab for this entry, or skip it (None) — keeping
            // the original gating: a backup restores unsaved content always; a
            // clean file-backed tab reopens only when `restore_session` is on.
            // S-04: a backup whose declared path was unsafe still restores its
            // UNSAVED CONTENT (never lose the user's work) but as a PATHLESS
            // scratch buffer — the attacker-chosen path is stripped so it can
            // neither auto-open nor become a silent save target.
            let candidate: Option<EditorTab> = if let Some(name) = &snap.backup {
                match session::read_backup(&bdir, name) {
                    Ok(content) => Some(EditorTab::from_backup(path.clone(), content)),
                    // Backup unreadable → fall back to the clean-file rule below.
                    // Only a SAFE path is re-openable from disk.
                    Err(_) if restore_session => {
                        path.clone().and_then(|p| EditorTab::from_path(p).ok())
                    }
                    Err(_) => None,
                }
            } else if restore_session {
                // A clean file-backed tab reopens ONLY from a safe path. An
                // unsafe path (path == None) is skipped entirely.
                path.clone().and_then(|p| EditorTab::from_path(p).ok())
            } else {
                None
            };
            let Some(candidate) = candidate else { continue };

            // Dedup key = host-FS identity of the restored tab's OWN canonical
            // path (a vanished file restores as a pathless scratch buffer, which
            // we never dedup).
            let key = candidate.doc.path().map(|p| {
                let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
                scribe_core::path_norm::normalize_for_compare(&canon)
            });
            match key.and_then(|k| seen.get(&k).copied().map(|j| (k, j))) {
                Some((_, j)) => {
                    // A second entry for an already-restored file: collapse into
                    // the one existing tab instead of opening a duplicate.
                    EditorTab::merge_restored_duplicate(&mut tabs[j], candidate);
                    snap_to_tab.insert(si, j);
                }
                None => {
                    let idx = tabs.len();
                    if let Some(p) = candidate.doc.path() {
                        let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
                        seen.insert(scribe_core::path_norm::normalize_for_compare(&canon), idx);
                    }
                    tabs.push(candidate);
                    snap_to_tab.insert(si, idx);
                }
            }
        }
        if tabs.is_empty() {
            return None;
        }
        // Remap the persisted active index through dedup.
        //
        // No clamp. Every value in `snap_to_tab` is an index we pushed ourselves
        // (each < tabs.len()), and `tabs` is non-empty by the check above — so
        // the `.min(tabs.len() - 1)` that used to sit here could never bind. It
        // was dead code, not a safety net: 1065 tests never once reached it, and
        // both mutations of its arithmetic were unkillable by construction.
        // `manifest.active` is untrusted (the manifest is user-writable), but it
        // is sanitised by the lookup itself: an out-of-range value simply misses
        // the map and falls back to the first tab.
        let active = snap_to_tab.get(&manifest.active).copied().unwrap_or(0);
        Some((tabs, active))
    }

    /// F-022 — Poll every file-backed tab's mtime. If a tab's disk mtime
    /// advanced AND the buffer is still clean (text == disk_text), re-read
    /// the file in place + surface a status toast. If the buffer is dirty,
    /// flag the user so save doesn't silently clobber their edits.
    ///
    /// P-06: throttled to once every `DISK_POLL_INTERVAL_FRAMES` frames so it
    /// does not `fs::metadata`-stat every open file-backed tab on every single
    /// frame. `current_frame` is `egui::Context::cumulative_pass_nr`. External
    /// changes are still detected — just on the next poll tick, not instantly.
    /// The O(n) `text == disk_text` compare is already gated on a changed mtime
    /// below, so it never runs on the common unchanged path.
    pub(super) fn poll_external_disk_changes(&mut self, current_frame: u64) {
        if !should_poll_disk(
            current_frame,
            self.last_disk_poll_frame,
            DISK_POLL_INTERVAL_FRAMES,
        ) {
            return;
        }
        self.last_disk_poll_frame = current_frame;
        // Snapshot first so we don't hold &mut self while mutating tabs.
        let mut to_reload: Vec<usize> = Vec::new();
        let mut to_warn: Vec<usize> = Vec::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            let Some(path) = tab.doc.path() else { continue };
            let Some(m) = file_mtime(path) else {
                continue;
            };
            if Some(m) != tab.disk_mtime {
                if tab.text == tab.disk_text {
                    to_reload.push(i);
                } else {
                    to_warn.push(i);
                }
            }
        }
        for i in to_reload {
            let Some(path) = self.tabs[i].doc.path().map(|p| p.to_path_buf()) else {
                continue;
            };
            // ENC-1: reload through the document's encoding-preserving path
            // (`decode_with(self.encoding)`), NOT UTF-8-only `read_to_string` —
            // a Shift-JIS/Latin-1 file stays in its detected encoding across an
            // external-edit reload, and a non-UTF-8 file reloads correctly
            // instead of silently failing the read (and stranding the change).
            if self.tabs[i].doc.reload_from_disk().is_ok() {
                let fresh = self.tabs[i].doc.text();
                self.tabs[i].set_text(fresh.clone());
                self.tabs[i].disk_text = fresh;
                if let Some(m) = file_mtime(&path) {
                    self.tabs[i].disk_mtime = Some(m);
                }
                self.tabs[i].external_change = false;
                // Change-bar: the reloaded content is the new clean reference.
                self.tabs[i].reset_change_baselines();
                self.status = format!("reloaded {} (external edit)", path.display());
            }
        }
        for i in to_warn {
            // The tab has unsaved local edits AND the file changed on disk — set
            // the persistent flag that drives the actionable banner (Reload /
            // Keep mine), so the user is prompted to update instead of getting a
            // fleeting toast and silently overwriting the newer disk version on
            // save. We do NOT refresh `disk_mtime` here, so the flag stays set
            // until the user resolves it via the banner (or saves / reopens).
            self.tabs[i].external_change = true;
        }
    }

    /// Fire plugin `on_save` hooks; apply any text transform they make.
    fn fire_save_hooks(&mut self, active: usize) {
        let mut pctx = PluginContext::new(self.tabs[active].text.clone());
        if self.plugins.fire_event(HookEvent::Save, &mut pctx).is_ok() {
            if pctx.text != self.tabs[active].text {
                self.tabs[active].set_text(pctx.text);
            }
            if let Some(n) = pctx.notifications.last() {
                self.status = n.clone();
            }
        }
    }

    /// Everything Save-As can decide WITHOUT the user: the file name to
    /// pre-fill and the dialog's filter list. `None` when there is no active
    /// tab.
    ///
    /// Split out of [`Self::save_as_active`] because that fn is an ADR-0007
    /// exclusion — it blocks on a native `rfd` dialog, so no headless test can
    /// drive it. That exclusion used to swallow this decision logic too, which
    /// left it wholly unasserted (mutating its `>=` and its `!=` changed nothing
    /// any test could see). The exclusion now covers only the dialog call.
    pub(super) fn save_as_prompt(&self) -> Option<SaveAsPrompt> {
        let active = self.active;
        if active >= self.tabs.len() {
            return None;
        }
        // The configured default format drives the suggestion (so a new file
        // offers e.g. `untitled.md`) and is the PRIMARY filter; the other
        // built-ins follow, then an "All files" catch-all.
        let fmt = self.config.integration.default_save_format;
        let stem = self.tabs[active]
            .doc
            .path()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned());
        let mut filters = vec![(fmt.filter_label(), fmt.extension())];
        for other in scribe_core::config::DefaultSaveFormat::ALL {
            if other != fmt {
                filters.push((other.filter_label(), other.extension()));
            }
        }
        filters.push(("All files", "*"));
        Some(SaveAsPrompt {
            suggested: fmt.suggested_file_name(stem.as_deref()),
            filters,
            fmt,
        })
    }

    /// Commit a Save-As to the `path` the user picked — everything AFTER the
    /// dialog returns, so it is testable without one.
    pub(super) fn commit_save_as(
        &mut self,
        path: &Path,
        fmt: scribe_core::config::DefaultSaveFormat,
    ) {
        let active = self.active;
        if active >= self.tabs.len() {
            return;
        }
        // If the user typed a name with no extension, append the configured
        // default (so `notes` → `notes.md`); a name that already carries an
        // explicit extension is respected exactly as given.
        let path = scribe_core::config::ensure_extension(path, fmt.extension());
        let text = self.tabs[active].text.clone();
        self.tabs[active].doc.set_text(&text);
        match self.tabs[active].doc.save_as(&path) {
            Ok(lossy) => {
                self.status = format!("saved {}", path.display());
                if lossy {
                    self.toast = Some(format!(
                        "Saved, but some characters can't be written as {} — they were \
                         replaced. Convert the file to UTF-8 to keep them.",
                        self.tabs[active].doc.encoding().name
                    ));
                }
                // Change-bar: the saved baseline now includes this session's
                // edits, so they flip from unsaved to saved.
                self.tabs[active].mark_change_saved();
            }
            Err(e) => {
                tracing::warn!("save failed: {e}");
                self.toast = Some(
                    "Couldn't save the file. Check that you have permission to write here \
                     and that the disk isn't full, then try again."
                        .into(),
                );
            }
        }
    }

    /// Save-As, end to end: decide → ask → commit. Keep this body a thin seam;
    /// anything decidable belongs in [`Self::save_as_prompt`] or
    /// [`Self::commit_save_as`].
    ///
    /// No longer an ADR-0007 exclusion. The dialog still cannot be driven by a
    /// test, but it is now the ONLY part that cannot: `super::dialogs` is
    /// headless under `cfg(test)` and a test injects the path the user "picked"
    /// via `dialogs::test_hooks::set_next_save_path`, so this whole flow is
    /// exercised for real. Nothing injected = the user cancelled.
    pub(super) fn save_as_active(&mut self) {
        let Some(prompt) = self.save_as_prompt() else {
            return;
        };
        if let Some(path) = super::dialogs::save_file(&prompt.suggested, &prompt.filters) {
            self.commit_save_as(&path, prompt.fmt);
        }
    }
}

/// What the Save-As dialog should ask, decided before it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SaveAsPrompt {
    /// The pre-filled file name, e.g. `notes.md`.
    pub suggested: String,
    /// `(label, extension)` filters IN ORDER — the configured format first, so
    /// it is the dialog's default.
    pub filters: Vec<(&'static str, &'static str)>,
    /// The configured format, appended to a name typed without an extension.
    pub fmt: scribe_core::config::DefaultSaveFormat,
}
