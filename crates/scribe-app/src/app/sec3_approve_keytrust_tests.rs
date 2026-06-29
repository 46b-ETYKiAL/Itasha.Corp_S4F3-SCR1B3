//! SEC-3 — `approve_plugin` must route through the pinned-key trust gate.
//!
//! Clicking "Approve & run" on a pending plugin is explicit first-contact
//! consent, but it must NEVER silently accept an author-key ROTATION (a possible
//! plugin takeover). The load path (`build_plugins`) already blocks a changed
//! key via `decide_key_trust`; this proves the APPROVE button does too, by
//! routing through `scribe_core::plugin::pinned_keys::decide_approval`.
//!
//! Non-vacuity: the mismatch test fails (the plugin becomes trusted) the moment
//! the SEC-3 guard is removed from `approve_plugin`.

#![allow(clippy::wildcard_imports)]
use super::*;

const KEY_A: &str = "RWQpinnedAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const KEY_B: &str = "RWQrotatedBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

/// Build a network-free, first-run-done app whose config dir is a per-instance
/// temp dir (via `new_test`), with strict signing enabled so the key-trust gate
/// is active.
fn signed_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    cfg.plugins.require_signed = true;
    ScribeApp::new_test(cfg)
}

/// Write a discoverable plugin (`plugin.toml` + entry) under
/// `<config_dir>/plugins/<id>/` declaring `author_pubkey = pubkey`.
fn write_plugin_fixture(config_dir: &std::path::Path, id: &str, pubkey: &str) {
    let pdir = config_dir.join("plugins").join(id);
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(
        pdir.join("plugin.toml"),
        format!(
            "id='{id}'\nname='{id}'\napi_version=1\nentry='main.rhai'\nauthor_pubkey='{pubkey}'\n"
        ),
    )
    .unwrap();
    std::fs::write(pdir.join("main.rhai"), "// noop").unwrap();
}

#[test]
fn approve_plugin_blocks_rotated_author_key() {
    let mut app = signed_app();
    let dir = app.config_dir.clone().expect("test config dir");

    // The plugin on disk now ships author key B...
    write_plugin_fixture(&dir, "evilplug", KEY_B);
    // ...but key A was previously pinned for this id (the legitimate author).
    let mut store = scribe_core::plugin::PinnedKeyStore::new(&dir);
    store.pin_or_match("evilplug", KEY_A).unwrap();
    app.pending_plugins.push("evilplug".to_string());

    app.approve_plugin("evilplug");

    // BLOCKED: a rotated key is never trusted, never loaded, stays pending.
    assert!(
        !app.config.plugins.trusted.contains_key("evilplug"),
        "rotated-key plugin must NOT be trusted on approve"
    );
    assert!(
        app.pending_plugins.iter().any(|p| p == "evilplug"),
        "rotated-key plugin must remain pending (approval refused)"
    );
    let toast = app.toast.clone().unwrap_or_default();
    assert!(
        toast.contains("NOT approved") && toast.contains("key changed"),
        "user must be warned about the key change, got: {toast:?}"
    );
}

#[test]
fn approve_plugin_allows_first_contact_key() {
    let mut app = signed_app();
    let dir = app.config_dir.clone().expect("test config dir");

    // First contact: no key pinned yet. Explicit approval pins key B and allows.
    write_plugin_fixture(&dir, "goodplug", KEY_B);
    app.pending_plugins.push("goodplug".to_string());

    app.approve_plugin("goodplug");

    // First-contact consent upgrades New -> Allow, so the plugin is trusted
    // (the dummy entry may or may not load, but trust is recorded first).
    assert!(
        app.config.plugins.trusted.contains_key("goodplug"),
        "first-contact approval must trust the plugin"
    );
    // And the key is now pinned to B for future sessions.
    let mut store = scribe_core::plugin::PinnedKeyStore::new(&dir);
    let outcome = store.pin_or_match("goodplug", KEY_B).unwrap();
    assert!(
        matches!(outcome, scribe_core::plugin::pinned_keys::PinOutcome::Match),
        "approval must have pinned key B (subsequent match), got {outcome:?}"
    );
}
