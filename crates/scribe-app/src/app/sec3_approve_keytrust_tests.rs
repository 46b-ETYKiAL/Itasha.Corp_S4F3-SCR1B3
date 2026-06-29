//! SEC-3 — `approve_plugin` must enforce the SAME strict-mode policy as the
//! load path (`build_plugins`) when `require_signed` is on:
//!   1. a signed plugin must carry BOTH an author key AND a signature,
//!   2. the minisign signature over the entry script must verify,
//!   3. the pinned author key must not have ROTATED (a possible takeover).
//!
//! Clicking "Approve & run" is explicit first-contact consent, but consent only
//! upgrades a New first-contact key to Allow — it never downgrades (1)–(3).
//!
//! Non-vacuity: each refusal/allow test bites if the corresponding guard in
//! `approve_plugin` is removed (proven by reverting each branch).

#![allow(clippy::wildcard_imports)]
use super::*;

/// A network-free, first-run-done app with strict signing on, config dir
/// redirected to a per-instance temp dir (via `new_test`).
fn signed_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    cfg.plugins.require_signed = true;
    ScribeApp::new_test(cfg)
}

const ENTRY: &str = "// noop";

/// Generate a fresh minisign keypair and return its full public-key box string.
fn gen_pk() -> (minisign::KeyPair, String) {
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pk_box = kp.pk.to_box().unwrap().to_string();
    (kp, pk_box)
}

/// Sign `ENTRY` with `kp` and return the `.minisig` box string.
fn sign_entry(kp: &minisign::KeyPair) -> String {
    minisign::sign(
        Some(&kp.pk),
        &kp.sk,
        std::io::Cursor::new(ENTRY.as_bytes()),
        Some("scr1b3-test"),
        Some("comment"),
    )
    .unwrap()
    .to_string()
}

/// Write a discoverable plugin under `<config_dir>/plugins/<id>/` whose manifest
/// declares the given (optional) author key + signature.
fn write_plugin(config_dir: &std::path::Path, id: &str, pubkey: Option<&str>, sig: Option<&str>) {
    let pdir = config_dir.join("plugins").join(id);
    std::fs::create_dir_all(&pdir).unwrap();
    let mut toml = format!("id='{id}'\nname='{id}'\napi_version=1\nentry='main.rhai'\n");
    if let Some(pk) = pubkey {
        toml.push_str(&format!("author_pubkey='''{pk}'''\n"));
    }
    if let Some(s) = sig {
        toml.push_str(&format!("signature='''{s}'''\n"));
    }
    std::fs::write(pdir.join("plugin.toml"), toml).unwrap();
    std::fs::write(pdir.join("main.rhai"), ENTRY).unwrap();
}

#[test]
fn approve_plugin_blocks_rotated_author_key() {
    let mut app = signed_app();
    let dir = app.config_dir.clone().expect("test config dir");

    // The plugin on disk ships key B with a VALID signature over the entry...
    let (kp_b, pk_b) = gen_pk();
    let sig_b = sign_entry(&kp_b);
    write_plugin(&dir, "evilplug", Some(&pk_b), Some(&sig_b));
    // ...but key A was previously pinned for this id (the legitimate author).
    let (_kp_a, pk_a) = gen_pk();
    let mut store = scribe_core::plugin::PinnedKeyStore::new(&dir);
    store.pin_or_match("evilplug", &pk_a).unwrap();
    app.pending_plugins.push("evilplug".to_string());

    app.approve_plugin("evilplug");

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
fn approve_plugin_allows_first_contact_signed_key() {
    let mut app = signed_app();
    let dir = app.config_dir.clone().expect("test config dir");

    // First contact: properly signed, no key pinned yet. Approval pins + allows.
    let (kp, pk) = gen_pk();
    let sig = sign_entry(&kp);
    write_plugin(&dir, "goodplug", Some(&pk), Some(&sig));
    app.pending_plugins.push("goodplug".to_string());

    app.approve_plugin("goodplug");

    assert!(
        app.config.plugins.trusted.contains_key("goodplug"),
        "first-contact signed approval must trust the plugin"
    );
    // The key is now pinned (subsequent presentation Matches).
    let mut store = scribe_core::plugin::PinnedKeyStore::new(&dir);
    let outcome = store.pin_or_match("goodplug", &pk).unwrap();
    assert!(
        matches!(outcome, scribe_core::plugin::pinned_keys::PinOutcome::Match),
        "approval must have pinned the author key, got {outcome:?}"
    );
}

#[test]
fn approve_plugin_refuses_unsigned_in_signed_mode() {
    let mut app = signed_app();
    let dir = app.config_dir.clone().expect("test config dir");

    // require_signed is on but the plugin carries no signature (and no key).
    write_plugin(&dir, "unsignedplug", None, None);
    app.pending_plugins.push("unsignedplug".to_string());

    app.approve_plugin("unsignedplug");

    assert!(
        !app.config.plugins.trusted.contains_key("unsignedplug"),
        "unsigned plugin must NOT be trusted under require_signed"
    );
    let toast = app.toast.clone().unwrap_or_default();
    assert!(
        toast.contains("NOT approved") && toast.contains("signed"),
        "user must be told signed mode requires a signed plugin, got: {toast:?}"
    );
}

#[test]
fn approve_plugin_refuses_bad_signature() {
    let mut app = signed_app();
    let dir = app.config_dir.clone().expect("test config dir");

    // Valid-looking author key but a signature that does NOT verify the entry.
    let (_kp, pk) = gen_pk();
    write_plugin(
        &dir,
        "tamperedplug",
        Some(&pk),
        Some("untrusted comment: forged\nRWQbogusSignatureThatWillNotVerifyAAAAAAAAAAAAAAAAAAAA"),
    );
    app.pending_plugins.push("tamperedplug".to_string());

    app.approve_plugin("tamperedplug");

    assert!(
        !app.config.plugins.trusted.contains_key("tamperedplug"),
        "plugin with a non-verifying signature must NOT be trusted"
    );
    let toast = app.toast.clone().unwrap_or_default();
    assert!(
        toast.contains("NOT approved") && toast.contains("signature"),
        "user must be told the signature did not verify, got: {toast:?}"
    );
}
