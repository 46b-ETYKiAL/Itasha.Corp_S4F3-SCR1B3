//! User-facing keymap schema + validation ([`Keybindings`] + [`KeybindingIssue`]).
//!
//! Ported from C0PL4ND's keybindings framework (M7), re-authored for the EDITOR
//! domain: the default set reproduces SCR1B3's CURRENT hard-wired shortcuts
//! EXACTLY (see `app::keyboard_input`), so shipping the schema is a zero-behaviour
//! change. `mod` is the platform command modifier (Ctrl on Windows/Linux, Cmd on
//! macOS) — the same `i.modifiers.command` the hard-wired handler keys off.
//!
//! [`Keybindings::validate`] surfaces the two silent failure modes a user-editable
//! keymap can drift into — a blank binding (an unreachable action) and two actions
//! bound to the same combo (a collision) — so the settings UI can warn instead of
//! the user wondering why a shortcut "does nothing".

use serde::{Deserialize, Serialize};

/// User-rebindable key bindings (action name -> key-combo string). Every field's
/// default is the combo SCR1B3 currently hard-wires for that action, so the
/// default keymap is behaviour-identical to today's editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keybindings {
    /// New file / tab.
    pub new_file: String,
    /// Open a file.
    pub open_file: String,
    /// Save the active file.
    pub save: String,
    /// Open the in-buffer find bar.
    pub find: String,
    /// Open project-wide find (find in files).
    pub find_in_files: String,
    /// Open find-and-replace.
    pub replace: String,
    /// Open the command palette.
    pub command_palette: String,
    /// Open the fuzzy file finder.
    pub fuzzy_finder: String,
    /// Go to line.
    pub goto_line: String,
    /// Go to symbol in the active buffer.
    pub goto_symbol: String,
    /// Open the recent-files list.
    pub recent_files: String,
    /// Close the active tab.
    pub close_tab: String,
    /// Cycle to the next tab.
    pub next_tab: String,
    /// Cycle to the previous tab.
    pub prev_tab: String,
    /// Reopen the most recently closed tab.
    pub reopen_tab: String,
    /// Toggle the multi-note grid.
    pub toggle_grid: String,
    /// Toggle line comments on the selection.
    pub toggle_comment: String,
    /// Jump to the matching bracket.
    pub jump_bracket: String,
    /// Toggle OS fullscreen.
    pub toggle_fullscreen: String,
    /// Toggle zen / distraction-free mode.
    pub toggle_zen: String,
    /// Cycle to the next theme.
    pub cycle_theme: String,
    /// Toggle the minimap.
    pub toggle_minimap: String,
    /// Toggle the markdown live-preview panel.
    pub toggle_md_preview: String,
    /// Fold every region in the active buffer.
    pub fold_all: String,
    /// Expand every folded region.
    pub expand_all: String,
    /// Increase the editor font size.
    pub increase_font: String,
    /// Decrease the editor font size.
    pub decrease_font: String,
    /// Reset the editor font size to the default.
    pub reset_font: String,
    /// Move the current line up.
    pub move_line_up: String,
    /// Move the current line down.
    pub move_line_down: String,
    /// Duplicate the current line.
    pub duplicate_line: String,
    /// Join the next line onto the current one.
    pub join_lines: String,
    /// Toggle a bookmark on the cursor line.
    pub toggle_bookmark: String,
    /// Jump to the next bookmark.
    pub next_bookmark: String,
    /// Jump to the previous bookmark.
    pub prev_bookmark: String,
}

impl Default for Keybindings {
    fn default() -> Self {
        // Each combo is the EXACT chord SCR1B3 currently hard-wires in
        // `app::keyboard_input` — reproducing today's behaviour with zero change.
        // `mod` = the platform command modifier (Ctrl / Cmd).
        Keybindings {
            new_file: "mod+n".into(),
            open_file: "mod+o".into(),
            save: "mod+s".into(),
            find: "mod+f".into(),
            find_in_files: "mod+shift+f".into(),
            replace: "mod+h".into(),
            command_palette: "mod+shift+p".into(),
            fuzzy_finder: "mod+p".into(),
            goto_line: "mod+g".into(),
            goto_symbol: "mod+shift+o".into(),
            recent_files: "mod+r".into(),
            close_tab: "mod+w".into(),
            next_tab: "mod+tab".into(),
            prev_tab: "mod+shift+tab".into(),
            reopen_tab: "mod+shift+r".into(),
            toggle_grid: "mod+backslash".into(),
            toggle_comment: "mod+slash".into(),
            jump_bracket: "mod+m".into(),
            toggle_fullscreen: "f11".into(),
            toggle_zen: "mod+period".into(),
            cycle_theme: "mod+shift+t".into(),
            toggle_minimap: "mod+shift+m".into(),
            toggle_md_preview: "mod+shift+v".into(),
            fold_all: "mod+shift+openbracket".into(),
            expand_all: "mod+shift+closebracket".into(),
            increase_font: "mod+equals".into(),
            decrease_font: "mod+minus".into(),
            reset_font: "mod+num0".into(),
            move_line_up: "alt+arrowup".into(),
            move_line_down: "alt+arrowdown".into(),
            duplicate_line: "mod+shift+d".into(),
            join_lines: "mod+j".into(),
            toggle_bookmark: "mod+f2".into(),
            next_bookmark: "f2".into(),
            prev_bookmark: "shift+f2".into(),
        }
    }
}

/// A problem found in a [`Keybindings`] set by [`Keybindings::validate`].
///
/// The bindings are user-editable, so two actions can end up bound to the SAME
/// combo (only one would ever fire) or a binding can be left blank (the action
/// becomes unreachable) — both silently. `validate` makes these explicit so the
/// settings UI can warn instead of the user wondering why a shortcut does nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeybindingIssue {
    /// `action` has an empty / whitespace-only combo — it can never trigger.
    Empty { action: &'static str },
    /// `actions` (>= 2) are all bound to the same normalized `combo` — they
    /// collide; at most one can win.
    Conflict {
        combo: String,
        actions: Vec<&'static str>,
    },
}

impl KeybindingIssue {
    /// A human-readable, settings-surfaceable description of the issue.
    pub fn message(&self) -> String {
        match self {
            KeybindingIssue::Empty { action } => {
                format!("'{action}' has no key bound — it cannot be triggered")
            }
            KeybindingIssue::Conflict { combo, actions } => {
                format!(
                    "'{combo}' is bound to multiple actions: {}",
                    actions.join(", ")
                )
            }
        }
    }
}

impl Keybindings {
    /// Every (action-name, combo) pair, in a stable declaration order. The single
    /// source of truth both [`Keybindings::validate`] and any UI iteration key off,
    /// so a new binding is covered by adding ONE line here.
    pub fn entries(&self) -> [(&'static str, &str); 35] {
        [
            ("new_file", &self.new_file),
            ("open_file", &self.open_file),
            ("save", &self.save),
            ("find", &self.find),
            ("find_in_files", &self.find_in_files),
            ("replace", &self.replace),
            ("command_palette", &self.command_palette),
            ("fuzzy_finder", &self.fuzzy_finder),
            ("goto_line", &self.goto_line),
            ("goto_symbol", &self.goto_symbol),
            ("recent_files", &self.recent_files),
            ("close_tab", &self.close_tab),
            ("next_tab", &self.next_tab),
            ("prev_tab", &self.prev_tab),
            ("reopen_tab", &self.reopen_tab),
            ("toggle_grid", &self.toggle_grid),
            ("toggle_comment", &self.toggle_comment),
            ("jump_bracket", &self.jump_bracket),
            ("toggle_fullscreen", &self.toggle_fullscreen),
            ("toggle_zen", &self.toggle_zen),
            ("cycle_theme", &self.cycle_theme),
            ("toggle_minimap", &self.toggle_minimap),
            ("toggle_md_preview", &self.toggle_md_preview),
            ("fold_all", &self.fold_all),
            ("expand_all", &self.expand_all),
            ("increase_font", &self.increase_font),
            ("decrease_font", &self.decrease_font),
            ("reset_font", &self.reset_font),
            ("move_line_up", &self.move_line_up),
            ("move_line_down", &self.move_line_down),
            ("duplicate_line", &self.duplicate_line),
            ("join_lines", &self.join_lines),
            ("toggle_bookmark", &self.toggle_bookmark),
            ("next_bookmark", &self.next_bookmark),
            ("prev_bookmark", &self.prev_bookmark),
        ]
    }

    /// Canonical form of a combo for conflict comparison: lowercased, trimmed,
    /// split on `+`, empties dropped, tokens sorted — so `"shift+mod+c"` and
    /// `"mod+shift+c"` compare equal. An all-empty combo normalizes to `""`.
    fn normalize_combo(combo: &str) -> String {
        let mut parts: Vec<String> = combo
            .split('+')
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty())
            .collect();
        parts.sort();
        parts.join("+")
    }

    /// Detect keybinding issues: blank bindings (unreachable actions) and combos
    /// bound to more than one action (collisions). Returns an empty Vec when the
    /// set is clean — the default set is clean by construction. Pure + order-
    /// deterministic (empties first in declaration order, then conflicts sorted by
    /// combo) so the settings surfacing is stable frame-to-frame.
    pub fn validate(&self) -> Vec<KeybindingIssue> {
        let entries = self.entries();
        let mut issues = Vec::new();

        // Blank bindings: an action with no resolvable combo can never fire.
        for (name, combo) in entries.iter() {
            if Self::normalize_combo(combo).is_empty() {
                issues.push(KeybindingIssue::Empty { action: name });
            }
        }

        // Collisions: group non-empty bindings by their normalized combo.
        let mut groups: Vec<(String, Vec<&'static str>)> = Vec::new();
        for (name, combo) in entries.iter() {
            let norm = Self::normalize_combo(combo);
            if norm.is_empty() {
                continue;
            }
            if let Some(slot) = groups.iter_mut().find(|(c, _)| *c == norm) {
                slot.1.push(name);
            } else {
                groups.push((norm, vec![name]));
            }
        }
        groups.sort_by(|a, b| a.0.cmp(&b.0));
        for (combo, actions) in groups {
            if actions.len() > 1 {
                issues.push(KeybindingIssue::Conflict { combo, actions });
            }
        }

        issues
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn default_keybindings_have_no_conflicts() {
        // The shipped default set is clean by construction: no action is blank and
        // no two actions share a combo, so `validate` returns nothing.
        assert!(
            Keybindings::default().validate().is_empty(),
            "the default keymap must be conflict-free: {:?}",
            Keybindings::default().validate()
        );
    }

    #[test]
    fn validate_detects_a_duplicate_combo_collision() {
        // Two actions bound to the same combo (even written in a different token
        // order) must be flagged as a Conflict listing both action names.
        let kb = Keybindings {
            save: "mod+k".into(),
            find: "k+mod".into(), // same combo, different token order
            ..Default::default()
        };
        let issues = kb.validate();
        let conflict = issues.iter().find_map(|i| match i {
            KeybindingIssue::Conflict { combo, actions } if combo == "k+mod" => Some(actions),
            _ => None,
        });
        let actions = conflict.expect("a k+mod conflict must be reported");
        assert!(actions.contains(&"save"));
        assert!(actions.contains(&"find"));
    }

    #[test]
    fn validate_detects_an_empty_binding() {
        // A blank (or whitespace-only) binding is an unreachable action and must be
        // flagged as Empty, naming the action.
        let kb = Keybindings {
            save: "   ".into(),
            ..Default::default()
        };
        let issues = kb.validate();
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, KeybindingIssue::Empty { action } if *action == "save")),
            "an empty binding must be flagged: {issues:?}"
        );
    }

    #[test]
    fn default_keymap_matches_current_hardwired_shortcuts() {
        // M7 (locked decision): the default action set MUST reproduce SCR1B3's
        // CURRENT hard-wired editor shortcuts EXACTLY — `mod` is the command
        // modifier the hard-wired handler keys off (`i.modifiers.command`). This
        // pins the parity so a future default edit that drifts from the wired
        // behaviour is caught.
        let kb = Keybindings::default();
        // File / edit.
        assert_eq!(kb.new_file, "mod+n"); // Ctrl+N
        assert_eq!(kb.open_file, "mod+o"); // Ctrl+O (!shift)
        assert_eq!(kb.save, "mod+s"); // Ctrl+S
        assert_eq!(kb.find, "mod+f"); // Ctrl+F (!shift)
        assert_eq!(kb.find_in_files, "mod+shift+f"); // Ctrl+Shift+F
        assert_eq!(kb.replace, "mod+h"); // Ctrl+H
        assert_eq!(kb.command_palette, "mod+shift+p"); // Ctrl+Shift+P
        assert_eq!(kb.fuzzy_finder, "mod+p"); // Ctrl+P (!shift)
        assert_eq!(kb.goto_line, "mod+g"); // Ctrl+G
        assert_eq!(kb.goto_symbol, "mod+shift+o"); // Ctrl+Shift+O
        assert_eq!(kb.recent_files, "mod+r"); // Ctrl+R (!shift)
                                              // Tabs.
        assert_eq!(kb.close_tab, "mod+w"); // Ctrl+W
        assert_eq!(kb.next_tab, "mod+tab"); // Ctrl+Tab
        assert_eq!(kb.prev_tab, "mod+shift+tab"); // Ctrl+Shift+Tab
        assert_eq!(kb.reopen_tab, "mod+shift+r"); // Ctrl+Shift+R
                                                  // View / toggles.
        assert_eq!(kb.toggle_grid, "mod+backslash"); // Ctrl+\
        assert_eq!(kb.toggle_comment, "mod+slash"); // Ctrl+/
        assert_eq!(kb.jump_bracket, "mod+m"); // Ctrl+M (!shift)
        assert_eq!(kb.toggle_fullscreen, "f11"); // F11
        assert_eq!(kb.toggle_zen, "mod+period"); // Ctrl+.
        assert_eq!(kb.cycle_theme, "mod+shift+t"); // Ctrl+Shift+T
        assert_eq!(kb.toggle_minimap, "mod+shift+m"); // Ctrl+Shift+M
        assert_eq!(kb.toggle_md_preview, "mod+shift+v"); // Ctrl+Shift+V
        assert_eq!(kb.fold_all, "mod+shift+openbracket"); // Ctrl+Shift+[
        assert_eq!(kb.expand_all, "mod+shift+closebracket"); // Ctrl+Shift+]
                                                             // Font.
        assert_eq!(kb.increase_font, "mod+equals"); // Ctrl+= / Ctrl++
        assert_eq!(kb.decrease_font, "mod+minus"); // Ctrl+-
        assert_eq!(kb.reset_font, "mod+num0"); // Ctrl+0
                                               // Line ops.
        assert_eq!(kb.move_line_up, "alt+arrowup"); // Alt+Up
        assert_eq!(kb.move_line_down, "alt+arrowdown"); // Alt+Down
        assert_eq!(kb.duplicate_line, "mod+shift+d"); // Ctrl+Shift+D
        assert_eq!(kb.join_lines, "mod+j"); // Ctrl+J
                                            // Bookmarks.
        assert_eq!(kb.toggle_bookmark, "mod+f2"); // Ctrl+F2
        assert_eq!(kb.next_bookmark, "f2"); // F2
        assert_eq!(kb.prev_bookmark, "shift+f2"); // Shift+F2
    }

    #[test]
    fn keybindings_backfill_and_round_trip_through_config() {
        // A config that predates the M7 field loads with the full default keymap
        // (additive backfill), and a Config round-trips its keybindings through
        // TOML without loss.
        let c = Config::from_toml_str("[editor]\ntab_width = 4\n").unwrap();
        assert_eq!(c.keybindings, Keybindings::default(), "absent => defaults");
        let back = Config::from_toml_str(&c.to_toml_string()).unwrap();
        assert_eq!(back.keybindings, c.keybindings, "keymap round-trips");
    }
}
