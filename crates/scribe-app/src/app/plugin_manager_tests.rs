//! Headless e2e (task #34): drive the **plugin manager** window through the REAL
//! `ScribeApp::frame_tick` render loop (egui_kittest, no GPU). The standalone
//! `plugin_manager.rs` tests drive its `show()` widget directly; this sibling
//! drives it through the FULL app — opened by `BuiltinCommand::OpenPluginManager`,
//! rendered from `frame_tick` against the app's discovered rows + config dir.
//!
//! Covered through the full app: open (`plugin_manager.open`), tab switching
//! (Loaded → Registry → Install) with the active-tab + per-tab content asserted,
//! the empty Loaded state, a real registry parse via the Registry "load" button,
//! and the Install verify SURFACE reachability.
//!
//! SKIPPED through the full app (documented, covered elsewhere): the Loaded-tab
//! enable-checkbox toggle and the "Approve & run" trust gate. Plugin DISCOVERY in
//! `build()` is gated on `config.plugins.enabled`, which `ScribeApp::new_test`
//! force-disables for deterministic, network-free, isolation-safe tests — so the
//! Loaded tab is always empty under `new_test` and no enable/pending row is ever
//! reachable here. Those click→state paths are fully covered at the `show()`
//! level in `crate::plugin_manager`
//! (`show_loaded_toggle_checkbox_raises_disable_action`,
//! `show_loaded_approve_click_raises_action`). Re-driving them here would require
//! fabricating a build-time discovery fixture the `new_test` harness cannot reach.
//!
//! Isolation: these tests do NOT mutate the process-global `SCR1B3_CONFIG_DIR`.
//! The Loaded tab is empty regardless of the config dir (discovery is off under
//! `new_test`), and the registry test points the registry path field DIRECTLY at
//! a tempdir file rather than relying on the config-dir default — so no env race
//! with sibling test modules.
#![allow(clippy::wildcard_imports)]
use super::*;
use egui_kittest::kittest::Queryable as _;

fn pm_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    ScribeApp::new_test(cfg)
}

fn harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 820.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

/// Open the manager via its builtin command and settle a couple of frames.
fn open_manager() -> egui_kittest::Harness<'static, ScribeApp> {
    let mut app = pm_app();
    app.execute_builtin(BuiltinCommand::OpenPluginManager);
    let mut h = harness(app);
    h.run();
    h.run();
    h
}

/// A minimal, valid registry `index.toml` body the Registry "load" button parses.
const SAMPLE_REGISTRY: &str = r#"
schema_version = 1

[[plugins]]
id = "com.example.hello"
name = "Hello World"
description = "Greets you on save"
author = "Ada"
version_stable = "1.2.0"
author_pubkey = "RWQexamplekey"

[[plugins.releases]]
version = "1.2.0"
tarball_url = "https://example.com/hello-1.2.0.tar.gz"
signature_url = "https://example.com/hello-1.2.0.tar.gz.minisig"
checksum_sha256 = "deadbeef"
api_version = 1
capabilities = ["read_buffer"]
"#;

/// The window opens on its default Loaded tab; the three tab headers + the
/// Loaded empty-state header all render.
#[test]
fn plugin_manager_opens_on_loaded_tab() {
    let h = open_manager();
    assert!(h.state().plugin_manager.open, "manager must be open");
    assert_eq!(
        h.state().plugin_manager.tab,
        crate::plugin_manager::PluginManagerTab::Loaded,
        "the default tab is Loaded"
    );
    // The three tab headers are reachable, and the Loaded tab's dir header paints.
    assert!(h.query_by_label("Loaded").is_some(), "Loaded tab header");
    assert!(
        h.query_by_label("Registry").is_some(),
        "Registry tab header"
    );
    assert!(h.query_by_label("Install").is_some(), "Install tab header");
    assert!(
        h.query_by_label("open folder").is_some(),
        "the Loaded tab's plugins-dir header always paints (even empty)"
    );
}

/// Clicking the **Registry** tab header switches the active tab and surfaces the
/// registry's "load" control (the Registry pane body).
#[test]
fn clicking_registry_tab_switches_and_renders_load_control() {
    let mut h = open_manager();
    h.get_by_label("Registry").click();
    h.run();
    assert_eq!(
        h.state().plugin_manager.tab,
        crate::plugin_manager::PluginManagerTab::Registry,
        "clicking the Registry header must activate the Registry tab"
    );
    assert!(
        h.query_by_label("load").is_some(),
        "the Registry pane must render its load button"
    );
}

/// Clicking the **Install** tab header switches the active tab and surfaces the
/// verify SURFACE (the gate is reachable). We assert the verify button renders —
/// without fabricating a signed tarball; the actual verification logic is tested
/// in `crate::plugin_manager::verify_install_*`.
#[test]
fn clicking_install_tab_switches_and_renders_verify_surface() {
    let mut h = open_manager();
    h.get_by_label("Install").click();
    h.run();
    assert_eq!(
        h.state().plugin_manager.tab,
        crate::plugin_manager::PluginManagerTab::Install,
        "clicking the Install header must activate the Install tab"
    );
    assert!(
        h.query_by_label("verify").is_some(),
        "the Install pane must render the verify control (the gate is reachable)"
    );
}

/// On the Registry tab, pointing the registry path field DIRECTLY at a real
/// `index.toml` and clicking **load** parses the registry into state and renders
/// its entry — the full-app round-trip through `load_registry` driven by a real
/// button click. The path is set explicitly (not via the config-dir default), so
/// the load is deterministic and free of any `SCR1B3_CONFIG_DIR` race.
#[test]
fn registry_load_button_parses_explicit_index() {
    let tmp = tempfile::tempdir().unwrap();
    let index = tmp.path().join("index.toml");
    std::fs::write(&index, SAMPLE_REGISTRY).unwrap();

    let mut h = open_manager();
    h.get_by_label("Registry").click();
    h.run();
    // Point the path field at our real file; load_registry reads this captured
    // string (no env dependency).
    h.state_mut().plugin_manager.registry_path = index.display().to_string();
    h.run();
    h.get_by_label("load").click();
    h.run();
    h.run();
    assert!(
        h.state().plugin_manager.registry.is_some(),
        "clicking load must parse the registry into state"
    );
    assert!(
        h.state().plugin_manager.registry_error.is_none(),
        "a valid registry must parse without error"
    );
    // The parsed entry renders in the list.
    assert!(
        h.query_by_label("Hello World").is_some(),
        "the parsed registry entry must render"
    );
}

/// The Loaded tab's empty state is the truthful surface under `new_test` (plugin
/// discovery is force-disabled): no plugin rows, but the dir header still paints.
/// This documents the SKIP boundary for the enable/approve paths (module header).
#[test]
fn loaded_tab_empty_under_new_test_discovery_disabled() {
    let h = open_manager();
    assert_eq!(
        h.state().plugin_manager.tab,
        crate::plugin_manager::PluginManagerTab::Loaded
    );
    // No discovered rows => no enable checkbox and no "Approve & run" CTA exist
    // through the full-app harness (discovery is gated off in new_test).
    assert!(
        h.query_by_label("Approve & run").is_none(),
        "no pending-approval row is reachable under new_test (discovery disabled)"
    );
    // But the empty Loaded surface still renders its dir header.
    assert!(
        h.query_by_label("open folder").is_some(),
        "the empty Loaded tab still paints its plugins-dir header"
    );
}

/// Tab round-trip: Loaded → Install → Loaded re-activates the Loaded tab and
/// re-paints its dir header, proving `selectable_value` tab state round-trips
/// through the full render loop.
#[test]
fn tab_switch_round_trips_back_to_loaded() {
    let mut h = open_manager();
    h.get_by_label("Install").click();
    h.run();
    assert_eq!(
        h.state().plugin_manager.tab,
        crate::plugin_manager::PluginManagerTab::Install
    );
    h.get_by_label("Loaded").click();
    h.run();
    assert_eq!(
        h.state().plugin_manager.tab,
        crate::plugin_manager::PluginManagerTab::Loaded,
        "clicking Loaded must switch back"
    );
    assert!(
        h.query_by_label("open folder").is_some(),
        "the Loaded pane re-renders its dir header after a round-trip"
    );
}

#[test]
fn discovered_plugin_rows_reflects_the_disabled_set() {
    // discovered_plugin_rows takes a plugins dir explicitly (bypassing the
    // discovery gating in new_test), so it can be exercised directly. A plugin on
    // disk that is in config.plugins.disabled must report enabled=false.
    let tmp = tempfile::tempdir().unwrap();
    let pdir = tmp.path().join("uppercase");
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(
        pdir.join("plugin.toml"),
        "id='uppercase'\nname='Uppercase'\napi_version=1\nentry='main.rhai'\n",
    )
    .unwrap();
    std::fs::write(pdir.join("main.rhai"), "// noop").unwrap();
    let mut cfg = Config::default();
    cfg.plugins.disabled.push("uppercase".to_string());
    let app = ScribeApp::new_test(cfg);
    let rows = app.discovered_plugin_rows(tmp.path());
    // `replace body with vec![]` (1572) would return no rows.
    assert_eq!(rows.len(), 1, "the on-disk plugin is discovered");
    // `delete !` on `enabled: !disabled.contains(id)` (1578) would flip it to true.
    assert!(!rows[0].enabled, "a plugin in the disabled set is reported as disabled");
    assert_eq!(rows[0].id, "uppercase");
}
