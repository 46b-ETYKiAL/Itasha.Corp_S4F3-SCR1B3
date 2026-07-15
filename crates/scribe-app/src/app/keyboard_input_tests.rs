//! Coverage for the shortcut handlers in `keyboard_input.rs` that no test drove.
//!
//! `e2e.rs` covers the keymap CONTRACT (a rebound chord fires, exact modifier
//! matching, an unparseable binding disables its action). This file covers the
//! individual handlers behind those chords — the overlays each shortcut opens,
//! the F1/Esc/F3 hard-wired keys, and drag-drop file open — which were reachable
//! only through a live frame and so sat uncovered.
//!
//! Everything drives the real `frame_tick` loop via the shared [`Driver`], so
//! these assert what a user pressing the key would actually get.
#![allow(clippy::wildcard_imports)]
use super::e2e::Driver;
use super::*;

fn app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    ScribeApp::new_test(cfg)
}

/// An app + a settled driver (the first frame does layout/first-run work).
fn driven() -> (ScribeApp, Driver) {
    let mut app = app();
    let d = Driver::new();
    d.idle(&mut app);
    (app, d)
}

const CMD: egui::Modifiers = egui::Modifiers::COMMAND;

// ---- overlay-opening shortcuts ----

// NOTE on `focus_*`: those flags are ONE-SHOT latches — the modal's own render
// requests focus and immediately clears the flag (`frame_modals.rs`). So they
// are only observable between the shortcut and the render, which is what
// `Driver::shortcuts` (shortcut layer only) gives us; after a full `key` frame
// they are correctly back to false.

#[test]
fn goto_line_opens_focused_with_a_cleared_query() {
    let (mut app, d) = driven();
    app.goto_query = "stale".into();

    d.shortcuts(&mut app, egui::Key::G, CMD);

    assert!(app.goto_open, "Ctrl+G opens go-to-line");
    assert!(
        app.focus_goto,
        "and requests focus, or the user has to click the field first"
    );
    assert!(
        app.goto_query.is_empty(),
        "reopening must not inherit the last query"
    );
}

#[test]
fn goto_line_focus_latch_is_consumed_by_the_render() {
    // The latch must not stay set, or the modal re-grabs focus every frame and
    // the user can never move to another field.
    let (mut app, d) = driven();
    d.key(&mut app, egui::Key::G, CMD);
    assert!(app.goto_open, "still open after the frame");
    assert!(
        !app.focus_goto,
        "the focus request is one-shot: the render consumes it"
    );
}

#[test]
fn goto_symbol_opens_focused_and_resets_its_selection() {
    let (mut app, d) = driven();
    app.goto_symbol_selected = 5;
    app.goto_symbol_query = "stale".into();

    d.shortcuts(&mut app, egui::Key::O, CMD | egui::Modifiers::SHIFT);

    assert!(app.goto_symbol_open, "Ctrl+Shift+O opens go-to-symbol");
    assert!(app.focus_goto_symbol);
    assert!(app.goto_symbol_query.is_empty());
    assert_eq!(app.goto_symbol_selected, 0);
}

#[test]
fn goto_symbol_reopen_does_not_resteal_focus_while_already_open() {
    // The `if !self.goto_symbol_open { focus = true }` guard: re-pressing the
    // chord while the modal is open must not yank focus back from the field the
    // user is typing in.
    let (mut app, d) = driven();
    d.shortcuts(&mut app, egui::Key::O, CMD | egui::Modifiers::SHIFT);
    assert!(app.goto_symbol_open);
    app.focus_goto_symbol = false; // the modal has taken focus; it is settled

    d.shortcuts(&mut app, egui::Key::O, CMD | egui::Modifiers::SHIFT);

    assert!(app.goto_symbol_open, "still open");
    assert!(
        !app.focus_goto_symbol,
        "focus must not be re-stolen while the modal is already open"
    );
}

#[test]
fn recent_files_opens_with_the_selection_reset() {
    let (mut app, d) = driven();
    app.recent_selected = 3;

    d.key(&mut app, egui::Key::R, CMD);

    assert!(app.recent_open, "Ctrl+R opens recent files");
    assert_eq!(app.recent_selected, 0, "the highlight starts at the top");
}

#[test]
fn f1_toggles_the_cheatsheet_both_ways() {
    let (mut app, d) = driven();
    assert!(!app.cheatsheet_open);

    d.key(&mut app, egui::Key::F1, egui::Modifiers::NONE);
    assert!(app.cheatsheet_open, "F1 opens the cheatsheet");

    d.key(&mut app, egui::Key::F1, egui::Modifiers::NONE);
    assert!(!app.cheatsheet_open, "F1 again closes it — it is a toggle");
}

// ---- Esc: the shared overlay-close ----

#[test]
fn escape_closes_every_overlay() {
    // Esc is the universal "get me out". Every overlay must be in this list —
    // one that is missed is an overlay the user cannot dismiss with the key
    // they will reach for.
    let (mut app, d) = driven();
    app.find_open = true;
    app.palette_open = true;
    app.cheatsheet_open = true;
    app.goto_open = true;
    app.goto_symbol_open = true;
    app.recent_open = true;
    app.recent_folders_open = true;
    app.welcome_open = true;
    app.fuzzy_open = true;
    app.find_in_files_open = true;

    d.key(&mut app, egui::Key::Escape, egui::Modifiers::NONE);

    assert!(!app.find_open, "find bar");
    assert!(!app.palette_open, "command palette");
    assert!(!app.cheatsheet_open, "cheatsheet");
    assert!(!app.goto_open, "go to line");
    assert!(!app.goto_symbol_open, "go to symbol");
    assert!(!app.recent_open, "recent files");
    assert!(!app.recent_folders_open, "recent folders");
    assert!(!app.welcome_open, "welcome");
    assert!(!app.fuzzy_open, "fuzzy finder");
    assert!(!app.find_in_files_open, "find-in-files results (PA-02)");
}

#[test]
fn escape_leaves_zen_mode_first_and_keeps_overlays_open() {
    // Ordering matters: Esc in zen mode restores the chrome FIRST, so the user
    // can see where they are. It must not also close their overlays in the same
    // press.
    let (mut app, d) = driven();
    app.zen_mode = true;
    app.find_open = true;

    d.key(&mut app, egui::Key::Escape, egui::Modifiers::NONE);
    assert!(!app.zen_mode, "the first Esc exits zen mode");
    assert!(
        app.find_open,
        "and must NOT also close the overlay — that is the second press"
    );

    d.key(&mut app, egui::Key::Escape, egui::Modifiers::NONE);
    assert!(!app.find_open, "the second Esc closes the overlay");
}

// ---- F3 find navigation (hard-wired, not rebindable) ----

#[test]
fn f3_only_navigates_while_the_find_bar_is_open() {
    // F3 is find-bar navigation, so it must be inert when the bar is closed
    // rather than a global action.
    let (mut app, d) = driven();

    app.find_open = false;
    let (_, nav) = d.shortcuts(&mut app, egui::Key::F3, egui::Modifiers::NONE);
    assert_eq!(nav, None, "F3 with the find bar closed does nothing");

    app.find_open = true;
    let (_, nav) = d.shortcuts(&mut app, egui::Key::F3, egui::Modifiers::NONE);
    assert_eq!(nav, Some(true), "F3 walks FORWARD through matches");

    let (_, nav) = d.shortcuts(&mut app, egui::Key::F3, egui::Modifiers::SHIFT);
    assert_eq!(nav, Some(false), "Shift+F3 walks BACKWARD");
}

// ---- F-011 drag and drop ----

#[test]
fn dropping_files_opens_each_one() {
    let (mut app, d) = driven();
    let dir = std::env::temp_dir().join(format!("scr1b3-kbd-drop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("dropped-a.md");
    let b = dir.join("dropped-b.md");
    std::fs::write(&a, "alpha").unwrap();
    std::fs::write(&b, "bravo").unwrap();
    let before = app.tabs.len();

    d.drop_files(&mut app, &[a, b]);

    assert_eq!(
        app.tabs.len(),
        before + 2,
        "both dropped files open as tabs"
    );
    let texts: Vec<_> = app.tabs.iter().map(|t| t.text.as_str()).collect();
    assert!(texts.contains(&"alpha") && texts.contains(&"bravo"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_drop_with_no_path_is_ignored() {
    // A drop can carry bytes with no path (dragged from a browser). It must be
    // skipped, not unwrapped.
    let (mut app, d) = driven();
    let before = app.tabs.len();

    d.frame_with(
        &mut app,
        egui::Modifiers::NONE,
        vec![],
        vec![egui::DroppedFile::default()],
    );

    assert_eq!(app.tabs.len(), before, "a pathless drop opens nothing");
}

// ---- tab cycling is suppressed by the completion popup ----

#[test]
fn tab_cycling_yields_to_the_completion_popup() {
    // The popup consumes Tab to accept a candidate; cycling tabs at the same
    // time would both accept the completion and switch away from the buffer.
    let (mut app, d) = driven();
    d.key(&mut app, egui::Key::N, CMD); // a second tab to cycle to
    let active = app.active;

    app.completion = Some(Completion {
        prefix_start: 0,
        items: vec!["candidate".into()],
        selected: 0,
    });
    d.key(&mut app, egui::Key::Tab, CMD);
    assert_eq!(
        app.active, active,
        "Ctrl+Tab must not switch tabs while the completion popup is open"
    );

    app.completion = None;
    d.key(&mut app, egui::Key::Tab, CMD);
    assert_ne!(
        app.active, active,
        "with the popup closed, Ctrl+Tab cycles again"
    );
}

// ---- the KEYBOARD half of the toggles/latches ----
//
// `execute_builtin_tests.rs` asserts zen, md-preview and focus-find through the
// command PALETTE. That is a different code path from the shortcut handler, and
// mutation testing proved the split matters: four `!` deletions in
// `keyboard_input.rs` survived the whole suite because the palette tests kept
// passing. It is the same shape as the fold-all bug this PR fixes — palette
// worked, shortcut was broken, nothing noticed. So: press the KEYS here.

#[test]
fn ctrl_f_on_a_closed_find_bar_focuses_the_field() {
    // If the latch does not fire, the bar opens but the caret stays in the
    // document — the user's next keystrokes are typed into their file instead of
    // the search box. `find_open` is true either way, so asserting only that is
    // blind to it.
    let (mut app, d) = driven();
    assert!(!app.find_open, "fixture starts closed");

    d.shortcuts(&mut app, egui::Key::F, CMD);

    assert!(app.find_open, "Ctrl+F opens the find bar");
    assert!(
        app.focus_find,
        "and focuses the field — opening it without focus sends typing to the document"
    );
}

#[test]
fn ctrl_f_on_an_already_open_find_bar_does_not_re_grab_focus() {
    // The other side of the same latch: focus is requested only on the OPEN
    // transition, so a second press cannot yank the caret back out of wherever
    // the user has put it.
    let (mut app, d) = driven();
    app.find_open = true;
    app.focus_find = false;

    d.shortcuts(&mut app, egui::Key::F, CMD);

    assert!(app.find_open, "still open");
    assert!(
        !app.focus_find,
        "focus is a first-open latch, not a re-grab on every press"
    );
}

#[test]
fn ctrl_shift_f_on_a_closed_find_in_files_bar_focuses_the_field() {
    let (mut app, d) = driven();
    assert!(!app.find_in_files_open, "fixture starts closed");

    d.shortcuts(&mut app, egui::Key::F, CMD | egui::Modifiers::SHIFT);

    assert!(app.find_in_files_open, "Ctrl+Shift+F opens find-in-files");
    assert!(app.focus_find_in_files, "and focuses its query field");
}

#[test]
fn ctrl_shift_f_on_an_already_open_find_in_files_bar_does_not_re_grab_focus() {
    let (mut app, d) = driven();
    app.find_in_files_open = true;
    app.focus_find_in_files = false;

    d.shortcuts(&mut app, egui::Key::F, CMD | egui::Modifiers::SHIFT);

    assert!(app.find_in_files_open, "still open");
    assert!(
        !app.focus_find_in_files,
        "focus is a first-open latch, not a re-grab on every press"
    );
}

#[test]
fn the_zen_shortcut_toggles_both_ways_and_closes_the_find_bars() {
    // Covered through the palette, never through the key. A dropped `!` here
    // makes Ctrl+. a dead key while the palette command keeps working.
    let (mut app, d) = driven();
    app.find_open = true;
    app.find_in_files_open = true;
    assert!(!app.zen_mode, "fixture starts out of zen");

    d.shortcuts(&mut app, egui::Key::Period, CMD);
    assert!(app.zen_mode, "Ctrl+. enters zen mode");
    assert!(!app.find_open, "entering zen closes the find bar");
    assert!(
        !app.find_in_files_open,
        "entering zen closes find-in-files too — nothing but the editor remains"
    );

    d.shortcuts(&mut app, egui::Key::Period, CMD);
    assert!(app.zen_mode.eq(&false), "and the same key leaves zen mode");
}

#[test]
fn the_markdown_preview_shortcut_toggles_both_ways() {
    let (mut app, d) = driven();
    assert!(!app.md_preview_open, "fixture starts with the pane closed");

    d.shortcuts(&mut app, egui::Key::V, CMD | egui::Modifiers::SHIFT);
    assert!(app.md_preview_open, "Ctrl+Shift+V opens the preview pane");

    d.shortcuts(&mut app, egui::Key::V, CMD | egui::Modifiers::SHIFT);
    assert!(!app.md_preview_open, "and the same key closes it again");
}

// ---- font zoom ----
//
// The Ctrl+scroll half of this shipped DEAD and no test noticed, because every
// test drove the keys. egui's `zoom_modifier` defaults to COMMAND, so a wheel
// event carrying Ctrl — which is exactly the gesture, and egui-winit always
// attaches the live modifiers — is folded into egui's own zoom accumulator and
// `smooth_scroll_delta` is left at ZERO. The old handler read that delta under
// `if cmd`, so `dy` was always 0.0 and the branch could not fire. Nothing in
// egui or eframe consumes `zoom_factor_delta` either, so the gesture did
// nothing at all.

/// One frame carrying a wheel gesture. `ctrl` is attached to the wheel event
/// itself, which is what egui-winit does and what makes egui treat it as a zoom.
fn wheel(app: &mut ScribeApp, dy: f32, ctrl: bool) -> Option<i8> {
    let ctx = egui::Context::default();
    let mods = if ctrl {
        egui::Modifiers::COMMAND
    } else {
        egui::Modifiers::NONE
    };
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1200.0, 800.0),
        )),
        modifiers: mods,
        events: vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, dy),
            modifiers: mods,
            phase: egui::TouchPhase::Move,
        }],
        ..Default::default()
    };
    let mut act = Pending::default();
    let mut nav = None;
    let _ = ctx.run(input, |ctx| {
        app.handle_keyboard_shortcuts(ctx, &mut act, &mut nav);
    });
    act.font_zoom
}

#[test]
fn ctrl_scroll_up_zooms_the_font_in() {
    let mut app = app();
    assert_eq!(
        wheel(&mut app, 4.0, true),
        Some(1),
        "Ctrl+scroll up must zoom in — this is the gesture that shipped dead"
    );
}

#[test]
fn ctrl_scroll_down_zooms_the_font_out() {
    let mut app = app();
    assert_eq!(
        wheel(&mut app, -4.0, true),
        Some(-1),
        "Ctrl+scroll down zooms out"
    );
}

#[test]
fn scrolling_without_ctrl_does_not_resize_the_font() {
    // Plain scrolling is scrolling. egui only reports a zoom gesture when the
    // wheel carries the zoom modifier, so this must stay silent in both
    // directions or every scroll would rescale the document.
    let mut app = app();
    assert_eq!(
        wheel(&mut app, 4.0, false),
        None,
        "plain scroll up is not a zoom"
    );
    assert_eq!(
        wheel(&mut app, -4.0, false),
        None,
        "plain scroll down is not a zoom"
    );
}

#[test]
fn a_zoom_gesture_inside_the_deadzone_is_ignored() {
    // Pins BOTH sides of ZOOM_DEADZONE. A threshold whose fixtures all sit far
    // out on one side reads as "covered" while nothing distinguishes `>` from
    // `>=` — so straddle it: at egui's default scroll_zoom_speed (1/200),
    // exp(0.4/200) = 1.0020 is inside and exp(0.6/200) = 1.0030 is outside.
    let mut app = app();
    assert_eq!(
        wheel(&mut app, 0.4, true),
        None,
        "jitter must not resize the font"
    );
    assert_eq!(wheel(&mut app, -0.4, true), None, "…in either direction");
    assert_eq!(
        wheel(&mut app, 0.6, true),
        Some(1),
        "but a real nudge just past the deadzone does zoom"
    );
    assert_eq!(
        wheel(&mut app, -0.6, true),
        Some(-1),
        "…in either direction"
    );
}

#[test]
fn the_font_zoom_chords_step_in_the_direction_they_name() {
    // `Some(-1)` vs `Some(1)` is one character, and the difference is Ctrl+Minus
    // making the text BIGGER. Assert the sign, not just that something happened.
    let (mut app, d) = driven();
    let (act, _) = d.shortcuts(&mut app, egui::Key::Equals, CMD);
    assert_eq!(act.font_zoom, Some(1), "Ctrl+= zooms IN");

    let (act, _) = d.shortcuts(&mut app, egui::Key::Minus, CMD);
    assert_eq!(act.font_zoom, Some(-1), "Ctrl+- zooms OUT");

    let (act, _) = d.shortcuts(&mut app, egui::Key::Num0, CMD);
    assert_eq!(act.font_zoom, Some(0), "Ctrl+0 resets to the default size");
}

#[test]
fn ctrl_shift_p_on_a_closed_palette_focuses_it() {
    let (mut app, d) = driven();
    assert!(!app.palette_open, "fixture starts closed");

    d.shortcuts(&mut app, egui::Key::P, CMD | egui::Modifiers::SHIFT);

    assert!(app.palette_open, "Ctrl+Shift+P opens the palette");
    assert!(
        app.focus_palette,
        "and focuses the query field — otherwise the user types into their document"
    );
}

#[test]
fn ctrl_shift_p_on_an_open_palette_does_not_re_grab_focus() {
    let (mut app, d) = driven();
    app.palette_open = true;
    app.focus_palette = false;

    d.shortcuts(&mut app, egui::Key::P, CMD | egui::Modifiers::SHIFT);

    assert!(app.palette_open, "still open");
    assert!(
        !app.focus_palette,
        "focus is a first-open latch, not a re-grab on every press"
    );
}
