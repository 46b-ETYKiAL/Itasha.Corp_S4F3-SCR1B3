//! Integration coverage for the change-bar gutter wiring on `ScribeApp` /
//! `EditorTab`: the edit -> save -> reload baseline transitions and the lazy
//! `ensure_change_states` cache. The pure classification logic is unit-tested
//! in `crate::change_bar`; here we verify the app plumbs the baselines through
//! correctly. (The gutter PAINT itself is visual and verified on-machine.)

use super::*;
use crate::change_bar::LineChange::{None as NoneL, Saved, Unsaved};

fn tab_with(text: &str) -> EditorTab {
    let mut t = EditorTab::scratch();
    t.text = text.to_string();
    t.session_baseline = text.to_string();
    t.saved_baseline = text.to_string();
    t
}

#[test]
fn change_bar_tracks_edit_then_save_then_reload() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs.clear();
    app.tabs.push(tab_with("alpha\nbeta\ngamma\n"));
    app.active = 0;

    // Freshly opened: no marks.
    app.ensure_change_states(0);
    assert_eq!(
        app.tabs[0].change_states,
        vec![NoneL, NoneL, NoneL],
        "no marks on a clean buffer"
    );

    // Edit line 2 -> Unsaved on that line only.
    app.tabs[0].set_text("alpha\nBETA\ngamma\n".to_string());
    app.ensure_change_states(0);
    assert_eq!(
        app.tabs[0].change_states,
        vec![NoneL, Unsaved, NoneL],
        "edited line is unsaved"
    );

    // Save -> the edited line flips to Saved (saved baseline now includes it).
    app.tabs[0].mark_change_saved();
    app.ensure_change_states(0);
    assert_eq!(
        app.tabs[0].change_states,
        vec![NoneL, Saved, NoneL],
        "edited line is saved (green) after save"
    );

    // Re-edit the same line without saving -> back to Unsaved.
    app.tabs[0].set_text("alpha\nBETA2\ngamma\n".to_string());
    app.ensure_change_states(0);
    assert_eq!(
        app.tabs[0].change_states,
        vec![NoneL, Unsaved, NoneL],
        "re-editing a saved line marks it unsaved again"
    );

    // Reload from disk resets both baselines -> clean.
    app.tabs[0].reset_change_baselines();
    app.ensure_change_states(0);
    assert_eq!(
        app.tabs[0].change_states,
        vec![NoneL, NoneL, NoneL],
        "reload clears all marks"
    );
}

#[test]
fn change_bar_cache_is_lazy_and_size_capped() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs.clear();
    app.tabs.push(tab_with("a\nb\n"));
    app.active = 0;

    // First compute populates the cache and records the edit_gen.
    app.ensure_change_states(0);
    let gen = app.tabs[0].change_gen;
    assert_eq!(gen, Some(app.tabs[0].edit_gen));

    // No edit -> no recompute (cache stays at the same generation).
    app.ensure_change_states(0);
    assert_eq!(app.tabs[0].change_gen, gen);

    // Disabling the feature short-circuits without touching the cache.
    app.config.editor.show_change_bar = false;
    app.tabs[0].set_text("a\nB\n".to_string());
    app.ensure_change_states(0);
    assert!(
        app.tabs[0].change_gen != Some(app.tabs[0].edit_gen),
        "recompute is skipped while the change bar is disabled"
    );
}

#[test]
fn change_bar_respects_disabled_default_paths() {
    // A brand-new scratch tab with no edits never paints a bar.
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs.clear();
    app.tabs.push(EditorTab::scratch());
    app.active = 0;
    app.ensure_change_states(0);
    assert!(
        app.tabs[0].change_states.iter().all(|s| *s == NoneL),
        "an untouched scratch tab has no change marks"
    );
}
