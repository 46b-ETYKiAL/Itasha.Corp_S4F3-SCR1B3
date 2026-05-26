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

pub use host::{CommandInfo, HookEvent, PluginContext, PluginHost};

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
}
