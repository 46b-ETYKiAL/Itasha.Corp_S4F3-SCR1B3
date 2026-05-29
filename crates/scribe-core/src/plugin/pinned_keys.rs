//! Phase 20 T20.2 — TOFU pinned author keys for plugins.
//!
//! On the **first install** of a plugin, the manifest-declared
//! `author_pubkey` is recorded under the plugin id. On any subsequent
//! install (update / reinstall / rerun), the new manifest's
//! `author_pubkey` must MATCH the pinned key. A different key triggers
//! a [`PinOutcome::Mismatch`] result; the install UI surfaces an
//! "Author key changed — accept new key?" modal. Silent rotation is
//! refused.
//!
//! Same discipline as SSH `known_hosts` and OpenBSD `signify`: the
//! first contact is the trust anchor; subsequent updates are verified
//! against it; rotation requires explicit user consent.
//!
//! ## Storage format
//!
//! A single TOML file at `<config_dir>/plugins/pinned-keys.toml`:
//!
//! ```toml
//! [plugins."com.example.uppercase"]
//! author_pubkey = "untrusted comment: ...\nRWQ..."
//! first_pinned_utc  = "2026-05-29T15:00:00Z"
//! last_verified_utc = "2026-05-29T15:00:00Z"
//! ```
//!
//! Plugin ids are reverse-DNS dotted strings, used as TOML table keys.
//! The TOML quote-string is required because the keys contain dots.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// What the store reported when a manifest's `(id, pubkey)` pair was
/// presented for matching.
#[derive(Debug, PartialEq, Eq)]
pub enum PinOutcome {
    /// No record for this plugin id existed. The caller MUST proceed to
    /// pin the key after the user consents to install.
    New,
    /// A record existed and the keys are byte-equal. Safe to install.
    Match,
    /// A record existed but the keys differ. The install UI must surface
    /// an explicit "key changed" modal; the user has to consent before
    /// the new key replaces the old.
    Mismatch { old: String, new: String },
}

/// On-disk pinned-keys store. Cheap to construct (just records a path);
/// the file is read on each call to amortise restart cost over the
/// "user opens settings, looks at 1 plugin" pattern.
#[derive(Debug)]
pub struct PinnedKeyStore {
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PinnedEntry {
    author_pubkey: String,
    #[serde(default)]
    first_pinned_utc: String,
    #[serde(default)]
    last_verified_utc: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    plugins: BTreeMap<String, PinnedEntry>,
}

impl PinnedKeyStore {
    /// New store rooted at `<config_dir>/plugins/pinned-keys.toml`. The
    /// file is NOT created until the first mutation lands.
    pub fn new(config_dir: &Path) -> Self {
        Self {
            path: config_dir.join("plugins").join("pinned-keys.toml"),
        }
    }

    /// For tests + tools: point at an explicit path.
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> std::io::Result<StoreFile> {
        match fs::read_to_string(&self.path) {
            Ok(s) => toml::from_str::<StoreFile>(&s)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoreFile::default()),
            Err(e) => Err(e),
        }
    }

    fn save(&self, store: &StoreFile) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(store)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        fs::write(&self.path, body)
    }

    /// Match-or-pin: if `plugin_id` has no record, the pubkey is pinned
    /// and `New` is returned. If it does, the stored key is compared
    /// byte-equally; `Match` or `Mismatch { old, new }` is returned.
    ///
    /// The "last_verified_utc" field is updated on every `Match` so a
    /// future audit-log feature can see when the key was last seen.
    pub fn pin_or_match(&mut self, plugin_id: &str, pubkey: &str) -> std::io::Result<PinOutcome> {
        let mut store = self.load()?;
        let now = current_utc_iso();
        match store.plugins.get_mut(plugin_id) {
            Some(entry) => {
                if entry.author_pubkey == pubkey {
                    entry.last_verified_utc = now;
                    self.save(&store)?;
                    Ok(PinOutcome::Match)
                } else {
                    Ok(PinOutcome::Mismatch {
                        old: entry.author_pubkey.clone(),
                        new: pubkey.to_string(),
                    })
                }
            }
            None => {
                store.plugins.insert(
                    plugin_id.to_string(),
                    PinnedEntry {
                        author_pubkey: pubkey.to_string(),
                        first_pinned_utc: now.clone(),
                        last_verified_utc: now,
                    },
                );
                self.save(&store)?;
                Ok(PinOutcome::New)
            }
        }
    }

    /// Replace the pinned key after explicit user consent (a Mismatch
    /// resolution). Updates `last_verified_utc` but PRESERVES
    /// `first_pinned_utc` for audit-trail purposes.
    pub fn replace_with_consent(
        &mut self,
        plugin_id: &str,
        new_pubkey: &str,
    ) -> std::io::Result<()> {
        let mut store = self.load()?;
        let now = current_utc_iso();
        let first_pinned = store
            .plugins
            .get(plugin_id)
            .map(|e| e.first_pinned_utc.clone())
            .unwrap_or_else(|| now.clone());
        store.plugins.insert(
            plugin_id.to_string(),
            PinnedEntry {
                author_pubkey: new_pubkey.to_string(),
                first_pinned_utc: first_pinned,
                last_verified_utc: now,
            },
        );
        self.save(&store)
    }
}

/// Stdlib-only ISO-8601 "now". `chrono` would be cleaner but adding a
/// crate for this single use is dep-bloat. The format is `YYYY-MM-
/// DDTHH:MM:SSZ` and is recorded as a string; we never parse it back
/// for arithmetic — the audit-log consumer that needs that will do
/// its own parse.
fn current_utc_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Civil-time conversion using the Gauss formula via mostly
    // integer math. Acceptable accuracy for an audit-only timestamp.
    let days = secs / 86_400;
    let remainder = secs % 86_400;
    let hour = remainder / 3600;
    let minute = (remainder % 3600) / 60;
    let second = remainder % 60;
    // 1970-01-01 was day 0. Calendar walk via Howard Hinnant's
    // days_from_civil inverse.
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Inverse of Howard Hinnant's days_from_civil — exact for any Gregorian
/// date and used by chrono / time / java.time. Returns `(year, month,
/// day)` for the Unix-day offset.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fresh_store() -> (tempfile::TempDir, PinnedKeyStore) {
        let dir = tempdir().expect("tempdir");
        let store = PinnedKeyStore::new(dir.path());
        (dir, store)
    }

    #[test]
    fn first_pin_returns_new_and_persists() {
        let (_dir, mut s) = fresh_store();
        let outcome = s
            .pin_or_match("com.example.uppercase", "untrusted comment: a\nRWQabc")
            .expect("pin");
        assert_eq!(outcome, PinOutcome::New);
        // Round-trip — the same id + key now matches.
        let again = s
            .pin_or_match("com.example.uppercase", "untrusted comment: a\nRWQabc")
            .expect("re-pin");
        assert_eq!(again, PinOutcome::Match);
    }

    #[test]
    fn second_pin_with_same_key_returns_match() {
        let (_dir, mut s) = fresh_store();
        s.pin_or_match("p.id", "K1").unwrap();
        assert_eq!(s.pin_or_match("p.id", "K1").unwrap(), PinOutcome::Match);
    }

    #[test]
    fn second_pin_with_different_key_returns_mismatch() {
        let (_dir, mut s) = fresh_store();
        s.pin_or_match("p.id", "K1").unwrap();
        let outcome = s.pin_or_match("p.id", "K2").unwrap();
        match outcome {
            PinOutcome::Mismatch { old, new } => {
                assert_eq!(old, "K1");
                assert_eq!(new, "K2");
            }
            other => panic!("expected mismatch, got {other:?}"),
        }
    }

    #[test]
    fn replace_with_consent_rotates_key_and_preserves_first_pinned() {
        let (_dir, mut s) = fresh_store();
        s.pin_or_match("p.id", "K_OLD").unwrap();
        s.replace_with_consent("p.id", "K_NEW").unwrap();
        // The new key now matches; the old does not.
        assert_eq!(s.pin_or_match("p.id", "K_NEW").unwrap(), PinOutcome::Match);
        match s.pin_or_match("p.id", "K_OLD").unwrap() {
            PinOutcome::Mismatch { old, .. } => assert_eq!(old, "K_NEW"),
            other => panic!("expected mismatch, got {other:?}"),
        }
    }

    #[test]
    fn iso_timestamp_format_is_well_formed() {
        let ts = current_utc_iso();
        // Shape: 1970-01-01T00:00:00Z ... 9999-12-31T23:59:59Z
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert!(ts.contains('T'));
    }

    /// Howard Hinnant's days_from_civil inverse — sanity check against
    /// known Unix-epoch dates so a future edit to the formula breaks
    /// loudly instead of silently shifting timestamps.
    #[test]
    fn civil_from_days_known_anchors() {
        // 1970-01-01 → day 0
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-01-01 → day 10957 (verified against `date -d 2000-01-01 +%s` / 86400)
        assert_eq!(civil_from_days(10957), (2000, 1, 1));
    }
}
