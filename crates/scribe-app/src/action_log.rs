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

/// The resolved log path, cached. `None` when disabled via env or when there is
/// no config dir (then `record` is a no-op).
fn path() -> Option<&'static PathBuf> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        if std::env::var_os("SCR1B3_NO_ACTION_LOG").is_some() {
            return None;
        }
        Config::config_dir().map(|d| d.join("session-actions.log"))
    })
    .as_ref()
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
    use super::{append_line, rotate_if_oversized};

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
}
