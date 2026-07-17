//! Telemetry-free, local-only session action log.
//!
//! Records high-level UI actions (tab / window / settings / command / error
//! events) to a file under the config dir so a session can be diagnosed after
//! the fact — including the "I clicked X and nothing happened" case, which shows
//! up as a MISSING entry (the handler never fired). No network, no analytics, no
//! data beyond what the user already sees on screen. Opt out entirely with the
//! `SCR1B3_NO_ACTION_LOG=1` environment variable.
//!
//! Location: `<config_dir>/session-actions.log` (one tab-separated record per
//! line: `<unix-seconds>\t<category>\t<detail>`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use scribe_core::Config;

/// Size cap for the action log. When the file grows past this, it is rotated
/// (the current file is renamed to `<name>.1`, replacing any previous `.1`) so
/// the live log restarts empty. Telemetry-free local diagnostics never need an
/// unbounded log; one rotated generation keeps recent history available.
pub const ACTION_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Resolve the action-log path from the environment and config dir. Pure — it
/// reads `SCR1B3_NO_ACTION_LOG` and `Config::config_dir()` on every call and
/// returns the log path, or `None` when logging is disabled or there is no
/// config dir. The process-global one-shot cache lives in [`path`]; the
/// resolution logic is split out here so it is unit-testable without priming
/// that cache.
fn resolve_log_path() -> Option<PathBuf> {
    if std::env::var_os("SCR1B3_NO_ACTION_LOG").is_some() {
        return None;
    }
    Config::config_dir().map(|d| d.join("session-actions.log"))
}

/// The resolved log path, cached. `None` when disabled via env or when there is
/// no config dir (then `record` is a no-op).
//
// `path` is a first-caller-wins `OnceLock` cache over `resolve_log_path`. Any of
// the 8 production `record` call sites primes it, and app-level tests trigger
// those under a shared `cargo test` process, so once the cache is set its value
// is fixed for the binary's lifetime — a body mutation (`-> None` /
// `-> Some(default)`) cannot be observed deterministically by a unit test
// (whichever test runs first decides the cached value). The resolution LOGIC it
// caches is fully covered by the `resolve_log_path` tests below; the two body
// mutants of this thin cache wrapper are pardoned in `.cargo/mutants.toml`
// (`exclude_re`) as the untestable process-global cache boundary.
fn path() -> Option<&'static PathBuf> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(resolve_log_path).as_ref()
}

/// Append one timestamped action record. Best-effort: never panics, never blocks
/// the UI on failure. Disabled (no-op) under `SCR1B3_NO_ACTION_LOG=1`.
pub fn record(category: &str, detail: &str) {
    if let Some(p) = path() {
        append_line(p, category, detail);
    }
}

/// The pure file-append behind [`record`], separated so it is unit-testable
/// against a temp path without touching the real config dir.
pub fn append_line(path: &Path, category: &str, detail: &str) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    rotate_if_oversized(path, ACTION_LOG_MAX_BYTES);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        // Keep one action per line: flatten any newlines/tabs in the detail.
        let detail = detail.replace(['\n', '\r', '\t'], " ");
        let _ = writeln!(f, "{ts}\t{category}\t{detail}");
    }
}

/// Best-effort log rotation: when `path` exceeds `max_bytes`, rename it to
/// `<path>.1` (overwriting any previous rotation) so the live log restarts
/// empty while one prior generation of history is retained. Never panics; any
/// I/O error leaves the log as-is (the next append simply tries again).
fn rotate_if_oversized(path: &Path, max_bytes: u64) {
    let oversized = std::fs::metadata(path)
        .map(|m| m.len() > max_bytes)
        .unwrap_or(false);
    if !oversized {
        return;
    }
    let mut rotated = path.as_os_str().to_owned();
    rotated.push(".1");
    // `rename` atomically replaces an existing `.1` on every supported OS.
    let _ = std::fs::rename(path, &rotated);
}

#[cfg(test)]
mod tests {
    use super::{append_line, resolve_log_path, rotate_if_oversized, ACTION_LOG_MAX_BYTES};

    #[test]
    fn a_log_exactly_at_the_cap_is_not_rotated() {
        // At len == max_bytes: clean `>` is false (not oversized, no rotation);
        // the `> -> >=` mutant rotates. No existing test seeds a file exactly at
        // the cap. Kills 75:26.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        let rotated = {
            let mut s = p.as_os_str().to_owned();
            s.push(".1");
            std::path::PathBuf::from(s)
        };
        let max: u64 = 4096;
        std::fs::write(&p, vec![b'x'; max as usize]).unwrap();
        rotate_if_oversized(&p, max);
        assert!(p.exists(), "a log exactly at the cap must not be rotated");
        assert!(!rotated.exists(), "no rotation at exactly the cap");
    }

    /// Serializes tests that mutate the process-global action-log env vars.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `body` with the two action-log env vars set as given, restoring the
    /// previous values afterwards. Serialized because these vars are process-global.
    fn with_env(no_log: Option<&str>, config_dir: Option<&std::path::Path>, body: impl FnOnce()) {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev_no = std::env::var_os("SCR1B3_NO_ACTION_LOG");
        let prev_cfg = std::env::var_os("SCR1B3_CONFIG_DIR");
        match no_log {
            Some(v) => std::env::set_var("SCR1B3_NO_ACTION_LOG", v),
            None => std::env::remove_var("SCR1B3_NO_ACTION_LOG"),
        }
        match config_dir {
            Some(d) => std::env::set_var("SCR1B3_CONFIG_DIR", d),
            None => std::env::remove_var("SCR1B3_CONFIG_DIR"),
        }
        body();
        match prev_no {
            Some(v) => std::env::set_var("SCR1B3_NO_ACTION_LOG", v),
            None => std::env::remove_var("SCR1B3_NO_ACTION_LOG"),
        }
        match prev_cfg {
            Some(v) => std::env::set_var("SCR1B3_CONFIG_DIR", v),
            None => std::env::remove_var("SCR1B3_CONFIG_DIR"),
        }
    }

    #[test]
    fn append_line_writes_one_tab_separated_record_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session-actions.log");
        append_line(&p, "tab", "switch -> notes.txt");
        append_line(&p, "error", "could not save settings");
        let body = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "one record per call");
        assert!(lines[0].ends_with("\ttab\tswitch -> notes.txt"));
        assert!(lines[1].ends_with("\terror\tcould not save settings"));
    }

    #[test]
    fn multiline_detail_stays_a_single_record() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        append_line(&p, "x", "line one\nline two\twith tab");
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body.lines().count(),
            1,
            "a multiline/tabbed detail must not split into several records"
        );
    }

    #[test]
    fn oversized_log_rotates_to_dot_one() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session-actions.log");
        let rotated = {
            let mut s = p.as_os_str().to_owned();
            s.push(".1");
            std::path::PathBuf::from(s)
        };
        // Seed a log larger than our tiny test threshold.
        std::fs::write(&p, "x".repeat(2048)).unwrap();
        // Below threshold → no rotation.
        rotate_if_oversized(&p, 4096);
        assert!(p.exists());
        assert!(!rotated.exists());
        // Above threshold → rotate to `<name>.1`; live log is gone (recreated
        // on the next append).
        rotate_if_oversized(&p, 1024);
        assert!(rotated.exists(), "rotated generation must exist");
        assert!(!p.exists(), "live log must be cleared after rotation");
        assert_eq!(std::fs::read_to_string(&rotated).unwrap().len(), 2048);
    }

    #[test]
    fn append_line_triggers_rotation_at_cap() {
        // End-to-end: a pre-existing oversized log is rotated by the next
        // append, and the new live log holds only the fresh record.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        std::fs::write(&p, "y".repeat(super::ACTION_LOG_MAX_BYTES as usize + 1)).unwrap();
        append_line(&p, "tab", "after rotation");
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(body.lines().count(), 1, "live log restarted after rotation");
        assert!(body.contains("after rotation"));
    }

    #[test]
    fn a_log_comfortably_under_the_cap_is_not_rotated() {
        // Pins the MAGNITUDE of ACTION_LOG_MAX_BYTES, not just its sign. The
        // rotation-at-cap test above writes `cap + 1`, so it passes for ANY
        // positive cap and cannot see the cap shrink. Here a 2 MiB log — well
        // under the documented 5 MiB cap, but larger than a shrunk-cap mutant
        // (`5 * 1024 * 1024` -> `5 + 1024 * 1024` = ~1 MiB, or `5 * 1024 + 1024`
        // = 6 KiB) — must NOT rotate: the original content survives the append.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        let rotated = {
            let mut s = p.as_os_str().to_owned();
            s.push(".1");
            std::path::PathBuf::from(s)
        };
        let under_cap = 2 * 1024 * 1024; // 2 MiB < 5 MiB cap, > any shrunk-cap mutant
        assert!(
            (under_cap as u64) < ACTION_LOG_MAX_BYTES,
            "test fixture must sit under the real cap"
        );
        std::fs::write(&p, "z".repeat(under_cap)).unwrap();
        append_line(&p, "tab", "still under the cap");
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(
            body.len() > under_cap,
            "a log under the {ACTION_LOG_MAX_BYTES}-byte cap must not be rotated away"
        );
        assert!(body.contains("still under the cap"));
        assert!(!rotated.exists(), "no rotation happens under the cap");
    }

    #[test]
    fn resolve_log_path_is_none_when_disabled_by_env() {
        // SCR1B3_NO_ACTION_LOG short-circuits to None even when a config dir IS
        // available — a mutant that drops that early return would leak a
        // Some(<config_dir>/session-actions.log) here.
        let dir = tempfile::tempdir().unwrap();
        with_env(Some("1"), Some(dir.path()), || {
            assert_eq!(
                resolve_log_path(),
                None,
                "the env opt-out must disable the log even with a config dir present"
            );
        });
    }

    #[test]
    fn resolve_log_path_points_into_the_config_dir_when_enabled() {
        // Enabled + a config dir => <config_dir>/session-actions.log. Kills the
        // `-> None` body mutant and any mutation of the config-dir join.
        let dir = tempfile::tempdir().unwrap();
        with_env(None, Some(dir.path()), || {
            assert_eq!(
                resolve_log_path(),
                Some(dir.path().join("session-actions.log")),
                "an enabled log resolves to <config_dir>/session-actions.log"
            );
        });
    }
}
