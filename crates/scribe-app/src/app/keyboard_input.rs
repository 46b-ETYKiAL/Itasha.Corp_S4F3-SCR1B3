//! Keyboard-shortcut handling for `frame_tick`, extracted from `mod.rs` (A-01 wave 3).
//!
//! Every chord here resolves from the user's `[keybindings]` config via
//! [`Keymap`](super::keymap::Keymap) — the config section is authoritative, not
//! decorative. The shipped defaults reproduce the chords this handler used to
//! hard-wire, so a user who never touches `[keybindings]` sees the same editor.
//!
//! Two deliberate differences from the old hard-wired form:
//! - **Modifiers match exactly.** The hard-wired tests were `cmd && key_pressed(K)`,
//!   which let Ctrl+Shift+S also fire plain Save and forced hand-written
//!   `!i.modifiers.shift` guards wherever a `mod+shift+…` action would otherwise
//!   collide. Exact matching makes those guards unnecessary and makes each
//!   binding mean one chord.
//! - **Only mapped actions are rebindable.** F1 (cheatsheet), Esc (close overlay),
//!   F3 (find-next) and Ctrl+scroll zoom stay hard-wired: they are not actions in
//!   the `[keybindings]` schema.
#![allow(clippy::wildcard_imports)]

use super::keymap::{action, Keymap};
use super::*;

/// Zoom-gesture deadzone, as a `zoom_delta()` multiplier.
///
/// Keeps the feel of the +/-0.5-point scroll deadzone this replaced: at egui's
/// default `scroll_zoom_speed` (1/200) a 0.5-point wheel step is
/// `exp(0.5/200) ~= 1.0025`. Below it, trackpad jitter must not resize the font.
const ZOOM_DEADZONE: f32 = 1.0025;

impl ScribeApp {
    /// Collect this frame's keyboard shortcuts into `act` (a `Pending` action
    /// set) and record the find-bar F3 navigation direction in `find_nav`.
    ///
    /// Chords come from the user's keymap; the surrounding app-state logic (what
    /// an action does, which overlay it focuses) is unchanged.
    pub(super) fn handle_keyboard_shortcuts(
        &mut self,
        ctx: &egui::Context,
        act: &mut Pending,
        find_nav: &mut Option<bool>,
    ) {
        // Config live-reloads, so re-resolve when (and only when) the user's
        // bindings actually changed. `Keymap` owns its data, which keeps
        // `self` free to be mutated inside the `ctx.input` closure below.
        if self.keymap_src != self.config.keybindings {
            self.keymap_src = self.config.keybindings.clone();
            self.keymap = Keymap::resolve(&self.keymap_src);
        }
        let km = &self.keymap;

        ctx.input(|i| {
            act.new = km.pressed(i, action::NEW_FILE);
            act.open = km.pressed(i, action::OPEN_FILE);
            act.save = km.pressed(i, action::SAVE);
            if km.pressed(i, action::FIND) {
                if !self.find_open {
                    self.focus_find = true;
                }
                self.find_open = true;
            }
            // Wave-5: project-wide find (find in files).
            if km.pressed(i, action::FIND_IN_FILES) {
                if !self.find_in_files_open {
                    self.focus_find_in_files = true;
                }
                self.find_in_files_open = true;
            }
            // Wave-5 P1: zen / distraction-free mode. Entering zen also closes the
            // find bars so nothing but the editor remains.
            if km.pressed(i, action::TOGGLE_ZEN) {
                self.zen_mode = !self.zen_mode;
                if self.zen_mode {
                    self.find_open = false;
                    self.find_in_files_open = false;
                }
            }
            // Wave-5 P1: toggle the markdown live-preview panel.
            if km.pressed(i, action::TOGGLE_MD_PREVIEW) {
                self.md_preview_open = !self.md_preview_open;
            }
            // The command palette (plugin + builtin cmds).
            if km.pressed(i, action::COMMAND_PALETTE) {
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
            // wave 1 keyboard shortcuts: close the active tab, toggle the
            // multi-note grid (F-003 entry-point fix), cycle tabs.
            if km.pressed(i, action::CLOSE_TAB) {
                act.close_active_tab = true;
            }
            if km.pressed(i, action::TOGGLE_GRID) {
                act.toggle_grid = true;
            }
            // Wave-2 keyboard fill-in (docs/audits/overlooked-surfaces-2026-05-29.md).
            if km.pressed(i, action::REPLACE) {
                act.open_replace = true;
            }
            if km.pressed(i, action::TOGGLE_COMMENT) {
                act.toggle_comment = true;
            }
            // Jump to the matching bracket.
            if km.pressed(i, action::JUMP_BRACKET) {
                act.jump_bracket = true;
            }
            if km.pressed(i, action::TOGGLE_FULLSCREEN) {
                act.toggle_fullscreen = true;
            }
            // F-018 — Ctrl+K Ctrl+T (cycle theme) approximated as a single-key
            // chord since egui has no native multi-key chord layer. F-031 —
            // toggle the minimap. Both persist via save_config.
            if km.pressed(i, action::CYCLE_THEME) {
                act.cycle_theme = true;
            }
            if km.pressed(i, action::TOGGLE_MINIMAP) {
                act.toggle_minimap = true;
            }
            // F-032 — fold every region in the active buffer / expand every
            // region. Switches the editor into fold-view mode so the user sees
            // the change immediately (otherwise the fold set is updated but the
            // normal central panel doesn't honor it).
            if km.pressed(i, action::FOLD_ALL) {
                act.fold_all = true;
            }
            if km.pressed(i, action::EXPAND_ALL) {
                act.expand_all = true;
            }
            // Font zoom: bound chords in / out / reset, plus hard-wired
            // Ctrl+scroll. Universal editor convenience.
            if km.pressed(i, action::INCREASE_FONT) {
                act.font_zoom = Some(1);
            }
            if km.pressed(i, action::DECREASE_FONT) {
                act.font_zoom = Some(-1);
            }
            if km.pressed(i, action::RESET_FONT) {
                act.font_zoom = Some(0);
            }
            // Ctrl+scroll never reached this handler. egui's `zoom_modifier`
            // defaults to COMMAND, so when a wheel event carries Ctrl (which
            // egui-winit always attaches) egui folds it into `zoom_factor_delta`
            // and leaves `smooth_scroll_delta` at ZERO. Reading the scroll delta
            // under `if cmd` was therefore dead: `dy` was always 0.0.
            //
            // `zoom_delta()` is the signal egui actually publishes for "the user
            // wants to zoom" — and it reports trackpad pinch too, so that now
            // zooms the font as well. It is a multiplier: 1.0 means no gesture.
            let zoom = i.zoom_delta();
            if zoom > ZOOM_DEADZONE {
                act.font_zoom = Some(1);
            } else if zoom < 1.0 / ZOOM_DEADZONE {
                act.font_zoom = Some(-1);
            }
            // Reopen the most recently closed tab (the default is Ctrl+Shift+R —
            // Ctrl+Shift+T is already the theme-cycle chord in this editor).
            if km.pressed(i, action::REOPEN_TAB) {
                act.reopen_tab = true;
            }
            // F-017 — move the cursor line up/down, duplicate it, join the next.
            if km.pressed(i, action::MOVE_LINE_UP) {
                act.move_line_up = true;
            }
            if km.pressed(i, action::MOVE_LINE_DOWN) {
                act.move_line_down = true;
            }
            if km.pressed(i, action::DUPLICATE_LINE) {
                act.duplicate_line = true;
            }
            if km.pressed(i, action::JOIN_LINES) {
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
            // Tab cycling is suppressed while the completion popup is open — it
            // consumes Tab to accept a candidate.
            if km.pressed(i, action::NEXT_TAB) && self.completion.is_none() {
                act.cycle_tab_next = true;
            }
            if km.pressed(i, action::PREV_TAB) && self.completion.is_none() {
                act.cycle_tab_prev = true;
            }
            // F-014: F1 toggles the keyboard cheatsheet — universal "help"
            // convention, deliberately not rebindable. The Esc handler below
            // closes it like any overlay.
            if i.key_pressed(egui::Key::F1) {
                self.cheatsheet_open = !self.cheatsheet_open;
            }
            // F-015 — the go-to-line modal.
            if km.pressed(i, action::GOTO_LINE) {
                self.goto_open = true;
                self.focus_goto = true;
                self.goto_query.clear();
            }
            // The go-to-symbol modal (jump to a definition in the active buffer).
            if km.pressed(i, action::GOTO_SYMBOL) {
                if !self.goto_symbol_open {
                    self.focus_goto_symbol = true;
                }
                self.goto_symbol_open = true;
                self.goto_symbol_query.clear();
                self.goto_symbol_selected = 0;
            }
            // F-012 — the recent-files modal.
            if km.pressed(i, action::RECENT_FILES) {
                self.recent_open = true;
                self.recent_selected = 0;
            }
            // Line bookmarks: toggle on the cursor line, jump to the next, jump
            // to the previous. Exact modifier matching keeps the three F2 chords
            // (Ctrl+F2 / F2 / Shift+F2 by default) from shadowing each other.
            if km.pressed(i, action::TOGGLE_BOOKMARK) {
                act.toggle_bookmark = true;
            }
            if km.pressed(i, action::NEXT_BOOKMARK) {
                act.next_bookmark = true;
            }
            if km.pressed(i, action::PREV_BOOKMARK) {
                act.prev_bookmark = true;
            }
            // #R6 — F3 / Shift+F3 cycle find matches while the find bar is open.
            // Not rebindable: it is find-bar navigation, not a global action.
            if self.find_open && i.key_pressed(egui::Key::F3) {
                *find_nav = Some(!i.modifiers.shift);
            }
            // F-010 — the fuzzy file finder (rebuilds the file index on first
            // open so cold-start cost lands here, not on launch).
            if km.pressed(i, action::FUZZY_FINDER) {
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
