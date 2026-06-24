//! QA security-workflow entry-surface drive (task #38): prove the W4 (plugin
//! trust/install) and W8 (self-updater) security workflows are REACHABLE from the
//! real `ScribeApp::frame_tick` render loop (egui_kittest, no GPU) — the
//! app-layer complement to the core scenario suite in
//! `scribe-core/tests/qa_workflow_security.rs`.
//!
//! The heavy security assertions (sandbox capability matrix, dual-gate
//! signature/checksum reject paths, anti-rollback, backup/rollback) live in
//! scribe-core where the pure verify/apply/trust logic is. This file asserts the
//! USER-FACING ENTRY POINTS exist and render: a plugin author can reach the
//! Install/verify gate, and a user can reach the updater "Check for updates"
//! control — without either driving real network (both surfaces are rendered
//! deterministically; no check is launched here).
//!
//! Non-redundant with `plugin_manager_tests.rs` (which already drives the
//! manager tab-switching in depth): here we frame the SAME reachable surfaces as
//! the security-workflow ENTRY assertions W4/W8 require, and add the Settings
//! Updates pane "Check for updates" control — the W8 entry the spec marks as a
//! BUTTON-surface gap — through the full app rather than the standalone
//! `settings::show` widget.

#![allow(clippy::wildcard_imports)]
use super::*;
use egui_kittest::kittest::Queryable as _;

/// A deterministic, network-free test app: first-run done, framed window,
/// updater left in its default OFF state by `new_test` (no on-launch check).
fn sec_app() -> ScribeApp {
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

// ===========================================================================
// W4 — plugin author's Install/verify gate is reachable through the full app.
// ===========================================================================

/// W4 entry: opening the plugin manager and switching to the **Install** tab
/// surfaces the **verify** control — the gate a plugin author drives to check a
/// tarball's SHA-256 + minisign before installing. The verification LOGIC is
/// asserted in scribe-core; here we only prove the gate is reachable (a user
/// cannot install a plugin without passing through a rendered verify surface).
#[test]
fn w4_install_verify_gate_is_reachable_from_the_app() {
    let mut app = sec_app();
    app.execute_builtin(BuiltinCommand::OpenPluginManager);
    let mut h = harness(app);
    h.run();
    h.run();
    assert!(
        h.state().plugin_manager.open,
        "the plugin manager must open"
    );

    h.get_by_label("Install").click();
    h.run();
    assert_eq!(
        h.state().plugin_manager.tab,
        crate::plugin_manager::PluginManagerTab::Install,
        "the Install tab must activate"
    );
    assert!(
        h.query_by_label("verify").is_some(),
        "the Install pane must render the verify control — the signature/checksum gate is reachable"
    );
}

// ===========================================================================
// W8 — the self-updater check surface is reachable from Settings → Updates.
// ===========================================================================

/// W8 entry: opening Settings and selecting the **Updates** pane surfaces the
/// "Check for updates" control. We render the pane deterministically and assert
/// the button is present WITHOUT clicking it (a click would start a real network
/// check); the verify→apply→rollback logic it ultimately drives is asserted in
/// scribe-core. This closes the "updater check BUTTON surface" entry the QA spec
/// marks as a gap.
#[test]
fn w8_updates_pane_check_control_is_reachable() {
    let mut h = harness(sec_app());
    h.state_mut().settings_open = true;
    h.run();
    h.get_by_label("Updates").click();
    h.run();
    assert!(
        h.state().settings_open,
        "settings must still be open on the Updates pane"
    );
    assert!(
        h.query_by_label("Check for updates").is_some(),
        "the Updates pane must render its 'Check for updates' control (the W8 entry point)"
    );
}

/// W8 entry (privacy posture): the default test app performs NO automatic update
/// check — `new_test` leaves the updater idle and the on-launch mode OFF, so
/// merely opening the app and rendering frames never launches a network check.
/// This pins the "no surprise network on launch" property the updater contract
/// relies on (the actual check is user-initiated from the surface above).
#[test]
fn w8_no_automatic_update_check_on_launch_in_default_app() {
    let mut h = harness(sec_app());
    // Several frames — a `notify`/`auto` launch mode would have surfaced a
    // pending toast or opened the yes/no prompt by now; neither must appear.
    for _ in 0..4 {
        h.run();
    }
    assert!(
        !h.state().updater.show_prompt,
        "no auto-update yes/no prompt may appear on a default launch"
    );
    assert!(
        h.state().updater.toast_pending.is_none(),
        "no auto-update toast may appear on a default launch"
    );
}
