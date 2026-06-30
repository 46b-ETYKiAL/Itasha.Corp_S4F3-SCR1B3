//! The picker / jump modal-overlay section of the per-frame render loop —
//! go-to-line, go-to-symbol, recent files, recent folders, the welcome
//! modal, and the fuzzy file finder. Extracted verbatim from `frame_tick`
//! (behavior-preserving) to shrink the render loop; each block is unchanged
//! and still gated on its own `self.<flag>_open`. Uses only the per-frame
//! accent + muted theme colours.
#![allow(clippy::wildcard_imports)]

use super::*;

impl ScribeApp {
    pub(super) fn render_picker_modals(
        &mut self,
        ctx: &egui::Context,
        accent: Color32,
        muted: Color32,
    ) {
        // ---- Go-to-line modal (Ctrl+G) ----
        //
        // F-015 from docs/audits/overlooked-surfaces-2026-05-29.md. Accepts
        // a 1-based line number, or `N:C` for line + column. On Enter, the
        // editor's scroll-to-line path (existing `pending_scroll`) takes
        // the modal's target.
        if self.goto_open {
            let mut want_apply = false;
            let mut want_close = false;
            egui::Window::new(
                RichText::new(format!(
                    "{}  go to line",
                    egui_phosphor::thin::ARROW_LINE_RIGHT
                ))
                .color(accent)
                .monospace(),
            )
            .collapsible(false)
            .resizable(false)
            // Consistent fixed width like the other modal pickers (was
            // content-sized, so it opened narrower/inconsistent).
            .default_width(400.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 100.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let r = ui.text_edit_singleline(&mut self.goto_query);
                    if self.focus_goto {
                        r.request_focus();
                        self.focus_goto = false;
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        want_apply = true;
                    }
                    if ui.button("Go").clicked() {
                        want_apply = true;
                    }
                    if ui.button("Close").clicked() {
                        want_close = true;
                    }
                });
                ui.label(
                    RichText::new("line, or line:column (e.g. 42:10)")
                        .color(muted)
                        .small(),
                );
            });
            if want_apply {
                if let Some((line, _col)) = parse_goto_query(&self.goto_query) {
                    self.goto_line(line);
                    self.goto_open = false;
                }
            }
            if want_close {
                self.goto_open = false;
            }
        }

        // ---- Go-to-symbol modal (Ctrl+Shift+O) ----
        //
        // Lists the active buffer's definition scopes (from
        // `editor_features::symbol_scopes`), filterable by a substring query.
        // Selecting an entry jumps to its start line via the existing
        // `goto_line` scroll pipe. Modelled on the recent-files modal.
        if self.goto_symbol_open {
            let active = self.active.min(self.tabs.len().saturating_sub(1));
            // Bound the scan like the breadcrumb/sticky path does.
            let symbols = if !self.tabs.is_empty() && self.tabs[active].text.len() <= 500_000 {
                crate::editor_features::symbol_scopes(&self.tabs[active].text)
            } else {
                Vec::new()
            };
            let q = self.goto_symbol_query.trim().to_lowercase();
            // PA-01: filter ONCE up front so keyboard nav (Up/Down/Enter) and the
            // rendered rows agree on the same set — mirroring the command-palette /
            // fuzzy-finder "rank once up front" pattern. Each entry carries its
            // start line + a display string; the index into this Vec is the
            // selectable highlight.
            let matches: Vec<(usize, String)> = symbols
                .iter()
                .filter(|s| q.is_empty() || s.label.to_lowercase().contains(&q))
                .map(|s| {
                    let indent = "  ".repeat(s.depth);
                    (
                        s.start_line,
                        format!("{indent}{}  ·  {}", s.label, s.start_line + 1),
                    )
                })
                .collect();
            let mut chosen: Option<usize> = None;
            let mut want_close = false;
            // Read Up/Down/Enter here (outside the window body). A singleline
            // TextEdit ignores these keys, so this does not fight the query field's
            // caret — same rationale as the command palette / fuzzy finder.
            let (up, down, enter) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if matches.is_empty() {
                self.goto_symbol_selected = 0;
                if enter {
                    want_close = true;
                }
            } else {
                self.goto_symbol_selected =
                    fuzzy_move_selection(self.goto_symbol_selected, matches.len(), up, down);
                if enter {
                    chosen = Some(matches[self.goto_symbol_selected].0);
                }
            }
            let selected = self.goto_symbol_selected;
            let mut query_changed = false;
            egui::Window::new(
                RichText::new(format!("{}  go to symbol", egui_phosphor::thin::DIAMOND))
                    .color(accent)
                    .monospace(),
            )
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 100.0])
            .show(ctx, |ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.goto_symbol_query)
                        .hint_text("filter symbols")
                        .desired_width(f32::INFINITY),
                );
                if self.focus_goto_symbol {
                    r.request_focus();
                    self.focus_goto_symbol = false;
                }
                query_changed = r.changed();
                ui.separator();
                if symbols.is_empty() {
                    ui.label(
                        RichText::new("no symbols in this buffer")
                            .color(muted)
                            .small(),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            for (idx, (start_line, display)) in matches.iter().enumerate() {
                                let label = RichText::new(display.clone()).monospace();
                                let row = ui.selectable_label(idx == selected, label);
                                if row.clicked() {
                                    chosen = Some(*start_line);
                                }
                                // Keep the keyboard-highlighted row in view.
                                if idx == selected && (up || down) {
                                    row.scroll_to_me(Some(egui::Align::Center));
                                }
                            }
                            if matches.is_empty() {
                                ui.label(RichText::new("no match").color(muted).small());
                            }
                        });
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "{}{} select · Enter jumps to selection · Esc closes",
                        egui_phosphor::thin::ARROW_UP,
                        egui_phosphor::thin::ARROW_DOWN
                    ))
                    .color(muted)
                    .small()
                    .monospace(),
                );
            });
            // A new filter invalidates the old highlight position — reset to the
            // top so Enter runs the new top match.
            if query_changed {
                self.goto_symbol_selected = 0;
            }
            if let Some(line0) = chosen {
                self.goto_line(line0 + 1);
                self.goto_symbol_open = false;
            } else if want_close {
                self.goto_symbol_open = false;
            }
        }

        // ---- Recent files modal (Ctrl+R) ----
        //
        // F-012 from docs/audits/overlooked-surfaces-2026-05-29.md. Pops
        // a list of the MRU recent files. Click an entry → open. Esc →
        // close. Persists nothing — the recent list itself is owned by
        // EditorConfig::recent_files (already saved on every open).
        if self.recent_open {
            let mut chosen: Option<PathBuf> = None;
            let mut still_open = true;
            // PA-03: Up/Down move the highlight, Enter opens the selection —
            // mirroring the fuzzy finder. Read the keys outside the window body.
            let count = self.config.editor.recent_files.len();
            let (up, down, enter) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if count == 0 {
                self.recent_selected = 0;
            } else {
                self.recent_selected = fuzzy_move_selection(self.recent_selected, count, up, down);
                if enter {
                    chosen = self
                        .config
                        .editor
                        .recent_files
                        .get(self.recent_selected)
                        .cloned();
                }
            }
            let selected = self.recent_selected;
            egui::Window::new(
                RichText::new(format!(
                    "{}  recent files",
                    egui_phosphor::thin::CLOCK_COUNTER_CLOCKWISE
                ))
                .color(accent)
                .monospace(),
            )
            .open(&mut still_open)
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 100.0])
            .show(ctx, |ui| {
                if self.config.editor.recent_files.is_empty() {
                    ui.label(
                        RichText::new("no recent files yet — open something first")
                            .color(muted)
                            .small(),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            for (idx, p) in self.config.editor.recent_files.iter().enumerate() {
                                let label = RichText::new(p.display().to_string()).monospace();
                                let row = ui.selectable_label(idx == selected, label);
                                if row.clicked() {
                                    chosen = Some(p.clone());
                                }
                                if idx == selected && (up || down) {
                                    row.scroll_to_me(Some(egui::Align::Center));
                                }
                            }
                        });
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "{}{} select · Enter opens · Ctrl+R or Esc to close",
                        egui_phosphor::thin::ARROW_UP,
                        egui_phosphor::thin::ARROW_DOWN
                    ))
                    .color(muted)
                    .small()
                    .monospace(),
                );
            });
            if let Some(p) = chosen {
                self.open_path(p);
                self.recent_open = false;
            } else if !still_open {
                self.recent_open = false;
            }
        }

        // ---- Recent folders modal ----
        // Mirrors the recent-files modal for folders opened as the file-tree
        // root. Click an entry → set it as the root (and re-record it MRU-front
        // via open_folder_root). The list is owned by EditorConfig::recent_folders.
        if self.recent_folders_open {
            let mut chosen: Option<PathBuf> = None;
            let mut still_open = true;
            // PA-03: Up/Down move the highlight, Enter opens the selection.
            let count = self.config.editor.recent_folders.len();
            let (up, down, enter) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if count == 0 {
                self.recent_folders_selected = 0;
            } else {
                self.recent_folders_selected =
                    fuzzy_move_selection(self.recent_folders_selected, count, up, down);
                if enter {
                    chosen = self
                        .config
                        .editor
                        .recent_folders
                        .get(self.recent_folders_selected)
                        .cloned();
                }
            }
            let selected = self.recent_folders_selected;
            egui::Window::new(
                RichText::new(format!(
                    "{}  recent folders",
                    egui_phosphor::thin::CLOCK_COUNTER_CLOCKWISE
                ))
                .color(accent)
                .monospace(),
            )
            .open(&mut still_open)
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 100.0])
            .show(ctx, |ui| {
                if self.config.editor.recent_folders.is_empty() {
                    ui.label(
                        RichText::new("no recent folders yet — open a folder first")
                            .color(muted)
                            .small(),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            for (idx, p) in self.config.editor.recent_folders.iter().enumerate() {
                                let label = RichText::new(p.display().to_string()).monospace();
                                let row = ui.selectable_label(idx == selected, label);
                                if row.clicked() {
                                    chosen = Some(p.clone());
                                }
                                if idx == selected && (up || down) {
                                    row.scroll_to_me(Some(egui::Align::Center));
                                }
                            }
                        });
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "{}{} select · Enter opens · Esc to close",
                        egui_phosphor::thin::ARROW_UP,
                        egui_phosphor::thin::ARROW_DOWN
                    ))
                    .color(muted)
                    .small()
                    .monospace(),
                );
            });
            if let Some(p) = chosen {
                self.open_folder_root(p);
                self.recent_folders_open = false;
            } else if !still_open {
                self.recent_folders_open = false;
            }
        }

        // ---- Welcome modal (F-013) ----
        //
        // First-launch greeter: open file, open folder, pick from recent,
        // open settings, see keyboard shortcuts. Dismiss with the close
        // button (sets first_run_completed) or Esc (suppress this session
        // only). The decision-to-open happens at build() time; this
        // renderer just paints the state.
        if self.welcome_open {
            let mut want_new = false;
            let mut want_open = false;
            let mut want_open_folder = false;
            let mut want_recent = false;
            let mut want_settings = false;
            let mut want_cheatsheet = false;
            let mut want_dismiss_permanent = false;
            let mut still_open = true;
            egui::Window::new(
                RichText::new(format!("welcome to {}", scribe_core::PRODUCT_NAME))
                    .color(accent)
                    .monospace(),
            )
            .open(&mut still_open)
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(scribe_core::PRODUCT_TAGLINE)
                        .color(muted)
                        .monospace(),
                );
                ui.add_space(10.0);
                // Phosphor glyphs (loaded thin font); the old emoji (📄📂🗂⌖⌨✓)
                // have no glyph in JetBrains Mono and rendered as tofu (#R5).
                if ui
                    .button(format!(
                        "{}  New file (Ctrl+N)",
                        egui_phosphor::thin::FILE_PLUS
                    ))
                    .clicked()
                {
                    want_new = true;
                }
                if ui
                    .button(format!(
                        "{}  Open file… (Ctrl+O)",
                        egui_phosphor::thin::FILE_TEXT
                    ))
                    .clicked()
                {
                    want_open = true;
                }
                if ui
                    .button(format!(
                        "{}  Open folder…",
                        egui_phosphor::thin::FOLDER_OPEN
                    ))
                    .clicked()
                {
                    want_open_folder = true;
                }
                if ui
                    .button(format!(
                        "{}  Recent files (Ctrl+R)",
                        egui_phosphor::thin::CLOCK_COUNTER_CLOCKWISE
                    ))
                    .clicked()
                {
                    want_recent = true;
                }
                ui.separator();
                if ui
                    .button(format!("{}  Open Settings", egui_phosphor::thin::GEAR_SIX))
                    .clicked()
                {
                    want_settings = true;
                }
                if ui
                    .button(format!(
                        "{}  Show keyboard shortcuts (F1)",
                        egui_phosphor::thin::KEYBOARD
                    ))
                    .clicked()
                {
                    want_cheatsheet = true;
                }
                ui.add_space(10.0);
                if ui
                    .button(format!(
                        "{}  Don't show this again",
                        egui_phosphor::thin::CHECK
                    ))
                    .clicked()
                {
                    want_dismiss_permanent = true;
                }
                ui.label(
                    RichText::new("Esc dismisses for this session only.")
                        .color(muted)
                        .small(),
                );
            });
            if want_new {
                self.new_tab();
                self.welcome_open = false;
            }
            if want_open {
                self.open_dialog();
                self.welcome_open = false;
            }
            if want_open_folder {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.file_tree_root = Some(folder);
                }
                self.welcome_open = false;
            }
            if want_recent {
                self.recent_open = true;
                self.recent_selected = 0;
                self.welcome_open = false;
            }
            if want_settings {
                self.settings_open = true;
                self.welcome_open = false;
            }
            if want_cheatsheet {
                self.cheatsheet_open = true;
                self.welcome_open = false;
            }
            if want_dismiss_permanent {
                self.config.editor.first_run_completed = true;
                self.save_config();
                self.welcome_open = false;
            }
            if !still_open {
                self.welcome_open = false;
            }
        }

        // ---- Fuzzy file finder modal (Ctrl+P) ----
        //
        // F-010 from docs/audits/overlooked-surfaces-2026-05-29.md. Pre-
        // scanned project paths filtered by a stdlib-only subsequence
        // scorer (crate::fuzzy). Up to 200 ranked matches.
        if self.fuzzy_open {
            let mut chosen: Option<PathBuf> = None;
            let mut still_open = true;
            // Rank once up front so keyboard nav + the row list agree on the set.
            let ranked = crate::fuzzy::rank(&self.fuzzy_index, &self.fuzzy_query, 200);
            // #73 keyboard nav: Up/Down move the highlight, Enter opens it. A
            // singleline TextEdit ignores Up/Down/Enter-as-newline, so reading
            // these keys here does not fight the query field's caret.
            let (up, down, enter) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if !ranked.is_empty() {
                self.fuzzy_selected =
                    fuzzy_move_selection(self.fuzzy_selected, ranked.len(), up, down);
                if enter {
                    chosen = Some(ranked[self.fuzzy_selected].clone());
                }
            } else {
                self.fuzzy_selected = 0;
            }
            let selected = self.fuzzy_selected;
            let mut query_changed = false;
            egui::Window::new(
                RichText::new(format!(
                    "{}  open file",
                    egui_phosphor::thin::MAGNIFYING_GLASS
                ))
                .color(accent)
                .monospace(),
            )
            .open(&mut still_open)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 80.0])
            .show(ctx, |ui| {
                let r = ui.text_edit_singleline(&mut self.fuzzy_query);
                if self.focus_fuzzy {
                    r.request_focus();
                    self.focus_fuzzy = false;
                }
                query_changed = r.changed();
                ui.label(
                    RichText::new(format!(
                        "indexed {} files · {}{} select · Enter open · Esc close",
                        self.fuzzy_index.len(),
                        egui_phosphor::thin::ARROW_UP,
                        egui_phosphor::thin::ARROW_DOWN
                    ))
                    .color(muted)
                    .small(),
                );
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        if ranked.is_empty() {
                            ui.label(RichText::new("no match").color(muted).small().monospace());
                        }
                        for (idx, p) in ranked.iter().enumerate() {
                            let label = RichText::new(p.display().to_string()).monospace();
                            let row = ui.selectable_label(idx == selected, label);
                            if row.clicked() {
                                chosen = Some(p.clone());
                            }
                            // Keep the keyboard-highlighted row in view.
                            if idx == selected && (up || down) {
                                row.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                    });
            });
            // A new query invalidates the old highlight position.
            if query_changed {
                self.fuzzy_selected = 0;
            }
            if let Some(p) = chosen {
                self.open_path(p);
                self.fuzzy_open = false;
            } else if !still_open {
                self.fuzzy_open = false;
            }
        }
    }
}
