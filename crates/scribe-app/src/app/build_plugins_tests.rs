//! Coverage for the plugin trust gate on the LOAD path (`build_plugins::load_plugins`).
//!
//! This is the gate that decides, at every launch, whether a script sitting in
//! the plugins dir gets executed. It was at 23.4% line coverage with no tests.
//!
//! The reason it went untested is worth naming, because it is not "backlog".
//! `sec3_approve_keytrust_tests.rs` covers `approve_plugin` and states in its
//! own header that `approve_plugin` must enforce "the SAME strict-mode policy
//! as the load path (`build_plugins`)". The policy therefore exists in TWO
//! places, and only the approve copy was tested — the copy that runs on every
//! launch against every plugin was not. Two implementations of one security
//! policy drift independently; testing one proves nothing about the other.
//!
//! ASSERT ON THE ACCEPT PATH TOO. A gate tested only on its refusals is not
//! tested: if the whole gate were deleted, every refusal test would still need
//! to fail for the suite to notice, and it is the accept tests that prove the
//! refusals are discriminating rather than a stuck "no". Each test below that
//! expects a load asserts the command actually appears.
//!
//! WHY THE ENTRY SCRIPT REGISTERS A COMMAND: a `// noop` entry exports nothing,
//! so `commands()` is empty whether or not the plugin loaded — asserting on it
//! would pass with the gate deleted. `ENTRY` registers a command so "loaded"
//! and "not loaded" are actually distinguishable.
//!
//! WHY THE ENV GUARD IS MANDATORY: `load_plugins` reads the process-global
//! `Config::config_dir()`, NOT the per-instance one. Without the redirect these
//! tests would read the real user's plugins dir — the same class of sandbox
//! escape as the Windows registry test that deleted real user state.
use scribe_core::Config;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Serializes tests that mutate the process-global `SCR1B3_CONFIG_DIR`.
/// cargo runs tests in parallel, so the redirect must be exclusive.
static CONFIG_DIR_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_config_dir<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
    let _guard = CONFIG_DIR_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var_os("SCR1B3_CONFIG_DIR");
    std::env::set_var("SCR1B3_CONFIG_DIR", dir);
    let out = body();
    match prev {
        Some(v) => std::env::set_var("SCR1B3_CONFIG_DIR", v),
        None => std::env::remove_var("SCR1B3_CONFIG_DIR"),
    }
    out
}

fn temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "scr1b3-buildplugins-{}-{}-{}",
        tag,
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    // A PID+counter name is unique among LIVE processes, not over time: PIDs
    // recycle and these dirs are never swept. Never inherit a prior run's state.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Registers a command, so "did it load?" is observable.
const ENTRY: &str = r#"fn probe_fn() { } register_command("probe", "Probe", "probe_fn");"#;

fn gen_pk() -> (minisign::KeyPair, String) {
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pk_box = kp.pk.to_box().unwrap().to_string();
    (kp, pk_box)
}

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

fn entry_sha() -> String {
    scribe_core::update::verify::sha256_hex(ENTRY.as_bytes())
}

/// Write a discoverable plugin under `<config_dir>/plugins/<id>/`.
fn write_plugin(
    config_dir: &Path,
    id: &str,
    pubkey: Option<&str>,
    sig: Option<&str>,
    min_app_version: Option<&str>,
) {
    let pdir = config_dir.join("plugins").join(id);
    std::fs::create_dir_all(&pdir).unwrap();
    let mut toml = format!("id='{id}'\nname='{id}'\napi_version=1\nentry='main.rhai'\n");
    if let Some(pk) = pubkey {
        toml.push_str(&format!("author_pubkey='''{pk}'''\n"));
    }
    if let Some(s) = sig {
        toml.push_str(&format!("signature='''{s}'''\n"));
    }
    if let Some(v) = min_app_version {
        toml.push_str(&format!("min_app_version='{v}'\n"));
    }
    std::fs::write(pdir.join("plugin.toml"), toml).unwrap();
    std::fs::write(pdir.join("main.rhai"), ENTRY).unwrap();
}

fn signed_config() -> Config {
    let mut cfg = Config::default();
    cfg.plugins.enabled = true;
    cfg.plugins.require_signed = true;
    cfg
}

fn tofu_config() -> Config {
    let mut cfg = Config::default();
    cfg.plugins.enabled = true;
    cfg.plugins.require_signed = false;
    cfg
}

fn loaded(cmds: &[scribe_core::plugin::CommandInfo]) -> bool {
    cmds.iter().any(|c| c.id == "probe")
}

/// Drive the real load path against `dir`.
fn run(
    dir: &Path,
    cfg: &Config,
) -> (
    Vec<String>,
    Vec<scribe_core::plugin::CommandInfo>,
    Option<String>,
) {
    with_config_dir(dir, || {
        let mut toast = None;
        let (_host, pending, cmds) = super::build_plugins::load_plugins(cfg, &mut toast);
        (pending, cmds, toast)
    })
}

// ---- strict (require_signed) mode: the ACCEPT path ----

#[test]
fn signed_mode_loads_a_signed_plugin_the_user_already_approved() {
    let dir = temp_dir("signed-accept");
    let (kp, pk) = gen_pk();
    write_plugin(&dir, "p", Some(&pk), Some(&sign_entry(&kp)), None);
    let mut cfg = signed_config();
    // Prior explicit consent to THIS exact entry script.
    cfg.plugins.trusted = BTreeMap::from([("p".to_string(), entry_sha())]);

    let (pending, cmds, _) = run(&dir, &cfg);

    assert!(
        loaded(&cmds),
        "a correctly-signed plugin the user approved MUST run — if this fails the \
         gate is refusing everything, which would make every refusal test below vacuous"
    );
    assert!(pending.is_empty(), "an approved plugin is not pending");
}

#[test]
fn signed_mode_loads_a_plugin_whose_pinned_key_still_matches() {
    let dir = temp_dir("signed-pinmatch");
    let (kp, pk) = gen_pk();
    write_plugin(&dir, "p", Some(&pk), Some(&sign_entry(&kp)), None);
    let mut cfg = signed_config();
    cfg.plugins.trusted = BTreeMap::from([("p".to_string(), entry_sha())]);

    // First run pins the author key.
    let _ = run(&dir, &cfg);
    // Second run: same key => Match => Allow.
    let (_, cmds, _) = run(&dir, &cfg);

    assert!(
        loaded(&cmds),
        "an unchanged pinned author key must keep loading across launches"
    );
}

// ---- strict mode: the REFUSAL paths ----

#[test]
fn signed_mode_refuses_an_unsigned_plugin() {
    let dir = temp_dir("signed-unsigned");
    write_plugin(&dir, "p", None, None, None);

    let (_, cmds, _) = run(&dir, &signed_config());

    assert!(
        !loaded(&cmds),
        "require_signed is on: an unsigned plugin must never execute"
    );
}

#[test]
fn signed_mode_refuses_a_plugin_whose_signature_does_not_verify() {
    let dir = temp_dir("signed-badsig");
    let (kp, pk) = gen_pk();
    // Sign with a DIFFERENT key than the manifest declares.
    let (other_kp, _) = gen_pk();
    let bad = sign_entry(&other_kp);
    let _ = kp;
    write_plugin(&dir, "p", Some(&pk), Some(&bad), None);

    let (_, cmds, _) = run(&dir, &signed_config());

    assert!(
        !loaded(&cmds),
        "a signature that does not verify against the declared key must never execute"
    );
}

#[test]
fn signed_mode_holds_a_first_contact_plugin_the_user_has_not_approved() {
    let dir = temp_dir("signed-firstcontact");
    let (kp, pk) = gen_pk();
    write_plugin(&dir, "p", Some(&pk), Some(&sign_entry(&kp)), None);

    // Signed correctly, but NO prior consent to this entry script.
    let (pending, cmds, toast) = run(&dir, &signed_config());

    assert!(
        !loaded(&cmds),
        "a valid signature is not consent: a first-seen author key must not \
         auto-execute — this is the drop-a-folder-in-and-it-runs hole"
    );
    assert_eq!(
        pending,
        vec!["p".to_string()],
        "it must be held for approval"
    );
    let t = toast.unwrap_or_default();
    assert!(
        t.contains("need your approval"),
        "the user must be told something is waiting, got: {t:?}"
    );
}

#[test]
fn signed_mode_blocks_a_plugin_whose_author_key_rotated() {
    let dir = temp_dir("signed-rotate");
    let (kp1, pk1) = gen_pk();
    write_plugin(&dir, "p", Some(&pk1), Some(&sign_entry(&kp1)), None);
    let mut cfg = signed_config();
    cfg.plugins.trusted = BTreeMap::from([("p".to_string(), entry_sha())]);

    // First run pins key 1 and loads.
    let (_, first, _) = run(&dir, &cfg);
    assert!(
        loaded(&first),
        "precondition: it loads under the original key"
    );

    // The author key changes underneath us — possible takeover.
    let (kp2, pk2) = gen_pk();
    write_plugin(&dir, "p", Some(&pk2), Some(&sign_entry(&kp2)), None);

    let (_, cmds, toast) = run(&dir, &cfg);

    assert!(
        !loaded(&cmds),
        "a ROTATED author key must block the load even though the new signature \
         verifies and the entry hash is still trusted — that combination is what \
         a takeover looks like"
    );
    let t = toast.unwrap_or_default();
    assert!(
        t.contains("BLOCKED") && t.contains("author key changed"),
        "the takeover must be surfaced as blocking, not a log line, got: {t:?}"
    );
}

#[test]
fn the_takeover_warning_outranks_the_softer_pending_toast() {
    let dir = temp_dir("signed-toastrank");
    // One plugin rotates its key; another is merely pending approval.
    let (kp1, pk1) = gen_pk();
    write_plugin(&dir, "rotator", Some(&pk1), Some(&sign_entry(&kp1)), None);
    let mut cfg = signed_config();
    cfg.plugins.trusted = BTreeMap::from([("rotator".to_string(), entry_sha())]);
    let _ = run(&dir, &cfg);

    let (kp2, pk2) = gen_pk();
    write_plugin(&dir, "rotator", Some(&pk2), Some(&sign_entry(&kp2)), None);
    let (kpx, pkx) = gen_pk();
    write_plugin(&dir, "newcomer", Some(&pkx), Some(&sign_entry(&kpx)), None);

    let (_, _, toast) = run(&dir, &cfg);

    let t = toast.unwrap_or_default();
    assert!(
        t.contains("BLOCKED"),
        "a possible takeover is the highest-severity plugin event and must win \
         the single toast slot over 'needs approval', got: {t:?}"
    );
}

// ---- default (trust-on-first-use by entry checksum) mode ----

#[test]
fn default_mode_loads_a_plugin_whose_entry_hash_the_user_trusted() {
    let dir = temp_dir("tofu-accept");
    write_plugin(&dir, "p", None, None, None);
    let mut cfg = tofu_config();
    cfg.plugins.trusted = BTreeMap::from([("p".to_string(), entry_sha())]);

    let (pending, cmds, _) = run(&dir, &cfg);

    assert!(
        loaded(&cmds),
        "an unsigned plugin whose exact entry hash the user trusted must run in \
         default mode"
    );
    assert!(pending.is_empty());
}

#[test]
fn default_mode_holds_an_untrusted_plugin_for_approval() {
    let dir = temp_dir("tofu-untrusted");
    write_plugin(&dir, "p", None, None, None);

    let (pending, cmds, _) = run(&dir, &tofu_config());

    assert!(
        !loaded(&cmds),
        "an unreviewed plugin must not auto-execute on next launch"
    );
    assert_eq!(pending, vec!["p".to_string()]);
}

#[test]
fn default_mode_refuses_a_plugin_whose_entry_changed_after_approval() {
    let dir = temp_dir("tofu-changed");
    write_plugin(&dir, "p", None, None, None);
    let mut cfg = tofu_config();
    // The user approved a DIFFERENT script body than the one on disk.
    cfg.plugins.trusted = BTreeMap::from([("p".to_string(), "0".repeat(64))]);

    let (_, cmds, _) = run(&dir, &cfg);

    assert!(
        !loaded(&cmds),
        "approval is bound to the exact entry hash: editing the script after \
         approval must revoke it, or approval is a one-time key to arbitrary code"
    );
}

// ---- gating ----

#[test]
fn a_disabled_plugin_system_loads_nothing() {
    let dir = temp_dir("disabled-system");
    write_plugin(&dir, "p", None, None, None);
    let mut cfg = tofu_config();
    cfg.plugins.enabled = false;
    cfg.plugins.trusted = BTreeMap::from([("p".to_string(), entry_sha())]);

    let (pending, cmds, _) = run(&dir, &cfg);

    assert!(
        !loaded(&cmds),
        "plugins.enabled=false must win even over an explicitly trusted plugin"
    );
    assert!(
        pending.is_empty(),
        "a disabled system has nothing to approve"
    );
}

#[test]
fn an_individually_disabled_plugin_is_skipped() {
    let dir = temp_dir("disabled-one");
    write_plugin(&dir, "p", None, None, None);
    let mut cfg = tofu_config();
    cfg.plugins.trusted = BTreeMap::from([("p".to_string(), entry_sha())]);
    cfg.plugins.disabled = vec!["p".to_string()];

    let (pending, cmds, _) = run(&dir, &cfg);

    assert!(
        !loaded(&cmds),
        "an explicitly disabled plugin must not run even though it is trusted"
    );
    assert!(
        pending.is_empty(),
        "a disabled plugin is a settled decision, not a pending one"
    );
}

#[test]
fn a_plugin_requiring_a_newer_app_is_skipped() {
    let dir = temp_dir("minver");
    write_plugin(&dir, "p", None, None, Some("9999.0.0"));
    let mut cfg = tofu_config();
    cfg.plugins.trusted = BTreeMap::from([("p".to_string(), entry_sha())]);

    let (_, cmds, _) = run(&dir, &cfg);

    assert!(
        !loaded(&cmds),
        "a plugin declaring a min_app_version above this build must not load — \
         it would fail in surprising ways instead"
    );
}

#[test]
fn an_empty_plugins_dir_is_quiet() {
    let dir = temp_dir("empty");
    std::fs::create_dir_all(dir.join("plugins")).unwrap();

    let (pending, cmds, toast) = run(&dir, &tofu_config());

    assert!(!loaded(&cmds));
    assert!(pending.is_empty());
    assert!(toast.is_none(), "nothing to say when there are no plugins");
}
