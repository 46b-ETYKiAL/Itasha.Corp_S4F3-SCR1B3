//! Phase 20 T20.3 — git-backed plugin registry: schema, parse, search.
//!
//! The plugin registry is a **single public GitHub repo** holding one
//! flat `index.toml` listing every approved plugin. Tarballs live in
//! each author's own GitHub Releases — the registry repo carries
//! metadata only. Authors land entries by sending a PR to the
//! registry repo; CI runs the same verify path the client runs
//! (SHA-256 + minisign), so a passing merge is a cryptographic
//! attestation that the metadata matches the tarball.
//!
//! This module ships the **foundation**: the TOML schema, parser, and
//! search helpers. The HTTP fetch path (`fetch_index(url, cache_dir)`
//! with ETag caching, ureq-backed) lands in the follow-up that brings
//! the HTTP dep into scribe-core. The schema here is forward-stable —
//! the follow-up consumes it byte-for-byte.
//!
//! ## Hard invariants
//!
//! - No server beyond `raw.githubusercontent.com` + GitHub Releases.
//! - No accounts: GitHub identity = author identity.
//! - No payments, no ratings, no telemetry — refresh is the only
//!   outbound call, no per-user identifier.
//! - Offline mode works via cached index + local install-from-file.
//!
//! ## Schema example
//!
//! ```toml
//! schema_version = 1
//!
//! [[plugins]]
//! id              = "com.example.uppercase"
//! name            = "Uppercase"
//! description     = "Uppercases the buffer text."
//! author          = "Example Author"
//! license         = "MIT OR Apache-2.0"
//! repository      = "https://github.com/example/scribe-uppercase"
//! version_stable  = "0.2.0"
//! min_app_version = "0.9.0"
//! capabilities    = ["buffer"]
//! author_pubkey   = "untrusted comment: ...\nRWQ..."
//!
//!   [[plugins.releases]]
//!   version         = "0.2.0"
//!   released_utc    = "2026-05-28T12:00:00Z"
//!   tarball_url     = "https://github.com/example/scribe-uppercase/releases/download/v0.2.0/plugin.tar.gz"
//!   signature_url   = "https://github.com/example/scribe-uppercase/releases/download/v0.2.0/plugin.tar.gz.minisig"
//!   checksum_sha256 = "ba78...015ad"
//!   min_app_version = "0.9.0"
//!   api_version     = 1
//!   capabilities    = ["buffer"]
//! ```

use serde::{Deserialize, Serialize};

/// The whole `index.toml` file. `schema_version` lets a future incompatible
/// shape land without breaking older clients (they refuse to parse and
/// surface "update SCR1B3 to use this plugin registry" instead of failing
/// silently).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryIndex {
    pub schema_version: u32,
    #[serde(default)]
    pub plugins: Vec<PluginEntry>,
}

/// One row per published plugin. The id is the primary key; once added
/// it is never removed (deprecation is a flag we'll add in the
/// follow-up).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub repository: String,
    /// The author-declared "current stable" SemVer. UIs filter to this
    /// release by default; older releases stay visible for downgrade.
    #[serde(default)]
    pub version_stable: String,
    #[serde(default)]
    pub min_app_version: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// The author's minisign public key, in the same multi-line "box"
    /// form `minisign-verify` consumes. This is the TOFU anchor.
    #[serde(default)]
    pub author_pubkey: String,
    /// Every published release in publish order. The registry never
    /// removes entries; deprecation will be a per-release flag in a
    /// follow-up.
    #[serde(default)]
    pub releases: Vec<Release>,
}

/// One published version of a plugin. Carries the URLs the install
/// path uses + the checksum & sig contents the verifier consumes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    #[serde(default)]
    pub released_utc: String,
    pub tarball_url: String,
    pub signature_url: String,
    pub checksum_sha256: String,
    #[serde(default)]
    pub min_app_version: String,
    pub api_version: u32,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl RegistryIndex {
    /// Schema version this build understands. A registry that declares a
    /// higher version is refused with a "newer SCR1B3 required" message
    /// rather than silently misinterpreted.
    pub const CURRENT_SCHEMA_VERSION: u32 = 1;

    /// Parse a `index.toml` body. Returns `Err(...)` on TOML parse
    /// failure OR when `schema_version > CURRENT_SCHEMA_VERSION`.
    pub fn from_toml_str(s: &str) -> crate::Result<Self> {
        let idx: RegistryIndex =
            toml::from_str(s).map_err(|e| crate::CoreError::Plugin(e.to_string()))?;
        if idx.schema_version > Self::CURRENT_SCHEMA_VERSION {
            return Err(crate::CoreError::Plugin(format!(
                "registry schema_version {} is newer than this build supports (max {}); \
                 update SCR1B3 to read this registry",
                idx.schema_version,
                Self::CURRENT_SCHEMA_VERSION
            )));
        }
        Ok(idx)
    }

    /// Case-insensitive substring search over `id` + `name` +
    /// `description` + `author`. Empty query returns every entry.
    /// `search` does NOT mutate; the caller decides what to do with the
    /// hits.
    pub fn search(&self, query: &str) -> Vec<&PluginEntry> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.plugins.iter().collect();
        }
        self.plugins
            .iter()
            .filter(|p| {
                p.id.to_lowercase().contains(&q)
                    || p.name.to_lowercase().contains(&q)
                    || p.description.to_lowercase().contains(&q)
                    || p.author.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Find a single plugin by exact id (case-sensitive — ids are
    /// reverse-DNS, lowercase by convention).
    pub fn by_id(&self, id: &str) -> Option<&PluginEntry> {
        self.plugins.iter().find(|p| p.id == id)
    }
}

impl PluginEntry {
    /// The most recent release in this entry, by Vec order (the
    /// registry maintains publish order).
    pub fn latest_release(&self) -> Option<&Release> {
        self.releases.last()
    }

    /// The release matching the `version_stable` declaration, if it
    /// resolves; falls back to the most-recent release.
    pub fn stable_release(&self) -> Option<&Release> {
        if !self.version_stable.is_empty() {
            if let Some(r) = self
                .releases
                .iter()
                .find(|r| r.version == self.version_stable)
            {
                return Some(r);
            }
        }
        self.latest_release()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_INDEX: &str = r#"
schema_version = 1

[[plugins]]
id = "com.example.uppercase"
name = "Uppercase"
description = "Uppercases the buffer text."
author = "Example Author"
license = "MIT OR Apache-2.0"
repository = "https://github.com/example/scribe-uppercase"
version_stable = "0.2.0"
min_app_version = "0.9.0"
capabilities = ["buffer"]
author_pubkey = "K1"

  [[plugins.releases]]
  version = "0.1.0"
  released_utc = "2026-04-12T12:00:00Z"
  tarball_url = "https://example.invalid/0.1.0/plugin.tar.gz"
  signature_url = "https://example.invalid/0.1.0/plugin.tar.gz.minisig"
  checksum_sha256 = "aa"
  api_version = 1
  capabilities = ["buffer"]

  [[plugins.releases]]
  version = "0.2.0"
  released_utc = "2026-05-28T12:00:00Z"
  tarball_url = "https://example.invalid/0.2.0/plugin.tar.gz"
  signature_url = "https://example.invalid/0.2.0/plugin.tar.gz.minisig"
  checksum_sha256 = "bb"
  api_version = 1
  capabilities = ["buffer"]

[[plugins]]
id = "dev.hjkl.vim-keys"
name = "Vim Keys"
description = "Modal Vim-style keybindings."
author = "hjkl"
license = "MIT"
repository = "https://github.com/hjkl-dev/scribe-vim-keys"
version_stable = "1.4.2"
capabilities = ["buffer"]
author_pubkey = "K2"

  [[plugins.releases]]
  version = "1.4.2"
  released_utc = "2026-05-01T00:00:00Z"
  tarball_url = "https://example.invalid/1.4.2/plugin.tar.gz"
  signature_url = "https://example.invalid/1.4.2/plugin.tar.gz.minisig"
  checksum_sha256 = "cc"
  api_version = 1
  capabilities = ["buffer"]
"#;

    #[test]
    fn parses_sample_index() {
        let idx = RegistryIndex::from_toml_str(SAMPLE_INDEX).expect("parse");
        assert_eq!(idx.schema_version, 1);
        assert_eq!(idx.plugins.len(), 2);
        assert_eq!(idx.plugins[0].id, "com.example.uppercase");
        assert_eq!(idx.plugins[0].releases.len(), 2);
        assert_eq!(idx.plugins[0].releases[1].version, "0.2.0");
        assert_eq!(idx.plugins[1].id, "dev.hjkl.vim-keys");
    }

    #[test]
    fn refuses_future_schema_version() {
        let body = "schema_version = 9999\n";
        let r = RegistryIndex::from_toml_str(body);
        assert!(r.is_err(), "future schema must reject");
        let msg = r.unwrap_err().to_string();
        assert!(msg.contains("update SCR1B3"), "got {msg:?}");
    }

    #[test]
    fn search_returns_all_on_empty_query() {
        let idx = RegistryIndex::from_toml_str(SAMPLE_INDEX).expect("parse");
        let hits = idx.search("");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn search_case_insensitive_substring_match() {
        let idx = RegistryIndex::from_toml_str(SAMPLE_INDEX).expect("parse");
        // Matches the description on the first plugin.
        let hits = idx.search("BUFFER TEXT");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "com.example.uppercase");
        // Matches the author on the second plugin.
        let hits = idx.search("hjkl");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "dev.hjkl.vim-keys");
    }

    /// Mutation guard for `search` (`||` → `&&` at the id-OR-name junction): a
    /// query that matches the NAME but NOT the id (nor description/author) must
    /// still be found. Plugin[1] name is "Vim Keys"; its id is
    /// "dev.hjkl.vim-keys" (hyphen, so it does NOT contain "vim keys"), and the
    /// description/author don't contain it either. Under the original `||` the
    /// name-hit alone matches; under the `&&` mutant `(id && name)` requires both
    /// id AND name, so the name-only hit would vanish — this asserts it does not.
    #[test]
    fn search_matches_on_name_only_or_semantics() {
        let idx = RegistryIndex::from_toml_str(SAMPLE_INDEX).expect("parse");
        // Sanity: the query matches NAME but none of id / description / author.
        let p = &idx.plugins[1];
        let q = "vim keys";
        assert!(p.name.to_lowercase().contains(q), "name must match");
        assert!(!p.id.to_lowercase().contains(q), "id must NOT match");
        assert!(
            !p.description.to_lowercase().contains(q),
            "description must NOT match"
        );
        assert!(
            !p.author.to_lowercase().contains(q),
            "author must NOT match"
        );

        let hits = idx.search("Vim Keys");
        assert_eq!(hits.len(), 1, "a name-only OR hit must still be found");
        assert_eq!(hits[0].id, "dev.hjkl.vim-keys");
    }

    #[test]
    fn by_id_finds_entry() {
        let idx = RegistryIndex::from_toml_str(SAMPLE_INDEX).expect("parse");
        let p = idx.by_id("com.example.uppercase").expect("found");
        assert_eq!(p.name, "Uppercase");
        assert!(idx.by_id("does.not.exist").is_none());
    }

    #[test]
    fn stable_release_resolves_when_present() {
        let idx = RegistryIndex::from_toml_str(SAMPLE_INDEX).expect("parse");
        let p = &idx.plugins[0];
        let r = p.stable_release().expect("stable resolves");
        assert_eq!(r.version, "0.2.0");
    }

    /// Mutation guard for `stable_release` (`delete !` at line 176): when
    /// `version_stable` points at a release that is NOT the latest, the resolved
    /// release MUST be the version_stable one, not the latest. The existing
    /// "resolves_when_present" test happens to set version_stable == latest, so
    /// the `!`-deleted mutant (which falls straight through to `latest_release`)
    /// returns the SAME release and survives. Pointing version_stable at the
    /// EARLIER release (0.1.0, while latest is 0.2.0) makes the two paths
    /// diverge: original returns 0.1.0; the `if version_stable.is_empty()` mutant
    /// skips the find and returns 0.2.0.
    #[test]
    fn stable_release_resolves_non_latest_pinned_version() {
        let mut idx = RegistryIndex::from_toml_str(SAMPLE_INDEX).expect("parse");
        // Pin the EARLIER release; latest by publish order is still 0.2.0.
        idx.plugins[0].version_stable = "0.1.0".into();
        let r = idx.plugins[0]
            .stable_release()
            .expect("the pinned stable release must resolve");
        assert_eq!(
            r.version, "0.1.0",
            "stable_release must honour version_stable, not fall through to latest"
        );
        // And the latest is genuinely different, so this is a real discriminator.
        assert_eq!(idx.plugins[0].latest_release().unwrap().version, "0.2.0");
    }

    #[test]
    fn stable_release_falls_back_to_latest_when_missing() {
        let mut idx = RegistryIndex::from_toml_str(SAMPLE_INDEX).expect("parse");
        // Point version_stable at something that doesn't exist.
        idx.plugins[0].version_stable = "9.9.9".into();
        let r = idx.plugins[0].stable_release().expect("fallback");
        assert_eq!(r.version, "0.2.0"); // latest by publish order
    }

    #[test]
    fn parse_error_surfaces_message() {
        let r = RegistryIndex::from_toml_str("not valid toml\n[[oops");
        assert!(r.is_err());
    }
}
