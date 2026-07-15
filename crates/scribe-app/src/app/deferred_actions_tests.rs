//! Coverage for `deferred_actions.rs` — the dispatcher that applies everything
//! a frame decided to do once the UI borrows are released.
//!
//! This is where nearly every keyboard shortcut and palette command actually
//! lands, so an unhandled or mis-wired branch here means a shortcut that quietly
//! does nothing. Each handler is small, but there are ~30 of them and the whole
//! set was reachable only through a live frame — hence 52% coverage on a file
//! with no logic that needs a renderer.
//!
//! Two branches are deliberately never exercised: `act.open` and
//! `act.open_folder` call `rfd::FileDialog` and block on a human. Per ADR-0007
//! they are an exclusion, not something to fake.
#![allow(clippy::wildcard_imports)]
use super::deferred_actions::DeferredFlags;
use super::*;

fn app() -> (ScribeApp, egui::Context) {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    (ScribeApp::new_test(cfg), egui::Context::default())
}

/// All flags off — the neutral baseline each test turns exactly one thing on in.
fn flags() -> DeferredFlags {
    DeferredFlags {
        run_cmd: None,
        run_builtin: None,
        save_cfg: false,
        open_from_tree: None,
        close_tree: false,
        start_lsp: false,
        want_open_cfg: false,
        want_restore_cfg: false,
        want_dismiss_cfg: false,
    }
}

/// Apply `act` with no frame-local flags set.
fn apply(app: &mut ScribeApp, ctx: &egui::Context, act: &mut Pending) {
    app.apply_deferred_actions(ctx, act, flags());
}

/// Apply only frame-local `flags` with an empty action set.
fn apply_flags(app: &mut ScribeApp, ctx: &egui::Context, flags: DeferredFlags) {
    app.apply_deferred_actions(ctx, &mut Pending::default(), flags);
}

// ---- the empty case ----

#[test]
fn an_empty_action_set_changes_nothing() {
    // The overwhelmingly common frame: nothing was requested, so nothing must
    // happen — no tab churn, no config write, no status text.
    let (mut app, ctx) = app();
    let before_tabs = app.tabs.len();
    let before_active = app.active;
    let before_status = app.status.clone();
    let before_cfg = app.config.clone();

    apply(&mut app, &ctx, &mut Pending::default());

    assert_eq!(app.tabs.len(), before_tabs);
    assert_eq!(app.active, before_active);
    assert_eq!(app.status, before_status);
    assert_eq!(
        app.config, before_cfg,
        "an idle frame must not touch config"
    );
}

// ---- tabs ----

#[test]
fn new_opens_a_tab_and_close_active_closes_one() {
    let (mut app, ctx) = app();
    let before = app.tabs.len();

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            new: true,
            ..Default::default()
        },
    );
    assert_eq!(app.tabs.len(), before + 1, "Ctrl+N adds a tab");

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            close_active_tab: true,
            ..Default::default()
        },
    );
    assert_eq!(app.tabs.len(), before, "Ctrl+W closes the active tab");
}

#[test]
fn cycle_tab_next_and_prev_wrap_in_both_directions() {
    let (mut app, ctx) = app();
    // Three tabs total (the starting scratch tab + two).
    apply(
        &mut app,
        &ctx,
        &mut Pending {
            new: true,
            ..Default::default()
        },
    );
    apply(
        &mut app,
        &ctx,
        &mut Pending {
            new: true,
            ..Default::default()
        },
    );
    let n = app.tabs.len();
    assert!(n >= 3);
    app.active = n - 1;

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            cycle_tab_next: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.active, 0,
        "Ctrl+Tab wraps past the last tab to the first"
    );

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            cycle_tab_prev: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.active,
        n - 1,
        "Ctrl+Shift+Tab wraps back past the first to the last"
    );
}

#[test]
fn cycle_tab_with_no_tabs_does_not_panic() {
    // The `!self.tabs.is_empty()` guard: cycling with everything closed must be
    // inert rather than dividing by zero.
    let (mut app, ctx) = app();
    app.tabs.clear();
    app.active = 0;
    apply(
        &mut app,
        &ctx,
        &mut Pending {
            cycle_tab_next: true,
            cycle_tab_prev: true,
            ..Default::default()
        },
    );
    assert!(app.tabs.is_empty());
}

#[test]
fn files_to_open_are_all_opened_and_the_queue_is_drained() {
    let (mut app, ctx) = app();
    let dir = std::env::temp_dir().join(format!("scr1b3-deferred-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.md");
    let b = dir.join("b.md");
    std::fs::write(&a, "aaa").unwrap();
    std::fs::write(&b, "bbb").unwrap();
    let before = app.tabs.len();

    let mut act = Pending {
        files_to_open: vec![a, b],
        ..Default::default()
    };
    apply(&mut app, &ctx, &mut act);

    assert_eq!(app.tabs.len(), before + 2, "both queued files open");
    assert!(
        act.files_to_open.is_empty(),
        "the queue MUST be drained, or every later frame reopens the same files"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_from_tree_opens_the_clicked_file() {
    let (mut app, ctx) = app();
    let dir = std::env::temp_dir().join(format!("scr1b3-deferred-tree-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("tree.md");
    std::fs::write(&f, "from the tree").unwrap();

    apply_flags(
        &mut app,
        &ctx,
        DeferredFlags {
            open_from_tree: Some(f),
            ..flags()
        },
    );

    assert_eq!(app.tabs[app.active].text, "from the tree");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn close_tree_clears_the_file_tree_root() {
    let (mut app, ctx) = app();
    app.file_tree_root = Some(PathBuf::from("."));
    apply_flags(
        &mut app,
        &ctx,
        DeferredFlags {
            close_tree: true,
            ..flags()
        },
    );
    assert!(app.file_tree_root.is_none());
}

// ---- toggles that persist ----

#[test]
fn toggle_grid_flips_the_setting_and_persists_it() {
    let (mut app, ctx) = app();
    let before = app.config.editor.grid_enabled;

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            toggle_grid: true,
            ..Default::default()
        },
    );

    assert_eq!(app.config.editor.grid_enabled, !before, "the toggle flips");
    assert!(app.status.contains("multi-note grid"), "and is reported");
    // A toggle the user has to redo on every launch is a bug: it must be saved.
    let saved = app.config_dir.as_ref().unwrap().join("scr1b3.toml");
    assert!(saved.exists(), "the flipped setting must be persisted");
}

#[test]
fn toggle_minimap_flips_the_setting_and_persists_it() {
    let (mut app, ctx) = app();
    let before = app.config.editor.show_minimap;

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            toggle_minimap: true,
            ..Default::default()
        },
    );

    assert_eq!(app.config.editor.show_minimap, !before);
    assert!(app.status.contains("minimap"));
    assert!(app
        .config_dir
        .as_ref()
        .unwrap()
        .join("scr1b3.toml")
        .exists());
}

#[test]
fn cycle_theme_advances_to_the_next_builtin_and_persists_it() {
    let (mut app, ctx) = app();
    let names = scribe_core::theme::Theme::builtin_names();
    let before = app.config.appearance.theme.clone();

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            cycle_theme: true,
            ..Default::default()
        },
    );

    assert_ne!(app.config.appearance.theme, before, "the theme advances");
    assert!(
        names.contains(&app.config.appearance.theme.as_str()),
        "and lands on a real builtin, got {:?}",
        app.config.appearance.theme
    );
    assert!(app.status.contains("theme:"));
}

#[test]
fn cycle_theme_wraps_from_the_last_builtin_back_to_the_first() {
    let (mut app, ctx) = app();
    let names = scribe_core::theme::Theme::builtin_names();
    app.config.appearance.theme = (*names.last().unwrap()).to_string();

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            cycle_theme: true,
            ..Default::default()
        },
    );

    assert_eq!(
        app.config.appearance.theme, names[0],
        "cycling past the last theme wraps to the first"
    );
}

#[test]
fn cycle_theme_from_an_unknown_theme_starts_at_the_second() {
    // An unknown theme name (hand-edited config) resolves to index 0, so the
    // next is index 1 — it must not panic or stall on the unknown value.
    let (mut app, ctx) = app();
    let names = scribe_core::theme::Theme::builtin_names();
    app.config.appearance.theme = "not-a-real-theme".into();

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            cycle_theme: true,
            ..Default::default()
        },
    );

    assert_eq!(app.config.appearance.theme, names[1 % names.len()]);
}

// ---- font zoom ----

#[test]
fn font_zoom_steps_up_and_down_and_zero_resets_to_the_default() {
    let (mut app, ctx) = app();
    let def = Config::default().fonts.editor_size;

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            font_zoom: Some(2),
            ..Default::default()
        },
    );
    assert_eq!(app.config.fonts.editor_size, def + 2.0, "Ctrl+= grows");
    assert!(app.status.contains("font size:"));

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            font_zoom: Some(-1),
            ..Default::default()
        },
    );
    assert_eq!(app.config.fonts.editor_size, def + 1.0, "Ctrl+- shrinks");

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            font_zoom: Some(0),
            ..Default::default()
        },
    );
    assert_eq!(app.config.fonts.editor_size, def, "Ctrl+0 resets");
}

#[test]
fn font_zoom_clamps_to_a_legible_range() {
    // Unclamped this reaches 0/negative font sizes — an unreadable, unrecoverable
    // window (you cannot see the menu to fix it).
    let (mut app, ctx) = app();

    for _ in 0..50 {
        apply(
            &mut app,
            &ctx,
            &mut Pending {
                font_zoom: Some(-5),
                ..Default::default()
            },
        );
    }
    assert_eq!(app.config.fonts.editor_size, 8.0, "clamped at the floor");

    for _ in 0..50 {
        apply(
            &mut app,
            &ctx,
            &mut Pending {
                font_zoom: Some(5),
                ..Default::default()
            },
        );
    }
    assert_eq!(app.config.fonts.editor_size, 32.0, "clamped at the ceiling");
}

// ---- find / replace ----

#[test]
fn open_replace_opens_the_find_bar_focused_on_replace() {
    let (mut app, ctx) = app();
    apply(
        &mut app,
        &ctx,
        &mut Pending {
            open_replace: true,
            ..Default::default()
        },
    );
    assert!(app.find_open, "Ctrl+H reuses the find bar");
    assert!(app.focus_replace, "with focus in the replace field");
}

// ---- folding ----

/// Open `name` holding `text` so the tab carries a real language hint (which is
/// what the fold-region choice keys off).
fn app_with_file(name: &str, text: &str) -> (ScribeApp, egui::Context) {
    let (mut app, ctx) = app();
    let dir = std::env::temp_dir().join(format!(
        "scr1b3-deferred-fold-{}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    app.open_path(path);
    (app, ctx)
}

#[test]
fn fold_all_folds_code_by_braces_and_switches_to_fold_view() {
    let (mut app, ctx) = app_with_file("f.rs", "fn a() {\n    body\n}\n");

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            fold_all: true,
            ..Default::default()
        },
    );

    assert_eq!(app.folds.len(), 1, "the one brace region folds");
    assert!(app.fold_view, "and fold view switches on so it is visible");
    assert!(app.status.contains("folded 1 region"));
}

#[test]
fn fold_all_folds_a_note_by_heading_not_by_braces() {
    // P2-4 regression: this handler used the brace-only `fold_regions`, so
    // Ctrl+Shift+[ in a markdown note found ZERO regions — it switched the user
    // into fold view with nothing folded and reported "folded 0 region(s)" —
    // while the palette's Fold All folded the same buffer by heading.
    let (mut app, ctx) = app_with_file("f.md", "# One\ntext\n# Two\nmore\n");

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            fold_all: true,
            ..Default::default()
        },
    );

    assert_eq!(
        app.folds.len(),
        2,
        "a note folds by heading SECTION (2 headings => 2 regions), got {:?}",
        app.folds
    );
    assert!(app.fold_view);
    assert!(app.status.contains("folded 2 region"));
}

#[test]
fn fold_all_shortcut_and_palette_command_agree() {
    // The same feature behind two doors: the Ctrl+Shift+[ shortcut (this
    // dispatcher) and the palette's BuiltinCommand::FoldAll. They must fold the
    // same buffer identically — they disagreed on notes until the fix above.
    for (name, body) in [
        ("p.md", "# One\ntext\n# Two\nmore\n"),
        ("p.rs", "fn a() {\n    body\n}\n"),
    ] {
        let (mut via_key, ctx) = app_with_file(name, body);
        apply(
            &mut via_key,
            &ctx,
            &mut Pending {
                fold_all: true,
                ..Default::default()
            },
        );

        let (mut via_palette, _) = app_with_file(name, body);
        via_palette.execute_builtin(BuiltinCommand::FoldAll);

        assert_eq!(
            via_key.folds, via_palette.folds,
            "{name}: the shortcut and the palette must fold identically"
        );
        assert_eq!(via_key.status, via_palette.status, "{name}: same report");
    }
}

#[test]
fn expand_all_clears_every_fold() {
    let (mut app, ctx) = app();
    app.folds.insert(0);
    app.folds.insert(2);

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            expand_all: true,
            ..Default::default()
        },
    );

    assert!(app.folds.is_empty());
    assert_eq!(app.status, "expanded all");
}

#[test]
fn fold_all_with_no_tabs_does_not_panic() {
    // The `self.active < self.tabs.len()` guard.
    let (mut app, ctx) = app();
    app.tabs.clear();
    apply(
        &mut app,
        &ctx,
        &mut Pending {
            fold_all: true,
            ..Default::default()
        },
    );
    assert!(app.folds.is_empty());
}

// ---- fuzzy finder ----

#[test]
fn open_fuzzy_builds_the_index_once_and_resets_the_query() {
    let (mut app, ctx) = app();
    let dir = std::env::temp_dir().join(format!("scr1b3-deferred-fuzzy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("indexed.md"), "x").unwrap();
    app.file_tree_root = Some(dir.clone());
    app.fuzzy_query = "stale query".into();
    app.fuzzy_selected = 7;

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            open_fuzzy: true,
            ..Default::default()
        },
    );

    assert!(app.fuzzy_open);
    assert!(app.focus_fuzzy);
    assert!(
        app.fuzzy_query.is_empty(),
        "reopening must not inherit the last query"
    );
    assert_eq!(app.fuzzy_selected, 0, "nor the last selection");
    assert!(
        !app.fuzzy_index.is_empty(),
        "the index is lazily built on first open"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- config banner (F-038) ----

#[test]
fn restore_cfg_resets_to_defaults_persists_and_clears_the_banner() {
    let (mut app, ctx) = app();
    app.config.editor.show_minimap = !Config::default().editor.show_minimap;
    app.config_error_banner = Some("bad config".into());

    apply_flags(
        &mut app,
        &ctx,
        DeferredFlags {
            want_restore_cfg: true,
            ..flags()
        },
    );

    assert_eq!(app.config, Config::default(), "everything back to defaults");
    assert!(
        app.config_error_banner.is_none(),
        "the banner that offered the fix must clear once it is applied"
    );
    assert_eq!(app.status, "config restored to defaults");
    assert!(
        app.config_dir
            .as_ref()
            .unwrap()
            .join("scr1b3.toml")
            .exists(),
        "the restored config must be written, not just held in memory"
    );
}

#[test]
fn dismiss_cfg_clears_the_banner_without_touching_the_config() {
    let (mut app, ctx) = app();
    app.config.editor.show_minimap = !Config::default().editor.show_minimap;
    let keep = app.config.clone();
    app.config_error_banner = Some("bad config".into());

    apply_flags(
        &mut app,
        &ctx,
        DeferredFlags {
            want_dismiss_cfg: true,
            ..flags()
        },
    );

    assert!(app.config_error_banner.is_none());
    assert_eq!(
        app.config, keep,
        "dismiss only hides the banner — it must not reset the user's settings"
    );
}

// ---- save_cfg ----

#[test]
fn save_cfg_writes_the_current_config() {
    let (mut app, ctx) = app();
    app.config.editor.show_minimap = !Config::default().editor.show_minimap;

    apply_flags(
        &mut app,
        &ctx,
        DeferredFlags {
            save_cfg: true,
            ..flags()
        },
    );

    let path = app.config_dir.as_ref().unwrap().join("scr1b3.toml");
    let written = std::fs::read_to_string(&path).expect("config written");
    assert!(
        written.contains("show_minimap"),
        "the live config is what gets written"
    );
}

#[test]
fn cycle_tab_prev_steps_back_one_from_a_middle_tab() {
    // `cycle_tab_next_and_prev_wrap_in_both_directions` starts prev from tab 0,
    // so it only ever takes the WRAP branch (`tabs.len() - 1`) — the ordinary
    // `self.active - 1` step never ran, and `- 1` could be `+ 1` with the suite
    // still green. That mutant is Ctrl+Shift+Tab moving FORWARD.
    let (mut app, ctx) = app();
    for _ in 0..2 {
        apply(
            &mut app,
            &ctx,
            &mut Pending {
                new: true,
                ..Default::default()
            },
        );
    }
    assert!(app.tabs.len() >= 3);
    app.active = 2;

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            cycle_tab_prev: true,
            ..Default::default()
        },
    );

    assert_eq!(
        app.active, 1,
        "Ctrl+Shift+Tab from a middle tab must step BACK one, not forward"
    );
}

#[test]
fn cycle_tab_next_steps_forward_one_from_a_middle_tab() {
    // The mirror case: `(active + 1) % len` from a middle tab, so the non-wrap
    // arm of next is pinned too.
    let (mut app, ctx) = app();
    for _ in 0..2 {
        apply(
            &mut app,
            &ctx,
            &mut Pending {
                new: true,
                ..Default::default()
            },
        );
    }
    app.active = 0;

    apply(
        &mut app,
        &ctx,
        &mut Pending {
            cycle_tab_next: true,
            ..Default::default()
        },
    );

    assert_eq!(app.active, 1, "Ctrl+Tab from tab 0 must step FORWARD one");
}

// ---- toggle_fullscreen ----
//
// The rest of the codebase treats ViewportCommands as "not headless-assertable"
// (see the caption-button tests in e2e_overlays.rs, which only assert no-panic).
// That is truer of Maximize/Minimize, which carry no argument to check — but
// Fullscreen carries the TARGET STATE, and `ctx.run` hands back every command
// it emitted in `FullOutput::viewport_output`. So the toggle IS assertable, and
// it needs to be: mutation testing showed the `!` could be deleted — making
// "toggle fullscreen" re-assert the state it is already in, i.e. do nothing —
// with the whole suite still green.

/// Run one frame that applies `act`, returning the viewport commands emitted.
fn viewport_cmds(app: &mut ScribeApp, act: &mut Pending) -> Vec<egui::ViewportCommand> {
    let ctx = egui::Context::default();
    let out = ctx.run(egui::RawInput::default(), |ctx| {
        app.apply_deferred_actions(ctx, act, flags());
    });
    out.viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

#[test]
fn toggle_fullscreen_asks_for_the_opposite_of_the_current_state() {
    // Starting windowed (the headless default), the toggle must request TRUE.
    // Without the `!` it would request `false` — the state it is already in —
    // and F11 would silently do nothing.
    let (mut app, _) = app();
    let cmds = viewport_cmds(
        &mut app,
        &mut Pending {
            toggle_fullscreen: true,
            ..Default::default()
        },
    );
    assert!(
        cmds.contains(&egui::ViewportCommand::Fullscreen(true)),
        "windowed => the toggle must ask to ENTER fullscreen, got: {cmds:?}"
    );
}

#[test]
fn no_fullscreen_request_when_the_action_was_not_asked_for() {
    // The negative control: without the flag, no Fullscreen command at all. If
    // this fired regardless, the test above would pass for the wrong reason.
    let (mut app, _) = app();
    let cmds = viewport_cmds(&mut app, &mut Pending::default());
    assert!(
        !cmds
            .iter()
            .any(|c| matches!(c, egui::ViewportCommand::Fullscreen(_))),
        "an idle frame must not touch fullscreen, got: {cmds:?}"
    );
}

// ---- bookmark navigation (F2 / Shift+F2) ----
//
// `pick_bookmark` is thoroughly unit-tested as a pure function, both directions
// including wrap. What was NOT tested is the WIRING: that `prev_bookmark` passes
// -1. Mutation testing deleted the `-` in `navigate_bookmark(-1)` and every test
// stayed green — Shift+F2 would jump FORWARD, i.e. "previous bookmark" and "next
// bookmark" become the same key. A correct helper called with the wrong argument
// is still a broken feature.

/// Put the caret on `line0` for `cursor_line0()`, which reads the 1-based pair.
fn set_caret_line0(app: &mut ScribeApp, line0: usize) {
    app.last_cursor_line_col = Some((line0 + 1, 1));
}

#[test]
fn prev_bookmark_goes_backward_and_next_goes_forward() {
    let (mut app, ctx) = app();
    let active = app.active;
    app.tabs[active].bookmarks = [2usize, 5, 9].into_iter().collect();

    set_caret_line0(&mut app, 5);
    apply(
        &mut app,
        &ctx,
        &mut Pending {
            prev_bookmark: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.status, "go to line 3",
        "Shift+F2 from line 6 must land on the bookmark ABOVE it (0-based 2)"
    );

    set_caret_line0(&mut app, 5);
    apply(
        &mut app,
        &ctx,
        &mut Pending {
            next_bookmark: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.status, "go to line 10",
        "F2 from line 6 must land on the bookmark BELOW it (0-based 9)"
    );
}

#[test]
fn bookmark_navigation_says_so_when_there_are_none() {
    // The empty-set arm: a plain message rather than a silent no-op, so F2 on a
    // buffer with no bookmarks explains itself.
    let (mut app, ctx) = app();
    assert!(app.tabs[app.active].bookmarks.is_empty());
    apply(
        &mut app,
        &ctx,
        &mut Pending {
            next_bookmark: true,
            ..Default::default()
        },
    );
    assert_eq!(app.status, "no bookmarks in this buffer");
}

// ---- want_open_cfg (F-038 "open the config file") ----
//
// Untested until mutation testing flipped `if !p.exists()` to `if p.exists()`
// with the suite green. That mutant is destructive: on a machine that HAS a
// config it rewrites the file from the in-memory struct — silently discarding
// the user's comments and formatting — and on a cold install it seeds nothing,
// so the button does nothing at all.
//
// These read the GLOBAL `Config::config_file_path()` (not the instance dir), so
// the env redirect has to be exclusive: cargo runs tests in parallel.
static CFG_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_config_dir<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
    let _guard = CFG_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var_os("SCR1B3_CONFIG_DIR");
    std::env::set_var("SCR1B3_CONFIG_DIR", dir);
    let out = body();
    match prev {
        Some(v) => std::env::set_var("SCR1B3_CONFIG_DIR", v),
        None => std::env::remove_var("SCR1B3_CONFIG_DIR"),
    }
    out
}

fn cfg_temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "scr1b3-open-cfg/{}-{}-{}",
        tag,
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn open_cfg_never_overwrites_an_existing_config_file() {
    // THE destructive case. The user's file is theirs — hand-written comments and
    // all. "Open" must open it, byte for byte, never rewrite it from the struct.
    let dir = cfg_temp_dir("existing");
    let path = dir.join("scr1b3.toml");
    let hand_written = "# my notes, keep me\n[editor]\nshow_minimap = true\n";
    std::fs::write(&path, hand_written).unwrap();
    let (mut app, ctx) = app();

    with_config_dir(&dir, || {
        apply_flags(
            &mut app,
            &ctx,
            DeferredFlags {
                want_open_cfg: true,
                ..flags()
            },
        );
    });

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        hand_written,
        "an existing config must be opened UNTOUCHED — never rewritten from the \
         in-memory struct, which would eat the user's comments"
    );
    assert!(
        app.tabs.iter().any(|t| t.text == hand_written),
        "and it must actually open in a tab"
    );
}

#[test]
fn open_cfg_seeds_defaults_on_a_cold_install() {
    // No file yet: seed it so the user has something to edit, then open it.
    let dir = cfg_temp_dir("cold");
    let path = dir.join("scr1b3.toml");
    assert!(!path.exists(), "fixture starts with no config file");
    let (mut app, ctx) = app();
    let before = app.tabs.len();

    with_config_dir(&dir, || {
        apply_flags(
            &mut app,
            &ctx,
            DeferredFlags {
                want_open_cfg: true,
                ..flags()
            },
        );
    });

    assert!(path.exists(), "a cold install seeds the config file");
    assert_eq!(
        app.tabs.len(),
        before + 1,
        "and opens it, not an empty buffer"
    );
    assert!(
        !std::fs::read_to_string(&path).unwrap().is_empty(),
        "the seed must have content"
    );
}

// ---- move line up / down ----
//
// `move_cursor_line` had been called with dir=1 by every test that touched it
// (a workflow test and a perf test that only watched edit_gen), so NOTHING
// asserted that "move line up" moves UP. Deleting the `-` from the dispatch's
// `move_cursor_line(-1)` — making Alt+Up push the line DOWN — survived the
// whole suite. Assert the direction, and assert it from the DISPATCH, since
// that is where the sign lives.

/// A three-line buffer with the caret parked on `line0` (0-based).
fn app_with_lines(line0: usize) -> (ScribeApp, egui::Context) {
    let (mut app, ctx) = app();
    let i = app.active;
    app.tabs[i].text = "alpha\nbravo\ncharlie".to_string();
    set_caret_line0(&mut app, line0);
    (app, ctx)
}

#[test]
fn move_line_up_moves_the_caret_line_above_its_predecessor() {
    let (mut app, ctx) = app_with_lines(1); // on "bravo"
    apply(
        &mut app,
        &ctx,
        &mut Pending {
            move_line_up: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.tabs[app.active].text, "bravo\nalpha\ncharlie",
        "Alt+Up swaps the caret line with the one ABOVE it"
    );
    assert_eq!(
        app.last_cursor_line_col.map(|(l, _)| l),
        Some(1),
        "and the caret follows the line it moved (now line 1)"
    );
}

#[test]
fn move_line_down_moves_the_caret_line_below_its_successor() {
    let (mut app, ctx) = app_with_lines(1); // on "bravo"
    apply(
        &mut app,
        &ctx,
        &mut Pending {
            move_line_down: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.tabs[app.active].text, "alpha\ncharlie\nbravo",
        "Alt+Down swaps the caret line with the one BELOW it"
    );
    assert_eq!(
        app.last_cursor_line_col.map(|(l, _)| l),
        Some(3),
        "and the caret follows the line it moved (now line 3)"
    );
}

#[test]
fn move_line_up_from_the_first_line_is_a_no_op() {
    // The top edge: there is nothing above line 0, so the buffer must be left
    // exactly as it was rather than wrapping or panicking.
    let (mut app, ctx) = app_with_lines(0);
    apply(
        &mut app,
        &ctx,
        &mut Pending {
            move_line_up: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.tabs[app.active].text, "alpha\nbravo\ncharlie",
        "nothing is above the first line"
    );
}

#[test]
fn move_line_down_from_the_last_line_is_a_no_op() {
    let (mut app, ctx) = app_with_lines(2);
    apply(
        &mut app,
        &ctx,
        &mut Pending {
            move_line_down: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.tabs[app.active].text, "alpha\nbravo\ncharlie",
        "nothing is below the last line"
    );
}

// ---- duplicate_cursor_line ----

#[test]
fn duplicate_line_inserts_an_exact_copy_below_the_cursor_line() {
    let (mut app, _ctx) = app_with_lines(1);
    app.duplicate_cursor_line();
    assert_eq!(app.tabs[app.active].text, "alpha\nbravo\nbravo\ncharlie");
}

#[test]
fn duplicate_line_on_an_empty_buffer_yields_two_empty_lines() {
    // The ONLY input that discriminates the `trailing_nl && last.is_empty()`
    // guard. On every other input `&&` and `||` agree, which is why flipping it
    // survived the whole suite. An empty buffer holds ONE empty line, so
    // duplicating it must give two — i.e. a single newline. With `||` the guard
    // pops the only line and the whole operation becomes a silent no-op.
    let (mut app, _ctx) = app();
    let i = app.active;
    app.tabs[i].text = String::new();
    set_caret_line0(&mut app, 0);

    app.duplicate_cursor_line();

    assert_eq!(
        app.tabs[i].text, "\n",
        "duplicating the empty line must produce a second one"
    );
}

#[test]
fn duplicate_line_keeps_a_trailing_newline_from_growing() {
    // The guard's real job: `"alpha\n"` splits to ["alpha", ""], and that
    // phantom empty last element must not be duplicated or counted as a line.
    let (mut app, _ctx) = app();
    let i = app.active;
    app.tabs[i].text = "alpha\n".to_string();
    set_caret_line0(&mut app, 0);

    app.duplicate_cursor_line();

    assert_eq!(
        app.tabs[i].text, "alpha\nalpha\n",
        "exactly one trailing newline survives"
    );
}
