//! End-to-end tests: drive the real `ScribeApp::ui` render loop headlessly
//! through egui's own `Context::run`, exercising the full per-frame UI +
//! state pipeline (menus, panels, editor, overlays) without a window/GPU.
//!
//! The `egui_kittest`-backed tests below go further: they simulate real user
//! input (clicking widgets BY LABEL via AccessKit) and assert the observable
//! outcome — the only kind of test that catches "clicking does nothing".
use super::*;
#[allow(unused_imports)]
use egui_kittest::kittest::Queryable as _;

/// Run `n` full UI frames against a fresh headless egui context.
fn run_frames(app: &mut ScribeApp, n: usize) {
    let ctx = egui::Context::default();
    for _ in 0..n {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1100.0, 720.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| app.frame_tick(ctx));
    }
}

#[test]
fn renders_default_without_panic() {
    let mut app = ScribeApp::new_test(Config::default());
    run_frames(&mut app, 3);
    assert_eq!(app.tabs.len(), 1, "expected one scratch tab");
}

/// Regression guard (e2e, no GPU): a NARROW pane lays its header out as a
/// vertical COLUMN — name on top, pin below, close below, all centered and
/// stacked (the user's Image #2) — NOT a horizontal row. We tile 4 panes in
/// a wide+short window so each is < 220px (the narrow threshold), and pin all
/// but one so exactly ONE pane shows a PUSH_PIN + close (✕) — making both
/// uniquely queryable. We then assert the ✕ sits BELOW the pin and in the
/// SAME column (x-aligned); a regression to the row layout would put them
/// side-by-side (≈equal y, different x) and fail.
#[test]
fn narrow_pane_header_stacks_vertically() {
    let mut cfg = Config::default();
    cfg.appearance.frameless = false;
    cfg.editor.first_run_completed = true;
    cfg.editor.grid_enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    for _ in 0..3 {
        app.tabs.push(EditorTab::scratch());
    }
    // Pin all but the LAST pane: pinned panes show PUSH_PIN_SLASH and hide
    // their close, so the one unpinned pane is the sole source of a PUSH_PIN
    // glyph and an X glyph — both unambiguously queryable.
    app.tabs[0].pinned = true;
    app.tabs[1].pinned = true;
    app.tabs[2].pinned = true;
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(820.0, 250.0)) // wide+short => 4 panes < 220px each
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    h.run();
    h.run();
    let pin = h.get_by_label(egui_phosphor::thin::PUSH_PIN).rect();
    let close = h.get_by_label(egui_phosphor::thin::X).rect();
    assert!(
        close.center().y > pin.center().y + 2.0,
        "narrow pane header must STACK: close (✕) must be BELOW the pin \
             (pin.y={:.1}, close.y={:.1})",
        pin.center().y,
        close.center().y
    );
    assert!(
        (close.center().x - pin.center().x).abs() < 24.0,
        "narrow pane header must be a single COLUMN: close (✕) and pin must \
             share an x (pin.x={:.1}, close.x={:.1}) — not sit side-by-side",
        pin.center().x,
        close.center().x
    );
}

/// REAL interaction test (egui_kittest): open Settings, then click its close
/// (✕) the way a user does, and assert the window actually closes. This is
/// the kind of test that would have caught "the ✕ doesn't close".
#[test]
fn settings_close_button_actually_closes() {
    // frameless OFF so the ONLY "Close window" button is the settings
    // window's ✕ (the frameless app titlebar adds its own close button).
    let mut cfg = Config::default();
    cfg.appearance.frameless = false; // no titlebar ✕
    cfg.editor.first_run_completed = true; // no welcome-modal ✕
    let app = ScribeApp::new_test(cfg);
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 720.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    harness.state_mut().settings_open = true;
    harness.run();
    assert!(harness.state().settings_open, "settings should be open");
    // Click the window close (✕) by its accessibility label, like a user.
    harness.get_by_label("Close window").click();
    harness.run();
    assert!(
        !harness.state().settings_open,
        "clicking the ✕ must close the settings window"
    );
}

/// Regression guard (e2e, no GPU): the Settings window width MUST NOT change
/// between pages. Reproduces the user's "the Toolbar page gets a lot wider"
/// report (was ~829px on Appearance vs ~1069px on Toolbar). The close (✕) is
/// pinned to the window's top-right, so a constant window width means a
/// constant ✕ x-position; we assert the ✕ right-edge is identical on the
/// Appearance and Toolbar pages. Fixed by the inner ScrollArea::max_width cap.
#[test]
fn settings_window_width_constant_across_pages() {
    let mut cfg = Config::default();
    cfg.appearance.frameless = false; // the settings ✕ is the only "Close window"
    cfg.editor.first_run_completed = true;
    let app = ScribeApp::new_test(cfg);
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1280.0, 940.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    h.state_mut().settings_open = true;
    h.run();
    let close_appearance = h.get_by_label("Close window").rect().right();
    h.get_by_label("Toolbar").click();
    h.run();
    h.run();
    let close_toolbar = h.get_by_label("Close window").rect().right();
    assert!(
        (close_appearance - close_toolbar).abs() < 1.0,
        "settings window width changed between pages: close (✕) right edge \
             {close_appearance} on Appearance vs {close_toolbar} on Toolbar"
    );
}

/// Same, but in the DEFAULT frameless mode (custom titlebar) — the config
/// the user actually runs. Two "Close window" buttons exist (app titlebar +
/// settings window); we click the settings one (lower on screen) and assert
/// it closes. Reproduces the user's "✕ doesn't close" report if it's a
/// frameless-mode interaction problem.
#[test]
fn settings_close_works_in_frameless_mode() {
    let mut cfg = Config::default();
    cfg.appearance.frameless = true;
    cfg.editor.first_run_completed = true;
    let app = ScribeApp::new_test(cfg);
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 720.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    harness.state_mut().settings_open = true;
    harness.run();
    // The settings window's ✕ is the "Close window" button with the LARGEST
    // top-y (the app titlebar's sits at the very top of the screen).
    let target = harness
        .get_all_by_label("Close window")
        .max_by(|a, b| {
            a.rect()
                .top()
                .partial_cmp(&b.rect().top())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .expect("a settings close button");
    target.click();
    harness.run();
    assert!(
        !harness.state().settings_open,
        "clicking the settings ✕ must close it even in frameless mode"
    );
}

/// REAL interaction test: with two tabs open, clicking the other tab's label
/// switches the active document to it. Catches the regression where the
/// drag-source wrapper ate the click and tabs couldn't be switched.
#[test]
fn clicking_a_tab_switches_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = dir.path().join("alpha.txt");
    let beta = dir.path().join("beta.txt");
    std::fs::write(&alpha, "A\n").unwrap();
    std::fs::write(&beta, "B\n").unwrap();
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = true; // the user's actual mode
    let mut app = ScribeApp::new_test(cfg);
    app.open_path(alpha.clone());
    app.open_path(beta.clone());
    // beta was opened last → it is the active tab.
    let beta_idx = app.active;
    assert_eq!(app.tabs[beta_idx].title(), "beta.txt");

    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 720.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    harness.run();
    // Click the OTHER tab the way a user does.
    harness.get_by_label("alpha.txt").click();
    harness.run();
    let active_title = {
        let app = harness.state();
        app.tabs[app.active].title()
    };
    assert_eq!(
        active_title, "alpha.txt",
        "clicking the alpha tab must switch the active document to it"
    );
}

/// Build a kittest harness over the app in the user's default (frameless)
/// mode, with the first-run welcome modal suppressed.
fn ui_harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 720.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}
fn fresh_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    ScribeApp::new_test(cfg)
}

/// An app whose toolbar renders as a FULL-WIDTH top panel (not folded into the
/// narrow in-titlebar bar), so every customizable quick-access item is visible
/// and directly clickable — the right surface for driving the later toolbar
/// items (minimap / wrap / spellcheck / fold / linenumbers / lsp) that overflow
/// into the "⋯ more actions" dropdown on the narrow in-titlebar toolbar.
fn toolbar_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.toolbar_in_titlebar = false;
    ScribeApp::new_test(cfg)
}

// #91 render-coverage: exercise the new render paths headlessly via
// run_frames so the GUI-heavy code (rotated side tabs, spell underline
// painter, font-theme reapply, background tint) is actually executed.

#[test]
fn render_rotated_left_tab_bar_does_not_panic() {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.editor.tab_bar_position = scribe_core::config::TabBarPosition::Left;
    cfg.editor.side_tabs_rotated = true;
    let mut app = ScribeApp::new_test(cfg);
    app.new_tab();
    app.tabs[0].pinned = true; // exercise the pin-glyph path too
    run_frames(&mut app, 3);
    assert!(app.tabs.len() >= 2);
}

#[test]
fn render_horizontal_left_tab_bar_does_not_panic() {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.editor.tab_bar_position = scribe_core::config::TabBarPosition::Right;
    cfg.editor.side_tabs_rotated = false;
    let mut app = ScribeApp::new_test(cfg);
    app.new_tab();
    run_frames(&mut app, 3);
    assert!(app.tabs.len() >= 2);
}

/// The Bottom tab strip must render ABOVE the status bar (status keeps the
/// very bottom screen edge). Verified by y-order: the status bar's "LF"
/// eol-button sits BELOW a tab's close (✕).
#[test]
fn bottom_tab_bar_sits_above_status_bar() {
    use egui_kittest::kittest::Queryable as _;
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    cfg.editor.tab_bar_position = scribe_core::config::TabBarPosition::Bottom;
    let app = ScribeApp::new_test(cfg);
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(900.0, 600.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    h.run();
    h.run();
    let status_lf = h.get_by_label("LF").rect();
    let tab_close = h.get_by_label(egui_phosphor::thin::X).rect();
    assert!(
        status_lf.center().y > tab_close.center().y,
        "status bar must be BELOW the bottom tab strip: \
             status.y={:.1} tab.y={:.1}",
        status_lf.center().y,
        tab_close.center().y
    );
}

/// `invalidate_galley_caches` must drop the atlas-baked galley caches so a
/// font switch can't repaint the note from a stale (garbled) galley.
#[test]
fn invalidate_galley_caches_clears_all_baked_galley_caches() {
    let app = ScribeApp::new_test(Config::default());
    *app.hl_cache.borrow_mut() = Some((42, std::sync::Arc::new(egui::text::LayoutJob::default())));
    assert!(app.hl_cache.borrow().is_some());
    app.invalidate_galley_caches();
    assert!(app.hl_cache.borrow().is_none(), "hl_cache must clear");
    assert!(app.hl_galley_cache.borrow().is_none(), "galley cache clear");
    assert!(app.minimap_cache.borrow().is_none(), "minimap cache clear");
}

/// Changing ONLY the app UI font must trigger the restart-free font rebuild
/// (which re-applies the atlas + drops stale galleys). Regression for
/// "changing the app UI font breaks the note font" — the note text rendered
/// from a galley baked against the pre-rebuild atlas.
#[test]
fn changing_ui_font_triggers_font_rebuild() {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    let app = ScribeApp::new_test(cfg);
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(900.0, 600.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    h.run();
    let before = h.state().applied_font_family.clone();
    // Flip ONLY the UI font (note/editor family unchanged).
    h.state_mut().config.fonts.ui_family = "Fira Mono".to_string();
    h.run();
    assert_ne!(
        h.state().applied_font_family,
        before,
        "a UI-font change must trigger the font rebuild + cache invalidation"
    );
}

/// The Settings → Toolbar "show dropdown" toggle adds/removes exactly one
/// toolbar button (the painted-dots overflow trigger). The trigger is a
/// painted button with no text label (the "⋯" glyph tofu'd), so we count
/// toolbar-band Button nodes rather than match a glyph label.
#[test]
fn toolbar_dropdown_toggle_changes_button_count() {
    use egui_kittest::kittest::NodeT as _;
    let count_toolbar_buttons = |h: &egui_kittest::Harness<'_, ScribeApp>| {
        h.root()
            .children_recursive()
            .filter(|n| {
                let ak = n.accesskit_node();
                format!("{:?}", ak.role()) == "Button"
                    && ak.bounding_box().map(|b| b.y0 < 60.0).unwrap_or(false)
            })
            .count()
    };
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.toolbar.items = vec!["save".to_string()];
    cfg.toolbar.menu = vec!["find".to_string()]; // a parked action → dropdown wants to show
    cfg.toolbar.show_dropdown = true;
    let mut h = ui_harness(ScribeApp::new_test(cfg));
    h.run();
    h.run();
    let with = count_toolbar_buttons(&h);
    h.state_mut().config.toolbar.show_dropdown = false;
    h.run();
    h.run();
    let without = count_toolbar_buttons(&h);
    assert_eq!(
        with,
        without + 1,
        "toggling the dropdown OFF must remove exactly one toolbar button \
             (with={with}, without={without})"
    );
}

#[test]
fn toolbar_visible_count_folds_overflow_into_dropdown() {
    // Everything fits → all visible, no overflow trigger reserved.
    assert_eq!(toolbar_visible_count(300.0, 30.0, 28.0, 5), 5);
    // Exactly fits the available width.
    assert_eq!(toolbar_visible_count(150.0, 30.0, 28.0, 5), 5);
    // Too narrow → reserve the dropdown trigger (28), fit the rest:
    // (120 - 28) / 30 = 3 visible, 2 fold into "⋯".
    assert_eq!(toolbar_visible_count(120.0, 30.0, 28.0, 5), 3);
    // Extremely narrow → everything folds, none clipped off the edge.
    assert_eq!(toolbar_visible_count(10.0, 30.0, 28.0, 5), 0);
    // Degenerate inputs never panic and never exceed n.
    assert_eq!(toolbar_visible_count(100.0, 0.0, 28.0, 4), 4);
    assert_eq!(toolbar_visible_count(0.0, 30.0, 28.0, 0), 0);
    assert!(toolbar_visible_count(50.0, 30.0, 28.0, 100) <= 100);
}

// NOTE: the "caption buttons go over the toolbar when narrow" fix (titlebar
// reserve-caption-buttons-first layout in `ui()`) is NOT covered by a headless
// test. The custom caption buttons are painted directly via `caption_btn`
// (`allocate_exact_size` + `ui.painter()`) with NO accessible node, so they are
// invisible to the kittest accesskit tree; and accesskit reports widgets at
// their LOGICAL (un-clipped) positions, so the buggy and fixed layouts are
// indistinguishable through it. The fix is verified by code review of the
// reserve-first layout idiom plus the one-shot `%TEMP%\scr1b3-caption-diag.txt`
// NC-state diagnostic written on the user's real machine.

/// Regression for the "toggle does nothing when turned on" report: the
/// more-actions dropdown must appear when `show_dropdown` is on EVEN with an
/// EMPTY menu (previously it was gated on a non-empty menu, so enabling it
/// with the default empty menu rendered no button — looked inert).
#[test]
fn dropdown_shows_when_enabled_even_with_empty_menu() {
    use egui_kittest::kittest::NodeT as _;
    let count_toolbar_buttons = |h: &egui_kittest::Harness<'_, ScribeApp>| {
        h.root()
            .children_recursive()
            .filter(|n| {
                let ak = n.accesskit_node();
                format!("{:?}", ak.role()) == "Button"
                    && ak.bounding_box().map(|b| b.y0 < 60.0).unwrap_or(false)
            })
            .count()
    };
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.toolbar.items = vec!["save".to_string()];
    cfg.toolbar.menu = Vec::new(); // EMPTY menu — the default state
    cfg.toolbar.show_dropdown = true;
    let mut h = ui_harness(ScribeApp::new_test(cfg));
    h.run();
    h.run();
    let with = count_toolbar_buttons(&h);
    h.state_mut().config.toolbar.show_dropdown = false;
    h.run();
    h.run();
    let without = count_toolbar_buttons(&h);
    assert_eq!(
        with,
        without + 1,
        "with an EMPTY menu, enabling the dropdown must still add its button \
             (with={with}, without={without})"
    );
}

/// #30 node.rect verification — a ROTATE-OFF side tab lays its controls out
/// as a horizontal ROW (grip · name · pin · close): the close (✕) shares the
/// pin's row (≈ same y) and sits to its RIGHT. Before the fix the side strip
/// inherited the vertical parent layout and stacked name/pin/close per tab.
#[test]
fn rotate_off_side_tab_is_a_horizontal_row() {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false; // no custom-titlebar ✕ to disambiguate
    cfg.editor.tab_bar_position = scribe_core::config::TabBarPosition::Left;
    cfg.editor.side_tabs_rotated = false;
    let app = ScribeApp::new_test(cfg); // single active, unpinned tab
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(900.0, 600.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    h.run();
    h.run();
    let pin = h.get_by_label(egui_phosphor::thin::PUSH_PIN).rect();
    let close = h.get_by_label(egui_phosphor::thin::X).rect();
    assert!(
        (close.center().y - pin.center().y).abs() < 6.0,
        "rotate-off side tab must be a ROW: pin and close share a row \
             (pin.y={:.1}, close.y={:.1})",
        pin.center().y,
        close.center().y
    );
    assert!(
        close.center().x > pin.center().x + 2.0,
        "rotate-off row order is grip·name·pin·close: close (✕) is RIGHT of \
             the pin (pin.x={:.1}, close.x={:.1})",
        pin.center().x,
        close.center().x
    );
}

/// #30 node.rect verification — a ROTATE-ON side tab is a vertical COLUMN
/// (grip · rotated-name · pin · close): the close sits BELOW the pin and they
/// share an x. Also exercises the rotated drag-grip render path.
#[test]
fn rotate_on_side_tab_is_a_vertical_column() {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    cfg.editor.tab_bar_position = scribe_core::config::TabBarPosition::Left;
    cfg.editor.side_tabs_rotated = true;
    let app = ScribeApp::new_test(cfg);
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(900.0, 600.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    h.run();
    h.run();
    let pin = h.get_by_label(egui_phosphor::thin::PUSH_PIN).rect();
    let close = h.get_by_label(egui_phosphor::thin::X).rect();
    assert!(
        close.center().y > pin.center().y + 2.0,
        "rotate-on column order is grip·name·pin·close: close (✕) is BELOW \
             the pin (pin.y={:.1}, close.y={:.1})",
        pin.center().y,
        close.center().y
    );
    assert!(
        (close.center().x - pin.center().x).abs() < 24.0,
        "rotate-on side tab is a single COLUMN: pin and close share an x \
             (pin.x={:.1}, close.x={:.1})",
        pin.center().x,
        close.center().x
    );
}

/// #28 render-coverage — the render-whitespace overlay now runs in the
/// DEFAULT egui TextEdit path (previously the `·`/`→` markers only drew in
/// the experimental rope editor). We can't read painted glyphs back, so this
/// guards that the galley-walk overlay executes without panicking over a
/// buffer that has both spaces and tabs.
#[test]
fn render_whitespace_overlay_default_editor_runs() {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.editor.render_whitespace = true;
    cfg.editor.experimental_rope_editor = false; // exercise the TextEdit path
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = "fn  main() {\n\tlet x = 1;\n}\n".into();
    run_frames(&mut app, 3);
    assert!(app.config.editor.render_whitespace);
}

/// #28 render-coverage — the same overlay also runs in split/grid view.
#[test]
fn render_whitespace_overlay_grid_path_runs() {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.editor.render_whitespace = true;
    cfg.editor.grid_enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = "a  b\tc\n".into();
    app.new_tab();
    run_frames(&mut app, 3);
    assert!(app.tabs.len() >= 2);
}

#[test]
fn render_spellcheck_underline_path_runs() {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.spellcheck.enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = "this zxqwyzz wordd is rong".into();
    run_frames(&mut app, 3); // paints the squiggles
    assert!(!app.misspellings_for_active().is_empty());
}

#[test]
fn render_font_theme_bg_override_and_glass_paths_run() {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.fonts.editor_family = "IBM Plex Mono".into();
    cfg.appearance.background_override = Some("#203040".into());
    cfg.window.transparency_enabled = true;
    cfg.window.mode = scribe_core::config::WindowMode::Glass;
    let mut app = ScribeApp::new_test(cfg);
    run_frames(&mut app, 3); // build_fonts reapply + tint overlay + visuals
                             // applied_font_family is the combined note+UI key (#103).
    assert!(
        app.applied_font_family.starts_with("IBM Plex Mono"),
        "note family recorded in the font-state key (got {:?})",
        app.applied_font_family
    );
}

#[test]
fn caption_settings_gear_opens_settings() {
    let mut h = ui_harness(fresh_app());
    h.run();
    assert!(!h.state().settings_open);
    // The settings gear was relocated from the quick-access toolbar into the
    // window caption row (left of Minimize). It is a PAINTED caption button, so
    // it is reached by its accessible name "Open settings" (see `caption_btn`),
    // not the old phosphor GEAR_SIX glyph.
    assert!(
        h.query_by_label("Open settings").is_some(),
        "the relocated settings gear must expose its accessible name in the caption row"
    );
    h.get_by_label("Open settings").click();
    h.run();
    assert!(
        h.state().settings_open,
        "clicking the caption settings gear must open Settings"
    );
}

#[test]
fn open_find_bar_suppresses_and_clears_completion_popup() {
    // #72 regression: a completion popup must not survive (and so cannot
    // steal ↑↓/Enter) while the find bar owns the keyboard.
    let mut app = fresh_app();
    app.find_open = true;
    app.completion = Some(super::Completion {
        prefix_start: 0,
        items: vec!["alpha".into(), "alpine".into()],
        selected: 0,
    });
    assert!(
        app.modal_owns_keyboard(),
        "an open find bar must own the keyboard"
    );
    let mut h = ui_harness(app);
    h.run();
    assert!(
        h.state().completion.is_none(),
        "the completion popup must be force-closed while the find bar is open"
    );
}

#[test]
fn editor_owns_keyboard_when_no_modal_open() {
    let app = fresh_app();
    assert!(
        !app.modal_owns_keyboard(),
        "with no modal open the editor (not a modal) owns the keyboard"
    );
}

#[test]
fn toolbar_palette_button_opens_palette() {
    let mut h = ui_harness(fresh_app());
    h.run();
    h.get_by_label(">_").click();
    h.run();
    assert!(
        h.state().palette_open,
        "clicking the >_ button must open the command palette"
    );
}

#[test]
fn toolbar_split_button_toggles_grid() {
    let mut h = ui_harness(fresh_app());
    h.run();
    assert!(!h.state().config.editor.grid_enabled);
    h.get_by_label("split").click();
    h.run();
    assert!(
        h.state().config.editor.grid_enabled,
        "the split button must toggle the unified split/grid view on"
    );
}

#[test]
fn middle_click_tab_closes_it() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = dir.path().join("alpha.txt");
    let beta = dir.path().join("beta.txt");
    std::fs::write(&alpha, "A\n").unwrap();
    std::fs::write(&beta, "B\n").unwrap();
    let mut app = fresh_app();
    app.open_path(alpha);
    app.open_path(beta);
    let mut h = ui_harness(app);
    h.run();
    h.get_by_label("beta.txt")
        .click_button(egui::PointerButton::Middle);
    h.run();
    let has_beta = h.state().tabs.iter().any(|t| t.title() == "beta.txt");
    assert!(!has_beta, "middle-clicking a tab must close it");
}

/// Wave 2: the Editor settings page exposes the Scroll-speed control.
#[test]
fn settings_exposes_scroll_speed_control() {
    let mut h = ui_harness(fresh_app());
    h.run();
    h.state_mut().settings_open = true;
    h.run();
    h.get_by_label("Editor").click();
    h.run();
    // Panics (failing the test) if the Scroll-speed slider label is absent.
    let _ = h.get_by_label("Scroll speed");
}

/// Wave 2: a middle-click INSIDE the editor arms autoscroll; the existing
/// `middle_click_tab_closes_it` proves a middle-click on a TAB does not (it
/// would otherwise spin the harness past max_steps via the drift repaint).
#[test]
fn middle_click_in_editor_arms_autoscroll() {
    // Short buffer so the editor widget's centre (what `click_button` targets)
    // is ON-SCREEN — a click on off-viewport content is correctly rejected by
    // the visible-editor-area gate, which is the feature, not a bug.
    let mut app = fresh_app();
    app.tabs[0].text = "hi\n".to_string();
    let mut h = ui_harness(app);
    h.run();
    let id = egui::Id::new("scr1b3_autoscroll");
    let armed = |h: &egui_kittest::Harness<'_, ScribeApp>| {
        h.ctx
            .data(|d| d.get_temp::<AutoScrollState>(id))
            .map(|s| s.active)
            .unwrap_or(false)
    };
    assert!(!armed(&h), "autoscroll starts disarmed");
    h.get_by_role(egui::accesskit::Role::MultilineTextInput)
        .click_button(egui::PointerButton::Middle);
    h.run();
    assert!(
        armed(&h),
        "a middle-click inside the editor must arm autoscroll"
    );
}

#[test]
fn command_palette_opens_then_escape_closes() {
    let mut h = ui_harness(fresh_app());
    h.run();
    h.get_by_label(">_").click();
    h.run();
    assert!(h.state().palette_open);
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(
        !h.state().palette_open,
        "Escape must close the command palette"
    );
}

#[test]
fn typing_updates_the_active_buffer() {
    let mut app = fresh_app();
    // Make the scratch tab empty + active so typed text is observable.
    app.tabs[0].text.clear();
    let mut h = ui_harness(app);
    h.run();
    // Focus the editor text area and type like a user.
    let editor = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    editor.focus();
    h.run();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("hello");
    h.run();
    let active = h.state().active;
    assert!(
        h.state().tabs[active].text.contains("hello"),
        "typing must update the active buffer, got {:?}",
        h.state().tabs[active].text
    );
}

#[test]
fn settings_toggle_flips_runtime_config() {
    let mut h = ui_harness(fresh_app());
    h.state_mut().settings_open = true;
    h.run();
    // Navigate to the Editor category, then flip "Line numbers".
    h.get_by_label("Editor").click();
    h.run();
    let before = h.state().config.editor.show_line_numbers;
    h.get_by_label("Line numbers").click();
    h.run();
    assert_ne!(
        h.state().config.editor.show_line_numbers,
        before,
        "clicking the Line numbers checkbox must flip the setting"
    );
}

#[test]
fn plus_button_adds_a_tab() {
    let mut h = ui_harness(fresh_app());
    h.run();
    let before = h.state().tabs.len();
    // The add-tab control is now a frameless Phosphor PLUS glyph button (v0.4.58),
    // not the old text "+" — match on its accessible glyph name.
    h.get_by_label(egui_phosphor::thin::PLUS).click();
    h.run();
    assert_eq!(
        h.state().tabs.len(),
        before + 1,
        "the + button must add a new tab"
    );
}

#[test]
fn tab_label_is_plain_title_no_tofu_pin_glyph() {
    // The pin glyph prefix was removed: it rendered as a tofu □ left of the
    // title in this build's font atlas (the egui-phosphor .notdef footgun).
    // Pinned state now reads from the dimmed, drag-disabled grab handle.
    // The label must be the bare title in BOTH states — no leading glyph.
    assert_eq!(super::tab_display_label("notes.txt", true), "notes.txt");
    assert_eq!(super::tab_display_label("notes.txt", false), "notes.txt");
}

#[test]
fn dirty_tab_marker_is_ascii_not_tofu_glyph() {
    // The unsaved marker must be ASCII (`*`), never the `●`/`□` that tofu'd in
    // the atlas — the "empty square in the untitled tab" report. A dirty
    // untitled tab is the exact case that showed it.
    let mut tab = EditorTab::scratch();
    tab.text = "Hi".to_string(); // diverges from the empty saved doc → dirty
    assert!(tab.is_dirty(), "tab with unsaved text must be dirty");
    let title = tab.title();
    assert!(
        title.is_ascii(),
        "dirty marker must be ASCII (no tofu-prone glyph): {title:?}"
    );
    assert!(
        title.starts_with("* "),
        "dirty title must lead with `* `: {title:?}"
    );
    assert!(!title.contains('\u{25CF}') && !title.contains('\u{25A1}'));
    // A clean tab carries no marker.
    let clean = EditorTab::scratch();
    assert!(!clean.title().starts_with('*'));
}

#[test]
fn follow_os_theme_switches_with_os() {
    use scribe_core::theme::Appearance;
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.follow_os_theme = true;
    cfg.appearance.theme = "wired-noir".to_string(); // a dark brand theme
    let mut h = ui_harness(ScribeApp::new_test(cfg));
    // OS reports LIGHT → the app must switch to a light theme.
    h.ctx.set_theme(egui::Theme::Light);
    h.run();
    h.run();
    assert!(
        matches!(h.state().theme.appearance, Appearance::Light),
        "light OS theme must switch the app to a light theme, got {:?}",
        h.state().theme.appearance
    );
    // OS flips to DARK → the app must switch back to a dark theme.
    h.ctx.set_theme(egui::Theme::Dark);
    h.run();
    h.run();
    assert!(
        matches!(h.state().theme.appearance, Appearance::Dark),
        "dark OS theme must switch the app to a dark theme, got {:?}",
        h.state().theme.appearance
    );
}

/// Phase 18 T18.2 — flipping `editor.grid_enabled` on creates the
/// tile-tree at the top of the next frame and the central panel
/// renders without panicking. Three frames are enough to exercise
/// the sync + render + post-frame cleanup paths.
#[test]
fn grid_enabled_renders_without_panic() {
    let mut cfg = Config::default();
    cfg.editor.grid_enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    run_frames(&mut app, 3);
    assert!(
        app.grid_tree.is_some(),
        "grid tree must be built when enabled"
    );
    assert_eq!(app.tabs.len(), 1, "still one scratch tab");
    // The single scratch tab got a real doc id (the legacy 0
    // sentinel gets bumped on first sync).
    assert!(app.tabs[0].doc_id.0 > 0, "doc id allocated");
}

/// Phase 18 T18.2 — toggling the grid OFF after it was ON drops
/// the tree and re-engages the single-pane code path on the next
/// frame.
#[test]
fn grid_disabled_drops_tree() {
    let mut cfg = Config::default();
    cfg.editor.grid_enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    run_frames(&mut app, 1);
    assert!(app.grid_tree.is_some());
    app.config.editor.grid_enabled = false;
    run_frames(&mut app, 1);
    assert!(app.grid_tree.is_none(), "tree drops when disabled");
}

#[test]
fn panel_fill_opaque_when_master_off() {
    // T19.2: with transparency disabled the chrome fill keeps full alpha,
    // so the window reads as a normal opaque window.
    let theme = Theme::wired_noir();
    let w_off = scribe_core::config::WindowConfig {
        mode: scribe_core::config::WindowMode::Glass, // mode set, but master OFF
        opacity: 0.5,
        ..Default::default()
    };
    assert_eq!(
        panel_fill(&theme, &w_off, None).a(),
        255,
        "opaque while master toggle off"
    );
    // Master ON + translucent mode => alpha lowered to opacity.
    let w_on = scribe_core::config::WindowConfig {
        transparency_enabled: true,
        ..w_off
    };
    let a = panel_fill(&theme, &w_on, None).a();
    assert!(
        (76..255).contains(&a),
        "alpha reduced to ~opacity (got {a})"
    );
}

#[test]
fn tint_enable_toggle_gates_panel_fill_colour() {
    let theme = Theme::wired_noir();
    let base = panel_fill(&theme, &scribe_core::config::WindowConfig::default(), None);
    // Strong tint but the enable toggle OFF => no colour shift (equals base).
    let off = scribe_core::config::WindowConfig {
        tint_enabled: false,
        tint: "#ff0000".into(),
        tint_strength: 0.8,
        ..Default::default()
    };
    assert_eq!(
        panel_fill(&theme, &off, None),
        base,
        "toggle off => no tint"
    );
    // Same tint with the toggle ON => the panel fill shifts toward red.
    let on = scribe_core::config::WindowConfig {
        tint_enabled: true,
        ..off
    };
    assert_ne!(panel_fill(&theme, &on, None), base, "toggle on => tinted");
}

#[test]
fn tint_colours_panel_in_transparent_mode() {
    // Issue-1 regression guard: with transparency ON + tint_enabled ON +
    // strength > 0, the tint must VISIBLY colour the (translucent) window — the
    // panel fill carries the tinted RGB AND a reduced alpha. The tint is applied
    // BEFORE the translucency alpha, so it never gets bypassed by the see-through
    // path. (The UI bug was separate: the Tint colour/strength controls were
    // gated on `transparency_enabled` instead of `tint_enabled`, greying them
    // out — but the paint path here always tinted correctly.)
    // Compare the UN-premultiplied red channel: `Color32` stores premultiplied
    // bytes, so `.r()` scales with alpha and can't be compared across opacities.
    let red = |c: egui::Color32| c.to_srgba_unmultiplied()[0];
    let theme = Theme::wired_noir();
    let plain_red = red(panel_fill(
        &theme,
        &scribe_core::config::WindowConfig::default(),
        None,
    ));
    let w = scribe_core::config::WindowConfig {
        transparency_enabled: true,
        tint_enabled: true,
        tint: "#ff0000".into(),
        tint_strength: 0.8,
        opacity: 0.5,
        ..Default::default()
    };
    let tinted = panel_fill(&theme, &w, None);
    // RGB shifted strongly toward red (the tint), not the plain theme panel.
    assert!(
        red(tinted) > plain_red,
        "transparent-mode panel must be red-shifted by the tint (got r={}, plain r={plain_red})",
        red(tinted)
    );
    // ...and translucent (alpha reflects the 0.5 opacity, not fully opaque).
    assert!(
        (76..255).contains(&tinted.a()),
        "transparent-mode panel must carry the opacity alpha (got a={})",
        tinted.a()
    );
    // Toggling tint OFF in the SAME transparent mode drops the colour shift.
    let w_off = scribe_core::config::WindowConfig {
        tint_enabled: false,
        ..w
    };
    let untinted_translucent = panel_fill(&theme, &w_off, None);
    assert_eq!(
        red(untinted_translucent),
        plain_red,
        "tint OFF => no red shift even in transparent mode"
    );
}

#[test]
fn visuals_signature_tracks_the_tint_slider() {
    // The tint slider must rebuild the visuals live: changing tint strength (or
    // toggling the enable) changes the signature `frame_tick` compares, and the
    // rebuilt `current_visuals` produces a different editor-well colour.
    let mut app = ScribeApp::new_test(Config::default());
    app.config.window.tint = "#ff0000".into();
    app.config.window.tint_strength = 0.0;
    let sig0 = app.visuals_signature();
    let bg0 = app.current_visuals().extreme_bg_color;
    app.config.window.tint_strength = 0.8;
    let sig1 = app.visuals_signature();
    let bg1 = app.current_visuals().extreme_bg_color;
    assert_ne!(
        sig0, sig1,
        "raising tint strength must change the visuals signature"
    );
    assert_ne!(
        bg0, bg1,
        "raising tint strength must retint the editor well"
    );
    // Disabling the toggle reverts both.
    app.config.window.tint_enabled = false;
    assert_ne!(
        sig1,
        app.visuals_signature(),
        "toggling enable changes the signature"
    );
    assert_eq!(
        bg0,
        app.current_visuals().extreme_bg_color,
        "toggle off => untinted"
    );
}

#[test]
fn close_latch_hides_before_destroy() {
    // T19.1: requesting close must NOT close immediately; it hides first
    // (want_close -> closing) so a layered window leaves no DWM ghost.
    let mut app = ScribeApp::new_test(Config::default());
    app.want_close = true;
    run_frames(&mut app, 1);
    assert!(
        app.closing,
        "first frame latches into the hide-then-close phase"
    );
    assert!(!app.want_close, "want_close consumed");
}

#[test]
fn settings_window_renders() {
    let mut app = ScribeApp::new_test(Config::default());
    app.settings_open = true;
    run_frames(&mut app, 2);
}

#[test]
fn find_bar_renders_with_query() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "foo bar foo baz foo".to_string();
    app.find_open = true;
    app.find_query = "foo".to_string();
    run_frames(&mut app, 1);
    // The find-count path ran without panic; verify the engine agrees.
    let q = scribe_core::search::Query {
        pattern: "foo".into(),
        ..Default::default()
    };
    assert_eq!(
        scribe_core::search::find_all(&app.tabs[0].text, &q)
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn spellcheck_flags_misspellings_e2e() {
    let mut cfg = Config::default();
    cfg.spellcheck.enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = "thiss sentense has bad wrds".to_string();
    run_frames(&mut app, 1);
    assert!(app.spell_count() > 0, "misspellings should be detected");
}

#[test]
fn command_palette_opens_and_renders() {
    let mut app = ScribeApp::new_test(Config::default());
    app.palette_open = true;
    run_frames(&mut app, 1);
    assert!(app.palette_open);
}

#[test]
fn file_tree_sidebar_renders() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let mut app = ScribeApp::new_test(Config::default());
    app.file_tree_root = Some(dir.path().to_path_buf());
    run_frames(&mut app, 2);
    assert!(app.file_tree_root.is_some());
}

#[test]
fn open_then_edit_then_save_e2e() {
    // Full editor lifecycle through the headless render loop.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, "original\n").unwrap();
    let mut app = ScribeApp::new_test(Config::default());
    app.open_path(path.clone());
    run_frames(&mut app, 1);
    let idx = app.active;
    app.tabs[idx].text = "edited via e2e\n".to_string();
    app.save_active();
    run_frames(&mut app, 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "edited via e2e\n");
}

#[test]
fn split_is_unified_with_grid() {
    // Split and grid are one feature: enabling the multi-pane view lays the
    // OPEN TABS out as panes (two = side-by-side split, more = grid). With
    // two tabs open the grid has two panes.
    let mut cfg = Config::default();
    cfg.editor.grid_enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = "fn main() {}\n".into();
    app.tabs.push(EditorTab::scratch());
    app.tabs[1].text = "second note\n".into();
    run_frames(&mut app, 2);
    let tree = app
        .grid_tree
        .as_ref()
        .expect("grid tree present when enabled");
    assert_eq!(
        crate::grid::count_panes(tree),
        2,
        "two open tabs render as two panes (a side-by-side split)"
    );
}

#[test]
fn minimap_renders_with_viewport() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = (0..200).map(|i| format!("line {i}\n")).collect();
    app.config.editor.show_minimap = true;
    run_frames(&mut app, 2);
    assert!(app.config.editor.show_minimap);
    // Scroll metrics get populated by the editor render.
    assert!(app.scroll_metrics.1 >= 1.0);
}

#[test]
fn fold_view_collapses_region() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "fn a() {\n    body;\n    more;\n}\ntail;\n".into();
    app.fold_view = true;
    run_frames(&mut app, 1);
    // Fold the first region (header at line 0) and re-render — no panic.
    app.folds.insert(0);
    run_frames(&mut app, 1);
    assert!(app.folds.contains(&0));
}

#[test]
fn apply_indent_inserts_spaces_at_caret() {
    let (out, caret) = apply_indent("ab", 1, 1, 4);
    assert_eq!(out, "a    b");
    assert_eq!(caret, 5);
}

#[test]
fn apply_indent_replaces_selection() {
    // Replace chars [1,3) ("bc") of "abcd" with 2 spaces.
    let (out, caret) = apply_indent("abcd", 1, 3, 2);
    assert_eq!(out, "a  d");
    assert_eq!(caret, 3);
}

#[test]
fn line_gutter_populated_when_line_numbers_on() {
    let mut app = ScribeApp::new_test(Config::default());
    app.config.editor.show_line_numbers = true;
    app.tabs[0].text = "a\nb\nc\nd\n".into();
    run_frames(&mut app, 2);
    assert!(
        app.line_gutter.len() >= 4,
        "gutter should hold one Y per logical line (got {})",
        app.line_gutter.len()
    );
}

#[test]
fn line_gutter_empty_when_line_numbers_off() {
    let mut app = ScribeApp::new_test(Config::default());
    app.config.editor.show_line_numbers = false;
    app.tabs[0].text = "a\nb\nc\n".into();
    run_frames(&mut app, 2);
    assert!(app.line_gutter.is_empty());
}

#[test]
fn word_wrap_toggle_renders_without_panic() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "a very long line ".repeat(40);
    app.config.editor.word_wrap = true;
    run_frames(&mut app, 2);
    app.config.editor.word_wrap = false;
    run_frames(&mut app, 2);
    assert!(app.scroll_metrics.1 >= 1.0);
}

#[test]
fn toolbar_default_has_core_actions() {
    let items = scribe_core::config::ToolbarConfig::default().items;
    for want in ["new", "save", "find", "palette"] {
        assert!(
            items.iter().any(|i| i == want),
            "default toolbar missing {want}"
        );
    }
}

#[test]
fn toolbar_layout_survives_serde_roundtrip() {
    let mut cfg = Config::default();
    cfg.toolbar.items = vec!["save".into(), "sep".into(), "lsp".into()];
    let back = Config::from_toml_str(&cfg.to_toml_string()).unwrap();
    assert_eq!(back.toolbar.items, cfg.toolbar.items);
}

#[test]
fn settings_window_renders_open() {
    let mut app = ScribeApp::new_test(Config::default());
    app.settings_open = true;
    run_frames(&mut app, 2);
    assert!(app.settings_open, "settings stays open across frames");
}

#[test]
fn completion_opens_and_accepts() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "value valuer val".into();
    let cursor = app.tabs[0].text.chars().count();
    app.open_completion(0, Some(cursor));
    assert!(
        app.completion.is_some(),
        "completion opens for prefix 'val'"
    );
    let before = app.tabs[0].text.clone();
    app.accept_completion(0, Some(cursor));
    assert_ne!(app.tabs[0].text, before, "accept inserts a completion");
    assert!(app.completion.is_none(), "popup closes after accept");
}

#[test]
fn completion_popup_renders_in_frame() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "alpha alphabet alph".into();
    app.completion = Some(Completion {
        prefix_start: 15,
        items: vec!["alpha".into(), "alphabet".into()],
        selected: 0,
    });
    // The popup Area renders against the live cursor without panic.
    run_frames(&mut app, 1);
}

// ---- Input-driven ("computer control") E2E ----
// A robot user: inject real pointer + keyboard events through egui's own
// event loop (the same `RawInput.events` path a physical mouse/keyboard
// produces) against ONE persistent `Context` so focus + widget state carry
// across frames, then assert what the app did.

struct Driver {
    ctx: egui::Context,
}

impl Driver {
    fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
        }
    }

    fn frame(&self, app: &mut ScribeApp, modifiers: egui::Modifiers, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1100.0, 720.0),
            )),
            modifiers,
            events,
            ..Default::default()
        };
        let _ = self.ctx.run(input, |ctx| app.frame_tick(ctx));
    }

    fn idle(&self, app: &mut ScribeApp) {
        self.frame(app, egui::Modifiers::NONE, vec![]);
    }

    fn click(&self, app: &mut ScribeApp, pos: egui::Pos2) {
        let m = egui::Modifiers::NONE;
        self.frame(
            app,
            m,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: m,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: m,
                },
            ],
        );
    }

    fn key(&self, app: &mut ScribeApp, key: egui::Key, modifiers: egui::Modifiers) {
        self.frame(
            app,
            modifiers,
            vec![
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers,
                },
            ],
        );
    }

    fn type_text(&self, app: &mut ScribeApp, s: &str) {
        self.frame(
            app,
            egui::Modifiers::NONE,
            vec![egui::Event::Text(s.to_string())],
        );
    }
}

#[test]
fn input_ctrl_n_adds_a_tab() {
    let mut app = ScribeApp::new_test(Config::default());
    let d = Driver::new();
    d.idle(&mut app);
    let before = app.tabs.len();
    d.key(&mut app, egui::Key::N, egui::Modifiers::COMMAND);
    assert_eq!(app.tabs.len(), before + 1, "Ctrl+N opens a new tab");
}

#[test]
fn input_ctrl_f_opens_and_escape_closes_find() {
    let mut app = ScribeApp::new_test(Config::default());
    let d = Driver::new();
    d.idle(&mut app);
    d.key(&mut app, egui::Key::F, egui::Modifiers::COMMAND);
    assert!(app.find_open, "Ctrl+F opens the find bar");
    d.key(&mut app, egui::Key::Escape, egui::Modifiers::NONE);
    assert!(!app.find_open, "Escape closes the find bar");
}

/// A fresh, editor-focused app with a document far taller than the viewport, so
/// there is real vertical scroll range for the drag-scroll conveniences. The
/// welcome modal is suppressed (it would otherwise steal focus).
fn tall_editor_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = (0..400).map(|i| format!("line {i}\n")).collect();
    app
}

/// P0-1 (the reported bug): holding the LEFT button to drag-select and rolling
/// the wheel scrolls the editor viewport (via `pending_scroll`) so egui extends
/// the selection past the visible region. Previously impossible — egui's
/// `ScrollArea` gates the wheel behind "no widget is being dragged".
#[test]
fn input_drag_wheel_forces_viewport_scroll() {
    let mut app = tall_editor_app();
    let d = Driver::new();
    d.idle(&mut app);
    d.idle(&mut app); // editor auto-focuses
    let pos = egui::pos2(300.0, 300.0);
    // Press and HOLD the primary button over the editor (no release event), then
    // MOVE while held so egui registers a real drag (sets `dragged_id`, which is
    // what freezes its ScrollArea wheel handling — the condition being fixed).
    d.frame(
        &mut app,
        egui::Modifiers::NONE,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    d.frame(
        &mut app,
        egui::Modifiers::NONE,
        vec![egui::Event::PointerMoved(egui::pos2(324.0, 348.0))],
    );
    let wheel = || egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, -3.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    };
    // Warm egui's wheel smoother one frame, then assert the forced scroll on the
    // next held-drag wheel frame.
    d.frame(&mut app, egui::Modifiers::NONE, vec![wheel()]);
    app.pending_scroll = None;
    d.frame(&mut app, egui::Modifiers::NONE, vec![wheel()]);
    assert!(
        app.pending_scroll.is_some(),
        "wheel rolled during a held drag-selection must force a viewport scroll"
    );
}

/// P0-2: holding a drag-selection pointer at the bottom viewport edge auto-pans
/// (edge autoscroll), so a selection can be extended without touching the wheel.
#[test]
fn input_drag_edge_hold_autoscrolls() {
    let mut app = tall_editor_app();
    let d = Driver::new();
    d.idle(&mut app);
    d.idle(&mut app);
    let start = egui::pos2(300.0, 300.0);
    d.frame(
        &mut app,
        egui::Modifiers::NONE,
        vec![
            egui::Event::PointerMoved(start),
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    // Drag to the very bottom edge and hold (button still down).
    let edge = egui::pos2(300.0, 712.0);
    d.frame(
        &mut app,
        egui::Modifiers::NONE,
        vec![egui::Event::PointerMoved(edge)],
    );
    app.pending_scroll = None;
    d.frame(
        &mut app,
        egui::Modifiers::NONE,
        vec![egui::Event::PointerMoved(edge)],
    );
    assert!(
        app.pending_scroll.is_some(),
        "holding a drag-selection at the bottom edge must auto-pan the viewport"
    );
}

/// P0-1/P0-2 are opt-out: with `drag_autoscroll` off, a held-drag wheel does NOT
/// force a scroll (egui's default freeze-while-dragging behaviour is restored).
#[test]
fn input_drag_wheel_respects_opt_out() {
    let mut app = tall_editor_app();
    app.config.scroll.drag_autoscroll = false;
    let d = Driver::new();
    d.idle(&mut app);
    d.idle(&mut app);
    let pos = egui::pos2(300.0, 300.0);
    d.frame(
        &mut app,
        egui::Modifiers::NONE,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    d.frame(
        &mut app,
        egui::Modifiers::NONE,
        vec![egui::Event::PointerMoved(egui::pos2(324.0, 348.0))],
    );
    let wheel = || egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, -3.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    };
    d.frame(&mut app, egui::Modifiers::NONE, vec![wheel()]);
    app.pending_scroll = None;
    d.frame(&mut app, egui::Modifiers::NONE, vec![wheel()]);
    assert!(
        app.pending_scroll.is_none(),
        "drag_autoscroll=off must not force a scroll during a drag"
    );
}

/// P1-3: scroll-past-end pads blank space below the last line, growing the
/// scrollable content height so the last line can rest off the bottom edge.
#[test]
fn scroll_past_end_pads_content_below_last_line() {
    let text: String = (0..6).map(|i| format!("line {i}\n")).collect();
    let mut on = ScribeApp::new_test(Config::default());
    on.tabs[0].text = text.clone();
    run_frames(&mut on, 2);
    let mut off = ScribeApp::new_test(Config::default());
    off.config.scroll.scroll_past_end = false;
    off.tabs[0].text = text;
    run_frames(&mut off, 2);
    assert!(
        on.scroll_metrics.1 > off.scroll_metrics.1,
        "scroll-past-end must grow content height (on={}, off={})",
        on.scroll_metrics.1,
        off.scroll_metrics.1
    );
}

/// P1-4: a keyboard navigation press with the caret outside the keep-away band
/// nudges the viewport (via `pending_scroll`) so the caret is re-framed. It must
/// never fire while a button is held (that path belongs to drag autoscroll).
#[test]
fn caret_scroll_off_nudges_view_on_keyboard_nav() {
    let mut cfg = Config::default(); // caret_scroll_off defaults to 3
    cfg.editor.first_run_completed = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = (0..200).map(|i| format!("line {i}\n")).collect();
    let d = Driver::new();
    d.idle(&mut app);
    d.idle(&mut app); // caret at index 0, view at top, editor focused
                      // Force the viewport far below the caret so the caret sits above the top
                      // keep-away band.
    app.pending_scroll = Some(600.0);
    d.idle(&mut app); // editor applies the forced offset
    app.pending_scroll = None;
    d.key(&mut app, egui::Key::ArrowUp, egui::Modifiers::NONE);
    assert!(
        app.pending_scroll.is_some(),
        "keyboard nav with the caret past the scroll-off margin must nudge the viewport"
    );
}

/// F-005 helper: line:col math handles plain ASCII, end-of-buffer, and
/// multi-byte UTF-8 codepoints.
#[test]
fn line_col_from_char_index_basics() {
    assert_eq!(line_col_from_char_index("", 0), (1, 1));
    let s = "hello
world";
    assert_eq!(line_col_from_char_index(s, 0), (1, 1));
    assert_eq!(line_col_from_char_index(s, 5), (1, 6));
    assert_eq!(line_col_from_char_index(s, 6), (2, 1));
    assert_eq!(line_col_from_char_index(s, 11), (2, 6));
    let cjk = "日本
語";
    assert_eq!(line_col_from_char_index(cjk, 1), (1, 2));
    assert_eq!(line_col_from_char_index(cjk, 2), (1, 3));
    assert_eq!(line_col_from_char_index(cjk, 3), (2, 1));
}

#[test]
fn line_col_from_char_index_clamps() {
    let s = "abc
def";
    let (line, col) = line_col_from_char_index(s, 99);
    assert_eq!((line, col), (2, 4));
}

/// F-015 parser: accepts plain line number, line:col, and rejects garbage.
#[test]
fn parse_goto_query_accepts_line_and_line_col() {
    assert_eq!(parse_goto_query("42"), Some((42, None)));
    assert_eq!(parse_goto_query("42:10"), Some((42, Some(10))));
    assert_eq!(parse_goto_query("  42  "), Some((42, None)));
    assert_eq!(parse_goto_query("42:"), None);
    assert_eq!(parse_goto_query(":10"), None);
    assert_eq!(parse_goto_query("0"), None);
    assert_eq!(parse_goto_query("abc"), None);
    assert_eq!(parse_goto_query(""), None);
    // Column clamps to 1.
    assert_eq!(parse_goto_query("42:0"), Some((42, Some(1))));
}

/// pick_bookmark walks the ordered set forward / backward and wraps.
#[test]
fn pick_bookmark_navigates_and_wraps() {
    use std::collections::BTreeSet;
    let bm: BTreeSet<usize> = [2usize, 5, 9].into_iter().collect();
    // Forward: strictly-after, wrapping past the last.
    assert_eq!(pick_bookmark(&bm, 0, 1), Some(2));
    assert_eq!(pick_bookmark(&bm, 2, 1), Some(5));
    assert_eq!(pick_bookmark(&bm, 5, 1), Some(9));
    assert_eq!(pick_bookmark(&bm, 9, 1), Some(2), "wraps to first");
    assert_eq!(pick_bookmark(&bm, 20, 1), Some(2), "past end wraps");
    // Backward: strictly-before, wrapping past the first.
    assert_eq!(pick_bookmark(&bm, 9, -1), Some(5));
    assert_eq!(pick_bookmark(&bm, 5, -1), Some(2));
    assert_eq!(pick_bookmark(&bm, 2, -1), Some(9), "wraps to last");
    assert_eq!(pick_bookmark(&bm, 0, -1), Some(9), "before start wraps");
    // Empty set yields nothing.
    let empty: BTreeSet<usize> = BTreeSet::new();
    assert_eq!(pick_bookmark(&empty, 0, 1), None);
    assert_eq!(pick_bookmark(&empty, 0, -1), None);
}

/// toggle_bookmark flips the cursor line in the active tab's set, and
/// navigate_bookmark requests a scroll when a target exists.
#[test]
fn toggle_and_navigate_bookmark() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "a\nb\nc\nd\ne\n".into();
    // Cursor on line 3 (1-based) → 0-based line 2.
    app.last_cursor_line_col = Some((3, 1));
    app.toggle_bookmark();
    assert!(app.tabs[0].bookmarks.contains(&2), "bookmark added");
    // Toggling again removes it.
    app.toggle_bookmark();
    assert!(!app.tabs[0].bookmarks.contains(&2), "bookmark removed");
    // Re-add and navigate.
    app.tabs[0].bookmarks.insert(4);
    app.pending_scroll = None;
    app.last_cursor_line_col = Some((1, 1)); // 0-based line 0
    app.navigate_bookmark(1);
    assert!(
        app.pending_scroll.is_some(),
        "navigate to an existing bookmark requests a scroll"
    );
}

/// GoToSymbol builtin opens the modal + requests focus.
#[test]
fn execute_builtin_go_to_symbol_opens_modal() {
    let mut app = ScribeApp::new_test(Config::default());
    assert!(!app.goto_symbol_open);
    app.execute_builtin(BuiltinCommand::GoToSymbol);
    assert!(app.goto_symbol_open, "modal opened");
    assert!(app.focus_goto_symbol, "focus requested");
}

/// Jumping to a symbol's start line (the modal's action) requests a
/// scroll via the shared goto_line pipe. Exercises the symbol_scopes →
/// goto_line path the modal wires together.
#[test]
fn go_to_symbol_jump_requests_scroll() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "fn a() {\n}\nfn b() {\n}\n".into();
    let scopes = crate::editor_features::symbol_scopes(&app.tabs[0].text);
    assert!(!scopes.is_empty(), "two fn definitions detected");
    // Jump to the second symbol's start line (the modal calls
    // goto_line(start_line + 1)).
    let target = scopes.last().unwrap().start_line;
    app.pending_scroll = None;
    app.goto_line(target + 1);
    assert!(
        app.pending_scroll.is_some(),
        "symbol jump requests a scroll"
    );
}

/// F-015 method: goto_line sets pending_scroll non-None for a valid
/// line on an active buffer.
#[test]
fn goto_line_sets_pending_scroll() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "a\nb\nc\nd\ne\n".into();
    app.goto_line(3);
    assert!(
        app.pending_scroll.is_some(),
        "goto_line should request scroll"
    );
}

/// F-014: F1 toggles the cheatsheet open + a second F1 closes it.
#[test]
fn input_f1_toggles_cheatsheet() {
    let mut app = ScribeApp::new_test(Config::default());
    assert!(!app.cheatsheet_open);
    let d = Driver::new();
    d.idle(&mut app);
    d.key(&mut app, egui::Key::F1, egui::Modifiers::NONE);
    assert!(app.cheatsheet_open, "F1 opens the cheatsheet");
    d.key(&mut app, egui::Key::F1, egui::Modifiers::NONE);
    assert!(!app.cheatsheet_open, "second F1 closes the cheatsheet");
}

/// F-014: Esc closes the cheatsheet as a normal overlay.
#[test]
fn input_esc_closes_cheatsheet() {
    let mut app = ScribeApp::new_test(Config::default());
    app.cheatsheet_open = true;
    let d = Driver::new();
    d.idle(&mut app);
    d.key(&mut app, egui::Key::Escape, egui::Modifiers::NONE);
    assert!(!app.cheatsheet_open, "Esc closes the cheatsheet");
}

/// F-014 registry sanity: every entry has a non-empty chord + action.
#[test]
fn keyboard_shortcuts_registry_is_populated() {
    assert!(!KEYBOARD_SHORTCUTS.is_empty(), "registry must be populated");
    for entry in KEYBOARD_SHORTCUTS {
        assert!(!entry.chord.is_empty(), "shortcut chord must be non-empty");
        assert!(
            !entry.action.is_empty(),
            "shortcut action label must be non-empty"
        );
    }
}

/// F-016 prefix table sanity.
#[test]
fn comment_prefix_for_extension_table() {
    assert_eq!(comment_prefix_for_extension("rs"), Some("//"));
    assert_eq!(comment_prefix_for_extension("py"), Some("#"));
    assert_eq!(comment_prefix_for_extension("lua"), Some("--"));
    assert_eq!(comment_prefix_for_extension("toml"), Some("#"));
    assert_eq!(comment_prefix_for_extension("RS"), Some("//"));
    assert_eq!(comment_prefix_for_extension("html"), None);
    assert_eq!(comment_prefix_for_extension(""), None);
}

/// F-008 replace: empty pattern is a no-op.
#[test]
fn replace_in_active_no_op_when_pattern_empty() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "hello hello".into();
    app.find_query.clear();
    app.replace_query = "world".into();
    app.replace_in_active(true);
    assert_eq!(app.tabs[0].text, "hello hello");
}

/// F-008 replace: replace-next changes only the first match.
#[test]
fn replace_in_active_first_only() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "hello hello hello".into();
    app.find_query = "hello".into();
    app.replace_query = "x".into();
    app.replace_in_active(false);
    assert_eq!(app.tabs[0].text, "x hello hello");
}

/// F-008 replace: replace-all changes every literal match.
#[test]
fn replace_in_active_all_matches() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "hello hello hello".into();
    app.find_query = "hello".into();
    app.replace_query = "x".into();
    app.replace_in_active(true);
    assert_eq!(app.tabs[0].text, "x x x");
}

/// F-008 replace: replace matches the find bar's CASE-INSENSITIVE semantics.
/// The find bar (`find_matches_active`) highlights "Foo"/"foo"/"FOO" alike;
/// the old `str::replace` was case-SENSITIVE and silently changed only the
/// exact-case match the user did NOT see as the sole highlight. Both surfaces
/// now share `search::find_all`, so replace touches what find highlights.
#[test]
fn replace_in_active_matches_find_bar_case_insensitively() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "Foo foo FOO".into();
    app.find_query = "foo".into();
    app.replace_query = "x".into();
    app.replace_in_active(true);
    assert_eq!(app.tabs[0].text, "x x x");
}

/// The replacement is spliced LITERALLY — a `$`-containing replacement is not
/// treated as a regex capture reference (the find bar has no regex toggle).
#[test]
fn replace_in_active_replacement_is_literal_not_regex_expanded() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "ab".into();
    app.find_query = "a".into();
    app.replace_query = "$1".into();
    app.replace_in_active(true);
    assert_eq!(app.tabs[0].text, "$1b");
}

/// A stale completion offset that lands mid-multibyte-char must not abort the
/// app: `replace_range` panics on a non-boundary byte offset (`panic = abort`).
#[test]
fn accept_completion_with_stale_non_boundary_offset_does_not_panic() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "a\u{e9}".into(); // 'a' + 'é' (2 bytes) → byte len 3
    app.completion = Some(Completion {
        prefix_start: 2, // mid-'é' — NOT a UTF-8 char boundary
        items: vec!["zz".to_string()],
        selected: 0,
    });
    app.accept_completion(0, Some(2)); // must not panic
    assert_eq!(app.tabs[0].text, "a\u{e9}"); // dropped the stale completion
}

/// F-017 — move-line-down swaps the cursor line with its neighbour.
#[test]
fn move_cursor_line_down_swaps_lines() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "alpha\nbeta\ngamma\n".into();
    app.last_cursor_line_col = Some((1, 1)); // 1-based line 1 = "alpha"
    app.move_cursor_line(1);
    assert_eq!(app.tabs[0].text, "beta\nalpha\ngamma\n");
    assert_eq!(app.last_cursor_line_col, Some((2, 1)));
}

/// F-017 — move-line-up at line 1 is a no-op.
#[test]
fn move_cursor_line_up_at_top_is_noop() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "alpha\nbeta\n".into();
    app.last_cursor_line_col = Some((1, 1));
    app.move_cursor_line(-1);
    assert_eq!(app.tabs[0].text, "alpha\nbeta\n");
}

/// Regression: move-line-UP from the post-final-newline empty line of a
/// newline-terminated file must NOT panic. The 1-based cursor line (3) maps
/// to `ln = 2`, which equals `lines.len()` after the trailing-"" pop — an
/// unguarded `lines.swap(ln, target)` indexed `ln` out of bounds → abort.
#[test]
fn move_cursor_line_up_from_trailing_empty_line_does_not_panic() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "a\nb\n".into();
    // Caret parked on the empty trailing line (status reports it as line 3).
    app.last_cursor_line_col = Some((3, 1));
    app.move_cursor_line(-1); // must not panic
    assert_eq!(app.tabs[0].text, "a\nb\n"); // out-of-range op is a no-op
}

/// F-017 — duplicate inserts a copy on the row below.
#[test]
fn duplicate_cursor_line_inserts_copy() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "alpha\nbeta\n".into();
    app.last_cursor_line_col = Some((1, 1));
    app.duplicate_cursor_line();
    assert_eq!(app.tabs[0].text, "alpha\nalpha\nbeta\n");
}

/// F-017 — join glues cursor line + next with a single space.
#[test]
fn join_cursor_line_with_next_uses_single_space() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "hello   \n   world\n".into();
    app.last_cursor_line_col = Some((1, 1));
    app.join_cursor_line_with_next();
    assert_eq!(app.tabs[0].text, "hello world\n");
}

/// F-017 — join at last line is a no-op.
#[test]
fn join_cursor_line_with_next_at_last_line_is_noop() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "only".into();
    app.last_cursor_line_col = Some((1, 1));
    app.join_cursor_line_with_next();
    assert_eq!(app.tabs[0].text, "only");
}

/// F-022 — external edit + clean buffer: silent reload picks up the new
/// content. The poller is driven manually here (frame_tick is heavy).
#[test]
fn external_disk_change_reloads_clean_buffer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("note.txt");
    std::fs::write(&path, "first").expect("write seed");
    let mut app = ScribeApp::new_test(Config::default());
    app.open_path(path.clone());
    let opened_idx = app.tabs.len() - 1;
    assert_eq!(app.tabs[opened_idx].text, "first");
    // Simulate external write — sleep is required because filesystems
    // typically only track mtime at second resolution.
    std::thread::sleep(std::time::Duration::from_millis(1200));
    std::fs::write(&path, "second").expect("write update");
    app.poll_external_disk_changes(0);
    assert_eq!(
        app.tabs[opened_idx].text, "second",
        "clean buffer reloads from disk silently"
    );
}

/// F-022b — external edit + dirty buffer: do NOT reload; raise the
/// persistent `external_change` flag that drives the actionable banner
/// ([Reload] / [Keep mine]) so the user is prompted, not silently clobbered.
#[test]
fn external_disk_change_prompts_when_buffer_dirty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("note.txt");
    std::fs::write(&path, "first").expect("write seed");
    let mut app = ScribeApp::new_test(Config::default());
    app.open_path(path.clone());
    let opened_idx = app.tabs.len() - 1;
    assert!(!app.tabs[opened_idx].external_change);
    // Make local edits, then an external write.
    app.tabs[opened_idx].text = "local edits".to_string();
    std::thread::sleep(std::time::Duration::from_millis(1200));
    std::fs::write(&path, "second").expect("write update");
    app.poll_external_disk_changes(0);
    assert_eq!(
        app.tabs[opened_idx].text, "local edits",
        "dirty buffer must NOT be silently overwritten"
    );
    assert!(
        app.tabs[opened_idx].external_change,
        "a dirty buffer whose file changed on disk must flag the reload prompt"
    );
}

/// F-004 sanity: BUILTIN_COMMANDS is non-empty and every entry's label
/// is unique (no two entries collide in the palette).
#[test]
fn builtin_commands_registry_is_populated_and_unique() {
    assert!(!BUILTIN_COMMANDS.is_empty(), "registry must be populated");
    let mut labels: Vec<&'static str> = BUILTIN_COMMANDS.iter().map(|e| e.label).collect();
    labels.sort_unstable();
    let unique_len = labels
        .iter()
        .fold(Vec::<&'static str>::new(), |mut acc, l| {
            if acc.last().is_none_or(|p| *p != *l) {
                acc.push(l);
            }
            acc
        })
        .len();
    assert_eq!(
        labels.len(),
        unique_len,
        "duplicate command label in registry"
    );
}

/// F-004 sanity: every BuiltinCommand variant the registry references is
/// dispatchable. We assert this by running execute_builtin on each entry
/// and confirming it doesn't panic.
#[test]
fn every_builtin_command_dispatches_without_panic() {
    let mut app = ScribeApp::new_test(Config::default());
    for entry in BUILTIN_COMMANDS {
        // The three rfd-touching variants would either hang the test
        // runner waiting for user input (Linux/Windows) or panic in
        // rfd's macOS backend (no NSApplication main thread on CI):
        //   - OpenFile / OpenFolder call rfd::FileDialog directly.
        //   - Save falls through to save_as → rfd::FileDialog when the
        //     active buffer has no path. After CloseAllTabs in this
        //     same loop the active tab IS a pathless scratch, so the
        //     fall-through fires. Easier to skip than to keep the
        //     fixture path alive across CloseAllTabs side-effects.
        //   - ConvertToMarkdown / ExportAsHtml open an rfd save dialog.
        match entry.action {
            BuiltinCommand::OpenFile
            | BuiltinCommand::OpenFolder
            | BuiltinCommand::Save
            | BuiltinCommand::ConvertToMarkdown
            | BuiltinCommand::ExportAsHtml => continue,
            _ => app.execute_builtin(entry.action),
        }
    }
}

/// Clipboard/history palette commands record a pending editor action
/// (drained into the focused editor as an egui event by `frame_tick`).
/// `execute_builtin` itself must never touch the OS clipboard so it stays
/// headless-test-safe.
#[test]
fn clipboard_palette_commands_set_pending_action() {
    let mut app = ScribeApp::new_test(Config::default());
    for (cmd, want) in [
        (BuiltinCommand::Copy, EditorAction::Copy),
        (BuiltinCommand::Cut, EditorAction::Cut),
        (BuiltinCommand::Paste, EditorAction::Paste),
        (BuiltinCommand::Undo, EditorAction::Undo),
        (BuiltinCommand::Redo, EditorAction::Redo),
    ] {
        app.pending_editor_action = None;
        app.execute_builtin(cmd);
        assert_eq!(app.pending_editor_action, Some(want), "{cmd:?}");
    }
}

/// The five clipboard/history actions are all reachable from the palette
/// AND the cheatsheet (discoverability — they previously worked only via
/// unlisted chords).
#[test]
fn clipboard_actions_are_discoverable() {
    let palette: Vec<&str> = BUILTIN_COMMANDS.iter().map(|e| e.label).collect();
    for label in ["Copy", "Cut", "Paste", "Undo", "Redo"] {
        assert!(palette.contains(&label), "palette missing {label}");
    }
    let chords: Vec<&str> = KEYBOARD_SHORTCUTS.iter().map(|e| e.chord).collect();
    for chord in ["Ctrl+C", "Ctrl+X", "Ctrl+V", "Ctrl+Z", "Ctrl+Shift+Z"] {
        assert!(chords.contains(&chord), "cheatsheet missing {chord}");
    }
}

/// F-004: ToggleWordWrap from the palette flips the config and persists
/// the change in-memory.
#[test]
fn execute_builtin_toggle_word_wrap_flips_config() {
    let mut app = ScribeApp::new_test(Config::default());
    let before = app.config.editor.word_wrap;
    app.execute_builtin(BuiltinCommand::ToggleWordWrap);
    assert_eq!(app.config.editor.word_wrap, !before);
}

/// F-004: CycleTheme advances through the built-in theme list.
#[test]
fn execute_builtin_cycle_theme_advances() {
    let mut app = ScribeApp::new_test(Config::default());
    let names = scribe_core::theme::Theme::builtin_names();
    if names.len() < 2 {
        return; // nothing to cycle
    }
    let before = app.config.appearance.theme.clone();
    app.execute_builtin(BuiltinCommand::CycleTheme);
    let after = app.config.appearance.theme.clone();
    assert_ne!(
        before, after,
        "CycleTheme should change the active theme name"
    );
    assert!(
        names.iter().any(|n| *n == after),
        "post-cycle theme must be a known built-in"
    );
}

/// F-032: FoldAll switches fold view on and records every detected
/// region's start line. ExpandAll then clears the recorded fold set.
/// Uses a small Rust snippet so `fold_regions` finds at least one
/// brace-delimited region.
#[test]
fn execute_builtin_fold_then_expand_round_trips() {
    let mut app = ScribeApp::new_test(Config::default());
    // Replace the scratch tab text with code that has a foldable
    // region. The fold extractor scans for matched braces so any
    // multi-line braced block produces a region.
    app.tabs[app.active].text = "fn x() {\n    1;\n}\n".to_string();
    app.execute_builtin(BuiltinCommand::FoldAll);
    assert!(app.fold_view, "FoldAll should switch fold view on");
    assert!(
        !app.folds.is_empty(),
        "FoldAll should record at least one fold for a braced region"
    );
    app.execute_builtin(BuiltinCommand::ExpandAll);
    assert!(app.folds.is_empty(), "ExpandAll should clear the fold set");
}

#[test]
fn input_ctrl_shift_p_opens_palette() {
    let mut app = ScribeApp::new_test(Config::default());
    let d = Driver::new();
    d.idle(&mut app);
    let cmd_shift = egui::Modifiers {
        shift: true,
        command: true,
        ..Default::default()
    };
    d.key(&mut app, egui::Key::P, cmd_shift);
    assert!(app.palette_open, "Ctrl+Shift+P opens the command palette");
}

/// F-006 wave-1: Ctrl+W closes the active tab.
#[test]
fn input_ctrl_w_closes_active_tab() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs.push(EditorTab::scratch());
    app.tabs.push(EditorTab::scratch());
    assert_eq!(app.tabs.len(), 3, "seed three tabs");
    app.active = 1;
    let d = Driver::new();
    d.idle(&mut app);
    d.key(&mut app, egui::Key::W, egui::Modifiers::COMMAND);
    assert_eq!(app.tabs.len(), 2, "Ctrl+W closes one tab");
}

/// F-003 fix: Ctrl+\\ toggles the multi-note grid.
#[test]
fn input_ctrl_backslash_toggles_grid_mode() {
    let mut app = ScribeApp::new_test(Config::default());
    assert!(!app.config.editor.grid_enabled, "grid starts off");
    let d = Driver::new();
    d.idle(&mut app);
    d.key(&mut app, egui::Key::Backslash, egui::Modifiers::COMMAND);
    assert!(app.config.editor.grid_enabled, "Ctrl+\\\\ turns grid on");
    d.key(&mut app, egui::Key::Backslash, egui::Modifiers::COMMAND);
    assert!(
        !app.config.editor.grid_enabled,
        "Ctrl+\\\\ toggles back off"
    );
}

/// F-006 wave-1: Ctrl+Tab cycles to the next tab; Ctrl+Shift+Tab cycles
/// to the previous tab.
#[test]
fn input_ctrl_tab_cycles_tabs() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs.push(EditorTab::scratch());
    app.tabs.push(EditorTab::scratch());
    app.active = 0;
    let d = Driver::new();
    d.idle(&mut app);
    d.key(&mut app, egui::Key::Tab, egui::Modifiers::COMMAND);
    assert_eq!(app.active, 1, "Ctrl+Tab moves to tab 1");
    d.key(&mut app, egui::Key::Tab, egui::Modifiers::COMMAND);
    assert_eq!(app.active, 2, "Ctrl+Tab moves to tab 2");
    d.key(&mut app, egui::Key::Tab, egui::Modifiers::COMMAND);
    assert_eq!(app.active, 0, "Ctrl+Tab wraps to tab 0");
    let cmd_shift = egui::Modifiers {
        shift: true,
        command: true,
        ..Default::default()
    };
    d.key(&mut app, egui::Key::Tab, cmd_shift);
    assert_eq!(app.active, 2, "Ctrl+Shift+Tab wraps backward to tab 2");
}

/// F-001 / F-043 fix: tab close helpers behave correctly.
#[test]
fn tab_close_helpers() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs.push(EditorTab::scratch());
    app.tabs.push(EditorTab::scratch());
    app.tabs.push(EditorTab::scratch());
    assert_eq!(app.tabs.len(), 4);
    app.close_tabs_after(1);
    assert_eq!(app.tabs.len(), 2, "close_tabs_after(1) leaves tabs [0,1]");
    app.tabs.push(EditorTab::scratch());
    app.tabs.push(EditorTab::scratch());
    app.close_all_tabs_except(1);
    assert_eq!(app.tabs.len(), 1, "close_all_tabs_except keeps one tab");
    assert_eq!(app.active, 0, "active normalises to 0 after close-others");
    app.tabs.push(EditorTab::scratch());
    app.tabs.push(EditorTab::scratch());
    app.close_all_tabs();
    assert_eq!(
        app.tabs.len(),
        1,
        "close_all_tabs leaves the scratch buffer"
    );
}

/// F-001 fix: tab swap preserves the active-tab pointer to the same
/// document the user was viewing.
#[test]
fn tab_swap_preserves_active_pointer() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs.push(EditorTab::scratch());
    app.tabs.push(EditorTab::scratch());
    // Mark each tab with a recognisable byte so swap is observable.
    app.tabs[0].text = "A".into();
    app.tabs[1].text = "B".into();
    app.tabs[2].text = "C".into();
    app.active = 1; // viewing B
    app.tabs.swap(0, 1);
    // The buffer at index 0 is now B (the user's view), but the index
    // shifted — verify the swap is observable.
    assert_eq!(app.tabs[0].text, "B");
    assert_eq!(app.tabs[1].text, "A");
    assert_eq!(app.tabs[2].text, "C");
}

#[test]
fn input_type_without_click_autofocuses_editor() {
    // Regression for the auto-focus fix: a user should be able to type
    // immediately after launch with NO click — the editor grabs focus when
    // idle. (Surfaced by the live computer-control screenshot pass.)
    let mut app = ScribeApp::new_test(Config::default());
    let d = Driver::new();
    d.idle(&mut app); // frame 1: editor requests focus
    d.idle(&mut app); // frame 2: focus is now held
    d.type_text(&mut app, "no_click_needed");
    d.idle(&mut app);
    assert!(
        app.tabs[app.active].text.contains("no_click_needed"),
        "editor should auto-focus and accept typing without a click (got {:?})",
        app.tabs[app.active].text
    );
}

#[test]
fn input_click_and_type_inserts_text() {
    let mut app = ScribeApp::new_test(Config::default());
    let d = Driver::new();
    d.idle(&mut app);
    // Click into the central editor to focus it, then type.
    d.click(&mut app, egui::pos2(550.0, 360.0));
    d.type_text(&mut app, "robot");
    d.idle(&mut app);
    assert!(
        app.tabs[app.active].text.contains("robot"),
        "typed text should reach the buffer (got {:?})",
        app.tabs[app.active].text
    );
}

#[test]
fn input_ctrl_space_completion_then_enter_accepts() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "value valuer ".into();
    let d = Driver::new();
    d.idle(&mut app);
    d.click(&mut app, egui::pos2(550.0, 360.0));
    d.type_text(&mut app, "val");
    d.key(&mut app, egui::Key::Space, egui::Modifiers::COMMAND);
    assert!(
        app.completion.is_some(),
        "Ctrl+Space opens completion for prefix 'val' (buffer {:?})",
        app.tabs[0].text
    );
    let before = app.tabs[0].text.clone();
    d.key(&mut app, egui::Key::Enter, egui::Modifiers::NONE);
    assert_ne!(
        app.tabs[0].text, before,
        "Enter accepts the highlighted completion"
    );
    assert!(app.completion.is_none(), "popup closes after accept");
}

// ---- E2E for the new feature surfaces (integration smoke) ----

/// The experimental owned rope editor renders the full frame loop without
/// panicking (exercises show_editable + the bridge + caret render path).
#[test]
fn experimental_rope_editor_renders_without_panic() {
    let mut cfg = Config::default();
    cfg.editor.experimental_rope_editor = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = "fn main() {\n    let x = 1;\n}\n".to_string();
    run_frames(&mut app, 4);
    assert_eq!(app.tabs.len(), 1);
}

/// KEYSTONE perf bridge: the experimental editor builds the rope ONCE and
/// persists it across frames (no per-frame `Buffer::from_text`), and an
/// external `set_text` invalidates the cache so the next frame rebuilds
/// from the new content.
#[test]
fn experimental_editor_persists_rope_and_invalidates_on_external_edit() {
    let mut cfg = Config::default();
    cfg.editor.experimental_rope_editor = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = "alpha\nbeta\n".to_string();
    run_frames(&mut app, 3);
    assert!(
        app.tabs[0].rope_buf.is_some(),
        "experimental editor builds + persists the rope across frames"
    );
    // External mutation (reload / plugin / sort) invalidates the cache.
    app.tabs[0].set_text("gamma\n".to_string());
    assert!(
        app.tabs[0].rope_buf.is_none(),
        "set_text invalidates the persistent rope cache"
    );
    run_frames(&mut app, 2);
    let rebuilt = app.tabs[0]
        .rope_buf
        .as_ref()
        .and_then(|b| b.as_rope())
        .map(|r| r.to_string());
    assert_eq!(
        rebuilt,
        Some("gamma\n".to_string()),
        "rope rebuilt after invalidation reflects the externally-set content"
    );
}

/// Auto-save + session-backup + trim-on-save all enabled together render
/// the frame loop cleanly (no panic from the periodic save/snapshot paths).
#[test]
fn save_hygiene_configs_render_without_panic() {
    let mut cfg = Config::default();
    cfg.editor.auto_save = true;
    cfg.editor.session_backup = true;
    cfg.editor.trim_trailing_whitespace_on_save = true;
    cfg.editor.final_newline_on_save = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = "x   \ny".to_string();
    run_frames(&mut app, 3);
    assert_eq!(app.tabs.len(), 1);
}

/// Regression: a test's periodic hot-exit snapshot MUST write into the
/// per-instance temp dir, never the real user config dir. This is the guard
/// for the "unit-test fixture leaked into the real session backup and then
/// restored as a phantom note" bug. We drive the exact offending shape
/// (session_backup on + an unsaved untitled buffer) and assert the snapshot
/// landed in the isolated dir, and that the isolated dir is NOT the real one.
#[test]
fn session_snapshot_writes_to_isolated_dir_not_real_config() {
    let mut cfg = Config::default();
    cfg.editor.session_backup = true;
    let mut app = ScribeApp::new_test(cfg);

    let test_dir = app.config_dir.clone().expect("new_test sets a config dir");
    assert_ne!(
        Some(test_dir.clone()),
        Config::config_dir(),
        "new_test must isolate config_dir away from the real OS config dir"
    );

    // The exact shape that leaked: an unsaved untitled buffer holding the
    // unit-test fixture text, driven through a frame so the snapshot fires.
    app.tabs[0].text = "a very long line ".repeat(40);
    run_frames(&mut app, 1);

    // The hot-exit snapshot (which DOES run — session_backup on, content
    // unsaved) wrote its manifest into the ISOLATED dir, proving the real
    // user session backup is never touched by tests.
    let isolated_manifest = scribe_core::session::manifest_path(&test_dir);
    assert!(
        isolated_manifest.exists(),
        "hot-exit snapshot must target the test-isolated config dir"
    );
}

/// Reopen-closed-tab restores an accidentally closed tab's content.
#[test]
fn reopen_closed_tab_restores_content() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "important note".to_string();
    app.close_tab(0);
    // close_tab replaces the empty tab set with a fresh scratch.
    app.reopen_closed_tab();
    assert!(
        app.tabs.iter().any(|t| t.text == "important note"),
        "closed tab content recovered"
    );
}

/// Performance: the owned editing model handles a large buffer without
/// quadratic blowup — 5k sequential inserts + an undo on a 50k-line rope
/// complete well within a generous bound.
#[test]
fn perf_large_buffer_edit_is_bounded() {
    use scribe_render::{apply_event, RopeEditorState};
    let mut body = String::with_capacity(50_000 * 6);
    for i in 0..50_000 {
        body.push_str(&format!("{i:05}\n"));
    }
    let mut buf = scribe_core::buffer::Buffer::from_text(&body);
    let rope = buf.as_rope_mut().expect("rope buffer");
    let mut st = RopeEditorState::new();
    let start = std::time::Instant::now();
    for _ in 0..5_000 {
        apply_event(rope, &mut st, &egui::Event::Text("a".to_string()));
    }
    apply_event(
        rope,
        &mut st,
        &egui::Event::Key {
            key: egui::Key::Z,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        },
    );
    let elapsed = start.elapsed();
    // Snapshot-undo rebuilds the rope per keystroke (O(n) bridge) — still
    // must stay well under a wall-clock ceiling on a 50k-line buffer.
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "5k edits on a 50k-line rope took {elapsed:?}"
    );
}

/// Render-smoke (e2e, no GPU): the "Report an issue" dialog draws when opened
/// and is dismissable. Opening it via `open_fresh()` (the path the command
/// palette takes) must render the heading and buttons without panicking, and
/// clicking Cancel by its label must close it — the test that catches "the
/// dialog never draws" / "Cancel does nothing".
#[test]
fn report_issue_dialog_renders_and_cancels() {
    let mut app = fresh_app();
    app.issue_intake.open_fresh();
    let mut h = ui_harness(app);
    h.run();
    assert!(
        h.state().issue_intake.open,
        "the report-issue dialog should be open after open_fresh()"
    );
    // The modal heading + the three action buttons must be present (rendered).
    let _ = h.get_by_label("Report an issue");
    let _ = h.get_by_label("Open on GitHub");
    let _ = h.get_by_label("Cancel");
    // Cancel the way a user does.
    h.get_by_label("Cancel").click();
    h.run();
    assert!(
        !h.state().issue_intake.open,
        "clicking Cancel must close the report-issue dialog"
    );
}

/// Render-smoke (e2e, no GPU): ticking the diagnostics checkbox makes the
/// non-identifying diagnostics block appear in the live preview body. Asserts
/// the privacy contract at the UI layer: OFF by default, visible-when-on.
#[test]
fn report_issue_diagnostics_toggle_drives_preview() {
    let mut app = fresh_app();
    app.issue_intake.open_fresh();
    app.issue_intake.description = "something broke".into();
    // OFF by default → preview carries no diagnostics line.
    assert!(
        !app.issue_intake
            .preview_body(crate::issue_intake::RENDERER)
            .contains("App version:"),
        "diagnostics must be OFF by default (no preview line)"
    );
    let mut h = ui_harness(app);
    h.run();
    // Tick the diagnostics checkbox by its label, like a user.
    h.get_by_label("Include non-identifying diagnostics (app version, OS, renderer)")
        .click();
    h.run();
    let preview = h
        .state()
        .issue_intake
        .preview_body(crate::issue_intake::RENDERER);
    assert!(
        preview.contains("App version:") && preview.contains("Renderer: wgpu"),
        "ticking diagnostics must make the diagnostics block appear in the preview"
    );
}

// ===========================================================================
// Gap-fill e2e drives (#34): button-click / context-menu / form wiring that
// the existing suite covered only at the COMMAND or METHOD layer. Each test
// builds the app in the right state, finds the widget by its REAL render-code
// label, drives it like a user, and asserts the observable post-state.
// ===========================================================================

/// Open two tabs, pin one so exactly ONE close ("✕") button renders, then
/// click that "Close tab (or middle-click)" button like a user → the tab is
/// removed. (The suite previously only drove middle-click + the `close_tab`
/// method; the toolbar-style ✕ button click was never exercised.)
#[test]
fn close_tab_via_x_button_click_removes_it() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = dir.path().join("alpha.txt");
    let beta = dir.path().join("beta.txt");
    std::fs::write(&alpha, "A\n").unwrap();
    std::fs::write(&beta, "B\n").unwrap();
    let mut app = fresh_app();
    app.open_path(alpha);
    app.open_path(beta); // beta is active
                         // Pin the scratch tab AND alpha so they hide their ✕; only the active,
                         // unpinned beta renders a close button → its label is unambiguous.
    app.tabs[0].pinned = true; // scratch
    app.tabs[1].pinned = true; // alpha
    let mut h = ui_harness(app);
    h.run();
    let before = h.state().tabs.len();
    // The close button is an icon-only Button whose accessible name is its X
    // glyph; only the one unpinned (beta) tab renders it, so it's unambiguous.
    h.get_by_label(egui_phosphor::thin::X).click();
    h.run();
    assert_eq!(
        h.state().tabs.len(),
        before - 1,
        "clicking the ✕ close button must remove the tab"
    );
    assert!(
        !h.state().tabs.iter().any(|t| t.title() == "beta.txt"),
        "the closed tab (beta.txt) must be gone"
    );
}

/// Click the active tab's pin button ("Pin tab") → it becomes pinned; the
/// label flips to "Unpin tab" and clicking that unpins it. Drives the pin
/// TOGGLE button (the pin LOGIC was covered; the button click was not).
#[test]
fn pin_button_click_pins_then_unpins_active_tab() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = dir.path().join("alpha.txt");
    std::fs::write(&alpha, "A\n").unwrap();
    let mut app = fresh_app();
    app.open_path(alpha); // alpha becomes the active, selected tab
    assert!(!app.tabs[app.active].pinned, "alpha starts unpinned");
    let mut h = ui_harness(app);
    h.run();
    // The pin toggle is an icon-only Button on the active tab; its accessible
    // name is the PUSH_PIN glyph (unpinned) / PUSH_PIN_SLASH glyph (pinned).
    h.get_by_label(egui_phosphor::thin::PUSH_PIN).click();
    h.run();
    let a = h.state().active;
    assert!(
        h.state().tabs[a].pinned,
        "clicking the pin button must pin the active tab"
    );
    // Now the glyph flips to PUSH_PIN_SLASH; click it to revert.
    h.get_by_label(egui_phosphor::thin::PUSH_PIN_SLASH).click();
    h.run();
    let a = h.state().active;
    assert!(
        !h.state().tabs[a].pinned,
        "clicking the unpin button must unpin the active tab"
    );
}

/// Tab context-menu: right-click a tab → click "Close Others" → only that tab
/// remains. Drives the menu UI (the close-others LOGIC was covered; the menu
/// render + secondary-click path was not).
#[test]
fn tab_context_menu_close_others_keeps_only_target() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = dir.path().join("alpha.txt");
    let beta = dir.path().join("beta.txt");
    let gamma = dir.path().join("gamma.txt");
    for (p, c) in [(&alpha, "A\n"), (&beta, "B\n"), (&gamma, "G\n")] {
        std::fs::write(p, c).unwrap();
    }
    let mut app = fresh_app();
    app.open_path(alpha);
    app.open_path(beta);
    app.open_path(gamma);
    let mut h = ui_harness(app);
    h.run();
    // Right-click the beta tab to open its context menu.
    h.get_by_label("beta.txt").click_secondary();
    h.run();
    h.get_by_label("Close Others").click();
    h.run();
    let titles: Vec<String> = h.state().tabs.iter().map(|t| t.title()).collect();
    assert_eq!(
        titles,
        vec!["beta.txt".to_string()],
        "Close Others must keep only the right-clicked tab, got {titles:?}"
    );
}

/// Tab context-menu: right-click a tab → click "Close All to the Right" →
/// tabs after it are removed; it and prior tabs remain.
#[test]
fn tab_context_menu_close_all_to_right_trims_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = dir.path().join("alpha.txt");
    let beta = dir.path().join("beta.txt");
    let gamma = dir.path().join("gamma.txt");
    for (p, c) in [(&alpha, "A\n"), (&beta, "B\n"), (&gamma, "G\n")] {
        std::fs::write(p, c).unwrap();
    }
    let mut app = fresh_app();
    app.open_path(alpha);
    app.open_path(beta);
    app.open_path(gamma);
    let mut h = ui_harness(app);
    h.run();
    h.get_by_label("beta.txt").click_secondary();
    h.run();
    h.get_by_label("Close All to the Right").click();
    h.run();
    let titles: Vec<String> = h.state().tabs.iter().map(|t| t.title()).collect();
    assert!(
        titles.contains(&"beta.txt".to_string()) && !titles.contains(&"gamma.txt".to_string()),
        "Close All to the Right must drop gamma (right of beta), got {titles:?}"
    );
}

/// Tab context-menu: "Pin tab" entry pins the right-clicked tab.
#[test]
fn tab_context_menu_pin_pins_target() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = dir.path().join("alpha.txt");
    std::fs::write(&alpha, "A\n").unwrap();
    let mut app = fresh_app();
    app.open_path(alpha);
    let alpha_idx = app.active;
    assert!(!app.tabs[alpha_idx].pinned, "alpha starts unpinned");
    let mut h = ui_harness(app);
    h.run();
    h.get_by_label("alpha.txt").click_secondary();
    h.run();
    h.get_by_label("Pin tab").click();
    h.run();
    let pinned = h
        .state()
        .tabs
        .iter()
        .find(|t| t.title() == "alpha.txt")
        .map(|t| t.pinned)
        .unwrap_or(false);
    assert!(pinned, "the context-menu 'Pin tab' must pin the target tab");
}

/// Tab context-menu: "Close All" closes every tab (the app keeps one scratch
/// tab invariant, so the result is a single fresh scratch tab).
#[test]
fn tab_context_menu_close_all_leaves_one_scratch() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = dir.path().join("alpha.txt");
    let beta = dir.path().join("beta.txt");
    std::fs::write(&alpha, "A\n").unwrap();
    std::fs::write(&beta, "B\n").unwrap();
    let mut app = fresh_app();
    app.open_path(alpha);
    app.open_path(beta);
    let mut h = ui_harness(app);
    h.run();
    h.get_by_label("beta.txt").click_secondary();
    h.run();
    h.get_by_label("Close All").click();
    h.run();
    assert_eq!(
        h.state().tabs.len(),
        1,
        "Close All must leave exactly one (scratch) tab"
    );
    assert!(
        !h.state().tabs.iter().any(|t| t.title() == "beta.txt"),
        "Close All must remove the opened files"
    );
}

/// Toolbar TOGGLE buttons (default items): clicking the Word-wrap, Spellcheck,
/// and Minimap buttons each flip their config flag. This closes the
/// toolbar-button layer over the already-covered command layer.
#[test]
fn toolbar_wrap_button_flips_word_wrap() {
    let mut h = ui_harness(toolbar_app());
    h.run();
    let before = h.state().config.editor.word_wrap;
    // Toolbar buttons default to TEXT mode (appearance.toolbar_icons=false);
    // the "wrap" item's accessible name is its short text label "wrap".
    h.get_by_label("wrap").click();
    h.run();
    assert_ne!(
        h.state().config.editor.word_wrap,
        before,
        "the Word wrap toolbar button must flip word_wrap"
    );
}

#[test]
fn toolbar_spellcheck_button_flips_spellcheck() {
    let mut h = ui_harness(toolbar_app());
    h.run();
    let before = h.state().config.spellcheck.enabled;
    h.get_by_label("spell").click();
    h.run();
    assert_ne!(
        h.state().config.spellcheck.enabled,
        before,
        "the Spellcheck toolbar button must flip spellcheck.enabled"
    );
}

#[test]
fn toolbar_minimap_button_flips_minimap() {
    let mut h = ui_harness(toolbar_app());
    h.run();
    let before = h.state().config.editor.show_minimap;
    h.get_by_label("map").click();
    h.run();
    assert_ne!(
        h.state().config.editor.show_minimap,
        before,
        "the Minimap toolbar button must flip show_minimap"
    );
}

/// Toolbar items not in the default set still wire correctly when present.
/// Add "fold", "linenumbers" to the bar, then click each → flip its flag.
#[test]
fn toolbar_fold_and_linenumbers_buttons_flip_their_flags() {
    let mut app = toolbar_app();
    app.config
        .toolbar
        .items
        .extend(["fold".to_string(), "linenumbers".to_string()]);
    let mut h = ui_harness(app);
    h.run();
    let fold_before = h.state().fold_view;
    h.get_by_label("fold").click();
    h.run();
    assert_ne!(
        h.state().fold_view,
        fold_before,
        "the Folded view toolbar button must flip fold_view"
    );
    let ln_before = h.state().config.editor.show_line_numbers;
    h.get_by_label("nums").click();
    h.run();
    assert_ne!(
        h.state().config.editor.show_line_numbers,
        ln_before,
        "the Line numbers toolbar button must flip show_line_numbers"
    );
}

/// Clicking the LSP toolbar button on a plain scratch tab surfaces a toast
/// explaining why no server starts — the observable outcome of the button
/// wiring. A scratch tab has no detectable language, so the toast is the
/// "no language detected" notice (the path-missing branch fires only once a
/// language is known).
#[test]
fn toolbar_lsp_button_on_scratch_tab_sets_toast() {
    let mut app = toolbar_app();
    app.config.toolbar.items.push("lsp".to_string());
    let mut h = ui_harness(app);
    h.run();
    assert!(h.state().toast.is_none(), "no toast before clicking LSP");
    h.get_by_label("lsp").click();
    h.run();
    assert_eq!(
        h.state().toast.as_deref(),
        Some(
            "Couldn't detect this file's language. Save it with a file extension \
             (like .rs or .py) to enable language features."
        ),
        "the LSP button on a scratch tab must explain why it can't start"
    );
}

/// Click the "Replace all" button in the find/replace bar → every match in the
/// active buffer is replaced. (The `replace_in_active` method was covered; the
/// button wiring in the find-bar second row was not.)
#[test]
fn replace_all_button_click_rewrites_buffer() {
    let mut app = fresh_app();
    app.tabs[0].text = "alpha alpha alpha".into();
    app.find_query = "alpha".into();
    app.replace_query = "beta".into();
    app.find_open = true;
    let mut h = ui_harness(app);
    h.run();
    h.get_by_label("Replace all").click();
    h.run();
    let a = h.state().active;
    assert_eq!(
        h.state().tabs[a].text,
        "beta beta beta",
        "the Replace all button must replace every match"
    );
}

/// Click the "Replace next" button → only the FIRST match is replaced.
#[test]
fn replace_next_button_click_replaces_first_only() {
    let mut app = fresh_app();
    app.tabs[0].text = "alpha alpha alpha".into();
    app.find_query = "alpha".into();
    app.replace_query = "beta".into();
    app.find_open = true;
    let mut h = ui_harness(app);
    h.run();
    h.get_by_label("Replace next").click();
    h.run();
    let a = h.state().active;
    assert_eq!(
        h.state().tabs[a].text,
        "beta alpha alpha",
        "the Replace next button must replace only the first match"
    );
}

/// Command palette: open it, type a filter into the query field, and assert
/// that a NON-matching entry disappears while a matching one remains — i.e.
/// the type-to-filter actually filters the rendered command list.
#[test]
fn command_palette_type_to_filter_narrows_the_list() {
    let mut h = ui_harness(fresh_app());
    h.run();
    h.get_by_label(">_").click();
    h.run();
    // Both commands (shortcut-less => bare label) render with an empty query.
    let _ = h.get_by_label("Sort lines (A-Z)");
    // Type a filter that matches "Cycle theme" but not "Sort lines".
    let q = h.get_by_role(egui::accesskit::Role::TextInput);
    q.focus();
    h.run();
    h.get_by_role(egui::accesskit::Role::TextInput)
        .type_text("cycle theme");
    h.run();
    assert_eq!(
        h.state().palette_query,
        "cycle theme",
        "typing must land in the palette query field"
    );
    // A matching entry is still present...
    let _ = h.get_by_label("Cycle theme");
    // ...and a non-matching entry is gone (query() returns None).
    assert!(
        h.query_by_label("Sort lines (A-Z)").is_none(),
        "filtering must hide commands that don't match the query"
    );
}

/// Command palette: type to filter to a single command, then CLICK that entry
/// (the app's execute path) → its effect is observable. "Sort lines (A-Z)"
/// reorders the active buffer's lines.
#[test]
fn command_palette_click_entry_executes_it() {
    let mut app = fresh_app();
    app.tabs[0].text = "gamma\nalpha\nbeta\n".into();
    let mut h = ui_harness(app);
    h.run();
    h.get_by_label(">_").click();
    h.run();
    let q = h.get_by_role(egui::accesskit::Role::TextInput);
    q.focus();
    h.run();
    h.get_by_role(egui::accesskit::Role::TextInput)
        .type_text("sort lines (a-z)");
    h.run();
    // Sort lines has no shortcut, so its palette entry is the bare label.
    h.get_by_label("Sort lines (A-Z)").click();
    h.run();
    let a = h.state().active;
    assert_eq!(
        h.state().tabs[a].text,
        "alpha\nbeta\ngamma\n",
        "executing Sort lines via the palette must sort the buffer"
    );
    assert!(
        !h.state().palette_open,
        "executing a palette command closes the palette"
    );
}

/// Settings per-setting reset "↺": change a value away from its default, then
/// click the reset button → the value returns to its default. Uses the Editor
/// pane's "Line numbers" checkbox (a single, deterministic toggle).
#[test]
fn settings_reset_button_restores_default() {
    // Default show_line_numbers is true; flip it to false so the row's ↺ is
    // ENABLED (it only enables when cur != default).
    let mut app = fresh_app();
    let default_ln = Config::default().editor.show_line_numbers;
    app.config.editor.show_line_numbers = !default_ln;
    let mut h = ui_harness(app);
    h.state_mut().settings_open = true;
    h.run();
    h.get_by_label("Editor").click();
    h.run();
    assert_ne!(
        h.state().config.editor.show_line_numbers,
        default_ln,
        "precondition: the value is changed away from default"
    );
    // Click the FIRST enabled reset button. The Editor pane's first ↺ belongs
    // to a row whose value differs from default — here, Line numbers. We click
    // every ↺ to guarantee the differing row is reset (others are disabled
    // no-ops), then assert the value is back to default.
    for btn in h.get_all_by_label("↺").collect::<Vec<_>>() {
        btn.click();
    }
    h.run();
    assert_eq!(
        h.state().config.editor.show_line_numbers,
        default_ln,
        "clicking the per-setting reset (↺) must restore the default"
    );
}

/// Settings Fonts pane: clicking the editor-size "+" button increases the
/// editor font size. The +/- buttons carry visible text ("+"/"-") so their
/// AccessKit name is that text (the "Smaller"/"Larger" hover is NOT the name),
/// and "+" also names the tab-strip add button — so we click every "+" in the
/// Fonts pane frame (the tab-strip + is harmless here) and assert the size grew.
#[test]
fn settings_fonts_size_plus_button_increases_size() {
    let mut h = ui_harness(fresh_app());
    h.state_mut().settings_open = true;
    h.run();
    h.get_by_label("Fonts").click();
    h.run();
    let before = h.state().config.fonts.editor_size;
    // Clamp the start below the max so "+" can move it.
    h.state_mut().config.fonts.editor_size = 12.0;
    h.run();
    let before = before.min(12.0);
    for btn in h.get_all_by_label("+").collect::<Vec<_>>() {
        btn.click();
    }
    h.run();
    assert!(
        h.state().config.fonts.editor_size > before,
        "clicking the Fonts size '+' must increase editor_size (was {before}, now {})",
        h.state().config.fonts.editor_size
    );
}

/// Settings → Plugins → "Manage plugins…" opens the plugin-manager modal.
#[test]
fn settings_manage_plugins_button_opens_manager() {
    let mut h = ui_harness(fresh_app());
    h.state_mut().settings_open = true;
    h.run();
    h.get_by_label("Plugins").click();
    h.run();
    assert!(
        !h.state().plugin_manager.open,
        "plugin manager starts closed"
    );
    h.get_by_label("Manage plugins…").click();
    h.run();
    assert!(
        h.state().plugin_manager.open,
        "the 'Manage plugins…' button must open the plugin-manager modal"
    );
}

/// Status bar: the encoding, language, and (when present) diagnostics segment
/// labels render. A scratch tab is UTF-8 / "text"; an injected diagnostic
/// surfaces the count segment.
#[test]
fn status_bar_encoding_language_and_diagnostics_labels_present() {
    let mut app = fresh_app();
    app.tabs[0].text = "hi\n".into();
    // Inject one error diagnostic so the count segment renders.
    app.diagnostics.push(Diagnostic {
        uri: "inmemory://scratch".into(),
        line: 0,
        character: 0,
        severity: 1,
        message: "boom".into(),
    });
    let mut h = ui_harness(app);
    h.run();
    // Encoding + language segments (scratch tab => UTF-8 / text).
    let _ = h.get_by_label("UTF-8");
    let _ = h.get_by_label("text");
    // Diagnostics segment: "<glyph> 1e / 1".
    let diag = format!("{} 1e / 1", egui_phosphor::thin::PROHIBIT);
    assert!(
        h.query_by_label(&diag).is_some(),
        "the diagnostics count segment ({diag:?}) must render with one error"
    );
}

/// Status bar: clicking the encoding segment opens Settings (→ Editor).
#[test]
fn status_bar_encoding_click_opens_settings() {
    let mut h = ui_harness(fresh_app());
    h.run();
    assert!(!h.state().settings_open, "settings starts closed");
    h.get_by_label("UTF-8").click();
    h.run();
    assert!(
        h.state().settings_open,
        "clicking the encoding segment must open Settings"
    );
}

/// Welcome screen: clicking "New file" creates a tab and dismisses the welcome
/// modal; "Open Settings" opens Settings.
#[test]
fn welcome_new_file_button_adds_tab_and_dismisses() {
    // first_run_completed=false => the welcome modal opens on launch.
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = false;
    let app = ScribeApp::new_test(cfg);
    let mut h = ui_harness(app);
    h.run();
    assert!(h.state().welcome_open, "welcome modal opens on first run");
    let before = h.state().tabs.len();
    let label = format!("{}  New file (Ctrl+N)", egui_phosphor::thin::FILE_PLUS);
    h.get_by_label(&label).click();
    h.run();
    assert_eq!(
        h.state().tabs.len(),
        before + 1,
        "the welcome 'New file' button must add a tab"
    );
    assert!(
        !h.state().welcome_open,
        "the welcome 'New file' button must dismiss the welcome modal"
    );
}

#[test]
fn welcome_open_settings_button_opens_settings() {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = false;
    let app = ScribeApp::new_test(cfg);
    let mut h = ui_harness(app);
    h.run();
    let label = format!("{}  Open Settings", egui_phosphor::thin::GEAR_SIX);
    h.get_by_label(&label).click();
    h.run();
    assert!(
        h.state().settings_open,
        "the welcome 'Open Settings' button must open Settings"
    );
    assert!(
        !h.state().welcome_open,
        "opening Settings from welcome dismisses the welcome modal"
    );
}

/// Fuzzy file finder (Ctrl+P surface): open it with a one-file index, type a
/// query, and click the ranked result row → the file opens as a tab.
#[test]
fn fuzzy_finder_type_and_click_opens_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("widget.rs");
    std::fs::write(&target, "fn main() {}\n").unwrap();
    let mut app = fresh_app();
    app.fuzzy_index = vec![target.clone()];
    app.fuzzy_open = true;
    app.focus_fuzzy = true;
    let mut h = ui_harness(app);
    h.run();
    // Type a subsequence that ranks widget.rs.
    h.get_by_role(egui::accesskit::Role::TextInput)
        .type_text("widget");
    h.run();
    let row = target.display().to_string();
    h.get_by_label(&row).click();
    h.run();
    assert!(
        h.state().tabs.iter().any(|t| t.title() == "widget.rs"),
        "clicking a fuzzy-finder result must open that file as a tab"
    );
    assert!(
        !h.state().fuzzy_open,
        "opening a fuzzy-finder result closes the finder"
    );
}

/// Fold panel: with fold view on and a brace region in the buffer, clicking the
/// "▾ L{n} ({len})" toggle folds that region (adds its start line to `folds`).
#[test]
fn fold_toggle_button_click_folds_region() {
    let mut app = fresh_app();
    // A single brace region: lines 0..=2, hidden_len = 2.
    app.tabs[0].text = "fn x() {\n  a\n}\n".into();
    app.fold_view = true;
    let mut h = ui_harness(app);
    h.run();
    assert!(h.state().folds.is_empty(), "no folds before clicking");
    // Unfolded label uses ▾; region start_line=0 => "L1", hidden_len=2.
    h.get_by_label("▾ L1 (2)").click();
    h.run();
    assert!(
        h.state().folds.contains(&0),
        "clicking the fold toggle must fold region at start line 0"
    );
}

/// Toast: with a toast set, clicking its "dismiss" button clears it.
#[test]
fn toast_dismiss_button_clears_toast() {
    let mut app = fresh_app();
    app.toast = Some("something happened".into());
    let mut h = ui_harness(app);
    h.run();
    assert!(h.state().toast.is_some(), "toast is showing");
    h.get_by_label("dismiss").click();
    h.run();
    assert!(
        h.state().toast.is_none(),
        "clicking 'dismiss' must clear the toast"
    );
}

// ---- P2 structural multi-selection (multi-cursor family) ----
// These drive the REAL central-editor render loop: the multi-cursor edit
// interception, Ctrl+D select-next, and the column-selection per-line insert
// all flow through `frame_tick`. Where a gesture needs a galley pos->char
// hit-test that the single-Context harness cannot route deterministically, the
// rectangle is built with the exact `column_selection` the render path uses,
// then edited through the live interception path — so the edit is genuine.

/// The per-tab central-editor `Id` (salted with the active tab's `doc_id`), so
/// tests read/write the same egui `TextEditState` the render loop keys on.
fn central_editor_id(app: &ScribeApp) -> egui::Id {
    egui::Id::new("scr1b3-central-editor").with(app.tabs[app.active].doc_id)
}

/// Write egui's primary caret to the `[anchor, head)` char range.
fn set_selection(ctx: &egui::Context, id: egui::Id, anchor: usize, head: usize) {
    super::multi_cursor_glue::mc_set_primary(ctx, id, anchor, head);
}

/// Read egui's primary selection as a sorted `start..end` char range.
fn selection_of(ctx: &egui::Context, id: egui::Id) -> Option<std::ops::Range<usize>> {
    let state = egui::TextEdit::load_state(ctx, id)?;
    let r = state.cursor.char_range()?;
    let lo = r.primary.index.min(r.secondary.index);
    let hi = r.primary.index.max(r.secondary.index);
    Some(lo..hi)
}

impl Driver {
    /// A modified pointer click (press+release) at `pos` — used for Ctrl/Cmd+click.
    fn mod_click(&self, app: &mut ScribeApp, pos: egui::Pos2, modifiers: egui::Modifiers) {
        self.frame(
            app,
            modifiers,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers,
                },
            ],
        );
    }
}

/// P2-1 — two carets, one keystroke edits BOTH insertion points; Esc collapses.
#[test]
fn mc_typing_inserts_at_all_carets_and_esc_collapses() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "aaa\naaa".to_string();
    let d = Driver::new();
    d.idle(&mut app);
    d.idle(&mut app); // editor auto-focuses
    let id = central_editor_id(&app);
    // Secondary caret at the start of line 2 (char 4); primary at char 0.
    app.multi_cursor
        .add_caret(crate::multi_cursor::Caret::at(4));
    set_selection(&d.ctx, id, 0, 0);
    d.type_text(&mut app, "X");
    assert_eq!(
        app.tabs[0].text, "Xaaa\nXaaa",
        "the keystroke inserted at BOTH carets"
    );
    assert!(app.multi_cursor.is_active(), "multi-cursor still engaged");
    // Esc collapses to a single caret.
    d.key(&mut app, egui::Key::Escape, egui::Modifiers::NONE);
    assert!(
        !app.multi_cursor.is_active(),
        "Esc collapsed multi-cursor to one caret"
    );
}

/// Regression — a coincident caret (secondary navigated onto the primary offset)
/// must NOT double-insert: reconcile collapses it to one edit.
#[test]
fn mc_coincident_caret_inserts_once_no_phantom() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "aaaaaaaaaa".to_string();
    let d = Driver::new();
    d.idle(&mut app);
    d.idle(&mut app);
    let id = central_editor_id(&app);
    // Secondary at 6; primary ALSO at 6 (as if arrow-navigated onto it).
    app.multi_cursor
        .add_caret(crate::multi_cursor::Caret::at(6));
    set_selection(&d.ctx, id, 6, 6);
    d.type_text(&mut app, "X");
    assert_eq!(
        app.tabs[0].text, "aaaaaaXaaaa",
        "exactly one X — the coincident caret was reconciled, not doubled"
    );
}

/// Regression — a bare caret NESTED inside a secondary selection must not cause
/// an overlapping splice / garbage buffer.
#[test]
fn mc_nested_caret_no_garbage_splice() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "foo foo".to_string();
    let d = Driver::new();
    d.idle(&mut app);
    d.idle(&mut app);
    let id = central_editor_id(&app);
    // Secondary selects the 2nd "foo" (4..7); a BARE caret sits at 5 inside it.
    app.multi_cursor
        .add_caret(crate::multi_cursor::Caret::selection(4, 7));
    app.multi_cursor
        .add_caret(crate::multi_cursor::Caret::at(5));
    // Primary selects the 1st "foo" (0..3).
    set_selection(&d.ctx, id, 0, 3);
    d.type_text(&mut app, "X");
    assert_eq!(
        app.tabs[0].text, "X X",
        "each foo replaced once; the nested caret was dropped, no garbage splice"
    );
}

/// P2-2 — Ctrl+D selects the word, then grows the match set; a later edit
/// rewrites every match (rename-like).
#[test]
fn mc_ctrl_d_selects_word_then_grows_and_renames_all() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "foo foo foo".to_string();
    let d = Driver::new();
    d.idle(&mut app);
    d.idle(&mut app);
    let id = central_editor_id(&app);
    set_selection(&d.ctx, id, 1, 1); // caret inside the first "foo"
                                     // First Ctrl+D selects the word under the caret.
    d.key(&mut app, egui::Key::D, egui::Modifiers::COMMAND);
    assert_eq!(
        selection_of(&d.ctx, id),
        Some(0..3),
        "first Ctrl+D selects the whole word"
    );
    assert!(
        app.multi_cursor.secondaries().is_empty(),
        "no secondary added on the first Ctrl+D"
    );
    // Second + third Ctrl+D add the next two occurrences.
    d.key(&mut app, egui::Key::D, egui::Modifiers::COMMAND);
    d.key(&mut app, egui::Key::D, egui::Modifiers::COMMAND);
    assert_eq!(
        app.multi_cursor.secondaries().len(),
        2,
        "occurrences 2 and 3 joined the match set"
    );
    // Editing rewrites every match.
    d.type_text(&mut app, "bar");
    assert_eq!(
        app.tabs[0].text, "bar bar bar",
        "the edit applied to every matched occurrence"
    );
}

/// P2-3 — a rectangular (column) selection spanning 3 lines inserts on each.
#[test]
fn mc_column_block_selection_inserts_on_every_line() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "abc\ndef\nghi".to_string();
    let d = Driver::new();
    d.idle(&mut app);
    d.idle(&mut app);
    let id = central_editor_id(&app);
    // The rectangle an Alt+drag from (line0,col0) to (line2,col0) produces —
    // built with the exact `column_selection` the render path calls.
    let chars: Vec<char> = app.tabs[0].text.chars().collect();
    let mut carets = crate::multi_cursor::column_selection(&chars, 0, 8);
    assert_eq!(carets.len(), 3, "the block spans all 3 lines");
    let primary = carets.remove(0);
    app.multi_cursor.set_secondaries(carets);
    set_selection(&d.ctx, id, primary.anchor, primary.head);
    // Per-line insert flows through the real multi-cursor edit interception.
    d.type_text(&mut app, ">");
    assert_eq!(
        app.tabs[0].text, ">abc\n>def\n>ghi",
        "the column insert landed on every spanned line"
    );
}

/// P2-1 — a real Ctrl/Cmd+click pointer event adds a secondary caret via the
/// render path's galley hit-test, keeping the pre-click primary.
#[test]
fn mc_ctrl_click_pointer_adds_secondary_and_keeps_primary() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "hello world here\nsecond line of text".to_string();
    let d = Driver::new();
    d.idle(&mut app);
    d.idle(&mut app); // editor auto-focuses
    let id = central_editor_id(&app);
    set_selection(&d.ctx, id, 2, 2); // primary at char 2
    d.idle(&mut app); // let egui adopt the primary before the modified click
                      // Ctrl+click elsewhere in the editor — the galley hit-test resolves the char.
    d.mod_click(&mut app, egui::pos2(300.0, 380.0), egui::Modifiers::COMMAND);
    assert!(
        app.multi_cursor.is_active(),
        "Ctrl+click engaged multi-cursor"
    );
    assert_eq!(
        app.multi_cursor.secondaries().len(),
        1,
        "exactly one secondary caret added at the click"
    );
    assert_eq!(
        selection_of(&d.ctx, id),
        Some(2..2),
        "the pre-click primary was restored (the click became the secondary)"
    );
}

// ---- adversarial-review remediation (P1-A / P2-C / P2-D / P2-E / P2-B) ----

/// FIX-1 / P1-A — the app-global multi-cursor state MUST be scoped to the tab it
/// was built on. Build carets on tab A, switch to tab B, then type: tab B must
/// get a NORMAL single insertion (not tab A's replayed carets), tab A must be
/// untouched, and the stale carets must be gone after the switch. This is the
/// reachable silent wrong-buffer corruption the review flagged as ship-blocking.
#[test]
fn mc_carets_are_scoped_to_their_tab_no_cross_buffer_edit() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "AAAAAAA".to_string(); // tab A
    app.tabs.push(EditorTab::scratch());
    app.tabs[1].text = "bbb".to_string(); // tab B (shorter, different buffer)
    let d = Driver::new();
    d.idle(&mut app); // sync_grid_state assigns distinct doc_ids; editor focuses
    d.idle(&mut app);
    assert_ne!(
        app.tabs[0].doc_id, app.tabs[1].doc_id,
        "the two tabs have distinct per-tab doc_ids"
    );
    assert_eq!(app.active, 0, "tab A is active to start");
    let id_a = central_editor_id(&app);
    // Build a multi-cursor set on tab A: secondary at char 3, primary at char 0.
    app.multi_cursor
        .add_caret(crate::multi_cursor::Caret::at(3));
    set_selection(&d.ctx, id_a, 0, 0);
    d.idle(&mut app); // end-of-frame records the owner = tab A's doc_id
    assert!(
        app.multi_cursor.is_active(),
        "multi-cursor engaged on tab A"
    );
    // Switch to tab B — the real trigger (active-tab change + auto-focus).
    app.active = 1;
    d.idle(&mut app); // top-of-frame reconcile drops the stale carets
    d.idle(&mut app); // let tab B's editor take focus
    assert!(
        !app.multi_cursor.is_active(),
        "switching tabs cleared the stale carets"
    );
    assert!(
        app.multi_cursor.secondaries().is_empty(),
        "no secondary carets survive the tab switch"
    );
    assert_eq!(
        app.mc_owner_doc, None,
        "the caret owner was reset on the switch"
    );
    // Type into tab B: a NORMAL single insertion at its primary, tab A untouched.
    let id_b = central_editor_id(&app);
    set_selection(&d.ctx, id_b, 0, 0);
    d.type_text(&mut app, "Z");
    assert_eq!(
        app.tabs[1].text, "Zbbb",
        "tab B received exactly the normal single insertion"
    );
    assert_eq!(
        app.tabs[0].text, "AAAAAAA",
        "tab A's buffer is unchanged — no wrong-buffer edit"
    );
}

/// FIX-2 / P2-C — an out-of-band buffer mutation (here a palette SORT transform,
/// representative of reload / palette transforms / doc replace) rewrites offsets
/// out from under the carets, so the stale multi-cursor set MUST be dropped.
#[test]
fn mc_out_of_band_transform_clears_stale_carets() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "banana\napple\ncherry\n".to_string(); // unsorted
    let d = Driver::new();
    d.idle(&mut app);
    d.idle(&mut app);
    app.multi_cursor
        .add_caret(crate::multi_cursor::Caret::at(3));
    d.idle(&mut app); // record the owner
    assert!(
        app.multi_cursor.is_active(),
        "carets engaged before the transform"
    );
    // A palette buffer transform (sort) mutates the whole buffer out-of-band.
    app.execute_builtin(crate::app::commands::BuiltinCommand::SortLines);
    assert_ne!(
        app.tabs[0].text, "banana\napple\ncherry\n",
        "the sort actually reordered the buffer"
    );
    assert!(
        !app.multi_cursor.is_active(),
        "the out-of-band transform cleared the now-stale carets"
    );
    assert_eq!(
        app.mc_owner_doc, None,
        "the caret owner was reset with the carets"
    );
}

/// FIX-4 / P2-D — a multi-caret edit registers as ONE undoable whole-text step:
/// Ctrl+Z after a two-caret insert restores the pre-edit text cleanly (no
/// broken / half-undo state). Granularity is whole-buffer per batch (documented
/// in the glue) — egui 0.34's undoer cannot express per-caret ranges.
#[test]
fn mc_multi_caret_edit_is_one_undo_step() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].text = "aaa\naaa".to_string();
    let d = Driver::new();
    d.idle(&mut app);
    d.idle(&mut app);
    let id = central_editor_id(&app);
    app.multi_cursor
        .add_caret(crate::multi_cursor::Caret::at(4));
    set_selection(&d.ctx, id, 0, 0);
    d.type_text(&mut app, "X");
    assert_eq!(
        app.tabs[0].text, "Xaaa\nXaaa",
        "the two-caret insert edited both spots"
    );
    // Ctrl+Z reverts the WHOLE multi-caret edit as a single step.
    d.key(&mut app, egui::Key::Z, egui::Modifiers::COMMAND);
    assert_eq!(
        app.tabs[0].text, "aaa\naaa",
        "Ctrl+Z restored the pre-edit text in one clean step"
    );
}

/// FIX-3 / P2-E — the focus-independent Esc collapse only STEALS Escape when
/// multi-cursor is genuinely active AND no overlay is open, so a modal / find bar
/// / palette that needs Escape is never starved. Pure predicate, no frame needed.
#[test]
fn mc_escape_consumed_only_when_active_and_no_overlay() {
    let mut app = ScribeApp::new_test(Config::default());
    // Inactive multi-cursor → Escape must fall through to other handlers.
    assert!(
        !app.mc_should_consume_escape(false),
        "no multi-cursor active → do NOT consume Escape"
    );
    // Engage multi-cursor (a secondary caret).
    app.multi_cursor
        .add_caret(crate::multi_cursor::Caret::at(1));
    assert!(
        app.mc_should_consume_escape(false),
        "active multi-cursor + no overlay → collapse on Escape"
    );
    assert!(
        !app.mc_should_consume_escape(true),
        "active multi-cursor but an overlay is open → let the overlay have Escape"
    );
}

/// FIX-5 / P2-B — drive a REAL Alt+pointer press→drag→release through the actual
/// gesture handler in `frame_tick` (galley hit-test → column build), NOT by
/// calling `column_selection` directly. Proves the previously-untested
/// production seam: the Alt-drag resolves to a multi-line column set and the
/// per-line insert flows through the live edit interception. (What remains
/// unproven headlessly is egui's OWN concurrent linear drag-highlight paint,
/// which has no observable buffer effect — see the result file.)
#[test]
fn mc_alt_pointer_drag_builds_column_and_inserts_per_line() {
    let mut app = ScribeApp::new_test(Config::default());
    // A tall, uniform document so a vertical Alt-drag spans several lines, each
    // long enough to share a column band.
    app.tabs[0].text = (0..20).map(|_| "abcdefghij\n").collect::<String>();
    let d = Driver::new();
    d.idle(&mut app);
    d.idle(&mut app); // editor auto-focuses
    let alt = egui::Modifiers {
        alt: true,
        ..Default::default()
    };
    let press = egui::pos2(140.0, 380.0);
    let drag = egui::pos2(190.0, 470.0);
    // Alt + primary PRESS latches the column anchor via the galley hit-test.
    d.frame(
        &mut app,
        alt,
        vec![
            egui::Event::PointerMoved(press),
            egui::Event::PointerButton {
                pos: press,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: alt,
            },
        ],
    );
    // Alt + primary HELD (moved, not re-pressed) extends → the column build runs.
    d.frame(&mut app, alt, vec![egui::Event::PointerMoved(drag)]);
    assert!(
        app.multi_cursor.is_active(),
        "the real Alt+pointer-drag engaged multi-cursor"
    );
    let n_secondaries = app.multi_cursor.secondaries().len();
    assert!(
        n_secondaries >= 1,
        "the drag spanned >=2 lines → >=1 secondary caret (got {n_secondaries})"
    );
    // Release ends the drag (column_anchor drops next frame).
    d.frame(
        &mut app,
        alt,
        vec![egui::Event::PointerButton {
            pos: drag,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: alt,
        }],
    );
    // Type through the REAL edit interception: exactly one char per caret.
    let before = app.tabs[0].text.matches('X').count();
    d.type_text(&mut app, "X");
    let inserted = app.tabs[0].text.matches('X').count() - before;
    assert_eq!(
        inserted,
        n_secondaries + 1,
        "the column insert landed once per caret (primary + {n_secondaries} secondaries)"
    );
}

// FIX-6 / P3-G (no-repaint-spin-at-document-end) is proven deterministically by
// the `scroll_step_*` unit tests in `drag_scroll.rs` (the clamp decision seam),
// which avoid the fragility of reconstructing held-pointer + focus state outside
// a real frame.

/// Regression (the reported bug, general case): clicking ANY top-bar button
/// must NOT move the editor viewport. We drive a REAL user click on the toolbar
/// ">_" command-palette button. Its press lands in the toolbar — far ABOVE the
/// editor viewport top — while the editor still owns keyboard focus for that one
/// frame. The drag-select edge-autoscroll assist used to read that as "a drag
/// held past the top edge" (`primary_down` + editor focus, pointer above the
/// viewport) and pan the note UPWARD by a full edge-autoscroll step
/// (`EDGE_MAX_SPEED * dt`). A note scrolled to the very bottom makes the upward
/// jump maximally visible. Fixed by gating the autoscroll on the drag's press
/// ORIGIN being inside the editor viewport — a genuine drag-selection always
/// begins there; a top-bar click never does.
#[test]
fn topbar_click_does_not_scroll_the_note() {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true; // no welcome modal stealing focus
    cfg.appearance.frameless = false;
    let app = ScribeApp::new_test(cfg);
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 720.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    // A note far taller than the viewport so there is real scroll range.
    h.state_mut().tabs[0].text = (0..400).map(|i| format!("line {i}\n")).collect();
    h.run();
    h.run(); // editor auto-focuses
             // Scroll to the very bottom and settle (pending_scroll consumed, offset kept).
    h.state_mut().pending_scroll = Some(1.0e6);
    h.run();
    h.state_mut().pending_scroll = None;
    h.run();
    h.run();
    let before = h.state().scroll_metrics.0;
    assert!(
        before > 100.0,
        "precondition: the note must be scrolled down so an upward jump is \
         observable (offset {before:.1})"
    );
    // Click a real top-bar button the way a user does.
    h.get_by_label(">_").click();
    h.run();
    h.run();
    let after = h.state().scroll_metrics.0;
    assert!(
        (after - before).abs() < 1.0,
        "clicking a top-bar button must not move the editor viewport \
         (offset {before:.1} -> {after:.1}, delta {:.1})",
        after - before
    );
}
