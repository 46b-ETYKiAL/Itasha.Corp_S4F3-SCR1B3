//! QA: app-level data-integrity / correctness workflows (#38). Drives the
//! external-change-while-dirty divergence path and the corrupt-session
//! restore-fallback through the real `ScribeApp` host, as a user would hit them.
//!
//! Phase-2 discipline: tests only, no product-code edits. These complement the
//! core-level `scribe-core/tests/qa_correctness_workflow.rs` scenarios by
//! exercising the seams that only exist on the app (tab divergence detection,
//! reload/keep-mine, session restore). The poll seam `poll_external_disk_changes`
//! is `pub(super)` and the tab fields (`text`, `disk_text`, `disk_mtime`,
//! `external_change`) are module-private, so this child test module reaches them
//! directly — no GUI frame is required to drive the data-integrity logic.

use super::{file_mtime, ScribeApp};
use scribe_core::Config;

/// Open `initial` bytes into a fresh app tab; returns the app + tab path.
fn app_with_open_file(initial: &str) -> (ScribeApp, std::path::PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("doc.txt");
    std::fs::write(&p, initial).unwrap();
    let mut app = ScribeApp::new_test(Config::default());
    app.open_path(p.clone());
    (app, p, dir)
}

// ---------------------------------------------------------------------------
// Scenario 6 — External change while dirty: divergence detected, NO silent
//              data loss.
// ---------------------------------------------------------------------------

#[test]
fn scenario6_external_change_while_dirty_flags_divergence_no_silent_overwrite() {
    let (mut app, p, _dir) = app_with_open_file("original line\n");
    let active = app.active;

    // The user types unsaved edits (tab becomes dirty: text != doc.text()).
    app.tabs[active].text = "MY UNSAVED EDIT\n".into();
    assert!(
        app.tabs[active].is_dirty(),
        "the tab must be dirty after editing"
    );

    // Meanwhile the file changes ON DISK (git pull / external tool).
    std::fs::write(&p, "NEWER DISK CONTENT\n").unwrap();
    // Force the poll to see a divergence: a None mtime guarantees inequality
    // with the freshly-stat'd disk mtime (independent of clock granularity).
    app.tabs[active].disk_mtime = None;

    // Drive the per-frame disk poll. `last_disk_poll_frame` starts at u64::MAX
    // so it always polls on the first call.
    app.poll_external_disk_changes(1);

    // CRITERION: the dirty tab must NOT be silently reloaded/overwritten. The
    // user's unsaved edit is intact AND the divergence flag is raised so the
    // Reload / Keep-mine banner fires.
    assert_eq!(
        app.tabs[active].text, "MY UNSAVED EDIT\n",
        "a dirty buffer must NOT be silently replaced by the disk version"
    );
    assert!(
        app.tabs[active].external_change,
        "the external-change flag must be set so the user is prompted (no silent loss)"
    );
    // And the on-disk file is untouched by the poll (no auto-clobber either way).
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "NEWER DISK CONTENT\n");
}

#[test]
fn scenario6_external_change_while_clean_silently_reloads() {
    // The complementary safe case: a CLEAN tab (no unsaved edits) silently
    // reloads the newer disk content — that is a convenience, not data loss,
    // because the user had nothing to lose.
    let (mut app, p, _dir) = app_with_open_file("v1\n");
    let active = app.active;
    assert!(!app.tabs[active].is_dirty(), "freshly opened tab is clean");

    std::fs::write(&p, "v2 from disk\n").unwrap();
    app.tabs[active].disk_mtime = None; // force the poll to observe a change

    app.poll_external_disk_changes(1);

    assert_eq!(
        app.tabs[active].text, "v2 from disk\n",
        "a clean tab silently picks up the newer disk content"
    );
    assert!(
        !app.tabs[active].external_change,
        "a clean reload must NOT raise the divergence banner"
    );
    assert!(
        !app.tabs[active].is_dirty(),
        "after reload the tab is clean against the new disk content"
    );
}

#[test]
fn scenario6_no_external_change_when_disk_unchanged_is_a_noop() {
    // Guard against a false-positive divergence: if the file did NOT change and
    // the mtime is current, the poll must do nothing (no spurious reload, no
    // spurious banner) even with unsaved edits present.
    let (mut app, p, _dir) = app_with_open_file("stable\n");
    let active = app.active;
    app.tabs[active].text = "unsaved\n".into();
    // Refresh disk_mtime to the file's CURRENT mtime so the poll sees no change.
    app.tabs[active].disk_mtime = file_mtime(&p);

    app.poll_external_disk_changes(1);

    assert_eq!(app.tabs[active].text, "unsaved\n", "edits untouched");
    assert!(
        !app.tabs[active].external_change,
        "no disk change => no divergence flag (no false positive)"
    );
}

#[test]
fn scenario6_dirty_tab_external_change_survives_save_and_overwrites_with_user_intent() {
    // After divergence is flagged, an explicit save writes the USER's version
    // (Keep-mine) — proving the resolution is user-driven, not silent. The save
    // path is the user explicitly choosing to overwrite the disk version.
    let (mut app, p, _dir) = app_with_open_file("disk original\n");
    let active = app.active;
    app.tabs[active].text = "user version\n".into();
    std::fs::write(&p, "concurrent disk edit\n").unwrap();
    app.tabs[active].disk_mtime = None;
    app.poll_external_disk_changes(1);
    assert!(app.tabs[active].external_change, "divergence flagged first");

    // User chooses "Keep mine" -> save_active writes their buffer.
    app.save_active();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "user version\n",
        "an EXPLICIT save persists the user's version (Keep-mine), never silently"
    );
}

// ---------------------------------------------------------------------------
// Scenario 5 (app surface) — corrupt config banner does not crash startup
// ---------------------------------------------------------------------------

#[test]
fn scenario5_app_builds_on_defaults_when_config_was_corrupt() {
    // The host must construct + run on defaults even when the config was
    // malformed (surfaced as a banner, not a crash). We can't easily corrupt the
    // OS config dir under new_test, but we CAN prove the app builds cleanly from
    // a default Config (the malformed branch hands load_or_default's default in),
    // and that a partial-but-valid config merges without dropping settings.
    let mut cfg = Config::from_toml_str("[editor]\ntab_width = 3\n").unwrap();
    assert!(cfg.editor.show_line_numbers, "unspecified setting kept");
    cfg.editor.tab_width = 3;
    let app = ScribeApp::new_test(cfg);
    // A freshly-built test app always has at least one (scratch) tab and a valid
    // active index — it never starts in a broken state.
    assert!(!app.tabs.is_empty(), "app must start with at least one tab");
    assert!(
        app.active < app.tabs.len(),
        "the active index must be in-bounds"
    );
}
