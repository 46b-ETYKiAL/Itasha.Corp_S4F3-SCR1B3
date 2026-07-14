//! User-facing keymap schema + validation ([`Keybindings`] + [`KeybindingIssue`]).
//!
//! Ported from C0PL4ND's keybindings framework (M7), re-authored for the EDITOR
//! domain: the default set reproduces SCR1B3's CURRENT hard-wired shortcuts
//! EXACTLY (see `app::keyboard_input`), so shipping the schema is a zero-behaviour
//! change. `mod` is the platform command modifier (Ctrl on Windows/Linux, Cmd on
//! macOS) — the same `i.modifiers.command` the hard-wired handler keys off.
//!
//! [`Keybindings::validate`] surfaces the silent failure modes a user-editable
//! keymap can drift into — a blank binding (an unreachable action), an
//! unparseable combo, and two actions bound to the same combo (a collision) — so
//! the settings UI can warn instead of the user wondering why a shortcut "does
//! nothing".
//!
//! [`Chord`] is the parsed form the editor's input layer matches against. It is
//! deliberately engine-neutral: it carries the modifier flags plus a canonical
//! key TOKEN (`"n"`, `"f11"`, `"arrowup"`), and the app crate maps that token to
//! its windowing library's key type. That keeps `scribe-core` free of any UI
//! dependency while still owning the single definition of "what does this combo
//! string mean".

use serde::{Deserialize, Serialize};

/// A parsed key combo: the modifier flags plus the canonical non-modifier key
/// token (lowercased, e.g. `"n"` / `"f11"` / `"arrowup"`).
///
/// `mod` is the platform command modifier (Ctrl on Windows/Linux, Cmd on macOS);
/// `ctrl` / `cmd` / `command` are accepted as aliases for it, and `option` as an
/// alias for `alt`, so a config written with either muscle-memory still parses.
///
/// Matching is EXACT on modifiers: a chord parsed from `"mod+o"` has
/// `shift == false` and must NOT fire when Shift is also held (that is what
/// keeps `mod+o` and `mod+shift+o` distinct actions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    /// The platform command modifier (Ctrl / Cmd) is required.
    pub cmd: bool,
    /// Shift is required.
    pub shift: bool,
    /// Alt / Option is required.
    pub alt: bool,
    /// Canonical lowercase key token — the one non-modifier token in the combo.
    pub key: String,
}

impl Chord {
    /// Parse a combo string such as `"mod+shift+f"`.
    ///
    /// Returns `None` when the combo is unusable: no non-modifier key
    /// (`""`, `"mod"`), or more than one non-modifier key (`"a+b"`). Tokens are
    /// trimmed and lowercased, so `"Mod + Shift + F"` parses like `"mod+shift+f"`.
    pub fn parse(combo: &str) -> Option<Self> {
        let mut chord = Self {
            cmd: false,
            shift: false,
            alt: false,
            key: String::new(),
        };
        let mut key_seen = false;
        for raw in combo.split('+') {
            let token = raw.trim().to_ascii_lowercase();
            if token.is_empty() {
                continue;
            }
            match token.as_str() {
                "mod" | "ctrl" | "control" | "cmd" | "command" => chord.cmd = true,
                "shift" => chord.shift = true,
                "alt" | "option" => chord.alt = true,
                _ => {
                    // A second non-modifier key is not a chord this editor can
                    // express (egui has no multi-key chord layer) — reject it
                    // rather than silently honouring only the last one.
                    if key_seen {
                        return None;
                    }
                    key_seen = true;
                    chord.key = token;
                }
            }
        }
        key_seen.then_some(chord)
    }

    /// The canonical rendering of this chord (`"mod+alt+shift+key"`, modifiers in
    /// a fixed order). Two combo strings that mean the same thing — `"shift+mod+c"`,
    /// `"ctrl+shift+c"`, `"mod+shift+c"` — share one canonical form, which is what
    /// makes [`Keybindings::validate`] conflict detection alias-aware.
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        if self.cmd {
            out.push_str("mod+");
        }
        if self.alt {
            out.push_str("alt+");
        }
        if self.shift {
            out.push_str("shift+");
        }
        out.push_str(&self.key);
        out
    }
}

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
    /// `action`'s combo cannot be parsed into a chord (no key, or more than one
    /// key — e.g. `"mod"` alone or `"a+b"`), so the action is unreachable. Without
    /// this the binding would look plausible in the file and simply never fire.
    Invalid { action: &'static str, combo: String },
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
            KeybindingIssue::Invalid { action, combo } => {
                format!("'{action}' has an unreadable key combo '{combo}' — it cannot be triggered")
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

    /// Detect keybinding issues: blank bindings, unparseable combos (both make an
    /// action unreachable), and combos bound to more than one action (collisions).
    /// Returns an empty Vec when the set is clean — the default set is clean by
    /// construction. Pure + order-deterministic (empties then invalids in
    /// declaration order, then conflicts sorted by combo) so the settings
    /// surfacing is stable frame-to-frame.
    ///
    /// Conflict detection keys off [`Chord::canonical`], the SAME parse the input
    /// layer matches with — so aliases collide the way they actually do at
    /// runtime (`"ctrl+s"` and `"mod+s"` are one combo, not two).
    pub fn validate(&self) -> Vec<KeybindingIssue> {
        let entries = self.entries();
        let mut issues = Vec::new();

        // Unreachable actions: a blank combo, or one that cannot parse into a
        // chord. Both would otherwise fail silently.
        for (name, combo) in entries.iter() {
            if combo.trim().is_empty() {
                issues.push(KeybindingIssue::Empty { action: name });
            } else if Chord::parse(combo).is_none() {
                issues.push(KeybindingIssue::Invalid {
                    action: name,
                    combo: (*combo).to_string(),
                });
            }
        }

        // Collisions: group parseable bindings by their canonical chord.
        let mut groups: Vec<(String, Vec<&'static str>)> = Vec::new();
        for (name, combo) in entries.iter() {
            let Some(canon) = Chord::parse(combo).map(|c| c.canonical()) else {
                continue;
            };
            if let Some(slot) = groups.iter_mut().find(|(c, _)| *c == canon) {
                slot.1.push(name);
            } else {
                groups.push((canon, vec![name]));
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
        // order) must be flagged as a Conflict listing both action names. The
        // reported combo is the canonical chord rendering (`mod+k`).
        let kb = Keybindings {
            save: "mod+k".into(),
            find: "k+mod".into(), // same combo, different token order
            ..Default::default()
        };
        let issues = kb.validate();
        let conflict = issues.iter().find_map(|i| match i {
            KeybindingIssue::Conflict { combo, actions } if combo == "mod+k" => Some(actions),
            _ => None,
        });
        let actions = conflict.expect("a mod+k conflict must be reported");
        assert!(actions.contains(&"save"));
        assert!(actions.contains(&"find"));
    }

    #[test]
    fn validate_detects_a_collision_written_with_modifier_aliases() {
        // `ctrl+k` and `mod+k` are the SAME chord at runtime (both mean the
        // command modifier), so binding two actions to them collides. The old
        // sort-the-raw-tokens normalization compared them as different strings
        // and missed this; canonicalizing through `Chord::parse` catches it.
        let kb = Keybindings {
            save: "mod+k".into(),
            find: "ctrl+k".into(),
            ..Default::default()
        };
        let conflict = kb.validate().into_iter().find_map(|i| match i {
            KeybindingIssue::Conflict { combo, actions } if combo == "mod+k" => Some(actions),
            _ => None,
        });
        let actions = conflict.expect("mod+k and ctrl+k are one chord and must collide");
        assert!(actions.contains(&"save"));
        assert!(actions.contains(&"find"));
    }

    #[test]
    fn validate_flags_an_unparseable_combo_as_invalid() {
        // A combo with no key (`"mod"`) or two keys (`"a+b"`) cannot fire. Before
        // `Invalid` existed these passed validation and then silently did nothing.
        let kb = Keybindings {
            save: "mod".into(),
            find: "a+b".into(),
            ..Default::default()
        };
        let issues = kb.validate();
        for action in ["save", "find"] {
            assert!(
                issues.iter().any(
                    |i| matches!(i, KeybindingIssue::Invalid { action: a, .. } if *a == action)
                ),
                "an unparseable combo for '{action}' must be flagged: {issues:?}"
            );
        }
    }

    #[test]
    fn chord_parses_modifiers_aliases_and_canonicalizes() {
        let c = Chord::parse("Mod + Shift + F").expect("a well-formed combo parses");
        assert_eq!(
            c,
            Chord {
                cmd: true,
                shift: true,
                alt: false,
                key: "f".into()
            }
        );
        assert_eq!(c.canonical(), "mod+shift+f");
        // Aliases fold onto the same canonical chord.
        for alias in [
            "ctrl+shift+f",
            "cmd+shift+f",
            "command+shift+f",
            "shift+mod+f",
        ] {
            assert_eq!(
                Chord::parse(alias).expect("alias parses").canonical(),
                "mod+shift+f",
                "'{alias}' must canonicalize like 'mod+shift+f'"
            );
        }
        assert_eq!(
            Chord::parse("option+arrowup")
                .expect("option aliases alt")
                .canonical(),
            "alt+arrowup"
        );
        // Unusable combos.
        assert!(Chord::parse("").is_none(), "empty combo");
        assert!(Chord::parse("mod+shift").is_none(), "modifiers with no key");
        assert!(Chord::parse("a+b").is_none(), "two non-modifier keys");
    }

    #[test]
    fn every_default_binding_parses_into_a_chord() {
        // The keymap is only authoritative if every shipped default actually
        // resolves — an unparseable default would be a dead action out of the box.
        for (action, combo) in Keybindings::default().entries() {
            assert!(
                Chord::parse(combo).is_some(),
                "default binding '{action}' = '{combo}' must parse into a chord"
            );
        }
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
