//! Coverage for the free functions + private methods in `app/mod.rs` that no
//! test reached: the launch restore-list (`session_file` / `load_session` /
//! `save_session`), the crash-spool drain, config hot-reload, the LSP-start
//! guards, plugin-command dispatch, and the `key_event` builder.
//!
//! ADR-0007 calls this file's residue "ordinary test backlog, NOT an exclusion"
//! — this is that backlog. What stays uncovered here is the genuinely
//! OS-bound surface: `rfd` dialogs, `arboard` clipboard, `open_in_file_manager`,
//! and the `eframe::App` trait impls that only a launched process enters.
#![allow(clippy::wildcard_imports)]
use super::*;

/// Serializes tests that mutate the process-global `SCR1B3_CONFIG_DIR`.
/// The restore-list + config-reload functions read the GLOBAL config dir
/// (not the instance one), so redirecting the env is the only way to drive
/// them — and cargo runs tests in parallel, so it must be exclusive.
static CONFIG_DIR_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_config_dir<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
    let _guard = CONFIG_DIR_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var_os("SCR1B3_CONFIG_DIR");
    std::env::set_var("SCR1B3_CONFIG_DIR", dir);
    let out = body();
    match prev {
        Some(v) => std::env::set_var("SCR1B3_CONFIG_DIR", v),
        None => std::env::remove_var("SCR1B3_CONFIG_DIR"),
    }
    out
}

fn temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "scr1b3-mod-logic/{}-{}-{}",
        tag,
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    ScribeApp::new_test(cfg)
}

// ---- the launch restore-list ----

#[test]
fn session_file_sits_in_the_config_dir() {
    let dir = temp_dir("session-file");
    let got = with_config_dir(&dir, session_file).expect("a config dir resolves to a session file");
    assert_eq!(got, dir.join("session.txt"));
}

#[test]
fn save_then_load_session_round_trips_the_open_paths() {
    let dir = temp_dir("session-rt");
    let paths = vec![PathBuf::from("/a/one.md"), PathBuf::from("/b/two.md")];

    let got = with_config_dir(&dir, || {
        save_session(&paths);
        load_session()
    });

    assert_eq!(got, paths, "what was saved is what comes back");
}

#[test]
fn load_session_is_empty_when_nothing_was_saved() {
    let dir = temp_dir("session-absent");
    let got = with_config_dir(&dir, load_session);
    assert!(
        got.is_empty(),
        "no file => nothing to restore, not an error"
    );
}

#[test]
fn load_session_skips_blank_lines() {
    // A hand-edited or partially-written list must not yield empty PathBufs,
    // which would then be "opened" as a tab pointing at nothing.
    let dir = temp_dir("session-blank");
    std::fs::write(dir.join("session.txt"), "/a/one.md\n\n   \n/b/two.md\n").unwrap();

    let got = with_config_dir(&dir, load_session);

    assert_eq!(
        got,
        vec![PathBuf::from("/a/one.md"), PathBuf::from("/b/two.md")],
        "blank and whitespace-only lines are dropped"
    );
}

#[test]
fn save_session_creates_the_config_dir_if_it_is_missing() {
    // Cold install: the dir does not exist yet. The restore list must still be
    // written rather than silently lost.
    let base = temp_dir("session-mkdir");
    let nested = base.join("not").join("created").join("yet");
    let paths = vec![PathBuf::from("/a/one.md")];

    let got = with_config_dir(&nested, || {
        save_session(&paths);
        load_session()
    });

    assert_eq!(got, paths, "the parent chain is created on demand");
}

#[test]
fn save_session_with_no_paths_writes_an_empty_list() {
    // Closing every tab must CLEAR the restore list, not leave the last one
    // behind to be reopened forever.
    let dir = temp_dir("session-empty");
    let got = with_config_dir(&dir, || {
        save_session(&[PathBuf::from("/a/one.md")]);
        save_session(&[]);
        load_session()
    });
    assert!(got.is_empty(), "an empty save clears the list");
}

// ---- session_signature ----

#[test]
fn session_signature_is_order_independent_and_ignores_untitled_tabs() {
    // The signature drives "did the open-file set change?", so it must not
    // churn just because tabs were reordered, and an untitled scratch buffer
    // has no path to record.
    let dir = temp_dir("sig");
    let a = dir.join("a.md");
    let b = dir.join("b.md");
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&b, "b").unwrap();

    let mut app1 = app();
    app1.open_path(a.clone());
    app1.open_path(b.clone());

    let mut app2 = app();
    app2.open_path(b);
    app2.open_path(a);

    assert_eq!(
        session_signature(&app1.tabs),
        session_signature(&app2.tabs),
        "the same files opened in a different order are the same session"
    );
    assert!(
        !session_signature(&app1.tabs).is_empty(),
        "file-backed tabs are recorded"
    );
    assert_eq!(
        session_signature(&app().tabs),
        "",
        "an untitled scratch tab has no path, so it contributes nothing"
    );
}

// ---- key_event ----

#[test]
fn key_event_builds_a_pressed_non_repeat_key() {
    // This drives egui's native undo/redo from the palette; a `pressed: false`
    // or `repeat: true` event would be ignored by TextEdit's command matching.
    let e = key_event(egui::Key::Z, egui::Modifiers::COMMAND);
    match e {
        egui::Event::Key {
            key,
            physical_key,
            pressed,
            repeat,
            modifiers,
        } => {
            assert_eq!(key, egui::Key::Z);
            assert!(physical_key.is_none(), "egui matches on the LOGICAL key");
            assert!(pressed, "a press, not a release");
            assert!(!repeat, "a fresh press, not a key-repeat");
            assert_eq!(modifiers, egui::Modifiers::COMMAND);
        }
        other => panic!("expected a Key event, got {other:?}"),
    }
}

// ---- path_to_uri ----

#[test]
fn path_to_uri_produces_a_file_uri_for_both_path_shapes() {
    // LSP wants URIs. A Windows drive path has no leading slash, so it needs
    // three; a POSIX path already starts with one and must not get four.
    assert_eq!(
        path_to_uri(Path::new(r"C:\src\main.rs")),
        "file:///C:/src/main.rs",
        "backslashes become forward slashes and the drive path gains file:///"
    );
    assert_eq!(
        path_to_uri(Path::new("/src/main.rs")),
        "file:///src/main.rs"
    );
}

// ---- drain_crash_spool ----

#[test]
fn crash_spool_drain_is_a_noop_when_reporting_is_off() {
    let mut app = app();
    app.config.reporting.crash_reports = crate::reporting::ReportingMode::Off;
    app.drain_crash_spool();
    assert!(
        !app.crash_consent.has_pending(),
        "an opted-OUT user must never be prompted about a crash report"
    );
}

#[test]
fn crash_spool_drain_queues_consent_when_set_to_ask() {
    // AskEachTime must load the spool so the consent dialog has something to
    // show; it must NOT send anything on its own.
    let mut app = app();
    app.config.reporting.crash_reports = crate::reporting::ReportingMode::AskEachTime;
    app.drain_crash_spool();
    // With an empty spool there is nothing pending — the point is that the
    // consent gate was wired to this app's config dir rather than the real one.
    assert!(!app.crash_consent.has_pending());
}

#[test]
fn crash_spool_drain_without_a_config_dir_is_a_noop() {
    let mut app = app();
    app.config_dir = None;
    app.config.reporting.crash_reports = crate::reporting::ReportingMode::Always;
    app.drain_crash_spool(); // must not panic or touch the real user dir
}

// ---- reload_config_from_disk ----

#[test]
fn config_reload_applies_an_external_edit() {
    // The fixture is serialized from a real Config so it carries the CURRENT
    // schema_version. A hand-written `[editor]` stub would deserialize as
    // schema_version 0 and the v0->v1 migration would re-assert the
    // experience-baseline toggles (show_minimap = true) right back over the
    // edit — correct behaviour, but it would make this test measure the
    // migration instead of the reload.
    let dir = temp_dir("reload-apply");
    let flipped = !Config::default().editor.show_minimap;
    let mut on_disk = Config::default();
    on_disk.editor.show_minimap = flipped;
    std::fs::write(dir.join("scr1b3.toml"), on_disk.to_toml_string()).unwrap();
    let mut app = app();
    let ctx = egui::Context::default();

    with_config_dir(&dir, || app.reload_config_from_disk(&ctx));

    assert_eq!(
        app.config.editor.show_minimap, flipped,
        "an external edit to the settings file is picked up"
    );
    assert_eq!(app.status, "config reloaded");
}

#[test]
fn config_reload_runs_the_schema_migration_on_a_pre_migration_file() {
    // The other side of the coin: a config predating the v0->v1 flip must have
    // the experience-baseline toggles re-asserted on load, or a good default
    // could never reach a user whose stored `false` sticks forever.
    let dir = temp_dir("reload-migrate");
    std::fs::write(
        dir.join("scr1b3.toml"),
        "[editor]\nshow_minimap = false\nshow_line_numbers = false\n",
    )
    .unwrap();
    let mut app = app();
    let ctx = egui::Context::default();

    with_config_dir(&dir, || app.reload_config_from_disk(&ctx));

    assert!(
        app.config.editor.show_minimap && app.config.editor.show_line_numbers,
        "a schema_version-0 file gets the v0->v1 baseline re-asserted"
    );
}

#[test]
fn config_reload_skips_when_nothing_changed() {
    // The watcher echoes our OWN save back at us. Re-applying it would reset
    // derived state (and re-theme) on every save.
    let dir = temp_dir("reload-same");
    let mut app = app();
    std::fs::write(dir.join("scr1b3.toml"), app.config.to_toml_string()).unwrap();
    app.status = "untouched".into();
    let ctx = egui::Context::default();

    with_config_dir(&dir, || app.reload_config_from_disk(&ctx));

    assert_eq!(
        app.status, "untouched",
        "an identical config must be skipped, not re-applied"
    );
}

#[test]
fn config_reload_keeps_the_previous_settings_when_the_file_is_broken() {
    // A typo in the settings file must NOT wipe the user's live settings back
    // to defaults — it must keep what is in memory and explain.
    let dir = temp_dir("reload-broken");
    std::fs::write(dir.join("scr1b3.toml"), "[editor]\nthis is not = = toml\n").unwrap();
    let mut app = app();
    app.config.editor.show_minimap = !Config::default().editor.show_minimap;
    let keep = app.config.clone();
    let ctx = egui::Context::default();

    with_config_dir(&dir, || app.reload_config_from_disk(&ctx));

    assert_eq!(
        app.config, keep,
        "a broken file must not clobber the settings already in use"
    );
    let toast = app.toast.expect("the failure is surfaced, not silent");
    assert!(
        toast.contains("previous settings are still in use"),
        "the toast must say the settings survived, got: {toast}"
    );
}

// ---- start_lsp_for_active guards ----

#[test]
fn start_lsp_explains_when_the_file_has_no_extension() {
    let mut app = app();
    let dir = temp_dir("lsp-nolang");
    let p = dir.join("no-extension");
    std::fs::write(&p, "x").unwrap();
    app.open_path(p);

    app.start_lsp_for_active();

    let toast = app.toast.expect("an actionable notice, not silence");
    assert!(
        toast.contains("file extension"),
        "the toast must tell the user what to DO, got: {toast}"
    );
}

#[test]
fn start_lsp_explains_when_the_buffer_was_never_saved() {
    // An untitled scratch buffer has no path for the server to open.
    let mut app = app();
    app.start_lsp_for_active();
    let toast = app.toast.expect("an actionable notice");
    assert!(toast.contains("Save the file first"), "got: {toast}");
}

// ---- run_plugin_command ----

#[test]
fn an_unknown_plugin_command_leaves_the_buffer_untouched_and_says_so() {
    let mut app = app();
    let active = app.active;
    app.tabs[active].set_text("my precious text".into());

    app.run_plugin_command("no-such-command");

    assert_eq!(
        app.tabs[active].text, "my precious text",
        "a failed plugin command must not eat the user's text"
    );
    let toast = app.toast.expect("the failure is surfaced");
    assert!(
        toast.contains("left unchanged"),
        "the toast must reassure that nothing was lost, got: {toast}"
    );
}

/// A test must never inherit a previous process's state.
///
/// `unique_test_config_dir` names dirs `scr1b3-test-{pid}-{seq}`, which is unique
/// among LIVE processes but not over time: PIDs recycle and these dirs are never
/// cleaned. A full-suite run really did fail this way — a stale dir already
/// pinned `goodplug`, so `approve_plugin_allows_first_contact_signed_key` was
/// handed a rotated key instead of first contact.
///
/// The loud failure was the lucky outcome. Inherited state does not reliably
/// fail a test; it makes the test cover something other than its name.
#[test]
fn a_stale_config_dir_is_wiped_before_a_test_gets_it() {
    let dir = std::env::temp_dir().join(format!("scr1b3-stale-probe-{}", std::process::id()));
    // Stand in for the dead process: leave a pinned key exactly where a real
    // stale dir carries one.
    let plugins = dir.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::write(plugins.join("pinned-keys.toml"), "goodplug = 'stale-key'").unwrap();
    assert!(
        plugins.join("pinned-keys.toml").exists(),
        "probe not staged"
    );

    let handed_back = ScribeApp::wiped(dir.clone());

    assert_eq!(
        handed_back, dir,
        "the dir handed back must be the one asked for"
    );
    assert!(
        !plugins.join("pinned-keys.toml").exists(),
        "a test would have inherited a pinned key from a dead process"
    );
    assert!(
        !dir.exists(),
        "the whole stale dir must be gone, not just emptied"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The common path: nothing to wipe is not an error.
#[test]
fn wiping_a_dir_that_was_never_created_is_fine() {
    let dir = std::env::temp_dir().join(format!("scr1b3-absent-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!dir.exists(), "probe must start absent");
    assert_eq!(
        ScribeApp::wiped(dir.clone()),
        dir,
        "NotFound must be ignored"
    );
}

/// The legacy paths-only restore must open ordinary files and refuse ones that
/// reach off this machine.
///
/// The in-diff mutation gate found `delete !` surviving on this guard — i.e.
/// inverting it into "skip the safe paths, open the unsafe ones" broke nothing,
/// because the only caller is gated on `watch_config` and could never run in a
/// test. `restore_legacy_tabs` takes the list as an argument for exactly this
/// reason.
///
/// The "safe file DOES open" half is what kills the inverted-guard mutant, and
/// it does so on every platform — unlike the reject half, which needs a
/// reachable UNC-classified path to discriminate (see `session_path_guard`).
#[test]
fn the_legacy_restore_opens_ordinary_files() {
    let dir = std::env::temp_dir().join(format!("scr1b3-legacy-ok-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.md");
    let b = dir.join("b.md");
    std::fs::write(&a, "first").unwrap();
    std::fs::write(&b, "second").unwrap();

    let tabs = ScribeApp::restore_legacy_tabs(vec![a, b]);

    assert_eq!(tabs.len(), 2, "both ordinary files must restore");
    assert_eq!(tabs[0].text, "first");
    assert_eq!(tabs[1].text, "second");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_legacy_restore_skips_a_vanished_file_without_losing_the_others() {
    let dir = std::env::temp_dir().join(format!("scr1b3-legacy-gone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let real = dir.join("real.md");
    std::fs::write(&real, "here").unwrap();
    let ghost = dir.join("ghost.md");

    let tabs = ScribeApp::restore_legacy_tabs(vec![ghost, real]);

    assert_eq!(
        tabs.len(),
        1,
        "the vanished entry is skipped, the real one still restores"
    );
    assert_eq!(tabs[0].text, "here");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The reject half. Unix-only for the same reason as the guard's own ordering
/// test: `//tmp/x` is UNC-classified AND reachable, so it is the only fixture
/// that can tell a working guard from a deleted one. An unreachable
/// `\attacker\share\x` would fail to open anyway and prove nothing.
#[cfg(unix)]
#[test]
fn the_legacy_restore_refuses_a_path_that_reaches_off_this_machine() {
    let dir = std::env::temp_dir().join(format!("scr1b3-legacy-remote-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let real = dir.join("notes.md");
    std::fs::write(&real, "openable").unwrap();
    let remote = std::path::PathBuf::from(format!("/{}", real.display()));
    assert!(
        crate::session_path_guard::is_unc_path(&remote),
        "fixture must be UNC-classified, else it proves nothing"
    );
    assert!(
        std::fs::read_to_string(&remote).is_ok(),
        "fixture must be READABLE, else the skip could come from the open failing"
    );

    let tabs = ScribeApp::restore_legacy_tabs(vec![remote]);

    assert!(
        tabs.is_empty(),
        "a path that reaches off this machine must not be auto-opened"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
