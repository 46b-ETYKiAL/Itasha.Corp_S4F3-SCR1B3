//! File-and-buffer operation methods for `ScribeApp`: the open dialog and
//! folder-root open, the generic buffer transform + change-state ensure,
//! Markdown/HTML export, and end-of-line conversion. Extracted from
//! `mod.rs` (A-01 wave 3 — behavior-preserving move; methods widened to
//! `pub(super)` for the parent + sibling call-sites incl `frame_tick`).
#![allow(clippy::wildcard_imports)]

use super::*;

impl ScribeApp {
    pub(super) fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            match EditorTab::from_path(path.clone()) {
                Ok(t) => {
                    self.tabs.push(t);
                    self.active = self.tabs.len() - 1;
                    self.status = format!("opened {}", path.display());
                }
                Err(e) => {
                    tracing::warn!("open failed for {}: {e}", path.display());
                    self.toast = Some(
                        "Couldn't open the file. It may have been moved or deleted, or you may \
                     not have permission to read it."
                            .into(),
                    );
                }
            }
        }
    }

    /// Open `folder` as the file-tree root and record it in the recent-folders
    /// MRU (persisted), mirroring the recent-files discipline.
    pub(super) fn open_folder_root(&mut self, folder: PathBuf) {
        scribe_core::config::record_recent_file(
            &mut self.config.editor.recent_folders,
            folder.clone(),
        );
        self.file_tree_root = Some(folder);
        self.save_config();
    }

    /// Apply a whole-buffer `&str -> String` transform to the active tab,
    /// skipping read-only-large buffers and no-op results. Shared by the
    /// line-operation commands (sort, trim, indent conversion, …).
    pub(super) fn apply_buffer_transform(&mut self, status: &str, f: impl Fn(&str) -> String) {
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        if active < self.tabs.len() && !self.tabs[active].doc.is_read_only_large() {
            let new = f(&self.tabs[active].text);
            if new != self.tabs[active].text {
                self.tabs[active].set_text(new);
                self.tabs[active].doc.mark_dirty();
                self.status = status.to_string();
            }
        }
    }

    /// Recompute the active tab's per-line change-bar states if the buffer
    /// moved since the cache was last built. A cheap no-op when nothing
    /// changed (keyed off the monotonic `edit_gen`). The O(n) line diff is
    /// skipped above a size cap — large files are low-value for a change bar
    /// and the diff would cost a frame.
    pub(super) fn ensure_change_states(&mut self, active: usize) {
        /// Max buffer size for which the change-bar diff runs.
        const CHANGE_BAR_MAX_BYTES: usize = 2 * 1024 * 1024;
        if !self.config.editor.show_change_bar || active >= self.tabs.len() {
            return;
        }
        let tab = &mut self.tabs[active];
        if tab.change_gen == Some(tab.edit_gen) {
            return; // cache is current
        }
        if tab.text.len() > CHANGE_BAR_MAX_BYTES {
            tab.change_states.clear();
            tab.change_gen = Some(tab.edit_gen);
            return;
        }
        tab.change_states = crate::change_bar::compute_change_states(
            &tab.session_baseline,
            &tab.saved_baseline,
            &tab.text,
        );
        tab.change_gen = Some(tab.edit_gen);
    }

    /// Convert the active buffer to Markdown (by file type) and save it as a
    /// `.md` file. The conversion is a pure `text -> markdown` transform
    /// (HTML/CSV/JSON/TOML/code/text); the source tab is left untouched — only
    /// the chosen `.md` file is written.
    pub(super) fn convert_to_markdown_active(&mut self) {
        let active = self.active;
        if active >= self.tabs.len() {
            return;
        }
        let text = self.tabs[active].text.clone();
        let ext = self.tabs[active].doc.language_hint();
        let md = crate::to_markdown::to_markdown(&text, ext.as_deref());
        let suggested = self.tabs[active]
            .doc
            .path()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .map(|stem| format!("{stem}.md"))
            .unwrap_or_else(|| "untitled.md".to_string());
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Markdown", &["md"])
            .set_file_name(&suggested)
            .save_file()
        {
            match std::fs::write(&path, md) {
                Ok(()) => self.status = format!("converted to Markdown → {}", path.display()),
                Err(e) => {
                    tracing::warn!("markdown convert write failed for {}: {e}", path.display());
                    self.toast = Some(
                        "Couldn't save the Markdown file. Try a different folder or filename."
                            .into(),
                    );
                }
            }
        }
    }

    /// Export the active buffer as a standalone HTML document (treating the
    /// buffer as Markdown). Writes a chosen `.html` file; the source is
    /// untouched. Pure pulldown-cmark rendering — no webview, no network.
    pub(super) fn export_html_active(&mut self) {
        let active = self.active;
        if active >= self.tabs.len() {
            return;
        }
        let html = crate::md_preview::to_html(&self.tabs[active].text);
        let suggested = self.tabs[active]
            .doc
            .path()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .map(|stem| format!("{stem}.html"))
            .unwrap_or_else(|| "untitled.html".to_string());
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("HTML", &["html"])
            .set_file_name(&suggested)
            .save_file()
        {
            match std::fs::write(&path, html) {
                Ok(()) => self.status = format!("exported HTML → {}", path.display()),
                Err(e) => {
                    tracing::warn!("html export write failed for {}: {e}", path.display());
                    self.toast = Some(
                        "Couldn't save the HTML file. Try a different folder or filename.".into(),
                    );
                }
            }
        }
    }

    /// Set the active document's line-ending style. The change applies on the
    /// next save (the on-disk EOL is written by `Document::save`).
    pub(super) fn set_active_eol(&mut self, eol: scribe_core::eol::Eol) {
        let active = self.active;
        if active >= self.tabs.len() {
            return;
        }
        self.tabs[active].doc.set_eol(eol);
        self.status = format!("line endings set to {} — save to apply", eol.label());
    }
}
