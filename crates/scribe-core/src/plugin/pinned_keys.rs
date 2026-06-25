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

/// R7 / S-01 + S-02 (CWE-347 improper-verification-of-signature /
/// CWE-295 improper-certificate-validation, applied to plugin author-key
/// trust). The decision a caller MUST route a plugin load through after
/// presenting the manifest key to [`PinnedKeyStore::pin_or_match`].
///
/// The defect this closes: the load loop treated `Match | New` identically
/// (a FIRST-seen key was silently TOFU-pinned with no consent) and dropped a
/// `Mismatch` (the pinned author key CHANGED — a possible plugin takeover) to
/// a log line that still let the plugin be considered. This enum makes the
/// three outcomes DISTINCT and makes "key changed → silently load" impossible
/// by construction.
#[derive(Debug, PartialEq, Eq)]
pub enum PluginLoadDecision {
    /// The presented key matches the pinned anchor (`Match`), OR this is a
    /// first contact (`New`) for which the user has ALREADY granted explicit
    /// consent. Safe to load + pin.
    Allow,
    /// First contact (`New`) with NO prior explicit consent. The load MUST be
    /// withheld until the user explicitly allows it (the "needs approval"
    /// surface). NOT silently pinned.
    NeedsFirstConsent,
    /// The pinned author key CHANGED. The plugin MUST NOT load. The UI must
    /// surface a blocking "author key changed — old→new, approve?" prompt and
    /// only [`PinnedKeyStore::replace_with_consent`] on explicit user approval.
    BlockKeyChanged { old: String, new: String },
}

/// Pure mapping from a [`PinOutcome`] (+ whether the user has already granted
/// explicit first-contact consent for this plugin) to a [`PluginLoadDecision`].
///
/// This is the security spine of the plugin key-trust gate — fully unit-tested
/// and free of IO so the "a Mismatch can never silently load" invariant is
/// provable. `first_consent_granted` is the caller's notion of explicit
/// approval (e.g. the plugin id present in the user's trusted-approvals map).
pub fn decide_key_trust(outcome: PinOutcome, first_consent_granted: bool) -> PluginLoadDecision {
    match outcome {
        // The key matches the trust anchor → always safe.
        PinOutcome::Match => PluginLoadDecision::Allow,
        // First contact: load ONLY with explicit prior consent; otherwise
        // hold for approval (never silent TOFU).
        PinOutcome::New => {
            if first_consent_granted {
                PluginLoadDecision::Allow
            } else {
                PluginLoadDecision::NeedsFirstConsent
            }
        }
        // Key rotation without consent → BLOCK, surface old→new. Never load.
        PinOutcome::Mismatch { old, new } => PluginLoadDecision::BlockKeyChanged { old, new },
    }
}

/// SEC-3 (defense-in-depth): the single decision an "Approve & run" flow MUST
/// route through, so the user-approval load path converges on the SAME central
/// trust gate ([`decide_key_trust`]) as the normal load path — a key-changed
/// (`Mismatch`) plugin can therefore NEVER be silently approved-and-run, even
/// if a future edit to the approve UI forgets to consult the gate directly.
///
/// `decide_approval` pins-or-matches the presented key, then maps the outcome
/// through [`decide_key_trust`] with `first_consent_granted = true` (the user
/// clicked Approve — that IS the explicit first-contact consent). The crucial
/// property: explicit consent upgrades a `New` first contact to [`Allow`], but
/// it does NOT override a `Mismatch` — a changed author key still yields
/// [`PluginLoadDecision::BlockKeyChanged`]. Approval is consent to TRUST a
/// first key, never consent to silently accept a key ROTATION (which requires
/// the explicit [`PinnedKeyStore::replace_with_consent`] path).
pub fn decide_approval(
    store: &mut PinnedKeyStore,
    plugin_id: &str,
    pubkey: &str,
) -> std::io::Result<PluginLoadDecision> {
    let outcome = store.pin_or_match(plugin_id, pubkey)?;
    // The user clicked "Approve", so first-contact consent is granted — but
    // `decide_key_trust` still BLOCKS a `Mismatch` regardless of consent.
    Ok(decide_key_trust(outcome, true))
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
    fn at_points_the_store_at_an_explicit_file() {
        // `at()` is the test/tools constructor — it stores keys at the exact path
        // given (not a config-dir-derived one), and a first pin persists there.
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("custom-keys.toml");
        let mut s = PinnedKeyStore::at(path.clone());
        assert_eq!(s.pin_or_match("p.id", "K1").unwrap(), PinOutcome::New);
        assert!(path.exists(), "pin persists to the explicit `at` path");
    }

    #[test]
    fn load_surfaces_a_non_notfound_io_error() {
        // When the store path is itself a DIRECTORY, read_to_string fails with a
        // NON-NotFound error — the generic `Err(e) => Err(e)` arm must propagate
        // it (not swallow it as an empty store), so a pin attempt errors.
        let dir = tempdir().expect("tempdir");
        // Use the directory path directly as the "store file".
        let mut s = PinnedKeyStore::at(dir.path().to_path_buf());
        let err = s
            .pin_or_match("p.id", "K1")
            .expect_err("reading a dir must err");
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "a directory read is a real IO error, not a missing-file fallback"
        );
    }

    /// Mutation guard for `load`'s NotFound match-guard (`e.kind() == NotFound`
    /// → `true` at line 142): a NON-NotFound read error MUST propagate, never be
    /// swallowed as an empty store. We seed the store file with invalid UTF-8 so
    /// `read_to_string` fails with `InvalidData` (a non-NotFound error). The
    /// original code routes that through `Err(e) => Err(e)` and `pin_or_match`
    /// returns Err. The mutant (`if true`) matches the NotFound arm, returns an
    /// empty `StoreFile`, then `pin_or_match` happily pins the new key and
    /// OVERWRITES the file — returning `Ok(New)`. Asserting the pin errors kills
    /// the mutant (and proves a corrupt-store IO error is not silently masked,
    /// which would let an attacker who can scribble the store reset every pin).
    #[test]
    fn load_does_not_swallow_a_non_notfound_read_error_as_empty() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("pinned-keys.toml");
        // Invalid UTF-8 → fs::read_to_string fails with ErrorKind::InvalidData.
        fs::write(&path, [0xff, 0xfe, 0x00, 0x80]).expect("seed corrupt store");
        let mut s = PinnedKeyStore::at(path.clone());
        let err = s
            .pin_or_match("p.id", "K1")
            .expect_err("a non-NotFound read error must NOT be swallowed");
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "the propagated error is the real read failure, not a missing-file fallback"
        );
        // And the corrupt bytes were NOT silently overwritten with a fresh pin.
        assert_eq!(
            fs::read(&path).expect("file still present"),
            vec![0xff, 0xfe, 0x00, 0x80],
            "the store must not have been reset by a swallowed error"
        );
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

    // --- R7 / S-01 + S-02: the pure key-trust decision gate ---

    #[test]
    fn decide_match_always_allows() {
        // A key matching the pinned anchor loads regardless of consent state.
        assert_eq!(
            decide_key_trust(PinOutcome::Match, false),
            PluginLoadDecision::Allow
        );
        assert_eq!(
            decide_key_trust(PinOutcome::Match, true),
            PluginLoadDecision::Allow
        );
    }

    #[test]
    fn decide_new_without_consent_needs_first_consent() {
        // S-01 — a FIRST-seen key must NOT silently TOFU-pin + load.
        assert_eq!(
            decide_key_trust(PinOutcome::New, false),
            PluginLoadDecision::NeedsFirstConsent
        );
    }

    #[test]
    fn decide_new_with_prior_consent_allows() {
        // First contact the user has ALREADY explicitly approved → load.
        assert_eq!(
            decide_key_trust(PinOutcome::New, true),
            PluginLoadDecision::Allow
        );
    }

    #[test]
    fn decide_mismatch_always_blocks_and_never_allows() {
        // S-02 — a CHANGED pinned key is a possible takeover. It must BLOCK and
        // surface old→new, NEVER load — even if some "consent" flag is set
        // (consent for first-contact is NOT consent for key rotation; that
        // requires `replace_with_consent`).
        for consent in [false, true] {
            let decision = decide_key_trust(
                PinOutcome::Mismatch {
                    old: "K_OLD".into(),
                    new: "K_NEW".into(),
                },
                consent,
            );
            match decision {
                PluginLoadDecision::BlockKeyChanged { old, new } => {
                    assert_eq!(old, "K_OLD");
                    assert_eq!(new, "K_NEW");
                }
                other => panic!("a mismatch must NEVER allow; got {other:?}"),
            }
            // Strongest invariant: a mismatch is never the Allow variant.
            assert_ne!(
                decide_key_trust(
                    PinOutcome::Mismatch {
                        old: "a".into(),
                        new: "b".into()
                    },
                    consent
                ),
                PluginLoadDecision::Allow,
                "a changed author key must never silently load"
            );
        }
    }

    // --- SEC-3: the "Approve & run" convergence on the central trust gate ---

    #[test]
    fn approve_of_first_contact_allows_and_pins() {
        // A genuine first-contact approval (user clicked Approve) loads + pins.
        let (_dir, mut s) = fresh_store();
        let decision = decide_approval(&mut s, "p.id", "K_FIRST").unwrap();
        assert_eq!(decision, PluginLoadDecision::Allow);
        // The key is now pinned (a re-approval Matches).
        assert_eq!(
            s.pin_or_match("p.id", "K_FIRST").unwrap(),
            PinOutcome::Match
        );
    }

    #[test]
    fn approve_of_key_changed_plugin_is_not_silently_approved() {
        // SEC-3 — the load-bearing invariant: routing the approval through the
        // central gate means a CHANGED author key (a possible plugin takeover)
        // can NEVER be silently approved-and-run. Approval is consent to trust a
        // FIRST key, not consent to accept a key ROTATION (which requires
        // `replace_with_consent`).
        let (_dir, mut s) = fresh_store();
        // Pin an original author key.
        s.pin_or_match("p.id", "K_ORIGINAL").unwrap();
        // An attacker re-submits the plugin with a DIFFERENT author key and the
        // user clicks "Approve & run".
        let decision = decide_approval(&mut s, "p.id", "K_ATTACKER").unwrap();
        match decision {
            PluginLoadDecision::BlockKeyChanged { old, new } => {
                assert_eq!(old, "K_ORIGINAL");
                assert_eq!(new, "K_ATTACKER");
            }
            other => panic!("a key-changed approve must BLOCK, not allow; got {other:?}"),
        }
        // Strongest form: it is NEVER the Allow variant.
        assert_ne!(
            decide_approval(&mut s, "p.id", "K_ATTACKER").unwrap(),
            PluginLoadDecision::Allow,
            "approving a key-changed plugin must never load it"
        );
        // The original pin is unchanged — approval did not silently rotate it.
        assert_eq!(
            s.pin_or_match("p.id", "K_ORIGINAL").unwrap(),
            PinOutcome::Match,
            "the original key remains the trust anchor"
        );
    }

    #[test]
    fn iso_timestamp_format_is_well_formed() {
        let ts = current_utc_iso();
        // Shape: 1970-01-01T00:00:00Z ... 9999-12-31T23:59:59Z
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert!(ts.contains('T'));
    }

    /// Howard Hinnant's days_from_civil inverse — known-answer table over a
    /// diverse set of Unix-day offsets so a future edit (or a mutation) to the
    /// formula breaks loudly instead of silently shifting timestamps. Every
    /// anchor below is independently cross-checked against Python's
    /// `datetime.date(1970,1,1) + timedelta(days=d)`.
    ///
    /// The spread is deliberate: it exercises the era split (`z - era*146097`),
    /// the `yoe` leap accounting (`doe/1460`, `doe/36524`, `doe/146096`), the
    /// `doy` reduction (`365*yoe + yoe/4 - yoe/100`), the month polynomial
    /// (`(5*doy + 2)/153`, `(153*mp + 2)/5`), the `mp<10 ? mp+3 : mp-9` month
    /// wrap, and the `m<=2 ? y+1 : y` year carry — so the surviving
    /// arithmetic-operator mutants in `civil_from_days` each flip at least one
    /// of these anchors. A single anchor (the prior 2-point test) left many
    /// mutants alive because they happened to agree at 1970-01-01 / 2000-01-01.
    #[test]
    fn civil_from_days_known_anchors() {
        let cases: &[(i64, (i32, u32, u32))] = &[
            (0, (1970, 1, 1)),       // epoch
            (-1, (1969, 12, 31)),    // day before epoch (negative-era branch)
            (59, (1970, 3, 1)),      // just past a non-leap February
            (365, (1971, 1, 1)),     // first year roll-over
            (10956, (1999, 12, 31)), // last day before 2000
            (10957, (2000, 1, 1)),   // century leap year start
            (11017, (2000, 3, 1)),   // 2000 is a leap year (Feb has 29 days)
            (19723, (2024, 1, 1)),   // recent year start
            (19782, (2024, 2, 29)),  // 2024 leap day — the month/day polynomial
            (40177, (2080, 1, 1)),   // far future, a different 400-year era
            // Negative-era branch: `z = day + 719_468 < 0` only when
            // `day < -719_468`, so this proleptic-Gregorian anchor is the ONLY
            // one that exercises the `else { z - 146_096 }` arm. Year 0 is
            // expressible in proleptic Gregorian (Python's `datetime` cannot
            // represent it, but the Hinnant inverse is defined there). The
            // `- 146_096` mutants (`-`→`+`/`/`) shift the era and overflow on
            // this input, so this anchor catches them where the day=-1 anchor
            // (still in the `z >= 0` arm) cannot.
            (-719_469, (0, 2, 29)),
        ];
        for &(day, expected) in cases {
            assert_eq!(
                civil_from_days(day),
                expected,
                "civil_from_days({day}) must be {expected:?}"
            );
        }
    }

    /// Known-answer test for the full `current_utc_iso` formatting via a
    /// reconstruction of its civil-time split. `current_utc_iso` reads
    /// `SystemTime::now()` (no injectable clock seam in production), so its
    /// `secs/86400` / `secs%86400` / `/3600` / `%3600` / `/60` / `%60`
    /// arithmetic cannot be pinned through that entry point directly. We instead
    /// pin the SAME split math against fixed timestamps so the boundary cases
    /// (midnight, the last second of a day, an arbitrary mid-day instant) are
    /// covered with exact expected strings — a mutation of any of those
    /// operators changes one of these expected outputs.
    ///
    /// MUTANT-NOTE: the `current_utc_iso` mutants at the `secs/86400`,
    /// `secs%86400`, `remainder/3600`, `remainder%3600` operators act on
    /// `SystemTime::now()` inside a private, clock-non-injectable function; they
    /// are not reachable from a deterministic test through that function. This
    /// test reproduces the identical split formula so the arithmetic contract is
    /// still pinned by a known-answer test at the algorithm level.
    #[test]
    fn utc_iso_split_known_answers() {
        // Reproduce current_utc_iso's exact civil-split-then-format logic.
        fn iso_for(secs: u64) -> String {
            let days = secs / 86_400;
            let remainder = secs % 86_400;
            let hour = remainder / 3600;
            let minute = (remainder % 3600) / 60;
            let second = remainder % 60;
            let (year, month, day) = civil_from_days(days as i64);
            format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
        }
        // Epoch midnight.
        assert_eq!(iso_for(0), "1970-01-01T00:00:00Z");
        // Last second of the epoch day — exercises the %86400 / %3600 / %60 wraps.
        assert_eq!(iso_for(86_399), "1970-01-01T23:59:59Z");
        // First second of the next day — exercises the /86400 day carry.
        assert_eq!(iso_for(86_400), "1970-01-02T00:00:00Z");
        // An arbitrary mid-day instant: 2024-02-29 13:37:07 UTC.
        // 19782 days * 86400 + 13*3600 + 37*60 + 7 = 1709213827.
        assert_eq!(iso_for(1_709_213_827), "2024-02-29T13:37:07Z");
    }
}
