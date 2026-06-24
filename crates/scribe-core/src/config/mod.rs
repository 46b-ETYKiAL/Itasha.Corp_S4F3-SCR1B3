//! User configuration (TOML, live-reloadable). Great defaults out of the box;
//! everything overridable. Parsing never panics — malformed config falls back
//! to defaults with a surfaced error.
//!
//! The config is split into cohesive submodules by domain; everything is
//! re-exported here so external callers keep the flat `scribe_core::config::X`
//! paths regardless of which submodule a type lives in.

mod appearance;
mod editor;
mod motion;
mod reporting;
mod system;
mod window;

pub use appearance::*;
pub use editor::*;
pub use motion::*;
pub use reporting::*;
pub use system::*;
pub use window::*;

use crate::error::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// serde default for opt-OUT booleans (fields that should be ON unless the
/// user turns them off, and ON for configs written before the field existed).
///
/// Shared across config submodules (editor / appearance / toolbar), so it lives
/// at the module root and is imported via `super::default_true`.
pub(crate) fn default_true() -> bool {
    true
}

/// Current config schema version. Bumped whenever a one-time migration is
/// needed (see [`Config::migrate`]). A config written before schema versioning
/// deserializes with `schema_version == 0` (the serde default for a missing
/// field) and is migrated up on load.
///
/// - v3 (W1TN3SS opt-in reporting): adds the [`ReportingConfig`] section. The
///   migration is purely ADDITIVE — both reporting streams default `Off`, so an
///   existing config that has never seen the section upgrades with reporting
///   fully OFF and with NO stored value overwritten (the opt-in, never-opt-out
///   invariant).
pub const CURRENT_SCHEMA_VERSION: u32 = 3;

/// Root config. `#[serde(default)]` everywhere so a partial user file merges
/// onto defaults rather than failing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// One-time-migration schema version. A fresh config is born at
    /// [`CURRENT_SCHEMA_VERSION`]; an existing config written before versioning
    /// loads as `0` (serde default for the missing field) and [`migrate`] brings
    /// it forward exactly once. NEVER hand-edit downward.
    ///
    /// [`migrate`]: Config::migrate
    #[serde(default)]
    pub schema_version: u32,
    pub editor: EditorConfig,
    pub appearance: AppearanceConfig,
    pub fonts: FontConfig,
    pub window: WindowConfig,
    pub updates: UpdateConfig,
    pub spellcheck: SpellcheckConfig,
    pub plugins: PluginConfig,
    pub toolbar: ToolbarConfig,
    #[serde(default)]
    pub motion: MotionConfig,
    #[serde(default)]
    pub scroll: ScrollConfig,
    /// W1TN3SS opt-in crash/error reporting (schema v3). BOTH streams default
    /// `Off` — SCR1B3 stays telemetry-free by default; nothing is ever
    /// transmitted without an explicit per-event consent. `#[serde(default)]`
    /// means a config written before v3 reads the whole section as `Off`.
    #[serde(default)]
    pub reporting: ReportingConfig,
}

impl Default for Config {
    fn default() -> Self {
        // A FRESH config is born already at the current schema version, so
        // `migrate` is a no-op for new users (no spurious first-run rewrite).
        // Only an EXISTING file (which deserializes `schema_version` to 0) is
        // migrated. Every other field defers to its own `Default`.
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            editor: EditorConfig::default(),
            appearance: AppearanceConfig::default(),
            fonts: FontConfig::default(),
            window: WindowConfig::default(),
            updates: UpdateConfig::default(),
            spellcheck: SpellcheckConfig::default(),
            plugins: PluginConfig::default(),
            toolbar: ToolbarConfig::default(),
            motion: MotionConfig::default(),
            scroll: ScrollConfig::default(),
            reporting: ReportingConfig::default(),
        }
    }
}

impl Config {
    /// Parse from a TOML string; on error, return defaults plus the error so
    /// the caller can surface it without losing the editor.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        toml::from_str(s).map_err(|e| CoreError::ConfigParse(e.to_string()))
    }

    pub fn to_toml_string(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Atomically write the config to `path` (serialize → temp file in the same
    /// dir → rename). Two correctness guarantees the plain `fs::write` lacked,
    /// both of which silently corrupted persistence:
    ///
    /// 1. **Atomic.** A `fs::write` truncates then streams, so the live config
    ///    WATCHER could fire mid-write and read a partially-written-but-still-
    ///    valid TOML (truncated at a table boundary). Because every section is
    ///    `#[serde(default)]`, the missing later sections deserialize to DEFAULTS,
    ///    and the reload then clobbered those in-memory settings back to default —
    ///    the "some settings revert after reopen" bug. A temp-then-rename means
    ///    the watcher only ever observes the complete file.
    /// 2. **Never writes empty.** If serialization fails, `to_toml_string`'s
    ///    `unwrap_or_default()` returns `""`; writing that would wipe the whole
    ///    config. We refuse to write an empty/failed serialization (returns an
    ///    error instead of destroying the file).
    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        let body = self.to_toml_string();
        if body.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "config serialization produced empty output; refusing to overwrite",
            ));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Temp in the SAME dir so the rename is a same-volume atomic replace.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body.as_bytes())?;
        std::fs::rename(&tmp, path)
    }

    /// Default per-OS config directory: e.g. `%APPDATA%/scr1b3` /
    /// `~/.config/scr1b3` / `~/Library/Application Support/scr1b3`.
    ///
    /// `SCR1B3_CONFIG_DIR` overrides the resolved path when set to a non-empty
    /// value. This enables a portable / "bring your own config dir" mode and is
    /// the supported way to point the app at an isolated directory (e.g. for QA
    /// or testing) — the `directories` crate resolves the Windows config root
    /// via `SHGetKnownFolderPath`, which ignores the `APPDATA` environment
    /// variable, so an env-redirect of `APPDATA` alone does NOT relocate config.
    pub fn config_dir() -> Option<PathBuf> {
        Self::config_dir_from(std::env::var_os("SCR1B3_CONFIG_DIR"))
    }

    /// Resolve the config dir given an explicit override value (pure — no env
    /// read), so the precedence is unit-testable without mutating process-global
    /// state. A non-empty override wins; otherwise fall back to the OS default.
    fn config_dir_from(override_dir: Option<std::ffi::OsString>) -> Option<PathBuf> {
        if let Some(override_dir) = override_dir {
            if !override_dir.is_empty() {
                return Some(PathBuf::from(override_dir));
            }
        }
        directories::ProjectDirs::from("com", "ItashaCorp", crate::CONFIG_DIR_NAME)
            .map(|d| d.config_dir().to_path_buf())
    }

    pub fn config_file_path() -> Option<PathBuf> {
        Self::config_dir().map(|d| d.join("scr1b3.toml"))
    }

    /// Apply one-time, version-gated migrations in place. Returns `true` when
    /// anything changed (the caller should then persist).
    ///
    /// **Why this exists.** Every section is `#[serde(default)]`, so a value
    /// STORED in the user's file always wins over the source default. That means
    /// a good default flipped on in a later release (line numbers, restore
    /// session, toolbar-in-titlebar, …) can NEVER reach a user whose config
    /// predates the flip — their stored value (or stored-`false`) sticks forever.
    /// Each migration step re-applies the intended experience-baseline ONCE, then
    /// bumps `schema_version`, so the user's own later deliberate changes are
    /// never overridden again (the step won't re-run).
    pub fn migrate(&mut self) -> bool {
        let original = self.schema_version;
        let mut changed = false;

        // v0 → v1: re-assert the experience-baseline toggles that ship on by
        // default but were stuck off for users whose config predates the flip.
        // One-shot: after this, `schema_version == 1` and the block is skipped,
        // so toggling any of these off later is respected.
        if self.schema_version < 1 {
            self.editor.show_line_numbers = true;
            self.editor.show_minimap = true;
            self.editor.word_wrap = true;
            self.editor.restore_session = true;
            self.editor.restore_cursor_position = true;
            self.appearance.toolbar_in_titlebar = true;
            self.toolbar.show_dropdown = true;
            self.schema_version = 1;
            changed = true;
        }

        // v1 → v2: the update-check default changed Manual → Notify. Upgrade only
        // configs still on the OLD default (Manual) so users who deliberately
        // chose Off or Auto keep their choice; the on-launch check is a single
        // telemetry-free GitHub-Releases query (no PII).
        if self.schema_version < 2 {
            if self.updates.mode == UpdateMode::Manual {
                self.updates.mode = UpdateMode::Notify;
            }
            self.schema_version = 2;
            changed = true;
        }

        // v2 → v3: the W1TN3SS opt-in reporting section landed. The step is
        // PURELY ADDITIVE and re-applies NOTHING — `reporting` is a brand-new
        // `#[serde(default)]` section, so a config that predates it already
        // deserialized with BOTH streams `Off` (the opt-in, never-opt-out
        // invariant). We deliberately do NOT touch `self.reporting` here: there
        // is no "good default we need to push onto old users" — the privacy
        // default IS `Off`, and forcing any value would be the exact default-on
        // anti-pattern the consent contract forbids. The step only stamps the
        // version so the additive section is recorded as migrated.
        if self.schema_version < 3 {
            self.schema_version = 3;
            changed = true;
        }

        // Migration invariants (debug-only): it must never LOWER the version,
        // and any config that started below the current schema must end exactly
        // at it. A FORWARD-version config (`original > CURRENT`, e.g. a file
        // written by a newer build then opened by an older one) is left untouched
        // and legitimately stays ahead — so we must NOT assert an upper bound
        // (the previous `schema_version <= CURRENT` assert panicked in debug
        // builds on exactly that downgrade case).
        debug_assert!(self.schema_version >= original);
        debug_assert!(
            original >= CURRENT_SCHEMA_VERSION || self.schema_version == CURRENT_SCHEMA_VERSION
        );
        changed
    }

    /// Load config from the OS config file, or defaults if absent/broken.
    /// Returns `(config, Option<error_message>)` — never fails to produce a
    /// usable config. An existing config is migrated up to the current schema on
    /// load (and the result persisted atomically so the migration is durable and
    /// runs exactly once).
    pub fn load_or_default() -> (Self, Option<String>) {
        let Some(path) = Self::config_file_path() else {
            return (Self::default(), None);
        };
        match std::fs::read_to_string(&path) {
            Ok(s) => match Self::from_toml_str(&s) {
                Ok(mut cfg) => {
                    if cfg.migrate() {
                        // Persist so the one-time migration doesn't re-run and the
                        // upgraded baseline is durable. Best-effort: a failed save
                        // doesn't block startup (it re-applies next launch), but we
                        // log it rather than swallowing it silently.
                        if let Err(e) = cfg.save_to(&path) {
                            tracing::warn!(
                                error = %e,
                                "config schema migration could not be persisted; \
                                 it will be re-applied on next launch"
                            );
                        }
                    }
                    (cfg, None)
                }
                Err(e) => {
                    // The file exists but is malformed (hand-edit typo, partial
                    // write, disk corruption). PRESERVE it as `<name>.toml.bak`
                    // BEFORE the app starts on defaults — otherwise the very next
                    // settings change saves defaults over the recoverable original
                    // and the user's real settings are lost for good. Best-effort:
                    // a failed backup must never block startup.
                    backup_corrupt_config(&path);
                    (Self::default(), Some(e.to_string()))
                }
            },
            Err(_) => (Self::default(), None), // absent = use defaults silently
        }
    }
}

/// Copy a malformed config file to a `<name>.toml.bak` sibling so a user's
/// hand-edited-but-broken (or partially-corrupted) settings are recoverable
/// after the app falls back to defaults. Best-effort — any IO error is
/// swallowed (the caller must not fail startup over a backup). Returns whether
/// the backup was written, for testing.
fn backup_corrupt_config(path: &std::path::Path) -> bool {
    let bak = path.with_extension("toml.bak");
    std::fs::copy(path, &bak).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip() {
        let c = Config::default();
        let s = c.to_toml_string();
        let back = Config::from_toml_str(&s).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn partial_merges_onto_defaults() {
        let c = Config::from_toml_str("[editor]\ntab_width = 2\n").unwrap();
        assert_eq!(c.editor.tab_width, 2);
        // unspecified fields keep defaults
        assert!(c.editor.show_line_numbers);
        assert_eq!(c.appearance.theme, "itasha-corp");
    }

    #[test]
    fn malformed_is_error_not_panic() {
        assert!(Config::from_toml_str("editor = [[[").is_err());
    }

    #[test]
    fn word_wrap_and_animations_on_by_default() {
        assert!(Config::default().editor.word_wrap);
        assert!(Config::default().motion.enabled);
    }

    #[test]
    fn fresh_config_is_born_at_current_schema_version() {
        // A new user's config is already current, so `migrate` is a no-op
        // (no spurious first-run rewrite).
        let mut c = Config::default();
        assert_eq!(c.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(!c.migrate(), "a current-version config must not migrate");
    }

    #[test]
    fn legacy_config_migrates_experience_defaults_once() {
        // An existing config with no schema_version (→ 0) that has the
        // experience toggles stored OFF — the exact "good default can't reach
        // the user" bug. Migration must flip them on and stamp version 1.
        let toml = "\
[editor]
show_line_numbers = false
show_minimap = false
word_wrap = false
restore_session = false
restore_cursor_position = false

[appearance]
toolbar_in_titlebar = false

[toolbar]
show_dropdown = false
";
        let mut c = Config::from_toml_str(toml).unwrap();
        assert_eq!(c.schema_version, 0, "legacy config loads as version 0");
        assert!(
            !c.editor.show_line_numbers,
            "stored false wins pre-migration"
        );

        assert!(c.migrate(), "v0 config must report a change");
        assert!(c.editor.show_line_numbers);
        assert!(c.editor.show_minimap);
        assert!(c.editor.word_wrap);
        assert!(c.editor.restore_session);
        assert!(c.editor.restore_cursor_position);
        assert!(c.appearance.toolbar_in_titlebar);
        assert!(c.toolbar.show_dropdown);
        assert_eq!(c.schema_version, CURRENT_SCHEMA_VERSION);

        // Idempotent: a second pass changes nothing, so a user who later turns
        // any of these OFF is respected (the v0 block never re-runs).
        assert!(
            !c.migrate(),
            "already-migrated config must not migrate again"
        );
        c.editor.show_minimap = false; // user's deliberate later choice
        assert!(!c.migrate());
        assert!(
            !c.editor.show_minimap,
            "migration must not override a v1 user choice"
        );
    }

    #[test]
    fn schema_version_round_trips() {
        let c = Config::default();
        let back = Config::from_toml_str(&c.to_toml_string()).unwrap();
        assert_eq!(back.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn update_mode_defaults_to_notify_and_v2_migrates_from_manual() {
        // Fresh default is Notify.
        assert_eq!(Config::default().updates.mode, UpdateMode::Notify);
        // A legacy config explicitly on the OLD Manual default migrates to Notify.
        let mut legacy = Config::from_toml_str("[updates]\nmode = \"manual\"\n").unwrap();
        assert_eq!(legacy.schema_version, 0);
        assert!(legacy.migrate());
        assert_eq!(legacy.updates.mode, UpdateMode::Notify);
        assert_eq!(legacy.schema_version, CURRENT_SCHEMA_VERSION);
        // A deliberate Off (or Auto) choice is preserved across migration.
        let mut off = Config::from_toml_str("[updates]\nmode = \"off\"\n").unwrap();
        assert!(off.migrate());
        assert_eq!(off.updates.mode, UpdateMode::Off);
    }

    #[test]
    fn v2_config_migrates_to_v3_with_reporting_off_and_stored_values_preserved() {
        // The exact opt-in invariant: an EXISTING v2 config (one that predates
        // the reporting section) upgrades to v3 with BOTH reporting streams Off
        // AND with every previously-stored value untouched. A default-on migrate
        // or a clobbered user value would breach the privacy contract.
        let toml = "\
schema_version = 2

[editor]
tab_width = 8
show_line_numbers = false

[updates]
mode = \"off\"
";
        let mut c = Config::from_toml_str(toml).unwrap();
        assert_eq!(c.schema_version, 2, "fixture loads as a v2 config");
        // The reporting section is absent in the v2 TOML, so it reads as Off.
        assert_eq!(c.reporting.crash_reports, ReportingMode::Off);
        assert_eq!(c.reporting.manual_issues, ReportingMode::Off);

        assert!(c.migrate(), "a v2 config must report a change to reach v3");
        assert_eq!(c.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(c.schema_version, 3);

        // Reporting stayed OFF (additive, never default-on).
        assert_eq!(
            c.reporting.crash_reports,
            ReportingMode::Off,
            "v2->v3 migrate must leave the crash stream OFF (opt-in only)"
        );
        assert_eq!(
            c.reporting.manual_issues,
            ReportingMode::Off,
            "v2->v3 migrate must leave the manual stream OFF (opt-in only)"
        );

        // Every stored value the user had on a v2 config survives the migrate
        // (stored-value-wins — the v2->v3 step touches nothing but the version).
        assert_eq!(c.editor.tab_width, 8, "stored tab_width preserved");
        assert!(
            !c.editor.show_line_numbers,
            "a stored-false experience toggle on an ALREADY-v2 config is the \
             user's choice and survives — the v0->v1 re-assert never re-runs"
        );
        assert_eq!(
            c.updates.mode,
            UpdateMode::Off,
            "stored update mode preserved"
        );

        // Idempotent: a second pass changes nothing.
        assert!(!c.migrate(), "an already-v3 config must not migrate again");
    }

    #[test]
    fn migrate_is_a_noop_and_never_panics_on_a_forward_version_config() {
        // A config written by a NEWER build (schema_version ahead of CURRENT),
        // then opened by an older build, must be left untouched — and must not
        // trip the debug-only migration invariants (the old `<= CURRENT` assert
        // panicked here in debug builds).
        // Built from TOML (not Default + field reassign — that trips clippy's
        // field_reassign_with_default) so we also confirm schema_version
        // round-trips through deserialization.
        let ahead = CURRENT_SCHEMA_VERSION + 5;
        let mut forward = Config::from_toml_str(&format!(
            "schema_version = {ahead}\n[updates]\nmode = \"off\"\n"
        ))
        .unwrap();
        assert_eq!(forward.schema_version, ahead);
        assert!(
            !forward.migrate(),
            "forward-version config must not be changed"
        );
        assert_eq!(forward.schema_version, ahead);
        assert_eq!(forward.updates.mode, UpdateMode::Off);
    }

    #[test]
    fn corrupt_config_is_backed_up_before_fallback_to_defaults() {
        // A malformed config file must be preserved as `<name>.toml.bak` so the
        // user's recoverable settings survive the app falling back to defaults.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scr1b3.toml");
        let corrupt = "this = is = not valid TOML [[[";
        std::fs::write(&path, corrupt).unwrap();

        assert!(backup_corrupt_config(&path), "backup must report success");

        let bak = path.with_extension("toml.bak");
        assert!(
            bak.exists(),
            "a .toml.bak must be written next to the original"
        );
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            corrupt,
            "the backup must hold the ORIGINAL (recoverable) bytes verbatim"
        );
        // The original is left in place (the app's atomic save will replace it).
        assert!(path.exists());
    }

    #[test]
    fn backup_corrupt_config_is_best_effort_on_a_missing_file() {
        // Never panics / never fails startup when there's nothing to copy.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.toml");
        assert!(!backup_corrupt_config(&missing));
    }

    #[test]
    fn config_dir_override_wins_when_non_empty() {
        // A non-empty SCR1B3_CONFIG_DIR value relocates the config dir verbatim
        // (portable / isolated-QA mode). Tested via the pure helper so no
        // process-global env mutation is needed (keeps the parallel test runner
        // deterministic).
        let custom = std::ffi::OsString::from(r"C:\tmp\scr1b3-qa");
        assert_eq!(
            Config::config_dir_from(Some(custom)),
            Some(std::path::PathBuf::from(r"C:\tmp\scr1b3-qa"))
        );
    }

    #[test]
    fn config_dir_override_ignored_when_empty() {
        // An empty override must fall through to the OS default, never resolve to
        // "" (which would put config at the process CWD).
        let from_empty = Config::config_dir_from(Some(std::ffi::OsString::new()));
        let from_none = Config::config_dir_from(None);
        assert_eq!(from_empty, from_none);
    }

    #[test]
    fn save_to_writes_atomically_and_round_trips() {
        // Regression for "settings revert after reopen": the save must be a single
        // atomic temp+rename (no partial file for the watcher to read), the temp
        // must be renamed away, and the on-disk TOML must round-trip exactly —
        // including populated path maps alongside changed scalar settings.
        let dir = std::env::temp_dir().join(format!("scr1b3-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scr1b3.toml");
        let mut c = Config::default();
        c.editor
            .scroll_positions
            .insert("/x/y.rs".to_string(), 42.0);
        c.editor.cursor_positions.insert("/x/y.rs".to_string(), 7);
        c.toolbar.show_dropdown = false;
        c.appearance.toolbar_in_titlebar = false;
        c.save_to(&path).expect("save_to must succeed");
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "the temp file must be renamed away (atomic write)"
        );
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(!s.is_empty(), "must never persist an empty config");
        let back = Config::from_toml_str(&s).unwrap();
        assert_eq!(
            back, c,
            "config must round-trip exactly through an atomic save"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- save_to refuse-empty + parent creation (previously uncovered) ----

    #[test]
    fn save_to_creates_missing_parent_directories() {
        // `save_to` must `create_dir_all` the parent so a first-run write into a
        // not-yet-existing config dir succeeds rather than erroring on ENOENT.
        let base = std::env::temp_dir().join(format!("scr1b3-mkdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let nested = base.join("a").join("b").join("scr1b3.toml");
        assert!(
            !nested.parent().unwrap().exists(),
            "parent absent before save"
        );
        Config::default()
            .save_to(&nested)
            .expect("save_to must create the parent chain and write");
        assert!(nested.exists(), "config written into the created dir tree");
        let _ = std::fs::remove_dir_all(&base);
    }

    // ---- load_or_default end-to-end via SCR1B3_CONFIG_DIR (previously uncovered) ----

    /// Serializes every test that mutates the process-global `SCR1B3_CONFIG_DIR`
    /// env var. Cargo runs tests in PARALLEL by default, so without this lock two
    /// `with_config_dir` windows overlap and clobber each other's redirect,
    /// producing flaky failures (the prior doc comment's `--test-threads=1`
    /// assumption was false). The mutex makes the env mutation atomic per test.
    static CONFIG_DIR_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `body` with `SCR1B3_CONFIG_DIR` pointed at `dir`, restoring the prior
    /// value afterwards. The `CONFIG_DIR_ENV_LOCK` guard guarantees no concurrent
    /// test observes the temporary env mutation, so this is safe under cargo's
    /// default parallel test runner.
    fn with_config_dir(dir: &std::path::Path, body: impl FnOnce()) {
        // Recover from a poisoned lock: a panicking test still left the env in a
        // restored state (or the next test overwrites it), so the guard's only
        // job is mutual exclusion, not protecting shared data.
        let _guard = CONFIG_DIR_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prev = std::env::var_os("SCR1B3_CONFIG_DIR");
        std::env::set_var("SCR1B3_CONFIG_DIR", dir);
        body();
        match prev {
            Some(v) => std::env::set_var("SCR1B3_CONFIG_DIR", v),
            None => std::env::remove_var("SCR1B3_CONFIG_DIR"),
        }
    }

    #[test]
    fn load_or_default_returns_defaults_when_no_file_present() {
        // An empty (but existing) config dir has no scr1b3.toml → defaults, no
        // error message, and nothing written.
        let dir = std::env::temp_dir().join(format!("scr1b3-load-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        with_config_dir(&dir, || {
            let (cfg, err) = Config::load_or_default();
            assert_eq!(cfg, Config::default(), "absent file => defaults");
            assert!(err.is_none(), "absent file is silent, not an error");
        });
        assert!(
            !dir.join("scr1b3.toml").exists(),
            "load must not create a file when none existed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_or_default_reads_existing_and_migrates_it_durably() {
        // A pre-migration file (schema_version 0, the serde default for a missing
        // key) is loaded, migrated up to the current schema, and the upgraded
        // baseline is persisted so the one-time migration never re-runs.
        let dir = std::env::temp_dir().join(format!("scr1b3-load-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scr1b3.toml");
        // A minimal legacy file: no schema_version, line numbers explicitly off.
        std::fs::write(&path, "[editor]\nshow_line_numbers = false\n").unwrap();
        with_config_dir(&dir, || {
            let (cfg, err) = Config::load_or_default();
            assert!(err.is_none(), "a well-formed file loads cleanly");
            assert_eq!(
                cfg.schema_version, CURRENT_SCHEMA_VERSION,
                "load_or_default must migrate up to the current schema"
            );
            // v0→v1 re-asserts the line-number baseline on a pre-migration file.
            assert!(cfg.editor.show_line_numbers, "v0->v1 re-asserts the toggle");
        });
        // The migration was persisted (so it runs exactly once): re-reading the
        // on-disk file now shows the bumped schema version.
        let persisted = Config::from_toml_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            persisted.schema_version, CURRENT_SCHEMA_VERSION,
            "the migrated config must be written back to disk"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_or_default_backs_up_a_malformed_file_and_falls_back() {
        // A corrupt config must NOT block startup: load_or_default returns
        // defaults + an error message AND preserves the original as a `.bak` so
        // the user's real settings are recoverable.
        let dir = std::env::temp_dir().join(format!("scr1b3-load-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scr1b3.toml");
        std::fs::write(&path, "this is = = not valid toml [[[").unwrap();
        with_config_dir(&dir, || {
            let (cfg, err) = Config::load_or_default();
            assert_eq!(cfg, Config::default(), "malformed => safe defaults");
            assert!(err.is_some(), "the parse error is surfaced, not swallowed");
        });
        assert!(
            path.with_extension("toml.bak").exists(),
            "the malformed original must be preserved as a .bak"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
