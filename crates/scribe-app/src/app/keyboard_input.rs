//! Keyboard-shortcut handling for `frame_tick`, extracted from `mod.rs` (A-01 wave 3).
//!
//! Behavior-neutral split: the single large `ctx.input(|i| { ... })` closure
//! that collected per-frame keyboard shortcuts into a `Pending` action set (and
//! the find-bar F3 navigation direction) is moved verbatim out of `frame_tick`.
//! The closure body is unchanged except that the two writebacks now target the
//! `&mut` parameters (`act` already auto-derefs; `find_nav` becomes `*find_nav`).
#![allow(clippy::wildcard_imports)]

use super::*;

impl ScribeApp {
    /// Collect this frame's keyboard shortcuts into `act` (a `Pending` action
    /// set) and record the find-bar F3 navigation direction in `find_nav`.
    ///
    /// Moved verbatim from the inline `ctx.input` closure in `frame_tick`; every
    /// chord, modal-open, and toggle is identical. Splitting it out lets
    /// `frame_tick` read as a short sequence of named steps.
    pub(super) fn handle_keyboard_shortcuts(
        &mut self,
        ctx: &egui::Context,
        act: &mut Pending,
        find_nav: &mut Option<bool>,
    ) {
        ctx.input(|i| {
            let cmd = i.modifiers.command;
            act.new = cmd && i.key_pressed(egui::Key::N);
            // Exclude shift so Ctrl+Shift+O (go-to-symbol, below) does not ALSO
            // fire the open-file dialog. Mirrors the Ctrl+F / Ctrl+P guards.
            act.open = cmd && !i.modifiers.shift && i.key_pressed(egui::Key::O);
            act.save = cmd && i.key_pressed(egui::Key::S);
            if cmd && !i.modifiers.shift && i.key_pressed(egui::Key::F) {
                if !self.find_open {
                    self.focus_find = true;
                }
                self.find_open = true;
            }
            // Wave-5: Ctrl/Cmd+Shift+F opens project-wide find (find in files).
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::F) {
                if !self.find_in_files_open {
                    self.focus_find_in_files = true;
                }
                self.find_in_files_open = true;
            }
            // Wave-5 P1: Ctrl+. toggles zen / distraction-free mode. Entering zen
            // also closes the find bars so nothing but the editor remains.
            if cmd && !i.modifiers.shift && i.key_pressed(egui::Key::Period) {
                self.zen_mode = !self.zen_mode;
                if self.zen_mode {
                    self.find_open = false;
                    self.find_in_files_open = false;
                }
            }
            // Wave-5 P1: Ctrl+Shift+V toggles the markdown live-preview panel.
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::V) {
                self.md_preview_open = !self.md_preview_open;
            }
            // Ctrl/Cmd+Shift+P opens the command palette (plugin + builtin cmds).
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::P) {
                if !self.palette_open {
                    self.focus_palette = true;
                }
                self.palette_open = true;
                self.palette_query.clear();
                // BUG-APP-01: a fresh open starts the keyboard highlight at the
                // top so Enter runs the first match.
                self.palette_selected = 0;
            }
            // F-006 fix from docs/audits/overlooked-surfaces-2026-05-29.md —
            // wave 1 keyboard shortcuts:
            // - Ctrl+W: close the active tab.
            // - Ctrl+\: toggle the multi-note grid (F-003 entry-point fix).
            // - Ctrl+Tab / Ctrl+Shift+Tab: cycle tabs (next / prev).
            if cmd && i.key_pressed(egui::Key::W) {
                act.close_active_tab = true;
            }
            if cmd && i.key_pressed(egui::Key::Backslash) {
                act.toggle_grid = true;
            }
            // Wave-2 keyboard fill-in (docs/audits/overlooked-surfaces-2026-05-29.md).
            if cmd && i.key_pressed(egui::Key::H) {
                act.open_replace = true;
            }
            if cmd && i.key_pressed(egui::Key::Slash) {
                act.toggle_comment = true;
            }
            // Jump to the matching bracket. !shift so it never collides with a
            // potential Ctrl+Shift+M binding.
            if cmd && !i.modifiers.shift && i.key_pressed(egui::Key::M) {
                act.jump_bracket = true;
            }
            if i.key_pressed(egui::Key::F11) {
                act.toggle_fullscreen = true;
            }
            // F-018 — Ctrl+K Ctrl+T (cycle theme) approximated as
            // Ctrl+Shift+T (single-key chord) since egui has no native
            // multi-key chord layer. F-031 — Ctrl+Shift+M toggles the
            // minimap. Both persist via save_config.
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::T) {
                act.cycle_theme = true;
            }
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::M) {
                act.toggle_minimap = true;
            }
            // F-032 — Ctrl+Shift+[ folds every region in the active buffer,
            // Ctrl+Shift+] expands every region. Switches the editor into
            // fold-view mode so the user sees the change immediately
            // (otherwise the fold set is updated but the normal central
            // panel doesn't honor it).
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::OpenBracket) {
                act.fold_all = true;
            }
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::CloseBracket) {
                act.expand_all = true;
            }
            // Font zoom: Ctrl+= / Ctrl++ in, Ctrl+- out, Ctrl+0 reset, and
            // Ctrl+scroll. Universal editor convenience.
            if cmd && (i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals)) {
                act.font_zoom = Some(1);
            }
            if cmd && i.key_pressed(egui::Key::Minus) {
                act.font_zoom = Some(-1);
            }
            if cmd && i.key_pressed(egui::Key::Num0) {
                act.font_zoom = Some(0);
            }
            if cmd {
                let dy = i.smooth_scroll_delta.y;
                if dy > 0.5 {
                    act.font_zoom = Some(1);
                } else if dy < -0.5 {
                    act.font_zoom = Some(-1);
                }
            }
            // Reopen the most recently closed tab (Ctrl+Shift+R — Ctrl+Shift+T
            // is already the theme-cycle chord in this editor).
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::R) {
                act.reopen_tab = true;
            }
            // F-017 — Alt+Up/Down move the cursor line; Ctrl+Shift+D
            // duplicates; Ctrl+J joins next.
            if i.modifiers.alt && i.key_pressed(egui::Key::ArrowUp) {
                act.move_line_up = true;
            }
            if i.modifiers.alt && i.key_pressed(egui::Key::ArrowDown) {
                act.move_line_down = true;
            }
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::D) {
                act.duplicate_line = true;
            }
            if cmd && i.key_pressed(egui::Key::J) {
                act.join_lines = true;
            }
            // F-011 — drag-drop file open. egui collects DroppedFile entries
            // into RawInput.dropped_files; consume them here so the deferred
            // application opens each as a new tab.
            for file in i.raw.dropped_files.iter() {
                if let Some(p) = file.path.clone() {
                    act.files_to_open.push(p);
                }
            }
            if cmd
                && i.key_pressed(egui::Key::Tab)
                && !i.modifiers.shift
                && self.completion.is_none()
            {
                act.cycle_tab_next = true;
            }
            if cmd
                && i.key_pressed(egui::Key::Tab)
                && i.modifiers.shift
                && self.completion.is_none()
            {
                act.cycle_tab_prev = true;
            }
            // F-014: F1 toggles the keyboard cheatsheet — universal "help"
            // convention. The Esc handler below closes it like any overlay.
            if i.key_pressed(egui::Key::F1) {
                self.cheatsheet_open = !self.cheatsheet_open;
            }
            // F-015 — Ctrl+G opens the go-to-line modal.
            if cmd && i.key_pressed(egui::Key::G) {
                self.goto_open = true;
                self.focus_goto = true;
                self.goto_query.clear();
            }
            // Ctrl+Shift+O opens the go-to-symbol modal (jump to a definition
            // in the active buffer).
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::O) {
                if !self.goto_symbol_open {
                    self.focus_goto_symbol = true;
                }
                self.goto_symbol_open = true;
                self.goto_symbol_query.clear();
                self.goto_symbol_selected = 0;
            }
            // F-012 — Ctrl+R opens the recent-files modal. Exclude shift so
            // Ctrl+Shift+R (reopen-closed-tab, above) does not ALSO open it.
            if cmd && !i.modifiers.shift && i.key_pressed(egui::Key::R) {
                self.recent_open = true;
                self.recent_selected = 0;
            }
            // Line bookmarks: Ctrl+F2 toggles on the cursor line; F2 jumps to
            // the next bookmark; Shift+F2 jumps to the previous one. Ctrl takes
            // priority so Ctrl+F2 never doubles as a plain-F2 navigate.
            if i.key_pressed(egui::Key::F2) {
                if cmd {
                    act.toggle_bookmark = true;
                } else if i.modifiers.shift {
                    act.prev_bookmark = true;
                } else {
                    act.next_bookmark = true;
                }
            }
            // #R6 — F3 / Shift+F3 cycle find matches while the find bar is open.
            if self.find_open && i.key_pressed(egui::Key::F3) {
                *find_nav = Some(!i.modifiers.shift);
            }
            // F-010 — Ctrl+P opens the fuzzy file finder (rebuilds the
            // file index on first open so cold-start cost lands here,
            // not on launch).
            if cmd && i.key_pressed(egui::Key::P) && !i.modifiers.shift {
                act.open_fuzzy = true;
            }
            if i.key_pressed(egui::Key::Escape) {
                // Esc exits zen mode / F11 fullscreen first so the chrome comes
                // back before any overlay close — one press to leave the
                // distraction-free / fullscreen surface.
                if self.zen_mode {
                    self.zen_mode = false;
                } else if i.viewport().fullscreen.unwrap_or(false) {
                    // Exit OS fullscreen via the existing deferred handler
                    // (it sends Fullscreen(false) since we are currently in it).
                    act.toggle_fullscreen = true;
                } else {
                    self.find_open = false;
                    self.palette_open = false;
                    self.cheatsheet_open = false;
                    self.goto_open = false;
                    self.goto_symbol_open = false;
                    self.recent_open = false;
                    self.recent_folders_open = false;
                    self.welcome_open = false;
                    self.fuzzy_open = false;
                    // PA-02: route the project-find results pane through the same
                    // centralized Esc-close as the other overlays.
                    self.find_in_files_open = false;
                }
            }
        });
    }
}
