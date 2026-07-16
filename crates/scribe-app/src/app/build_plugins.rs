//! Plugin discovery + trust-gating extracted from `ScribeApp::build` (A-01).
//!
//! Behavior-neutral split: the body below is moved verbatim from the original
//! `build` method (only dedented one level and given an explicit return tuple).
//! The trust gate (#R6 / S-01 / S-02 / R7) is unchanged — every decision path,
//! warning, and `toast` mutation is identical to the inline version.

use scribe_core::plugin::{self, CommandInfo, PluginHost};
use scribe_core::Config;

/// The single strict-mode (`require_signed`) admission decision, shared by the
/// startup load path ([`load_plugins`]) and the user "Approve & run" path
/// ([`crate::app::ScribeApp::approve_plugin`]) so the signature + pinned-key
/// policy lives in ONE place instead of two hand-maintained copies (SEC-3
/// defense-in-depth). A `Mismatch` (changed author key) can NEVER become
/// `Allow`, even with `first_consent` — that invariant lives in the pure
/// `decide_key_trust` gate this routes through.
#[derive(Debug)]
pub(super) enum SignedAdmission {
    /// Signature verifies and the author key is trusted (match, or first
    /// contact with prior consent) — the plugin may run.
    Allow,
    /// First-seen author key with no prior consent — hold for approval.
    NeedsFirstConsent,
    /// No author key / signature in signed-only mode.
    Unsigned,
    /// The minisign signature did not verify the entry script.
    BadSignature,
    /// The pinned author key CHANGED (possible takeover) — never load.
    BlockKeyChanged { old: String, new: String },
    /// The pinned-key store could not be read.
    StoreError,
}

/// Decide whether a discovered plugin may run under `require_signed` mode.
/// `first_consent` is the caller's explicit-consent signal: `entry_is_trusted(...)`
/// for the load path, `true` for the user-clicked approve path.
pub(super) fn admit_signed_plugin(
    key_store: &mut scribe_core::plugin::PinnedKeyStore,
    plugin_id: &str,
    author_pubkey: Option<&str>,
    signature: Option<&str>,
    entry_src: &[u8],
    first_consent: bool,
) -> SignedAdmission {
    let (Some(pk), Some(sig)) = (author_pubkey, signature) else {
        return SignedAdmission::Unsigned;
    };
    if scribe_core::update::verify::verify_signature(entry_src, sig, pk).is_err() {
        return SignedAdmission::BadSignature;
    }
    let outcome = match key_store.pin_or_match(plugin_id, pk) {
        Ok(o) => o,
        Err(_) => return SignedAdmission::StoreError,
    };
    use scribe_core::plugin::pinned_keys::PluginLoadDecision;
    match scribe_core::plugin::pinned_keys::decide_key_trust(outcome, first_consent) {
        PluginLoadDecision::Allow => SignedAdmission::Allow,
        PluginLoadDecision::NeedsFirstConsent => SignedAdmission::NeedsFirstConsent,
        PluginLoadDecision::BlockKeyChanged { old, new } => {
            SignedAdmission::BlockKeyChanged { old, new }
        }
    }
}

/// Discover, trust-gate, and load user plugins for `ScribeApp::build`.
///
/// Returns the populated `PluginHost`, the list of plugin ids held back pending
/// user approval, and the command list exported by the loaded plugins. `toast`
/// is updated in place exactly as the original inline block did (skipped /
/// pending / BLOCKED-key-changed messages, with the same priority ordering).
pub(super) fn load_plugins(
    config: &Config,
    toast: &mut Option<String>,
) -> (PluginHost, Vec<String>, Vec<CommandInfo>) {
    // Load user mods/plugins (no-build-step Rhai scripts) from the plugins
    // dir, unless the user disabled the plugin system.
    //
    // #R6 — TRUST GATE. A plugin script is only ever executed when EITHER:
    //   * `require_signed` is on AND it carries a minisign signature over
    //     the entry script from a pinned author key that verifies, OR
    //   * the user has approved THIS EXACT entry script (its SHA-256 is in
    //     `config.plugins.trusted`).
    // Otherwise the plugin is held back as "pending approval" and NOT run.
    // This closes the prior gap where dropping any folder into the plugins
    // dir auto-executed unsigned, unreviewed code on the next launch.
    let mut plugins = PluginHost::new();
    let mut pending_plugins: Vec<String> = Vec::new();
    // S-02 — plugins whose pinned author key CHANGED (possible takeover).
    // These are BLOCKED from loading and surfaced distinctly so the user
    // can decide whether to approve the new key (key rotation), never a
    // silent log line.
    let mut key_changed_plugins: Vec<String> = Vec::new();
    if config.plugins.enabled {
        if let Some(dir) = Config::config_dir() {
            let (found, errors) = plugin::discover(&dir.join("plugins"));
            let mut key_store = scribe_core::plugin::PinnedKeyStore::new(&dir);
            for p in found {
                if config.plugins.disabled.contains(&p.manifest.id) {
                    continue;
                }
                // Honor the manifest's declared `min_app_version` floor. The
                // helper + its tests existed but were never wired into the
                // load loop, so a plugin requiring a newer host would load
                // and then fail in surprising ways. Fail-safe: refuse to load
                // an incompatible plugin and warn.
                if !p.manifest.is_app_version_ok(env!("CARGO_PKG_VERSION")) {
                    tracing::warn!(
                        "plugin '{}' skipped: requires app >= {:?}, this build is {}",
                        p.manifest.id,
                        p.manifest.min_app_version,
                        env!("CARGO_PKG_VERSION")
                    );
                    continue;
                }
                let Ok(src) = std::fs::read_to_string(p.entry_path()) else {
                    continue;
                };
                let sha = scribe_core::update::verify::sha256_hex(src.as_bytes());
                let may_run = if config.plugins.require_signed {
                    // R7 / S-01 + S-02 — strict mode: the author-key trust
                    // decision is EXPLICIT (no silent TOFU, no silent
                    // key-rotation) and routes through the single shared
                    // `admit_signed_plugin` gate so the load + approve paths
                    // can never drift. `first_consent` is the user's explicit
                    // first-contact signal (the entry-hash trusted-approvals
                    // map). A `Mismatch` (key change) can never become `Allow`.
                    let first_consent = scribe_core::plugin::entry_is_trusted(
                        &p.manifest.id,
                        &sha,
                        &config.plugins.trusted,
                    );
                    match admit_signed_plugin(
                        &mut key_store,
                        &p.manifest.id,
                        p.manifest.author_pubkey.as_deref(),
                        p.manifest.signature.as_deref(),
                        src.as_bytes(),
                        first_consent,
                    ) {
                        SignedAdmission::Allow => true,
                        SignedAdmission::NeedsFirstConsent => {
                            tracing::warn!(
                                "plugin '{}' held: first-seen author key needs your explicit approval before it runs",
                                p.manifest.id
                            );
                            pending_plugins.push(p.manifest.id.clone());
                            false
                        }
                        SignedAdmission::Unsigned => {
                            tracing::warn!(
                                "plugin '{}' rejected: require_signed is on but it is unsigned (no author key / signature)",
                                p.manifest.id
                            );
                            false
                        }
                        SignedAdmission::BadSignature => {
                            tracing::warn!(
                                "plugin '{}' rejected: require_signed is on but the signature does not verify",
                                p.manifest.id
                            );
                            false
                        }
                        SignedAdmission::BlockKeyChanged { old, new } => {
                            // S-02 — the pinned author key CHANGED (possible
                            // takeover). NEVER load. Surface a BLOCKING old→new
                            // warning; rotation requires explicit
                            // `replace_with_consent`.
                            tracing::warn!(
                                "plugin '{}' BLOCKED: author key changed (old={old} new={new}) — possible takeover; approve the new key in Settings → Plugins before it can run",
                                p.manifest.id
                            );
                            key_changed_plugins.push(p.manifest.id.clone());
                            false
                        }
                        SignedAdmission::StoreError => {
                            tracing::warn!(
                                "plugin '{}' rejected: pinned-key store error",
                                p.manifest.id
                            );
                            continue;
                        }
                    }
                } else {
                    // Default mode: trust-on-first-use by entry checksum.
                    scribe_core::plugin::entry_is_trusted(
                        &p.manifest.id,
                        &sha,
                        &config.plugins.trusted,
                    )
                };
                if !may_run {
                    if !config.plugins.require_signed {
                        pending_plugins.push(p.manifest.id.clone());
                    }
                    continue;
                }
                if let Err(e) = plugins.load_script(&p.manifest.id, &src) {
                    tracing::warn!("plugin load failed: {e}");
                }
            }
            if !errors.is_empty() && toast.is_none() {
                *toast = Some(format!("{} plugin(s) skipped (see log)", errors.len()));
            }
            if !pending_plugins.is_empty() && toast.is_none() {
                *toast = Some(format!(
                    "{} plugin(s) need your approval before they run — open Settings \
                     → Plugins → Manage plugins",
                    pending_plugins.len()
                ));
            }
            // S-02 — a CHANGED author key is the highest-severity plugin
            // event (possible takeover): surface it with priority over the
            // softer "skipped" / "pending" toasts so the user sees it.
            if !key_changed_plugins.is_empty() {
                *toast = Some(format!(
                    "⚠ {} plugin(s) BLOCKED — author key changed since last run \
                     (possible takeover). Open Settings → Plugins to review the \
                     new key before allowing it.",
                    key_changed_plugins.len()
                ));
            }
        }
    }
    let plugin_cmds = plugins.commands();
    (plugins, pending_plugins, plugin_cmds)
}
