//! #88 — the app background override repaints panel/window fills
//! independently of the theme; None follows the theme.
use super::ScribeApp;
use scribe_core::Config;

#[test]
fn override_repaints_panel_and_window_fill() {
    let mut cfg = Config::default();
    cfg.appearance.background_override = Some("#112233".into());
    let app = ScribeApp::new_test(cfg);
    let v = app.current_visuals();
    let want = egui::Color32::from_rgb(0x11, 0x22, 0x33);
    assert_eq!(v.panel_fill, want);
    assert_eq!(v.window_fill, want);
}

#[test]
fn none_follows_theme_not_the_override_colour() {
    let cfg = Config::default(); // background_override = None
    let app = ScribeApp::new_test(cfg);
    let v = app.current_visuals();
    // Whatever the theme is, it must NOT be the arbitrary override colour.
    assert_ne!(v.panel_fill, egui::Color32::from_rgb(0x11, 0x22, 0x33));
}

#[test]
fn linked_note_background_follows_the_app_override() {
    // #106 — linked (default): the note well (extreme_bg_color) tracks the
    // app background override.
    let mut cfg = Config::default();
    cfg.appearance.link_backgrounds = true;
    cfg.appearance.background_override = Some("#112233".into());
    let app = ScribeApp::new_test(cfg);
    let v = app.current_visuals();
    let want = egui::Color32::from_rgb(0x11, 0x22, 0x33);
    assert_eq!(v.panel_fill, want);
    assert_eq!(v.extreme_bg_color, want, "linked note follows app bg");
}

#[test]
fn unlinked_note_background_is_independent() {
    // #106 — unlinked: app + note backgrounds are set separately.
    let mut cfg = Config::default();
    cfg.appearance.link_backgrounds = false;
    cfg.appearance.background_override = Some("#112233".into());
    cfg.appearance.note_background_override = Some("#445566".into());
    let app = ScribeApp::new_test(cfg);
    let v = app.current_visuals();
    assert_eq!(v.panel_fill, egui::Color32::from_rgb(0x11, 0x22, 0x33));
    assert_eq!(
        v.extreme_bg_color,
        egui::Color32::from_rgb(0x44, 0x55, 0x66)
    );
}
