//! Headless e2e (task #34): drive the in-app **theme editor** through the REAL
//! `ScribeApp::frame_tick` render loop (egui_kittest, no GPU). The theme editor
//! had ZERO interaction tests at the *full-app* level — `theme_editor.rs` unit
//! tests drive its `show()` widget in isolation, but nothing drove it through the
//! settings window the user actually opens.
//!
//! The editor renders inside the **Appearance** settings pane (the default pane),
//! so opening settings surfaces its `name` field + Save / Reset / New buttons.
//! These tests open settings, confirm the editor's live preview rendered, then
//! drive **Reset** (reverts the working copy) and **New from current** (seeds a
//! copy) — the env-independent actions that change NO files.
//!
//! The **Save theme** persist path (write a user TOML + switch the active theme)
//! reads the GLOBAL `Config::config_dir()` across frames, which a parallel test
//! run can relocate via the process-global `SCR1B3_CONFIG_DIR` env. To keep this
//! suite isolation-safe (no real-config-dir pollution, no cross-module env race),
//! Save is NOT clicked here — it is covered hermetically at the `show()` level by
//! `crate::theme_editor::tests::show_renders_preview_saves_and_reports_changed_config`
//! (which writes + reparses the TOML under its own `SCR1B3_CONFIG_DIR` guard).
//! What this file adds is the full-app reachability of the editor + its non-
//! persisting actions through the real Settings window.
#![allow(clippy::wildcard_imports)]
use super::*;
use egui_kittest::kittest::Queryable as _;

/// A first-run, non-frameless app started on a KNOWN built-in theme so the editor
/// seeds a real palette (not the house fallback).
fn theme_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    cfg.appearance.theme = "wired-noir".to_string();
    ScribeApp::new_test(cfg)
}

fn harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1180.0, 900.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

/// Open settings (the default Appearance pane carries the theme editor) and
/// settle several frames so the editor seeds its working copy + live preview.
fn open_settings_on_appearance() -> egui_kittest::Harness<'static, ScribeApp> {
    let mut h = harness(theme_app());
    h.state_mut().settings_open = true;
    h.run();
    h.run();
    h
}

/// Opening settings on Appearance renders the theme editor: its live preview
/// (the `● READY` status-bar token) and its Save + Reset controls are reachable.
#[test]
fn theme_editor_renders_in_appearance_pane() {
    let h = open_settings_on_appearance();
    assert!(h.state().settings_open, "settings must be open");
    assert!(
        h.query_by_label("Save theme").is_some(),
        "the theme editor's Save control must render in the Appearance pane"
    );
    assert!(
        h.query_by_label("Reset").is_some(),
        "the theme editor's Reset control must render"
    );
    assert!(
        h.query_by_label("New from current").is_some(),
        "the theme editor's New-from-current control must render"
    );
    assert!(
        h.query_by_label("● READY").is_some(),
        "the theme editor's live preview must render"
    );
}

/// Clicking **Reset** re-seeds the working copy from the active theme and shows
/// its status, WITHOUT changing the active theme (Reset never persists, never
/// touches the config dir).
#[test]
fn reset_reverts_working_copy_without_changing_active_theme() {
    let mut h = open_settings_on_appearance();
    let before = h.state().config.appearance.theme.clone();
    h.get_by_label("Reset").click();
    h.run();
    assert_eq!(
        h.state().config.appearance.theme,
        before,
        "Reset must NOT change the active theme"
    );
    assert!(
        h.query_by_label("reset to the active theme").is_some(),
        "Reset must surface its status line"
    );
}

/// Clicking **New from current** seeds a fresh working copy named `<active>-copy`
/// and surfaces its status — the "duplicate this theme" entry point — without
/// changing the active theme or writing any file.
#[test]
fn new_from_current_seeds_a_copy() {
    let mut h = open_settings_on_appearance();
    let before = h.state().config.appearance.theme.clone();
    h.get_by_label("New from current").click();
    h.run();
    assert_eq!(
        h.state().config.appearance.theme,
        before,
        "New from current must NOT change the active theme (it only seeds a copy)"
    );
    assert!(
        h.query_by_label("started a new theme from the active one")
            .is_some(),
        "New from current must surface its status line"
    );
}

/// Reset → New round-trip: both status lines reachable in sequence, and the
/// editor's live preview keeps rendering across the action sequence — proving the
/// editor stays in a coherent, re-renderable state through the full render loop.
#[test]
fn reset_then_new_keeps_editor_coherent() {
    let mut h = open_settings_on_appearance();
    h.get_by_label("Reset").click();
    h.run();
    assert!(h.query_by_label("reset to the active theme").is_some());
    h.get_by_label("New from current").click();
    h.run();
    assert!(
        h.query_by_label("started a new theme from the active one")
            .is_some(),
        "New from current must surface its status after a prior Reset"
    );
    assert!(
        h.query_by_label("● READY").is_some(),
        "the editor preview must still render after Reset + New"
    );
    assert!(
        h.state().settings_open,
        "settings must stay open through the action sequence"
    );
}
