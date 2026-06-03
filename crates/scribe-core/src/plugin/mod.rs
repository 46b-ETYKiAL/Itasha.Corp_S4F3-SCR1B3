//! User plugin / mod / extension system.
//!
//! Two-track design (per research): a **Rhai scripting "easy mode"** (this v1 —
//! pure-Rust, sandboxed, NO build step: drop a `.rhai` file in the plugins dir)
//! plus a documented WASM/`wasmtime` power track for compiled extensions
//! (ADR-0008). Rhai is chosen for v1 because it is pure-Rust (guaranteed to
//! build everywhere, no C toolchain), has the strongest default sandbox (no
//! ambient filesystem/network/process access), and needs no toolchain from the
//! mod author — the core requirement that "ordinary users can build mods".
//!
//! Security model: the Rhai sandbox grants NO ambient capabilities. Anything
//! privileged (filesystem, network, process spawn) would be a host-mediated
//! function gated by a manifest-declared `Capability` + user consent — none are
//! exposed in v1, so a script mod can only transform buffer text and surface
//! notifications/commands. This matches the research finding that
//! capability-consent — not signatures — is the real security gate.

mod host;
pub mod integrity;
pub mod pinned_keys;
pub mod registry;

pub use host::{CommandInfo, HookEvent, PluginContext, PluginHost};
pub use integrity::verify_plugin_tarball;
pub use pinned_keys::{PinOutcome, PinnedKeyStore};
pub use registry::{PluginEntry, RegistryIndex, Release};

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The plugin API version this build of SCR1B3 implements. A plugin manifest
/// declaring a higher `api_version` is refused (forward-incompatible).
pub const PLUGIN_API_VERSION: u32 = 1;

/// Capabilities a plugin may request. v1 exposes none of the privileged ones to
/// scripts; they exist so the manifest + consent flow is in place for the WASM
/// power track and future host-mediated APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Read/transform the active buffer text (granted to all script plugins).
    Buffer,
    /// Read files (host-mediated, user-consented). Not exposed to v1 scripts.
    FilesystemRead,
    /// Write files (host-mediated, user-consented). Not exposed to v1 scripts.
    FilesystemWrite,
    /// Network access (host-mediated, user-consented). Not exposed to v1 scripts.
    Network,
    /// Spawn a process (e.g. a language server). Not exposed to v1 scripts.
    Process,
}

impl Capability {
    /// Whether this capability is privileged (requires explicit user consent).
    pub fn is_privileged(self) -> bool {
        !matches!(self, Capability::Buffer)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    /// Rhai script — no build step (v1 easy mode).
    #[default]
    Script,
    /// Compiled WASM component (power track; loaded by the wasmtime host).
    Wasm,
}

/// `plugin.toml` manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub api_version: u32,
    #[serde(default)]
    pub kind: PluginKind,
    /// Entry file relative to the plugin directory (e.g. `main.rhai`).
    pub entry: String,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub description: String,

    // ---- Phase 20 T20.2: signed-plugin fields. All Option so existing
    // unsigned script plugins keep loading; the host enforces the gate
    // for compiled / registry-installed plugins as the WASM track lands.
    /// Minimum SCR1B3 app version required for this plugin (semver string).
    /// Refused when the running host is older than the declared minimum so
    /// a plugin authored against newer host-API extensions doesn't crash
    /// the host. `None` means 'no minimum' — the api_version gate alone
    /// is consulted.
    #[serde(default)]
    pub min_app_version: Option<String>,

    /// SHA-256 of the artifact this manifest covers (lowercase hex). The
    /// host computes the hash of the entry file (or the compiled WASM
    /// blob for the WASM track) on load and refuses on mismatch. `None`
    /// for the local-development unsigned case.
    #[serde(default)]
    pub checksum_sha256: Option<String>,

    /// Author's ed25519 public key (`untrusted comment` minisign form, or
    /// raw base64). Pinned at install time; the host refuses updates that
    /// arrive signed by a different key (prevents author-takeover).
    #[serde(default)]
    pub author_pubkey: Option<String>,

    /// Minisign detached signature over the plugin's **entry script** (the
    /// exact bytes that execute), verified against `author_pubkey`. With
    /// `plugins.require_signed` on, the host runs a plugin only when this
    /// signature verifies under a pinned author key — authenticating the code
    /// that runs, not just the manifest. `None` for the unsigned
    /// local-development path (which instead goes through the trust-on-first-use
    /// entry-checksum approval gate).
    #[serde(default)]
    pub signature: Option<String>,
}

impl PluginManifest {
    pub fn from_toml_str(s: &str) -> Result<Self, String> {
        toml::from_str(s).map_err(|e| e.to_string())
    }

    /// Whether this build can load the plugin (api_version not from the future).
    pub fn is_compatible(&self) -> bool {
        self.api_version <= PLUGIN_API_VERSION
    }

    /// Privileged capabilities this plugin requests (need user consent).
    pub fn privileged(&self) -> Vec<Capability> {
        self.capabilities
            .iter()
            .copied()
            .filter(|c| c.is_privileged())
            .collect()
    }

    /// True if the running app build satisfies the manifest-declared
    /// `min_app_version`. Returns true unconditionally when the manifest
    /// declares no minimum (the manifest field is `Option<String>` to
    /// keep older plugins parsing without panicking).
    ///
    /// `app` is parsed as a SemVer 2.0 string; a parse error on either
    /// side returns `false` so we err on the side of rejecting a plugin
    /// rather than silently lying about compatibility.
    pub fn is_app_version_ok(&self, app: &str) -> bool {
        let Some(req) = self.min_app_version.as_deref() else {
            return true;
        };
        match (
            semver::Version::parse(req.trim()),
            semver::Version::parse(app.trim()),
        ) {
            (Ok(min), Ok(running)) => running >= min,
            _ => false,
        }
    }
}

/// A plugin discovered on disk (manifest + its directory).
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub dir: PathBuf,
}

impl DiscoveredPlugin {
    pub fn entry_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.entry)
    }
}

/// Discover plugins under `plugins_dir`: each subdirectory containing a
/// `plugin.toml` is a candidate. Malformed manifests are skipped (reported via
/// the returned error list) — one bad plugin never blocks the others.
/// Trust-on-first-use decision: a discovered plugin's entry script may run in
/// the default (unsigned) path only when the user has approved THIS EXACT
/// script — i.e. `trusted[id]` equals the script's current SHA-256. A brand-new
/// plugin (absent id) or a silently-modified one (changed hash) returns `false`
/// so it is held back rather than auto-executed.
pub fn entry_is_trusted(
    id: &str,
    entry_sha256: &str,
    trusted: &std::collections::BTreeMap<String, String>,
) -> bool {
    trusted.get(id).map(|s| s == entry_sha256).unwrap_or(false)
}

pub fn discover(plugins_dir: &Path) -> (Vec<DiscoveredPlugin>, Vec<String>) {
    let mut found = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(plugins_dir) else {
        return (found, errors); // no plugins dir = no plugins (not an error)
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let manifest_path = dir.join("plugin.toml");
        let Ok(text) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        match PluginManifest::from_toml_str(&text) {
            Ok(m) if m.is_compatible() => found.push(DiscoveredPlugin { manifest: m, dir }),
            Ok(m) => errors.push(format!(
                "plugin '{}' requires api_version {} > supported {}",
                m.id, m.api_version, PLUGIN_API_VERSION
            )),
            Err(e) => errors.push(format!("{}: {e}", manifest_path.display())),
        }
    }
    (found, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parse_and_compat() {
        let m = PluginManifest::from_toml_str(
            r#"
id = "uppercase"
name = "Uppercase"
version = "0.1.0"
api_version = 1
kind = "script"
entry = "main.rhai"
capabilities = ["buffer"]
description = "Uppercases the buffer"
"#,
        )
        .unwrap();
        assert_eq!(m.id, "uppercase");
        assert!(m.is_compatible());
        assert!(m.privileged().is_empty());
    }

    #[test]
    fn future_api_version_incompatible() {
        let m = PluginManifest::from_toml_str("id='x'\nname='x'\napi_version=99\nentry='m.rhai'\n")
            .unwrap();
        assert!(!m.is_compatible());
    }

    #[test]
    fn privileged_capabilities_flagged() {
        let m = PluginManifest::from_toml_str(
            "id='x'\nname='x'\napi_version=1\nentry='m.rhai'\ncapabilities=['buffer','network']\n",
        )
        .unwrap();
        assert_eq!(m.privileged(), vec![Capability::Network]);
    }

    #[test]
    fn entry_is_trusted_only_on_exact_hash_match() {
        use std::collections::BTreeMap;
        let mut trusted = BTreeMap::new();
        trusted.insert("uppercase".to_string(), "abc123".to_string());

        // Approved id + matching current hash -> may run.
        assert!(entry_is_trusted("uppercase", "abc123", &trusted));
        // Approved id but the script CHANGED (different hash) -> held back.
        assert!(!entry_is_trusted("uppercase", "deadbeef", &trusted));
        // Brand-new, never-approved plugin -> held back (no auto-run).
        assert!(!entry_is_trusted("totally-new", "abc123", &trusted));
        // Empty trust set -> nothing runs.
        assert!(!entry_is_trusted("uppercase", "abc123", &BTreeMap::new()));
    }

    #[test]
    fn discover_skips_missing_dir() {
        let (found, errors) = discover(Path::new("/nonexistent/scr1b3/plugins"));
        assert!(found.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn discover_finds_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let pdir = tmp.path().join("uppercase");
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(
            pdir.join("plugin.toml"),
            "id='uppercase'\nname='Uppercase'\napi_version=1\nentry='main.rhai'\n",
        )
        .unwrap();
        std::fs::write(pdir.join("main.rhai"), "// noop").unwrap();
        let (found, errors) = discover(tmp.path());
        assert_eq!(found.len(), 1);
        assert!(errors.is_empty());
        assert_eq!(found[0].manifest.id, "uppercase");
    }

    // ---- Phase 20 T20.2 is_app_version_ok regression tests ----

    fn manifest_with_min(min: Option<&str>) -> PluginManifest {
        PluginManifest {
            id: "p".into(),
            name: "p".into(),
            version: String::new(),
            api_version: 1,
            kind: PluginKind::default(),
            entry: "main.rhai".into(),
            capabilities: Vec::new(),
            description: String::new(),
            min_app_version: min.map(str::to_owned),
            checksum_sha256: None,
            author_pubkey: None,
            signature: None,
        }
    }

    #[test]
    fn is_app_version_ok_true_when_no_min_declared() {
        let m = manifest_with_min(None);
        assert!(m.is_app_version_ok("0.1.0"));
        assert!(m.is_app_version_ok("99.99.99"));
    }

    #[test]
    fn is_app_version_ok_true_when_app_equal_to_min() {
        let m = manifest_with_min(Some("1.2.3"));
        assert!(m.is_app_version_ok("1.2.3"));
    }

    #[test]
    fn is_app_version_ok_true_when_app_greater_than_min() {
        let m = manifest_with_min(Some("1.2.3"));
        assert!(m.is_app_version_ok("1.2.4"));
        assert!(m.is_app_version_ok("2.0.0"));
    }

    #[test]
    fn is_app_version_ok_false_when_app_less_than_min() {
        let m = manifest_with_min(Some("1.2.3"));
        assert!(!m.is_app_version_ok("1.2.2"));
        assert!(!m.is_app_version_ok("0.9.0"));
    }

    #[test]
    fn is_app_version_ok_false_on_parse_error() {
        let m = manifest_with_min(Some("1.2.3"));
        assert!(!m.is_app_version_ok("not-a-version"));
        let bad = manifest_with_min(Some("also-bad"));
        assert!(!bad.is_app_version_ok("1.0.0"));
    }
}
