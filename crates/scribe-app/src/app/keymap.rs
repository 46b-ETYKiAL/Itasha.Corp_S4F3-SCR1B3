//! Resolve the user's `[keybindings]` config into egui chords the input layer
//! matches against.
//!
//! `scribe-core` owns the combo GRAMMAR ([`Chord::parse`]) and stays free of any
//! UI dependency, so it hands back a canonical key TOKEN (`"n"`, `"f11"`,
//! `"arrowup"`). This module is the other half: it binds that token to an
//! [`egui::Key`] and answers "did the user press the chord bound to <action>
//! this frame?".
//!
//! Modifier matching is EXACT — a chord bound to `mod+o` does not fire when Shift
//! is also held. That is what keeps `mod+o` (open file) and `mod+shift+o` (go to
//! symbol) distinct without the hand-written `!i.modifiers.shift` guards the
//! hard-wired handler used to need.

use super::*;
use scribe_core::config::{Chord, Keybindings};

/// Action names, matching [`Keybindings::entries`] exactly.
///
/// The input layer refers to actions through these consts rather than bare string
/// literals so a typo is a compile error, not an action that silently never
/// fires. `action_names_match_the_config_schema` pins the two lists together in
/// BOTH directions, so a binding added to the config schema without a const here
/// (or vice versa) fails the suite.
pub(super) mod action {
    pub const NEW_FILE: &str = "new_file";
    pub const OPEN_FILE: &str = "open_file";
    pub const SAVE: &str = "save";
    pub const FIND: &str = "find";
    pub const FIND_IN_FILES: &str = "find_in_files";
    pub const REPLACE: &str = "replace";
    pub const COMMAND_PALETTE: &str = "command_palette";
    pub const FUZZY_FINDER: &str = "fuzzy_finder";
    pub const GOTO_LINE: &str = "goto_line";
    pub const GOTO_SYMBOL: &str = "goto_symbol";
    pub const RECENT_FILES: &str = "recent_files";
    pub const CLOSE_TAB: &str = "close_tab";
    pub const NEXT_TAB: &str = "next_tab";
    pub const PREV_TAB: &str = "prev_tab";
    pub const REOPEN_TAB: &str = "reopen_tab";
    pub const TOGGLE_GRID: &str = "toggle_grid";
    pub const TOGGLE_COMMENT: &str = "toggle_comment";
    pub const JUMP_BRACKET: &str = "jump_bracket";
    pub const TOGGLE_FULLSCREEN: &str = "toggle_fullscreen";
    pub const TOGGLE_ZEN: &str = "toggle_zen";
    pub const CYCLE_THEME: &str = "cycle_theme";
    pub const TOGGLE_MINIMAP: &str = "toggle_minimap";
    pub const TOGGLE_MD_PREVIEW: &str = "toggle_md_preview";
    pub const FOLD_ALL: &str = "fold_all";
    pub const EXPAND_ALL: &str = "expand_all";
    pub const INCREASE_FONT: &str = "increase_font";
    pub const DECREASE_FONT: &str = "decrease_font";
    pub const RESET_FONT: &str = "reset_font";
    pub const MOVE_LINE_UP: &str = "move_line_up";
    pub const MOVE_LINE_DOWN: &str = "move_line_down";
    pub const DUPLICATE_LINE: &str = "duplicate_line";
    pub const JOIN_LINES: &str = "join_lines";
    pub const TOGGLE_BOOKMARK: &str = "toggle_bookmark";
    pub const NEXT_BOOKMARK: &str = "next_bookmark";
    pub const PREV_BOOKMARK: &str = "prev_bookmark";

    /// Every action const, for the schema-parity test. Test-only: the runtime
    /// refers to each action by name, never by iterating this list.
    #[cfg(test)]
    pub const ALL: &[&str] = &[
        NEW_FILE,
        OPEN_FILE,
        SAVE,
        FIND,
        FIND_IN_FILES,
        REPLACE,
        COMMAND_PALETTE,
        FUZZY_FINDER,
        GOTO_LINE,
        GOTO_SYMBOL,
        RECENT_FILES,
        CLOSE_TAB,
        NEXT_TAB,
        PREV_TAB,
        REOPEN_TAB,
        TOGGLE_GRID,
        TOGGLE_COMMENT,
        JUMP_BRACKET,
        TOGGLE_FULLSCREEN,
        TOGGLE_ZEN,
        CYCLE_THEME,
        TOGGLE_MINIMAP,
        TOGGLE_MD_PREVIEW,
        FOLD_ALL,
        EXPAND_ALL,
        INCREASE_FONT,
        DECREASE_FONT,
        RESET_FONT,
        MOVE_LINE_UP,
        MOVE_LINE_DOWN,
        DUPLICATE_LINE,
        JOIN_LINES,
        TOGGLE_BOOKMARK,
        NEXT_BOOKMARK,
        PREV_BOOKMARK,
    ];
}

/// Map a canonical key token from [`Chord::parse`] onto an [`egui::Key`].
///
/// Resolved against egui's OWN key table rather than a hand-copied match, so the
/// accepted spellings track the egui version in the lockfile instead of drifting
/// from it. Two spellings are accepted per key:
/// - [`egui::Key::name`] — the display name (`"Backslash"`, `"Up"`, `"0"`, `"["`).
/// - the variant name via `Debug` (`"ArrowUp"`, `"Num0"`, `"OpenBracket"`), which
///   is the spelling [`egui::Key::from_name`] documents and our defaults use.
///
/// Both are compared case-insensitively, which is what lets the lowercase tokens
/// the config grammar produces (`"arrowup"`) resolve. `every_default_binding_
/// resolves_to_the_expected_key` pins all 35 defaults, so a future egui rename
/// fails the suite rather than silently killing a shortcut.
fn key_from_token(token: &str) -> Option<egui::Key> {
    egui::Key::ALL.iter().copied().find(|k| {
        k.name().eq_ignore_ascii_case(token) || format!("{k:?}").eq_ignore_ascii_case(token)
    })
}

/// How a key reads on a keyboard, for display in the cheatsheet / palette.
///
/// [`egui::Key::name`] is already right for letters (`"N"`), digits (`"0"`),
/// arrows (`"Up"`), `"Tab"` and `"F11"`; only punctuation needs overriding,
/// because a user looks for `Ctrl+\`, not `Ctrl+Backslash`.
fn key_display(key: egui::Key) -> &'static str {
    match key {
        egui::Key::Backslash => "\\",
        egui::Key::Slash => "/",
        egui::Key::OpenBracket => "[",
        egui::Key::CloseBracket => "]",
        egui::Key::Period => ".",
        egui::Key::Comma => ",",
        egui::Key::Equals => "=",
        egui::Key::Minus => "-",
        egui::Key::Plus => "+",
        egui::Key::Semicolon => ";",
        egui::Key::Quote => "'",
        egui::Key::Backtick => "`",
        egui::Key::Colon => ":",
        egui::Key::Pipe => "|",
        egui::Key::Questionmark => "?",
        egui::Key::Exclamationmark => "!",
        egui::Key::OpenCurlyBracket => "{",
        egui::Key::CloseCurlyBracket => "}",
        other => other.name(),
    }
}

/// Localize a HARD-CODED chord string (the `chord` / `shortcut` fallback text of
/// a non-rebindable shortcut) for the current platform.
///
/// Those strings are written `"Ctrl+C"`, but the handlers behind them key off
/// `egui::Modifiers::COMMAND` (e.g. Undo is `Key::Z + COMMAND`) and `ALT` — which
/// are Cmd and Option on macOS. So the literal text was already wrong for every
/// macOS user, before any of this was rebindable. Rebindable rows render through
/// [`ResolvedChord::display`], which is already platform-correct; without the same
/// treatment here the cheatsheet would read half "Cmd+E", half "Ctrl+C" on macOS.
///
/// A no-op everywhere else.
pub(super) fn platform_chord_text(text: &str) -> String {
    if cfg!(target_os = "macos") {
        text.replace("Ctrl+", "Cmd+").replace("Alt+", "Option+")
    } else {
        text.to_string()
    }
}

/// A chord resolved all the way to an [`egui::Key`] plus its required modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ResolvedChord {
    cmd: bool,
    shift: bool,
    alt: bool,
    key: egui::Key,
}

impl ResolvedChord {
    /// Render as a user-facing chord, e.g. `"Ctrl+Shift+F"`.
    ///
    /// `mod` prints per platform — **Cmd** on macOS, **Ctrl** elsewhere — which
    /// also fixes the old hard-coded tables, whose literal `"Ctrl+…"` strings were
    /// wrong for every macOS user regardless of rebinding.
    fn display(self) -> String {
        let mut out = String::new();
        if self.cmd {
            out.push_str(if cfg!(target_os = "macos") {
                "Cmd+"
            } else {
                "Ctrl+"
            });
        }
        if self.alt {
            out.push_str(if cfg!(target_os = "macos") {
                "Option+"
            } else {
                "Alt+"
            });
        }
        if self.shift {
            out.push_str("Shift+");
        }
        out.push_str(key_display(self.key));
        out
    }
}

/// The user's keymap, resolved once per `[keybindings]` change.
///
/// Entries are declaration-ordered, parallel to [`Keybindings::entries`]. An
/// action whose combo is blank / unparseable / names an unknown key resolves to
/// `None` and simply never fires — `Keybindings::validate` is what surfaces that
/// to the user.
/// Deliberately NOT `Default`: an empty keymap answers "no" to every action, so a
/// defaulted one would silently disable every shortcut in the editor. Build it
/// with [`Keymap::resolve`] from a real [`Keybindings`] — for the stock chords,
/// `Keymap::resolve(&Keybindings::default())`.
#[derive(Debug, Clone)]
pub(super) struct Keymap {
    chords: Vec<(&'static str, Option<ResolvedChord>)>,
}

impl Keymap {
    /// Resolve every binding in `kb` into a matchable chord.
    pub(super) fn resolve(kb: &Keybindings) -> Self {
        let chords = kb
            .entries()
            .iter()
            .map(|(name, combo)| {
                let resolved = Chord::parse(combo).and_then(|c| {
                    key_from_token(&c.key).map(|key| ResolvedChord {
                        cmd: c.cmd,
                        shift: c.shift,
                        alt: c.alt,
                        key,
                    })
                });
                (*name, resolved)
            })
            .collect();
        Self { chords }
    }

    fn chord(&self, action: &str) -> Option<ResolvedChord> {
        self.chords
            .iter()
            .find(|(name, _)| *name == action)
            .and_then(|(_, chord)| *chord)
    }

    /// Actions whose combo is a well-formed chord but names a key that does not
    /// exist (`mod+nosuchkey`), reported as ready-made user-facing messages.
    ///
    /// [`Keybindings::validate`] cannot catch this and should not try: it lives in
    /// `scribe-core` and owns the combo GRAMMAR, while the key TABLE belongs to the
    /// UI layer — to a grammar check, `nosuchkey` is a perfectly good key token.
    /// Only resolution knows better. Reporting it here is what stops an unknown
    /// key name from becoming exactly the silent dead shortcut the validation
    /// exists to prevent.
    ///
    /// Blank and unparseable combos are NOT reported here — `validate` already
    /// covers those, and double-reporting one binding would be noise.
    pub(super) fn unknown_key_messages(kb: &Keybindings) -> Vec<String> {
        kb.entries()
            .iter()
            .filter(|(_, combo)| {
                // Only combos that parse (so not Empty/Invalid) but whose key is
                // unresolvable.
                Chord::parse(combo).is_some_and(|c| key_from_token(&c.key).is_none())
            })
            .map(|(action, combo)| {
                format!("'{action}' is bound to '{combo}', which is not a key on your keyboard — it cannot be triggered")
            })
            .collect()
    }

    /// How the chords for `actions` currently read, for help surfaces.
    ///
    /// Returns `None` when `actions` is empty — the caller's hard-coded string is
    /// then correct, because the shortcut is not rebindable (Esc, F1, Ctrl+C …).
    /// Several actions render joined by `" / "`, which is how the font-zoom row
    /// shows in/out/reset as one line.
    ///
    /// An action that resolves to nothing renders as `"unbound"` rather than being
    /// skipped: the whole point of reading this off the live keymap is that the
    /// help never claims a key that would not work. If EVERY action is unbound the
    /// row says so honestly.
    pub(super) fn display_for(&self, actions: &[&str]) -> Option<String> {
        if actions.is_empty() {
            return None;
        }
        let shown: Vec<String> = actions
            .iter()
            .map(|a| {
                self.chord(a)
                    .map_or_else(|| "unbound".to_string(), ResolvedChord::display)
            })
            .collect();
        Some(shown.join(" / "))
    }

    /// Did the user press the chord bound to `action` this frame?
    ///
    /// Modifiers must match EXACTLY, so `mod+o` does not fire on Ctrl+Shift+O.
    pub(super) fn pressed(&self, i: &egui::InputState, action: &str) -> bool {
        let Some(c) = self.chord(action) else {
            return false;
        };
        let mods_ok = |shift: bool| {
            i.modifiers.command == c.cmd && i.modifiers.alt == c.alt && i.modifiers.shift == shift
        };
        if i.key_pressed(c.key) && mods_ok(c.shift) {
            return true;
        }
        // Shifted-symbol tolerance. On most layouts `+` IS Shift+`=`, so a press
        // of Ctrl+`+` arrives as (Key::Plus, shift: true). Exact matching alone
        // would stop the default `mod+equals` zoom-in from firing for anyone who
        // types Ctrl++ — which the hard-wired handler accepted (it tested
        // `Plus || Equals`). Accept the Plus press for an `equals` chord that
        // does not itself ask for Shift, and keep the other modifiers exact.
        if c.key == egui::Key::Equals
            && !c.shift
            && i.key_pressed(egui::Key::Plus)
            && i.modifiers.command == c.cmd
            && i.modifiers.alt == c.alt
        {
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_names_match_the_config_schema() {
        // Bidirectional parity: every const names a real binding, and every
        // binding in the schema has a const. A binding added to `Keybindings`
        // without wiring an action here fails HERE, which is the check that was
        // missing when the whole `[keybindings]` section shipped unwired.
        let kb = Keybindings::default();
        let schema: Vec<&str> = kb.entries().iter().map(|(n, _)| *n).collect();
        for name in action::ALL {
            assert!(
                schema.contains(name),
                "action const '{name}' is not a field in the Keybindings schema"
            );
        }
        for name in &schema {
            assert!(
                action::ALL.contains(name),
                "Keybindings field '{name}' has no action const — it cannot be wired to input"
            );
        }
        assert_eq!(
            action::ALL.len(),
            schema.len(),
            "action list must not duplicate"
        );
    }

    #[test]
    fn every_default_binding_resolves_to_the_expected_key() {
        // Pins the token -> egui::Key mapping for all 35 shipped defaults. This is
        // the guard on `key_from_token` reading egui's own tables: an egui rename
        // (or a Debug-format change) breaks this test instead of silently
        // resolving a shortcut to `None` and killing it at runtime.
        let km = Keymap::resolve(&Keybindings::default());
        let expect: &[(&str, bool, bool, bool, egui::Key)] = &[
            // (action, cmd, shift, alt, key)
            (action::NEW_FILE, true, false, false, egui::Key::N),
            (action::OPEN_FILE, true, false, false, egui::Key::O),
            (action::SAVE, true, false, false, egui::Key::S),
            (action::FIND, true, false, false, egui::Key::F),
            (action::FIND_IN_FILES, true, true, false, egui::Key::F),
            (action::REPLACE, true, false, false, egui::Key::H),
            (action::COMMAND_PALETTE, true, true, false, egui::Key::P),
            (action::FUZZY_FINDER, true, false, false, egui::Key::P),
            (action::GOTO_LINE, true, false, false, egui::Key::G),
            (action::GOTO_SYMBOL, true, true, false, egui::Key::O),
            (action::RECENT_FILES, true, false, false, egui::Key::R),
            (action::CLOSE_TAB, true, false, false, egui::Key::W),
            (action::NEXT_TAB, true, false, false, egui::Key::Tab),
            (action::PREV_TAB, true, true, false, egui::Key::Tab),
            (action::REOPEN_TAB, true, true, false, egui::Key::R),
            (
                action::TOGGLE_GRID,
                true,
                false,
                false,
                egui::Key::Backslash,
            ),
            (action::TOGGLE_COMMENT, true, false, false, egui::Key::Slash),
            (action::JUMP_BRACKET, true, false, false, egui::Key::M),
            (
                action::TOGGLE_FULLSCREEN,
                false,
                false,
                false,
                egui::Key::F11,
            ),
            (action::TOGGLE_ZEN, true, false, false, egui::Key::Period),
            (action::CYCLE_THEME, true, true, false, egui::Key::T),
            (action::TOGGLE_MINIMAP, true, true, false, egui::Key::M),
            (action::TOGGLE_MD_PREVIEW, true, true, false, egui::Key::V),
            (action::FOLD_ALL, true, true, false, egui::Key::OpenBracket),
            (
                action::EXPAND_ALL,
                true,
                true,
                false,
                egui::Key::CloseBracket,
            ),
            (action::INCREASE_FONT, true, false, false, egui::Key::Equals),
            (action::DECREASE_FONT, true, false, false, egui::Key::Minus),
            (action::RESET_FONT, true, false, false, egui::Key::Num0),
            (action::MOVE_LINE_UP, false, false, true, egui::Key::ArrowUp),
            (
                action::MOVE_LINE_DOWN,
                false,
                false,
                true,
                egui::Key::ArrowDown,
            ),
            (action::DUPLICATE_LINE, true, true, false, egui::Key::D),
            (action::JOIN_LINES, true, false, false, egui::Key::J),
            (action::TOGGLE_BOOKMARK, true, false, false, egui::Key::F2),
            (action::NEXT_BOOKMARK, false, false, false, egui::Key::F2),
            (action::PREV_BOOKMARK, false, true, false, egui::Key::F2),
        ];
        assert_eq!(
            expect.len(),
            action::ALL.len(),
            "every action must be pinned here"
        );
        for (name, cmd, shift, alt, key) in expect {
            let got = km
                .chord(name)
                .unwrap_or_else(|| panic!("default binding '{name}' must resolve to a chord"));
            assert_eq!(
                got,
                ResolvedChord {
                    cmd: *cmd,
                    shift: *shift,
                    alt: *alt,
                    key: *key
                },
                "default binding '{name}' resolved to the wrong chord"
            );
        }
    }

    #[test]
    fn chords_render_with_this_platforms_modifier_names() {
        // Both render paths — resolved chords and hard-coded fallback text — must
        // agree on what `mod` is called here, or the cheatsheet reads half
        // "Cmd+E" and half "Ctrl+C" on macOS.
        let km = Keymap::resolve(&Keybindings::default());
        let save = km.display_for(&[action::SAVE]).expect("save is bound");
        let zoom = km
            .display_for(&[
                action::INCREASE_FONT,
                action::DECREASE_FONT,
                action::RESET_FONT,
            ])
            .expect("font zoom is bound");
        if cfg!(target_os = "macos") {
            assert_eq!(save, "Cmd+S");
            assert_eq!(zoom, "Cmd+= / Cmd+- / Cmd+0");
            assert_eq!(platform_chord_text("Ctrl+C"), "Cmd+C");
            assert_eq!(platform_chord_text("Ctrl+Alt+X"), "Cmd+Option+X");
            assert_eq!(
                km.display_for(&[action::MOVE_LINE_UP]).unwrap(),
                "Option+Up"
            );
        } else {
            assert_eq!(save, "Ctrl+S");
            assert_eq!(zoom, "Ctrl+= / Ctrl+- / Ctrl+0");
            assert_eq!(platform_chord_text("Ctrl+C"), "Ctrl+C", "no-op off macOS");
            assert_eq!(platform_chord_text("Ctrl+Alt+X"), "Ctrl+Alt+X");
            assert_eq!(km.display_for(&[action::MOVE_LINE_UP]).unwrap(), "Alt+Up");
        }
        // Punctuation reads as the key on the keyboard, not egui's variant name.
        assert!(
            km.display_for(&[action::TOGGLE_GRID])
                .unwrap()
                .ends_with('\\'),
            "toggle_grid must render as the backslash key"
        );
        // An unbound action says so rather than naming a key that does nothing.
        let km = Keymap::resolve(&Keybindings {
            save: String::new(),
            ..Default::default()
        });
        assert_eq!(km.display_for(&[action::SAVE]).as_deref(), Some("unbound"));
        // No actions => the caller's static text is correct.
        assert_eq!(km.display_for(&[]), None);
    }

    #[test]
    fn key_from_token_accepts_both_spellings_and_rejects_junk() {
        assert_eq!(key_from_token("arrowup"), Some(egui::Key::ArrowUp));
        assert_eq!(key_from_token("up"), Some(egui::Key::ArrowUp));
        assert_eq!(key_from_token("num0"), Some(egui::Key::Num0));
        assert_eq!(key_from_token("0"), Some(egui::Key::Num0));
        assert_eq!(key_from_token("openbracket"), Some(egui::Key::OpenBracket));
        assert_eq!(key_from_token("f11"), Some(egui::Key::F11));
        assert_eq!(key_from_token("n"), Some(egui::Key::N));
        assert_eq!(key_from_token("nope"), None);
        assert_eq!(key_from_token(""), None);
    }

    #[test]
    fn an_unresolvable_binding_yields_no_chord() {
        // A combo naming a key egui does not have must resolve to None (the action
        // never fires) rather than falling back to some other key.
        let km = Keymap::resolve(&Keybindings {
            save: "mod+nosuchkey".into(),
            ..Default::default()
        });
        assert_eq!(km.chord(action::SAVE), None);
        // Unrelated bindings still resolve.
        assert!(km.chord(action::FIND).is_some());
    }
}
