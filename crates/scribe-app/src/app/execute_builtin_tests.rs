//! WU-1 coverage: drive the `execute_builtin` command reducer headlessly.
//!
//! `execute_builtin` is the single routing point shared by the Ctrl+Shift+P
//! palette and the keyboard chords (the registry in `app/commands.rs` declares
//! the entries; this is where they take effect). The view-toggle, tab-cycle,
//! and theme-cycle arms are pure state mutations with no `egui::Context`, no
//! GPU paint, and no `rfd` dialog, so they run through `ScribeApp::new_test`
//! without a frame. The dialog-bound arms (`OpenFile`/`OpenFolder` →
//! `rfd::FileDialog`) are deliberately NOT exercised here — those are in the
//! structurally-uncoverable set (WU-0).

use super::{BuiltinCommand, EditorTab, ScribeApp};
use scribe_core::Config;

fn app() -> ScribeApp {
    ScribeApp::new_test(Config::default())
}

// ---- view toggles flip the backing config/state flag ----

#[test]
fn toggle_split_view_flips_grid_enabled() {
    let mut a = app();
    let before = a.config.editor.grid_enabled;
    a.execute_builtin(BuiltinCommand::ToggleSplitView);
    assert_eq!(a.config.editor.grid_enabled, !before);
    a.execute_builtin(BuiltinCommand::ToggleSplitView);
    assert_eq!(a.config.editor.grid_enabled, before, "toggles back");
}

#[test]
fn toggle_minimap_flips_flag() {
    let mut a = app();
    let before = a.config.editor.show_minimap;
    a.execute_builtin(BuiltinCommand::ToggleMinimap);
    assert_eq!(a.config.editor.show_minimap, !before);
}

#[test]
fn toggle_word_wrap_flips_flag() {
    let mut a = app();
    let before = a.config.editor.word_wrap;
    a.execute_builtin(BuiltinCommand::ToggleWordWrap);
    assert_eq!(a.config.editor.word_wrap, !before);
}

#[test]
fn toggle_line_numbers_flips_flag() {
    let mut a = app();
    let before = a.config.editor.show_line_numbers;
    a.execute_builtin(BuiltinCommand::ToggleLineNumbers);
    assert_eq!(a.config.editor.show_line_numbers, !before);
}

#[test]
fn toggle_change_bar_flips_flag() {
    let mut a = app();
    let before = a.config.editor.show_change_bar;
    a.execute_builtin(BuiltinCommand::ToggleChangeBar);
    assert_eq!(a.config.editor.show_change_bar, !before);
}

#[test]
fn toggle_spellcheck_flips_flag() {
    let mut a = app();
    let before = a.config.spellcheck.enabled;
    a.execute_builtin(BuiltinCommand::ToggleSpellcheck);
    assert_eq!(a.config.spellcheck.enabled, !before);
}

#[test]
fn toggle_markdown_preview_and_diff_view_are_session_flags() {
    let mut a = app();
    assert!(!a.md_preview_open);
    a.execute_builtin(BuiltinCommand::ToggleMarkdownPreview);
    assert!(a.md_preview_open);

    assert!(!a.diff_view_open);
    a.execute_builtin(BuiltinCommand::ToggleDiffView);
    assert!(a.diff_view_open);
}

#[test]
fn toggle_zen_closes_find_panes_when_entering() {
    let mut a = app();
    a.find_open = true;
    a.find_in_files_open = true;
    a.execute_builtin(BuiltinCommand::ToggleZen);
    assert!(a.zen_mode, "zen entered");
    assert!(!a.find_open, "find closed on zen enter");
    assert!(!a.find_in_files_open, "find-in-files closed on zen enter");
    // Leaving zen does not reopen the panes.
    a.execute_builtin(BuiltinCommand::ToggleZen);
    assert!(!a.zen_mode);
}

// ---- modal-open commands set the right open-flag ----

#[test]
fn open_settings_find_palette_set_their_open_flags() {
    let mut a = app();
    a.execute_builtin(BuiltinCommand::OpenSettings);
    assert!(a.settings_open);

    let mut a = app();
    a.execute_builtin(BuiltinCommand::OpenFind);
    assert!(a.find_open);
    assert!(a.focus_find);

    let mut a = app();
    a.execute_builtin(BuiltinCommand::OpenPalette);
    assert!(a.palette_open);
    assert!(a.focus_palette);

    let mut a = app();
    a.execute_builtin(BuiltinCommand::OpenRecentFolder);
    assert!(a.recent_folders_open);
}

// ---- tab cycle / close arms ----

#[test]
fn cycle_tab_next_wraps_around() {
    let mut a = app();
    a.tabs.clear();
    a.tabs.push(EditorTab::scratch());
    a.tabs.push(EditorTab::scratch());
    a.tabs.push(EditorTab::scratch());
    a.active = 0;
    a.execute_builtin(BuiltinCommand::CycleTabNext);
    assert_eq!(a.active, 1);
    a.execute_builtin(BuiltinCommand::CycleTabNext);
    assert_eq!(a.active, 2);
    a.execute_builtin(BuiltinCommand::CycleTabNext);
    assert_eq!(a.active, 0, "wraps to first");
}

#[test]
fn cycle_tab_prev_wraps_around() {
    let mut a = app();
    a.tabs.clear();
    a.tabs.push(EditorTab::scratch());
    a.tabs.push(EditorTab::scratch());
    a.active = 0;
    a.execute_builtin(BuiltinCommand::CycleTabPrev);
    assert_eq!(a.active, 1, "wraps to last from first");
    a.execute_builtin(BuiltinCommand::CycleTabPrev);
    assert_eq!(a.active, 0);
}

#[test]
fn close_all_tabs_leaves_one_scratch_tab() {
    let mut a = app();
    a.tabs.push(EditorTab::scratch());
    a.tabs.push(EditorTab::scratch());
    a.execute_builtin(BuiltinCommand::CloseAllTabs);
    assert_eq!(a.tabs.len(), 1, "always keeps one buffer open");
    assert_eq!(a.active, 0);
}

#[test]
fn new_file_appends_a_tab() {
    let mut a = app();
    let before = a.tabs.len();
    a.execute_builtin(BuiltinCommand::NewFile);
    assert_eq!(a.tabs.len(), before + 1);
}

// ---- theme cycle advances to a different built-in theme ----

#[test]
fn cycle_theme_advances_to_a_different_builtin() {
    let mut a = app();
    let start = a.config.appearance.theme.clone();
    a.execute_builtin(BuiltinCommand::CycleTheme);
    let after = a.config.appearance.theme.clone();
    // There is more than one built-in theme, so cycling once must change it.
    assert_ne!(start, after, "cycle theme must move to the next theme");
    assert!(
        scribe_core::theme::Theme::builtin_names().contains(&after.as_str()),
        "lands on a real built-in theme"
    );
}

// ---- fold / expand operate on the active buffer ----

#[test]
fn expand_all_clears_folds() {
    let mut a = app();
    a.folds.insert(0);
    a.folds.insert(3);
    a.execute_builtin(BuiltinCommand::ExpandAll);
    assert!(a.folds.is_empty(), "expand clears every fold");
}

// ---- buffer transforms rewrite the active tab's text ----

fn app_with_text(text: &str) -> ScribeApp {
    let mut a = app();
    let active = a.active;
    a.tabs[active].text = text.into();
    a
}

#[test]
fn sort_lines_orders_active_buffer() {
    let mut a = app_with_text("charlie\nalpha\nbravo");
    a.execute_builtin(BuiltinCommand::SortLines);
    assert_eq!(a.tabs[a.active].text, "alpha\nbravo\ncharlie");
}

#[test]
fn sort_lines_unique_dedups_and_orders() {
    let mut a = app_with_text("b\na\nb\na");
    a.execute_builtin(BuiltinCommand::SortLinesUnique);
    assert_eq!(a.tabs[a.active].text, "a\nb");
}

#[test]
fn trim_trailing_whitespace_strips_line_ends() {
    let mut a = app_with_text("alpha   \nbeta\t");
    a.execute_builtin(BuiltinCommand::TrimTrailingWhitespace);
    assert_eq!(a.tabs[a.active].text, "alpha\nbeta");
}

#[test]
fn ensure_final_newline_appends_one() {
    let mut a = app_with_text("no newline");
    a.execute_builtin(BuiltinCommand::EnsureFinalNewline);
    assert!(a.tabs[a.active].text.ends_with('\n'));
}

#[test]
fn convert_indent_to_spaces_then_tabs_roundtrips_leading_indent() {
    let mut a = app_with_text("\tcode");
    a.config.editor.tab_width = 4;
    a.execute_builtin(BuiltinCommand::ConvertIndentToSpaces);
    assert_eq!(a.tabs[a.active].text, "    code", "tab → 4 spaces");
    a.execute_builtin(BuiltinCommand::ConvertIndentToTabs);
    assert_eq!(a.tabs[a.active].text, "\tcode", "4 spaces → tab");
}

// ---- ctx-deferred commands set the pending flag frame_tick drains ----

#[test]
fn clipboard_commands_queue_pending_editor_action() {
    use super::EditorAction;
    for (cmd, want) in [
        (BuiltinCommand::Copy, EditorAction::Copy),
        (BuiltinCommand::Cut, EditorAction::Cut),
        (BuiltinCommand::Paste, EditorAction::Paste),
        (BuiltinCommand::Undo, EditorAction::Undo),
        (BuiltinCommand::Redo, EditorAction::Redo),
    ] {
        let mut a = app();
        a.execute_builtin(cmd);
        assert_eq!(
            a.pending_editor_action,
            Some(want),
            "{cmd:?} should queue {want:?}"
        );
    }
}

#[test]
fn editor_intent_commands_set_their_pending_flags() {
    let mut a = app();
    a.execute_builtin(BuiltinCommand::JumpMatchingBracket);
    assert!(a.pending_jump_bracket);

    let mut a = app();
    a.execute_builtin(BuiltinCommand::InsertDateTime);
    assert!(a.pending_insert_datetime);

    let mut a = app();
    a.execute_builtin(BuiltinCommand::DuplicateSelection);
    assert!(a.pending_dup_selection);
}
