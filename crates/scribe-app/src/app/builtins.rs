//! Command-dispatch methods for `ScribeApp`: `execute_builtin` (the single
//! route for every `BuiltinCommand` from the palette / keyboard) and
//! `drain_pending_editor_action`. Extracted from `mod.rs` (A-01 wave 3 —
//! behavior-preserving move; methods widened to `pub(super)` for the parent
//! + sibling call-sites incl `frame_tick`).
#![allow(clippy::wildcard_imports)]

use super::*;

impl ScribeApp {
    /// Dispatch a [`BuiltinCommand`] selected from the command palette.
    ///
    /// Every editor action surfaced in `BUILTIN_COMMANDS` routes through here
    /// so the keyboard shortcut and the palette entry produce identical state
    /// changes (no drift between the two surfaces). Touches `self.config`
    /// then persists via `save_config` so toggles survive a restart.
    pub(super) fn execute_builtin(&mut self, cmd: BuiltinCommand) {
        // Action-log every command dispatch so a session is diagnosable: a
        // command the user invoked that "did nothing" still leaves a trace here.
        crate::action_log::record("cmd", &format!("{cmd:?}"));
        match cmd {
            BuiltinCommand::NewFile => self.new_tab(),
            BuiltinCommand::OpenFile => self.open_dialog(),
            BuiltinCommand::OpenFolder => {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.status = format!("folder: {}", folder.display());
                    self.open_folder_root(folder);
                }
            }
            BuiltinCommand::OpenRecentFolder => {
                self.recent_folders_open = true;
                self.recent_folders_selected = 0;
            }
            BuiltinCommand::Save => self.save_active(),
            BuiltinCommand::ReportIssue => self.issue_intake.open_fresh(),
            BuiltinCommand::ConvertToMarkdown => self.convert_to_markdown_active(),
            BuiltinCommand::ExportAsHtml => self.export_html_active(),
            BuiltinCommand::SetLineEndingsLf => self.set_active_eol(scribe_core::eol::Eol::Lf),
            BuiltinCommand::SetLineEndingsCrlf => self.set_active_eol(scribe_core::eol::Eol::Crlf),
            BuiltinCommand::SetLineEndingsCr => self.set_active_eol(scribe_core::eol::Eol::Cr),
            BuiltinCommand::CloseActiveTab => self.close_tab(self.active),
            BuiltinCommand::CloseAllTabs => {
                self.tabs.clear();
                self.tabs.push(EditorTab::scratch());
                self.active = 0;
            }
            BuiltinCommand::CycleTabNext => {
                if !self.tabs.is_empty() {
                    self.active = (self.active + 1) % self.tabs.len();
                }
            }
            BuiltinCommand::CycleTabPrev => {
                if !self.tabs.is_empty() {
                    self.active = if self.active == 0 {
                        self.tabs.len() - 1
                    } else {
                        self.active - 1
                    };
                }
            }
            BuiltinCommand::ToggleSplitView => {
                // Unified with the grid: "split" is the multi-pane view of the
                // open tabs (side-by-side for two, a grid for more).
                self.config.editor.grid_enabled = !self.config.editor.grid_enabled;
                self.save_config();
            }
            BuiltinCommand::ToggleMinimap => {
                self.config.editor.show_minimap = !self.config.editor.show_minimap;
                self.save_config();
            }
            BuiltinCommand::ToggleZen => {
                // Runtime session state — no config write (zen is never persisted).
                self.zen_mode = !self.zen_mode;
                if self.zen_mode {
                    self.find_open = false;
                    self.find_in_files_open = false;
                }
            }
            BuiltinCommand::ToggleMarkdownPreview => {
                self.md_preview_open = !self.md_preview_open;
            }
            BuiltinCommand::ToggleDiffView => {
                self.diff_view_open = !self.diff_view_open;
            }
            BuiltinCommand::ToggleSpellcheck => {
                self.config.spellcheck.enabled = !self.config.spellcheck.enabled;
                self.save_config();
            }
            BuiltinCommand::ToggleWordWrap => {
                self.config.editor.word_wrap = !self.config.editor.word_wrap;
                self.save_config();
            }
            BuiltinCommand::ToggleLineNumbers => {
                self.config.editor.show_line_numbers = !self.config.editor.show_line_numbers;
                self.save_config();
            }
            BuiltinCommand::ToggleChangeBar => {
                self.config.editor.show_change_bar = !self.config.editor.show_change_bar;
                self.save_config();
            }
            BuiltinCommand::OpenSettings => {
                self.settings_open = true;
            }
            BuiltinCommand::OpenFind => {
                self.find_open = true;
                self.focus_find = true;
            }
            BuiltinCommand::OpenPalette => {
                // Self-referential entry — leaves the palette open as it was.
                self.palette_open = true;
                self.focus_palette = true;
            }
            BuiltinCommand::CycleTheme => {
                let names = scribe_core::theme::Theme::builtin_names();
                if !names.is_empty() {
                    let cur = &self.config.appearance.theme;
                    let idx = names.iter().position(|n| *n == cur.as_str()).unwrap_or(0);
                    let next = names[(idx + 1) % names.len()].to_string();
                    self.config.appearance.theme = next.clone();
                    self.save_config();
                    self.status = format!("theme: {next}");
                }
            }
            BuiltinCommand::StartLsp => self.start_lsp_for_active(),
            BuiltinCommand::FoldAll => {
                if self.active < self.tabs.len() {
                    let text = self.tabs[self.active].text.clone();
                    let regions = crate::editor_features::fold_regions(&text);
                    self.folds = regions.iter().map(|r| r.start_line).collect();
                    self.fold_view = true;
                    self.status = format!("folded {} region(s)", regions.len());
                }
            }
            BuiltinCommand::ExpandAll => {
                self.folds.clear();
                self.status = String::from("expanded all");
            }
            BuiltinCommand::OpenPluginManager => {
                self.plugin_manager
                    .ensure_defaults(Config::config_dir().as_deref());
                self.plugin_manager.open = true;
            }
            BuiltinCommand::SortLines => {
                let active = self.active.min(self.tabs.len().saturating_sub(1));
                if active < self.tabs.len() && !self.tabs[active].doc.is_read_only_large() {
                    let sorted = scribe_core::text_ops::sort_lines(&self.tabs[active].text);
                    if sorted != self.tabs[active].text {
                        self.tabs[active].set_text(sorted);
                        self.tabs[active].doc.mark_dirty();
                        self.status = "sorted lines (A-Z)".to_string();
                    }
                }
            }
            BuiltinCommand::SortLinesUnique => self.apply_buffer_transform(
                "sorted lines (unique)",
                scribe_core::text_ops::sort_lines_unique,
            ),
            BuiltinCommand::TrimTrailingWhitespace => self.apply_buffer_transform(
                "trimmed trailing whitespace",
                scribe_core::text_ops::trim_trailing_whitespace,
            ),
            BuiltinCommand::EnsureFinalNewline => self.apply_buffer_transform(
                "ensured a final newline",
                scribe_core::text_ops::ensure_final_newline,
            ),
            BuiltinCommand::ConvertIndentToSpaces => {
                let w = self.config.editor.tab_width;
                self.apply_buffer_transform("converted indentation to spaces", |t| {
                    scribe_core::text_ops::tabs_to_spaces(t, w)
                });
            }
            BuiltinCommand::ConvertIndentToTabs => {
                let w = self.config.editor.tab_width;
                self.apply_buffer_transform("converted indentation to tabs", |t| {
                    scribe_core::text_ops::spaces_to_tabs(t, w)
                });
            }
            BuiltinCommand::RevealInExplorer => {
                let active = self.active.min(self.tabs.len().saturating_sub(1));
                let dir = self
                    .tabs
                    .get(active)
                    .and_then(|t| t.doc.path())
                    .and_then(|p| p.parent())
                    .map(|d| d.to_path_buf());
                match dir {
                    Some(d) => open_in_file_manager(&d),
                    None => {
                        self.toast = Some(
                            "Save this note first to show it in your file manager.".to_string(),
                        )
                    }
                }
            }
            BuiltinCommand::CopyFilePath => {
                let active = self.active.min(self.tabs.len().saturating_sub(1));
                let path = self
                    .tabs
                    .get(active)
                    .and_then(|t| t.doc.path())
                    .map(|p| p.display().to_string());
                match path {
                    Some(p) => match write_clipboard_text(&p) {
                        Ok(()) => self.status = format!("copied path: {p}"),
                        Err(e) => {
                            tracing::warn!("copy file path to clipboard failed: {e}");
                            self.toast =
                                Some("Couldn't copy the path to the clipboard. Try again.".into());
                        }
                    },
                    None => {
                        self.toast = Some("Save this note first to copy its file path.".to_string())
                    }
                }
            }
            // These three need the egui TextEditState (ctx), unavailable here;
            // set a flag that frame_tick drains (see the act.* keyboard path).
            BuiltinCommand::JumpMatchingBracket => self.pending_jump_bracket = true,
            BuiltinCommand::InsertDateTime => self.pending_insert_datetime = true,
            BuiltinCommand::DuplicateSelection => self.pending_dup_selection = true,
            // Clipboard / history actions: record the request; `frame_tick`
            // drains it into the focused editor as a native egui event.
            BuiltinCommand::Copy => self.pending_editor_action = Some(EditorAction::Copy),
            BuiltinCommand::Cut => self.pending_editor_action = Some(EditorAction::Cut),
            BuiltinCommand::Paste => self.pending_editor_action = Some(EditorAction::Paste),
            BuiltinCommand::Undo => self.pending_editor_action = Some(EditorAction::Undo),
            BuiltinCommand::Redo => self.pending_editor_action = Some(EditorAction::Redo),
            BuiltinCommand::ToggleBookmark => self.toggle_bookmark(),
            BuiltinCommand::NextBookmark => self.navigate_bookmark(1),
            BuiltinCommand::PrevBookmark => self.navigate_bookmark(-1),
            BuiltinCommand::GoToSymbol => {
                self.goto_symbol_open = true;
                self.focus_goto_symbol = true;
                self.goto_symbol_selected = 0;
                self.goto_symbol_query.clear();
            }
        }
    }

    /// Drain a palette-requested clipboard/history action by injecting the
    /// corresponding egui event into the input queue and focusing the central
    /// editor, so egui's `TextEdit` performs it natively this frame. Called at
    /// the top of `frame_tick`, before any panel renders, so the editor (shown
    /// later in the same frame) sees the event. `Paste` reads the OS clipboard
    /// via `arboard`; a read failure surfaces a toast rather than panicking.
    pub(super) fn drain_pending_editor_action(&mut self, ctx: &egui::Context) {
        let Some(action) = self.pending_editor_action.take() else {
            return;
        };
        let editor_id = egui::Id::new("scr1b3-central-editor");
        // Focus the editor so the injected event is delivered to it.
        ctx.memory_mut(|m| m.request_focus(editor_id));
        let event = match action {
            EditorAction::Copy => egui::Event::Copy,
            EditorAction::Cut => egui::Event::Cut,
            EditorAction::Paste => match read_clipboard_text() {
                Ok(text) => egui::Event::Paste(text),
                Err(e) => {
                    tracing::warn!("clipboard read for paste failed: {e}");
                    self.toast = Some(
                        "Couldn't read the clipboard. Copy the text again, then paste.".into(),
                    );
                    return;
                }
            },
            EditorAction::Undo => key_event(egui::Key::Z, egui::Modifiers::COMMAND),
            EditorAction::Redo => key_event(
                egui::Key::Z,
                egui::Modifiers {
                    command: true,
                    shift: true,
                    ..Default::default()
                },
            ),
        };
        ctx.input_mut(|i| i.events.push(event));
    }
}
