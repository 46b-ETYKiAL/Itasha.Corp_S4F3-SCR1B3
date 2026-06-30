//! Default-app / file-association model (schema v4).
//!
//! The single source of truth for the file types SCR1B3 can register itself as
//! the default handler for, mapped to each OS's identifier scheme:
//!
//! - **Windows** — a file EXTENSION (no dot) plus the per-extension ProgID we
//!   register under `HKCU\Software\Classes`.
//! - **macOS** — a Uniform Type Identifier (UTI). System UTIs resolve through
//!   the conformance tree (`.rs`/`.c`/`.py` → `public.source-code` →
//!   `public.plain-text`); only Markdown/JSON need a distinct UTI claimed by name.
//! - **Linux** — a freedesktop MIME type, set as default via `xdg-mime` /
//!   `~/.config/mimeapps.list`.
//!
//! Shared by the Settings UI, the per-OS registration backends in
//! `scribe-app::integration`, and (eventually) the installer manifests, so the
//! claimed set can never drift between them. Pure data + mapping — fully
//! unit-tested below.

use serde::{Deserialize, Serialize};

/// A logical group of file types the user can ask SCR1B3 to become the default
/// app for. Coarser than raw extensions so the Settings UI stays a short, legible
/// checklist while each group still expands to the right per-OS identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClaimType {
    /// `.txt` and the plain-text family.
    PlainText,
    /// Markdown notes (`.md`, `.markdown`).
    Markdown,
    /// JSON documents (`.json`).
    Json,
    /// Source code across common languages.
    SourceCode,
}

impl ClaimType {
    /// Every claimable group, in display order.
    pub const ALL: [ClaimType; 4] = [
        ClaimType::PlainText,
        ClaimType::Markdown,
        ClaimType::Json,
        ClaimType::SourceCode,
    ];

    /// Stable serialization key (persisted in [`IntegrationConfig::claimed_types`]
    /// and used as a config/CLI token — NEVER change an existing value).
    pub fn key(self) -> &'static str {
        match self {
            ClaimType::PlainText => "plain_text",
            ClaimType::Markdown => "markdown",
            ClaimType::Json => "json",
            ClaimType::SourceCode => "source_code",
        }
    }

    /// Human label for the Settings checklist.
    pub fn label(self) -> &'static str {
        match self {
            ClaimType::PlainText => "Plain text (.txt)",
            ClaimType::Markdown => "Markdown (.md)",
            ClaimType::Json => "JSON (.json)",
            ClaimType::SourceCode => "Source code (.rs, .py, .js, …)",
        }
    }

    /// Resolve a persisted [`key`](Self::key) back to its group.
    pub fn from_key(key: &str) -> Option<ClaimType> {
        ClaimType::ALL.into_iter().find(|c| c.key() == key)
    }

    /// The Windows ProgID SCR1B3 registers for this group under
    /// `HKCU\Software\Classes\<ProgID>`. One ProgID per group; every extension in
    /// [`windows_extensions`](Self::windows_extensions) points its
    /// `OpenWithProgids` at it.
    pub fn windows_progid(self) -> &'static str {
        match self {
            ClaimType::PlainText => "SCR1B3.txt",
            ClaimType::Markdown => "SCR1B3.md",
            ClaimType::Json => "SCR1B3.json",
            ClaimType::SourceCode => "SCR1B3.source",
        }
    }

    /// File extensions (NO leading dot) this group claims on Windows.
    pub fn windows_extensions(self) -> &'static [&'static str] {
        match self {
            ClaimType::PlainText => &["txt", "text", "log"],
            ClaimType::Markdown => &["md", "markdown", "mdown", "mkd"],
            ClaimType::Json => &["json", "jsonc"],
            ClaimType::SourceCode => &[
                "rs", "c", "h", "cpp", "cc", "cxx", "hpp", "py", "js", "mjs", "cjs", "ts", "tsx",
                "jsx", "go", "java", "rb", "php", "sh", "bash", "zsh", "toml", "yaml", "yml",
                "xml", "css", "scss", "html", "htm", "lua", "sql", "kt", "swift", "dart", "zig",
                "ini", "cfg", "conf",
            ],
        }
    }

    /// macOS Uniform Type Identifiers this group claims.
    pub fn macos_utis(self) -> &'static [&'static str] {
        match self {
            ClaimType::PlainText => &["public.plain-text"],
            ClaimType::Markdown => &["net.daringfireball.markdown"],
            ClaimType::Json => &["public.json"],
            ClaimType::SourceCode => &["public.source-code"],
        }
    }

    /// freedesktop MIME types this group claims on Linux.
    pub fn linux_mimes(self) -> &'static [&'static str] {
        match self {
            ClaimType::PlainText => &["text/plain"],
            ClaimType::Markdown => &["text/markdown"],
            ClaimType::Json => &["application/json"],
            ClaimType::SourceCode => &[
                "text/x-csrc",
                "text/x-c++src",
                "text/x-chdr",
                "text/x-rust",
                "text/x-python",
                "application/javascript",
                "text/x-go",
                "text/x-java-source",
                "application/x-shellscript",
                "application/toml",
                "text/x-yaml",
                "application/xml",
                "text/css",
                "text/html",
                "text/x-lua",
                "application/sql",
            ],
        }
    }
}

/// OS-integration preferences (schema v4). DEFAULTS OFF — SCR1B3 never registers
/// itself as a file handler without an explicit user action in Settings (mirrors
/// the opt-in `reporting` contract: no surprise OS-surface changes). A config
/// written before v4 reads this whole section as the all-off default via
/// `#[serde(default)]`.
///
/// The derived `Default` IS the opt-in-off state (`register_file_types = false`,
/// no claimed types, never registered) — the privacy default the contract
/// requires; the field defaults are asserted by `integration_config_defaults_off`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct IntegrationConfig {
    /// The user asked SCR1B3 to register as a default file-type handler. Until
    /// this is set true (via Settings), no registration is ever performed.
    pub register_file_types: bool,
    /// Persisted [`ClaimType::key`] tokens the user opted to claim. An empty list
    /// while `register_file_types` is on means "the default set" — see
    /// [`claimed_types`](Self::claimed_types).
    pub claimed_types: Vec<String>,
    /// Unix seconds of the last successful registration (for the Settings status
    /// line). `None` until the first successful register.
    pub last_registration_unix: Option<u64>,
}

impl IntegrationConfig {
    /// The resolved set of claim groups: the persisted keys parsed back to
    /// [`ClaimType`]s, or — when none are stored — the full default set. Unknown
    /// / stale keys are ignored (forward-compatible). Order follows
    /// [`ClaimType::ALL`] and is de-duplicated.
    pub fn claimed_types(&self) -> Vec<ClaimType> {
        if self.claimed_types.is_empty() {
            return ClaimType::ALL.to_vec();
        }
        ClaimType::ALL
            .into_iter()
            .filter(|c| self.claimed_types.iter().any(|k| k == c.key()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_roundtrips_for_every_group() {
        for c in ClaimType::ALL {
            assert_eq!(ClaimType::from_key(c.key()), Some(c), "key {:?}", c.key());
        }
        assert_eq!(ClaimType::from_key("nope"), None);
    }

    #[test]
    fn keys_and_progids_are_unique() {
        let keys: Vec<_> = ClaimType::ALL.iter().map(|c| c.key()).collect();
        let progids: Vec<_> = ClaimType::ALL.iter().map(|c| c.windows_progid()).collect();
        for i in 0..ClaimType::ALL.len() {
            for j in (i + 1)..ClaimType::ALL.len() {
                assert_ne!(keys[i], keys[j], "duplicate key");
                assert_ne!(progids[i], progids[j], "duplicate progid");
            }
        }
    }

    #[test]
    fn every_group_maps_to_non_empty_identifiers_on_every_os() {
        for c in ClaimType::ALL {
            assert!(!c.windows_extensions().is_empty(), "win ext {:?}", c);
            assert!(!c.macos_utis().is_empty(), "mac uti {:?}", c);
            assert!(!c.linux_mimes().is_empty(), "linux mime {:?}", c);
            assert!(c.windows_progid().starts_with("SCR1B3."), "progid {:?}", c);
            // Extensions carry no dot (the registry key is built as `.<ext>`).
            for e in c.windows_extensions() {
                assert!(!e.starts_with('.') && !e.is_empty(), "ext {e:?}");
            }
        }
    }

    #[test]
    fn integration_config_defaults_off() {
        let c = IntegrationConfig::default();
        assert!(!c.register_file_types);
        assert!(c.claimed_types.is_empty());
        assert!(c.last_registration_unix.is_none());
    }

    #[test]
    fn empty_claim_list_resolves_to_the_full_default_set() {
        let c = IntegrationConfig::default();
        assert_eq!(c.claimed_types(), ClaimType::ALL.to_vec());
    }

    #[test]
    fn explicit_claim_list_resolves_in_canonical_order_ignoring_unknown() {
        let c = IntegrationConfig {
            register_file_types: true,
            // out of order + an unknown key
            claimed_types: vec!["json".into(), "bogus".into(), "plain_text".into()],
            last_registration_unix: None,
        };
        assert_eq!(
            c.claimed_types(),
            vec![ClaimType::PlainText, ClaimType::Json],
            "resolves in ClaimType::ALL order, unknown keys dropped"
        );
    }

    #[test]
    fn integration_config_toml_roundtrip() {
        let c = IntegrationConfig {
            register_file_types: true,
            claimed_types: vec!["plain_text".into(), "markdown".into()],
            last_registration_unix: Some(1_700_000_000),
        };
        let s = toml::to_string(&c).unwrap();
        let back: IntegrationConfig = toml::from_str(&s).unwrap();
        assert_eq!(c, back);
    }
}
