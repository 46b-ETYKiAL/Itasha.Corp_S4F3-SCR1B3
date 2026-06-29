//! Tested logs for the app I/O instrumentation — each asserts that an important
//! failure path emits the expected `tracing` event (level + message), using the
//! in-process `crate::log_capture` helper. Native app, no browser: these drive
//! the real app methods headless.

#![allow(clippy::wildcard_imports)]
use super::*;
use crate::log_capture;
use tracing::Level;

fn app_in(config_dir: &std::path::Path) -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.plugins.require_signed = true;
    let mut a = ScribeApp::new_test(cfg);
    a.config_dir = Some(config_dir.to_path_buf());
    a
}

fn write_plugin(dir: &std::path::Path, id: &str, pubkey: Option<&str>, sig: Option<&str>) {
    let pdir = dir.join("plugins").join(id);
    std::fs::create_dir_all(&pdir).unwrap();
    let mut toml = format!("id='{id}'\nname='{id}'\napi_version=1\nentry='main.rhai'\n");
    if let Some(pk) = pubkey {
        toml.push_str(&format!("author_pubkey='''{pk}'''\n"));
    }
    if let Some(s) = sig {
        toml.push_str(&format!("signature='''{s}'''\n"));
    }
    std::fs::write(pdir.join("plugin.toml"), toml).unwrap();
    std::fs::write(pdir.join("main.rhai"), "// noop").unwrap();
}

#[test]
fn hot_exit_backup_write_failure_logs_error() {
    // H1: when a dirty tab's content backup can't be written, the manifest will
    // record it as unrecoverable — that data-loss must be logged, not swallowed.
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path());
    // Force write_backup to fail: make the backup directory path a FILE.
    let bdir = scribe_core::session::backup_dir(dir.path());
    if let Some(parent) = bdir.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&bdir, "not a directory").unwrap();
    // An untitled tab with content triggers a content backup.
    app.tabs[0].set_text("unsaved work\n".to_string());

    let (_, logs) = log_capture::capture(|| app.snapshot_session_backups());
    assert!(
        logs.has(Level::ERROR, "hot-exit backup write failed"),
        "expected a data-loss error log, got: {:?}",
        logs.events()
    );
}

#[test]
fn approve_unsigned_plugin_logs_security_warn() {
    // A1: a security rejection (unsigned plugin in signed-only mode) must leave
    // an audit-trail log, not just a toast.
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path());
    write_plugin(dir.path(), "unsignedplug", None, None);
    app.pending_plugins.push("unsignedplug".to_string());

    let (_, logs) = log_capture::capture(|| app.approve_plugin("unsignedplug"));
    assert!(
        logs.has(Level::WARN, "unsigned in signed-only mode"),
        "expected a security audit warn, got: {:?}",
        logs.events()
    );
    // The rejection actually blocked it.
    assert!(!app.config.plugins.trusted.contains_key("unsignedplug"));
}

#[test]
fn approve_bad_signature_logs_security_warn() {
    // A1: a non-verifying signature must be logged as a security rejection.
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path());
    write_plugin(
        dir.path(),
        "tamperedplug",
        Some("RWQnotarealkeyAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        Some("untrusted comment: forged\nRWQbogusSignatureBBBBBBBBBBBBBBBBBBBBBBBB"),
    );
    app.pending_plugins.push("tamperedplug".to_string());

    let (_, logs) = log_capture::capture(|| app.approve_plugin("tamperedplug"));
    assert!(
        logs.has(Level::WARN, "signature did not verify"),
        "expected a signature-verify-fail warn, got: {:?}",
        logs.events()
    );
    assert!(!app.config.plugins.trusted.contains_key("tamperedplug"));
}

#[test]
fn save_config_failure_logs_warn() {
    // The settings save failing (here: the config dir is actually a file) must
    // be logged, not just toasted.
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, "x").unwrap(); // a FILE where a dir is expected
    let mut app = app_in(&blocker);

    let (_, logs) = log_capture::capture(|| app.save_config());
    assert!(
        logs.has(Level::WARN, "settings save failed"),
        "expected a settings-save-failed warn, got: {:?}",
        logs.events()
    );
}
