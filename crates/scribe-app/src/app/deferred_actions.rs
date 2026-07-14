//! Deferred-action dispatch for `frame_tick`, extracted from `mod.rs` (A-01 wave 3).
//!
//! Behavior-neutral split: the long tail of `if act.X { ... }` handlers that
//! run AFTER all per-frame UI borrows are released is moved verbatim out of
//! `frame_tick`. The handlers consume the `Pending` action set plus a handful
//! of frame-local flags (command/builtin to run, config-banner intents, file-
//! tree open/close, LSP start). They are grouped into `DeferredFlags` so the
//! method signature stays readable; the bodies are unchanged.
#![allow(clippy::wildcard_imports)]

use super::*;

/// Frame-local deferred flags collected during `frame_tick`'s UI pass and
/// applied once all UI borrows are released. Bundling them keeps
/// `apply_deferred_actions` from taking nine positional booleans/options.
pub(super) struct DeferredFlags {
    pub run_cmd: Option<String>,
    pub run_builtin: Option<BuiltinCommand>,
    pub save_cfg: bool,
    pub open_from_tree: Option<PathBuf>,
    pub close_tree: bool,
    pub start_lsp: bool,
    pub want_open_cfg: bool,
    pub want_restore_cfg: bool,
    pub want_dismiss_cfg: bool,
}

impl ScribeApp {
    /// Apply the deferred actions collected during the UI pass, after all UI
    /// borrows are released.
    ///
    /// Moved verbatim from the inline tail of `frame_tick`; every handler, its
    /// order, and its conditions are identical. The frame-local flags arrive in
    /// `flags` (a `DeferredFlags`) instead of as loose locals.
    pub(super) fn apply_deferred_actions(
        &mut self,
        ctx: &egui::Context,
        act: &mut Pending,
        flags: DeferredFlags,
    ) {
        let DeferredFlags {
            run_cmd,
            run_builtin,
            save_cfg,
            open_from_tree,
            close_tree,
            start_lsp,
            want_open_cfg,
            want_restore_cfg,
            want_dismiss_cfg,
        } = flags;
        if act.new {
            self.new_tab();
        }
        if act.open {
            self.open_dialog();
        }
        if act.save {
            self.save_active();
        }
        if let Some(cmd) = run_cmd {
            self.run_plugin_command(&cmd);
            self.palette_open = false;
        }
        if let Some(builtin) = run_builtin {
            self.execute_builtin(builtin);
            self.palette_open = false;
        }
        if save_cfg {
            self.save_config();
        }
        if act.open_folder {
            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                self.status = format!("folder: {}", folder.display());
                self.file_tree_root = Some(folder);
            }
        }
        // F-006 wave-1 fixes from docs/audits/overlooked-surfaces-2026-05-29.md.
        if act.close_active_tab {
            self.close_tab(self.active);
        }
        if act.toggle_grid {
            self.config.editor.grid_enabled = !self.config.editor.grid_enabled;
            self.save_config();
            self.status = format!(
                "multi-note grid: {}",
                if self.config.editor.grid_enabled {
                    "on"
                } else {
                    "off"
                }
            );
        }
        if act.cycle_tab_next && !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
        if act.cycle_tab_prev && !self.tabs.is_empty() {
            self.active = if self.active == 0 {
                self.tabs.len() - 1
            } else {
                self.active - 1
            };
        }
        // Wave-2 deferred handlers.
        if act.open_replace {
            // Re-use the existing find bar; focus the replace field.
            self.find_open = true;
            self.focus_replace = true;
        }
        if act.toggle_comment {
            self.toggle_comment_active();
        }
        if act.toggle_fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(
                !ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)),
            ));
        }
        if act.move_line_up {
            self.move_cursor_line(-1);
        }
        if act.move_line_down {
            self.move_cursor_line(1);
        }
        if act.duplicate_line {
            self.duplicate_cursor_line();
        }
        if act.join_lines {
            self.join_cursor_line_with_next();
        }
        if act.cycle_theme {
            let names = scribe_core::theme::Theme::builtin_names();
            if !names.is_empty() {
                let cur = &self.config.appearance.theme;
                let idx = names.iter().position(|n| *n == cur.as_str()).unwrap_or(0);
                let next = names[(idx + 1) % names.len()].to_string();
                self.config.appearance.theme = next.clone();
                self.reapply_theme(ctx);
                self.save_config();
                self.status = format!("theme: {next}");
            }
        }
        // Font zoom (Ctrl+= / Ctrl+- / Ctrl+0 / Ctrl+scroll).
        if let Some(z) = act.font_zoom {
            let def = scribe_core::config::Config::default().fonts.editor_size;
            let size = &mut self.config.fonts.editor_size;
            *size = match z {
                0 => def,
                d => (*size + d as f32).clamp(8.0, 32.0),
            };
            self.save_config();
            self.status = format!("font size: {:.0}", self.config.fonts.editor_size);
        }
        // Reopen the most recently closed tab.
        if act.reopen_tab {
            self.reopen_closed_tab();
        }
        // Line bookmarks (Ctrl+F2 toggle, F2 next, Shift+F2 prev).
        if act.toggle_bookmark {
            self.toggle_bookmark();
        }
        if act.next_bookmark {
            self.navigate_bookmark(1);
        }
        if act.prev_bookmark {
            self.navigate_bookmark(-1);
        }
        if act.toggle_minimap {
            self.config.editor.show_minimap = !self.config.editor.show_minimap;
            self.save_config();
            self.status = format!(
                "minimap: {}",
                if self.config.editor.show_minimap {
                    "on"
                } else {
                    "off"
                }
            );
        }
        // F-032: Ctrl+Shift+[ / Ctrl+Shift+] — fold-all / expand-all.
        // Re-extract regions against the current buffer so the action is
        // always applied to what the user sees, then switch on fold-view
        // so the change is visible in the central panel.
        if act.fold_all && self.active < self.tabs.len() {
            let text = self.tabs[self.active].text.clone();
            // P2-4: markdown/text notes fold by heading section, code by braces.
            // This handler predates `fold_regions_for` and was moved here
            // verbatim, so it kept calling the brace-only `fold_regions` — which
            // finds nothing in a note. The Ctrl+Shift+[ shortcut therefore
            // switched the user into fold view with zero regions folded, while
            // the palette's `BuiltinCommand::FoldAll` and the gutter's
            // "fold all" button (both language-aware) worked on the same buffer.
            let lang = self.tabs[self.active].doc.language_hint();
            let regions = crate::editor_features::fold_regions_for(&text, lang.as_deref());
            self.folds = regions.iter().map(|r| r.start_line).collect();
            self.fold_view = true;
            self.status = format!("folded {} region(s)", regions.len());
        }
        if act.expand_all {
            self.folds.clear();
            self.status = String::from("expanded all");
        }
        if act.open_fuzzy {
            // Lazy-build the index on first open so cold-start latency
            // lands here, not in build(). Rebuild whenever the project
            // root changes.
            if self.fuzzy_index.is_empty() {
                let root = self
                    .file_tree_root
                    .clone()
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| PathBuf::from("."));
                self.fuzzy_index = crate::fuzzy::scan_project(&root, crate::fuzzy::FUZZY_SCAN_CAP);
            }
            self.fuzzy_open = true;
            self.focus_fuzzy = true;
            self.fuzzy_query.clear();
            self.fuzzy_selected = 0;
        }
        for p in act.files_to_open.drain(..) {
            self.open_path(p);
        }
        if let Some(p) = open_from_tree {
            self.open_path(p);
        }
        if close_tree {
            self.file_tree_root = None;
        }
        if start_lsp {
            self.start_lsp_for_active();
        }
        // F-038 — apply deferred config-banner actions.
        if want_open_cfg {
            if let Some(p) = Config::config_file_path() {
                // Ensure the file actually exists before trying to open it
                // (cold install: write defaults first so the user can edit).
                if !p.exists() {
                    if let Some(parent) = p.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Err(e) = std::fs::write(&p, self.config.to_toml_string()) {
                        // Surface the failure instead of swallowing it and then
                        // opening a file that does not exist (confusing empty
                        // buffer). Mirrors save_config's error handling.
                        let msg = format!("could not create config file: {e}");
                        crate::action_log::record("error", &msg);
                        self.toast = Some(msg);
                    }
                }
                // Only open if the file is actually present (it pre-existed, or
                // the seed write above succeeded).
                if p.exists() {
                    self.open_path(p);
                }
            }
        }
        if want_restore_cfg {
            self.config = Config::default();
            self.save_config();
            self.reapply_theme(ctx);
            self.config_error_banner = None;
            self.status = "config restored to defaults".to_string();
        }
        if want_dismiss_cfg {
            self.config_error_banner = None;
        }
    }
}
