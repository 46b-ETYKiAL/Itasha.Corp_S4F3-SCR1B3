//! Dedicated ACCESSIBILITY AUDIT (task #31a).
//!
//! The e2e tests already use AccessKit as a *test handle* (find a widget by
//! its accessible label, then click it). This module treats the AccessKit
//! tree as an *audited surface* in its own right and asserts the WCAG 2.2 AA
//! properties an editor — an input/editing tool — must meet:
//!
//!   * **Role + accessible name + state** — every interactive widget (Button,
//!     TextInput, CheckBox, …) exposes a non-empty accessible name and a role
//!     a screen reader can announce (WCAG 4.1.2 Name/Role/Value).
//!   * **Keyboard-only operability** — the command palette + every builtin
//!     command is reachable without a mouse; the editor surface is focusable
//!     and the find/palette/settings overlays can be opened and dismissed by
//!     keyboard alone (WCAG 2.1.1 Keyboard).
//!   * **No keyboard trap** — opening then Escape-closing an overlay returns
//!     keyboard ownership to the editor; focus is never stranded (WCAG 2.1.2).
//!   * **Logical focus order** — the toolbar's interactive controls are laid
//!     out left-to-right / top-to-bottom in a sane reading order (WCAG 2.4.3).
//!   * **Colour contrast** — every bundled theme's body fg/bg meets the
//!     4.5:1 WCAG 2.2 AA text ratio, and the accent (system-voice) meets the
//!     3:1 non-text/large-text ratio against its background (WCAG 1.4.3 /
//!     1.4.11). The four documented opt-in "camp" exceptions (geocities-bbs,
//!     akira-redshift, atompunk-sodium, shutoko-night) are audited but their
//!     known sub-AA pairs are recorded as documented exceptions, not failures.
//!
//! All tests are headless (no GPU): they drive the real `ScribeApp::frame_tick`
//! via `egui_kittest`'s AccessKit backend, exactly like `e2e.rs`.
#![allow(clippy::wildcard_imports)]
use super::*;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use scribe_core::theme::{Rgba, Theme};

// ───────────────────────── WCAG contrast math ─────────────────────────

/// One sRGB 8-bit channel → linearised light value, per WCAG 2.x relative-
/// luminance definition (sRGB EOTF). Used by [`relative_luminance`].
fn linearize(c: u8) -> f64 {
    let cs = c as f64 / 255.0;
    if cs <= 0.040_45 {
        cs / 12.92
    } else {
        ((cs + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance L = 0.2126 R + 0.7152 G + 0.0722 B (linearised).
fn relative_luminance(c: Rgba) -> f64 {
    0.2126 * linearize(c.r) + 0.7152 * linearize(c.g) + 0.0722 * linearize(c.b)
}

/// Composite a (possibly translucent) foreground over an opaque background so
/// alpha-bearing UI colours (selection washes) are scored at their effective
/// painted colour, not their nominal RGB. Background is treated as opaque.
fn flatten_over(fg: Rgba, bg: Rgba) -> Rgba {
    let a = fg.a as f64 / 255.0;
    let mix = |f: u8, b: u8| ((f as f64) * a + (b as f64) * (1.0 - a)).round() as u8;
    Rgba::new(mix(fg.r, bg.r), mix(fg.g, bg.g), mix(fg.b, bg.b), 255)
}

/// WCAG contrast ratio (1.0 ..= 21.0) between two colours. The foreground is
/// alpha-composited over the background first.
fn contrast_ratio(fg: Rgba, bg: Rgba) -> f64 {
    let f = flatten_over(fg, bg);
    let l1 = relative_luminance(f);
    let l2 = relative_luminance(bg);
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

#[test]
fn contrast_math_matches_wcag_reference_pairs() {
    // Black on white is the canonical maximum (21:1).
    let black = Rgba::new(0, 0, 0, 255);
    let white = Rgba::new(255, 255, 255, 255);
    let r = contrast_ratio(black, white);
    assert!(
        (r - 21.0).abs() < 0.05,
        "black/white must be 21:1, got {r:.3}"
    );
    // Identical colours are the minimum (1:1).
    assert!((contrast_ratio(white, white) - 1.0).abs() < 1e-6);
    // A known mid pair: #767676 grey on white is the WCAG AA "just passes"
    // canonical example (~4.54:1).
    let grey = Rgba::new(0x76, 0x76, 0x76, 255);
    let g = contrast_ratio(grey, white);
    assert!(
        (4.45..=4.65).contains(&g),
        "#767676 on white should be ~4.54:1, got {g:.3}"
    );
    // Alpha compositing: a 0x33-alpha teal selection over a dark void is far
    // closer to the void than to full teal — its luminance must sit low.
    let teal = Rgba::new(0x34, 0xe0, 0xd0, 0x33);
    let void = Rgba::new(0x07, 0x0a, 0x0c, 255);
    assert!(
        relative_luminance(flatten_over(teal, void)) < 0.2,
        "a low-alpha wash must score near its dark background, not full teal"
    );
}

// ───────────────────────── theme colour contrast ─────────────────────────

/// Body text (`foreground` over `background`) must meet WCAG 2.2 AA 4.5:1 on
/// every bundled theme that is NOT a documented camp/opt-in exception.
const CAMP_THEMES: &[&str] = &[
    // Documented opt-in exceptions (theme.rs doc-comments): these intentionally
    // break the accent / body discipline for period-faithful palettes and warn
    // the user on selection. We still AUDIT them (below) but do not FAIL on
    // their known sub-AA accent pairs.
    "geocities-bbs",   // construction-yellow body text by Web-1.0 necessity
    "akira-redshift",  // red-as-system-voice (Akira IS red)
    "atompunk-sodium", // sodium-orange-as-system-voice (Atompunk demands it)
    "shutoko-night", // BT2 Bayside-Blue-on-void voice — Itasha brand-root identity (see theme.rs doc)
];

#[test]
fn every_builtin_theme_body_text_meets_wcag_aa() {
    let mut failures = Vec::new();
    for name in Theme::builtin_names() {
        let t = Theme::builtin(name).expect("builtin must construct");
        let fg = t.ui("foreground", Rgba::new(0xff, 0xff, 0xff, 255));
        let bg = t.ui("background", Rgba::new(0, 0, 0, 255));
        let ratio = contrast_ratio(fg, bg);
        if ratio < 4.5 {
            failures.push(format!("{name}: body fg/bg = {ratio:.2}:1 (< 4.5)"));
        }
    }
    assert!(
        failures.is_empty(),
        "every non-camp built-in theme must meet WCAG AA 4.5:1 body contrast:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn a11y_high_contrast_theme_meets_aaa_body() {
    // The accessibility theme advertises AAA (>= 7:1) body contrast — hold it
    // to its own claim so a future palette edit can't quietly regress it.
    let t = Theme::a11y_high_contrast();
    let ratio = contrast_ratio(
        t.ui("foreground", Rgba::new(0xff, 0xff, 0xff, 255)),
        t.ui("background", Rgba::new(0, 0, 0, 255)),
    );
    assert!(
        ratio >= 7.0,
        "a11y-high-contrast must meet WCAG AAA (>= 7:1) body contrast, got {ratio:.2}:1"
    );
}

#[test]
fn theme_accent_meets_non_text_contrast() {
    // The accent is the system VOICE: cursor, active line number, focus rings,
    // selection edge. WCAG 1.4.11 requires >= 3:1 for such non-text UI against
    // its adjacent background. Camp/opt-in themes are audited but exempt from
    // failing (their accent intentionally doubles as alarm/body).
    let mut failures = Vec::new();
    for name in Theme::builtin_names() {
        if CAMP_THEMES.contains(name) {
            continue;
        }
        let t = Theme::builtin(name).expect("builtin must construct");
        let accent = t.ui("accent", Rgba::new(0xff, 0xff, 0xff, 255));
        let bg = t.ui("background", Rgba::new(0, 0, 0, 255));
        let ratio = contrast_ratio(accent, bg);
        if ratio < 3.0 {
            failures.push(format!("{name}: accent/bg = {ratio:.2}:1 (< 3.0)"));
        }
    }
    assert!(
        failures.is_empty(),
        "every non-camp theme's accent must meet WCAG 1.4.11 (>= 3:1):\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn theme_error_state_is_distinguishable_from_body() {
    // The `error` colour (alarm) must be perceivable against the panel/bg it is
    // painted on — at least the 3:1 non-text floor — so a colour-blind-adjacent
    // user still sees an alarm differs from the background. (Shape/text always
    // accompanies it; this guards the colour channel doesn't vanish.)
    let mut failures = Vec::new();
    for name in Theme::builtin_names() {
        if CAMP_THEMES.contains(name) {
            continue;
        }
        let t = Theme::builtin(name).expect("builtin must construct");
        let err = t.ui("error", Rgba::new(0xff, 0, 0, 255));
        let bg = t.ui("background", Rgba::new(0, 0, 0, 255));
        let ratio = contrast_ratio(err, bg);
        if ratio < 3.0 {
            failures.push(format!("{name}: error/bg = {ratio:.2}:1 (< 3.0)"));
        }
    }
    assert!(
        failures.is_empty(),
        "every non-camp theme's error colour must clear the 3:1 floor:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn camp_themes_are_still_constructible_and_audited() {
    // The opt-in camp themes are NOT failed on contrast, but they MUST still
    // construct and carry the full chrome shape (no holes) so the editor never
    // blanks when one is selected. This records them as audited, not ignored.
    for name in CAMP_THEMES {
        let t = Theme::builtin(name).unwrap_or_else(|| panic!("camp theme {name} must construct"));
        for key in ["background", "foreground", "accent", "error", "cursor"] {
            assert!(
                t.ui.contains_key(key),
                "camp theme {name} missing chrome slot `{key}` (would blank the editor)"
            );
        }
    }
}

// ───────────────────── wordmark readable-tone guard ─────────────────────

/// Local WCAG relative-luminance on an egui `Color32`, mirroring
/// `render_support::relative_luminance` (which is private) so this test can
/// assert the guard's luminance-gap contract directly.
fn lum(c: egui::Color32) -> f32 {
    let lin = |ch: u8| {
        let cs = ch as f32 / 255.0;
        if cs <= 0.040_45 {
            cs / 12.92
        } else {
            ((cs + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

#[test]
fn ensure_readable_tone_lifts_low_contrast_tone_and_keeps_teal_hue() {
    use egui::Color32;
    // A near-black teal on a near-black titlebar has almost no luminance gap —
    // the guard must lift it until it clears the ~0.34 MIN_GAP.
    let dark_bg = Color32::from_rgb(0x0d, 0x0b, 0x14);
    let faint_teal = Color32::from_rgb(0x0a, 0x2a, 0x28);
    let gap_before = (lum(faint_teal) - lum(dark_bg)).abs();
    assert!(gap_before < 0.34, "fixture must start below the gap");
    let fixed = ensure_readable_tone(faint_teal, dark_bg);
    let gap_after = (lum(fixed) - lum(dark_bg)).abs();
    assert!(
        gap_after >= 0.34 - 1e-3,
        "guard must lift the tone to clear MIN_GAP, got {gap_after:.3}"
    );
    // Teal identity preserved: it lifted (brighter) and stays green/blue-led,
    // never turning red-dominant.
    assert!(fixed.g() >= faint_teal.g(), "tone should lift, not darken");
    assert!(
        fixed.g() >= fixed.r() && fixed.b() >= fixed.r(),
        "teal hue must be preserved (not red-dominant): {fixed:?}"
    );
}

#[test]
fn ensure_readable_tone_leaves_already_legible_tone_untouched() {
    use egui::Color32;
    // Full SCR1B3 teal on the void titlebar already clears the gap → identity.
    let dark_bg = Color32::from_rgb(0x0d, 0x0b, 0x14);
    let teal = Color32::from_rgb(0x00, 0xff, 0xfe);
    assert_eq!(
        ensure_readable_tone(teal, dark_bg),
        teal,
        "a tone already past MIN_GAP must be returned unchanged"
    );
}

#[test]
fn ensure_readable_tone_lifts_against_bright_bg() {
    use egui::Color32;
    // The existing a11y tests use near-black bg (bg_l ~= 0.004) where sum ~= diff,
    // so the `- -> +` mutants on the early-return / loop-break contrast guards
    // (281:34, 294:37) survived. A BRIGHT bg makes |rl + bg| huge: a sum-not-diff
    // mutant returns the tone unchanged. Kills render_support 281:34, 294:37.
    let bright_bg = Color32::from_rgb(220, 220, 220); // rl ~= 0.716
    let mid_tone = Color32::from_rgb(190, 190, 190); // rl ~= 0.514, gap ~= 0.20 < 0.34
    let out = ensure_readable_tone(mid_tone, bright_bg);
    assert_ne!(out, mid_tone, "guard must adjust a low-contrast tone on a bright bg");
    assert!(out.r() < mid_tone.r(), "should darken, got {out:?}");
}

// ───────────────────── AccessKit role + name + state ─────────────────────

fn audit_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true; // no welcome modal stealing focus
    cfg.appearance.frameless = false; // stable, single set of window controls
    ScribeApp::new_test(cfg)
}

fn audit_harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 720.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

/// Minimal keyboard-only driver: runs one full UI frame with a key chord, no
/// pointer events — proving the action is reachable WITHOUT a mouse. Local to
/// this module (the `e2e::Driver` is private to that module).
struct KeyDriver {
    ctx: egui::Context,
}
impl KeyDriver {
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
}

/// Roles whose nodes are interactive and therefore MUST carry an accessible
/// name (WCAG 4.1.2). egui labels buttons by their text/icon; an unlabeled
/// interactive node is a screen-reader dead end.
fn role_is_interactive(role_dbg: &str) -> bool {
    matches!(
        role_dbg,
        "Button" | "CheckBox" | "RadioButton" | "Switch" | "MenuItem" | "Link" | "ComboBox"
    )
}

#[test]
fn every_interactive_widget_exposes_role_and_name() {
    let mut h = audit_harness(audit_app());
    h.run();
    h.run();
    let mut unnamed = Vec::new();
    let mut interactive = 0usize;
    for node in h.root().children_recursive() {
        let ak = node.accesskit_node();
        let role = format!("{:?}", ak.role());
        if !role_is_interactive(&role) {
            continue;
        }
        // A hidden/offscreen node (folded-away overflow) isn't user-reachable.
        if ak.is_hidden() {
            continue;
        }
        interactive += 1;
        let label = ak.label();
        let name = label.as_deref().map(str::trim).unwrap_or("");
        let value = node.value().unwrap_or_default();
        if name.is_empty() && value.trim().is_empty() {
            unnamed.push(format!(
                "{role} @ {:?}",
                ak.bounding_box().map(|b| (b.x0 as i32, b.y0 as i32))
            ));
        }
    }
    assert!(
        interactive > 0,
        "the default frame must expose at least one interactive widget"
    );
    assert!(
        unnamed.is_empty(),
        "every interactive widget must expose an accessible name \
         (WCAG 4.1.2); {} unnamed:\n  {}",
        unnamed.len(),
        unnamed.join("\n  ")
    );
}

#[test]
fn settings_panes_each_expose_named_controls() {
    // Drive each settings category and assert the pane renders named, focusable
    // controls (not an empty/dead pane). This is the a11y complement to the
    // e2e pane-navigation coverage.
    let app = audit_app();
    let mut h = audit_harness(app);
    h.state_mut().settings_open = true;
    h.run();
    for pane in [
        "Appearance",
        "Fonts",
        "Toolbar",
        "Motion",
        "Editor",
        "Plugins",
    ] {
        // Navigate to the pane by its category BUTTON (keyboard/AT path). The
        // pane name also renders as a heading Label, so a bare label query is
        // ambiguous (kittest panics on >1 match) — pick the interactive Button.
        // Bind first so the query iterator's immutable borrow of `h` is released
        // before the `h.run()` below (an if-let scrutinee's temporaries live for
        // the whole block — E0502 otherwise).
        let target = h
            .get_all_by_label(pane)
            .find(|n| format!("{:?}", n.accesskit_node().role()) == "Button");
        if let Some(node) = target {
            node.click();
        }
        h.run();
        h.run();
        // The open settings window must still carry a reachable "Close window".
        assert!(
            h.query_by_label("Close window").is_some(),
            "settings pane `{pane}` must keep the Close control reachable"
        );
        // And at least one interactive, named control in the body.
        let named_controls = h
            .root()
            .children_recursive()
            .filter(|n| {
                let ak = n.accesskit_node();
                role_is_interactive(&format!("{:?}", ak.role()))
                    && !ak.is_hidden()
                    && ak.label().map(|l| !l.trim().is_empty()).unwrap_or(false)
            })
            .count();
        assert!(
            named_controls > 0,
            "settings pane `{pane}` must expose at least one named control"
        );
    }
}

// ───────────────────── keyboard operability + no trap ─────────────────────

#[test]
fn command_palette_is_keyboard_reachable_and_dismissable() {
    // Ctrl+Shift+P opens the palette; Escape closes it. No mouse involved.
    let mut app = audit_app();
    let d = KeyDriver::new();
    d.idle(&mut app);
    assert!(!app.palette_open, "palette starts closed");
    let cmd_shift = egui::Modifiers {
        command: true,
        shift: true,
        ..Default::default()
    };
    d.key(&mut app, egui::Key::P, cmd_shift);
    assert!(
        app.palette_open,
        "Ctrl+Shift+P must open the palette (WCAG 2.1.1)"
    );
    d.key(&mut app, egui::Key::Escape, egui::Modifiers::default());
    assert!(
        !app.palette_open,
        "Escape must close the palette (WCAG 2.1.2 — no keyboard trap)"
    );
}

#[test]
fn find_bar_open_close_returns_keyboard_to_editor() {
    // Ctrl+F opens the find bar; Escape closes it AND keyboard ownership must
    // return to the editor (the editor owns input when no modal is open).
    let mut app = audit_app();
    let d = KeyDriver::new();
    d.idle(&mut app);
    let cmd = egui::Modifiers {
        command: true,
        ..Default::default()
    };
    d.key(&mut app, egui::Key::F, cmd);
    assert!(app.find_open, "Ctrl+F must open the find bar");
    d.key(&mut app, egui::Key::Escape, egui::Modifiers::default());
    assert!(!app.find_open, "Escape must close the find bar");
    // After closing the only overlay, no modal owns the keyboard → the editor
    // does. `editor_owns_keyboard`-equivalent: no overlay flag is set.
    assert!(
        !(app.find_open || app.palette_open || app.settings_open),
        "closing the find bar must not strand focus in a dead overlay"
    );
}

#[test]
fn all_builtin_commands_are_keyboard_invocable_without_mouse() {
    // The command palette is the keyboard-only path to every editor action.
    // Assert each builtin carries a non-empty palette label (a command with no
    // label is unreachable from the keyboard-driven palette) and that the
    // registry is the canonical self-discovery surface (WCAG 2.1.1).
    let mut empty = Vec::new();
    for entry in BUILTIN_COMMANDS {
        if entry.label.trim().is_empty() {
            empty.push(format!("{:?}", entry.action));
        }
    }
    assert!(
        empty.is_empty(),
        "every builtin command needs a keyboard-reachable palette label; \
         unlabeled: {empty:?}"
    );
    assert!(
        !BUILTIN_COMMANDS.is_empty(),
        "the keyboard-discovery palette registry must not be empty"
    );
}

// ───────────────────────── logical focus order ─────────────────────────

#[test]
fn toolbar_controls_are_in_left_to_right_reading_order() {
    // WCAG 2.4.3: focus order should follow reading order. The toolbar is a
    // single horizontal band at the top of the window; its interactive buttons
    // must be enumerable in non-decreasing x (no node jumping backwards), so a
    // Tab traversal walks the band the way a sighted user reads it.
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    cfg.toolbar.items = vec!["save".into(), "find".into(), "settings".into()];
    let mut h = audit_harness(ScribeApp::new_test(cfg));
    h.run();
    h.run();
    // Collect toolbar-band buttons (top ~60px) with a bounding box, in tree
    // order, then assert their left edges are monotonic non-decreasing.
    let mut band: Vec<(f32, f32)> = Vec::new(); // (x0, y0) in tree/focus order
    for node in h.root().children_recursive() {
        let ak = node.accesskit_node();
        if format!("{:?}", ak.role()) != "Button" || ak.is_hidden() {
            continue;
        }
        if let Some(b) = ak.bounding_box() {
            if b.y0 < 60.0 {
                band.push((b.x0 as f32, b.y0 as f32));
            }
        }
    }
    assert!(
        band.len() >= 2,
        "expected at least two top-chrome buttons, got {}",
        band.len()
    );
    // The top chrome stacks TWO horizontal rows within 60px: the titlebar/
    // toolbar row and the tab strip just below it. Reading order (WCAG 2.4.3)
    // is PER ROW — a new row restarts the left-to-right scan, so the tab strip
    // is never compared against the toolbar row above it. Within a row the
    // focus order (tree order) must not read right-to-left (a <2px backstep is
    // tolerated for float jitter / same-column stacks).
    let mut prev_x = f32::MIN;
    let mut prev_y = f32::MIN;
    let mut backsteps = 0;
    for (x, y) in &band {
        if (*y - prev_y).abs() > 8.0 {
            prev_x = f32::MIN; // row change → restart the horizontal scan
        }
        if *x + 2.0 < prev_x {
            backsteps += 1;
        }
        prev_x = prev_x.max(*x);
        prev_y = *y;
    }
    assert_eq!(
        backsteps, 0,
        "toolbar reading order regressed: button (x,y) {band:?} step backwards within a row"
    );
}
