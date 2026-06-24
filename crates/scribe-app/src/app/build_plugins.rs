//! Plugin discovery + trust-gating extracted from `ScribeApp::build` (A-01).
//!
//! Behavior-neutral split: the body below is moved verbatim from the original
//! `build` method (only dedented one level and given an explicit return tuple).
//! The trust gate (#R6 / S-01 / S-02 / R7) is unchanged — every decision path,
//! warning, and `toast` mutation is identical to the inline version.

use scribe_core::plugin::{self, CommandInfo, PluginHost};
use scribe_core::Config;

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
                    // decision is now EXPLICIT (no silent TOFU, no silent
                    // key-rotation). We first verify the minisign signature
                    // over the entry script, then route the pinned-key
                    // outcome through the pure `decide_key_trust` gate:
                    //   * Match               → Allow (anchor matches)
                    //   * New + prior consent  → Allow (user already approved
                    //                            this exact entry script)
                    //   * New + no consent     → held for approval (pending)
                    //   * Mismatch (key change)→ BLOCKED; surfaced old→new;
                    //                            NEVER loads without explicit
                    //                            `replace_with_consent`.
                    match (&p.manifest.author_pubkey, &p.manifest.signature) {
                        (Some(pk), Some(sig)) => {
                            let sig_ok = scribe_core::update::verify::verify_signature(
                                src.as_bytes(),
                                sig,
                                pk,
                            )
                            .is_ok();
                            if !sig_ok {
                                tracing::warn!(
                                    "plugin '{}' rejected: require_signed is on but the \
                                     signature does not verify",
                                    p.manifest.id
                                );
                                false
                            } else {
                                // The entry-hash trusted-approvals map is the
                                // user's explicit first-contact consent signal.
                                let first_consent = scribe_core::plugin::entry_is_trusted(
                                    &p.manifest.id,
                                    &sha,
                                    &config.plugins.trusted,
                                );
                                let outcome = match key_store.pin_or_match(&p.manifest.id, pk) {
                                    Ok(o) => o,
                                    Err(e) => {
                                        tracing::warn!(
                                            "plugin '{}' rejected: pinned-key store \
                                                 error: {e}",
                                            p.manifest.id
                                        );
                                        continue;
                                    }
                                };
                                match scribe_core::plugin::pinned_keys::decide_key_trust(
                                    outcome,
                                    first_consent,
                                ) {
                                    scribe_core::plugin::pinned_keys::PluginLoadDecision::Allow => true,
                                    scribe_core::plugin::pinned_keys::PluginLoadDecision::NeedsFirstConsent => {
                                        tracing::warn!(
                                            "plugin '{}' held: first-seen author key needs \
                                             your explicit approval before it runs",
                                            p.manifest.id
                                        );
                                        pending_plugins.push(p.manifest.id.clone());
                                        false
                                    }
                                    scribe_core::plugin::pinned_keys::PluginLoadDecision::BlockKeyChanged {
                                        old,
                                        new,
                                    } => {
                                        // S-02 — the pinned author key CHANGED
                                        // (possible takeover). NEVER load. Surface
                                        // a BLOCKING old→new warning; rotation
                                        // requires explicit `replace_with_consent`.
                                        tracing::warn!(
                                            "plugin '{}' BLOCKED: author key changed \
                                             (old={old} new={new}) — possible takeover; \
                                             approve the new key in Settings → Plugins \
                                             before it can run",
                                            p.manifest.id
                                        );
                                        key_changed_plugins.push(p.manifest.id.clone());
                                        false
                                    }
                                }
                            }
                        }
                        _ => {
                            tracing::warn!(
                                "plugin '{}' rejected: require_signed is on but it is \
                                 unsigned (no author key / signature)",
                                p.manifest.id
                            );
                            false
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
