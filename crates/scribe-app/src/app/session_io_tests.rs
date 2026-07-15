//! Coverage for `session_io.rs`: save, the hot-exit backup snapshot, restore
//! from the manifest, and external-disk-change polling.
//!
//! This is the code that owns the user's unsaved work across a crash, so its
//! failure modes are the expensive kind (lost notes, a note reopened twice and
//! silently diverging, an attacker-chosen path becoming a save target). All of
//! it is real file IO against temp dirs — nothing here needs a render loop.
//!
//! `save_as_active` is covered here too, which it never used to be. Per ADR-0007
//! it was excluded because it opens a native `rfd::FileDialog` and blocks on a
//! human — true of the dialog, but the exclusion had swallowed the whole
//! function, including everything decidable BEFORE the dialog and everything
//! done AFTER it, none of which needs a human. Mutation testing found that dead
//! zone: its `>=` bound check and its `!=` filter loop could both be inverted
//! with every test green, and later the whole body could be replaced with `()`.
//!
//! So it is split — `save_as_prompt` decides, `commit_save_as` writes — and the
//! dialog's ANSWER is injected via `dialogs::test_hooks::set_next_save_path`.
//! Stubbing the OS boundary is not the same as supplying the wire: the tests run
//! the real prompt and the real commit, and only the OS's reply is faked. The
//! exclusion is now the `rfd` call itself and nothing else.
#![allow(clippy::wildcard_imports)]
use super::*;

/// A process-unique temp dir per call (parallel tests must not share paths).
fn temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "scr1b3-session-io-tests/{}-{}-{}",
        tag,
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Serializes tests that mutate the process-global `SCR1B3_CONFIG_DIR`.
/// `restore_tabs_from_manifest` is an associated fn that reads the GLOBAL
/// `Config::config_dir()` (not the instance `config_dir`), so redirecting the
/// env is the only way to drive it — and cargo runs tests in parallel, so the
/// redirect must be exclusive or two tests clobber each other.
static CONFIG_DIR_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_config_dir<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
    // A poisoned lock only means some test panicked; the guard's job is mutual
    // exclusion, not protecting data, so recover rather than cascade failures.
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

/// An app with one tab opened from a real file on disk.
fn app_with_file(name: &str, text: &str) -> (ScribeApp, PathBuf) {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    let mut app = ScribeApp::new_test(cfg);
    let path = temp_dir("file").join(name);
    std::fs::write(&path, text).unwrap();
    app.open_path(path.clone());
    (app, path)
}

// ---- save_active ----

#[test]
fn save_active_writes_the_buffer_and_refreshes_the_disk_baseline() {
    let (mut app, path) = app_with_file("n.md", "before");
    let active = app.active;
    app.tabs[active].set_text("after".into());
    assert!(app.tabs[active].is_dirty(), "edited buffer is dirty");

    app.save_active();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "after");
    assert!(
        app.status.contains("saved"),
        "the save must be reported, got: {:?}",
        app.status
    );
    // F-022: the disk baseline must advance, or the next poll false-positives
    // an "external change" against our own write.
    assert_eq!(app.tabs[active].disk_text, "after");
    assert!(app.tabs[active].disk_mtime.is_some(), "mtime recaptured");
    assert!(!app.tabs[active].is_dirty(), "a saved buffer is clean");
}

// ---- plugin on_save hooks ----
//
// `fire_save_hooks` had NO test at all: cargo-mutants could replace the whole
// function with `()` and every test stayed green. It lets a plugin rewrite the
// user's buffer on every save, which is about as consequential as this file
// gets. These drive it through `save_active`, the real caller, rather than
// reaching for the private fn — the wiring is the part worth protecting.

/// An app with one file-backed tab plus `script` loaded as a plugin.
fn app_with_save_hook(name: &str, text: &str, script: &str) -> (ScribeApp, PathBuf) {
    let (mut app, path) = app_with_file(name, text);
    app.plugins
        .load_script("test-hook", script)
        .expect("script");
    (app, path)
}

#[test]
fn a_save_hook_transform_is_applied_to_the_live_buffer() {
    let (mut app, _path) = app_with_save_hook(
        "hook.md",
        "before",
        r#"
        fn on_save() { set_buffer_text(buffer_text().to_upper()); }
        on_event("save", "on_save");
        "#,
    );
    let active = app.active;
    app.tabs[active].set_text("hello".into());

    app.save_active();

    assert_eq!(
        app.tabs[active].text, "HELLO",
        "the on_save hook's transform must land in the live buffer"
    );
}

#[test]
fn a_save_hook_that_changes_nothing_does_not_touch_the_buffer() {
    // The `pctx.text != text` guard exists so a hook that only notifies does not
    // fabricate an edit. `set_text` bumps `edit_gen` (and drops the rope cache),
    // so an unchanged generation is the proof that it was never called.
    let (mut app, _path) = app_with_save_hook(
        "noop.md",
        "before",
        r#"
        fn on_save() { notify("looked, touched nothing"); }
        on_event("save", "on_save");
        "#,
    );
    let active = app.active;
    app.tabs[active].set_text("hello".into());
    let gen_before = app.tabs[active].edit_gen;

    app.save_active();

    assert_eq!(app.tabs[active].text, "hello", "text is unchanged");
    assert_eq!(
        app.tabs[active].edit_gen, gen_before,
        "a hook that changes nothing must not re-set the buffer"
    );
}

#[test]
fn the_last_save_hook_notification_becomes_the_status_line() {
    let (mut app, _path) = app_with_save_hook(
        "note.md",
        "before",
        r#"
        fn on_save() { notify("first"); notify("formatted by plugin"); }
        on_event("save", "on_save");
        "#,
    );
    app.save_active();

    assert_eq!(
        app.status, "formatted by plugin",
        "the LAST notification wins, and it overrides the `saved …` status"
    );
}

#[test]
fn save_active_applies_the_opt_in_hygiene_and_reflects_it_into_the_buffer() {
    // trim-trailing-whitespace + final-newline are opt-in, and the CLEANED text
    // must land in the live buffer too — otherwise the buffer and the file
    // disagree the instant after a save and the tab re-dirties itself.
    let (mut app, path) = app_with_file("h.md", "x");
    let active = app.active;
    app.config.editor.trim_trailing_whitespace_on_save = true;
    app.config.editor.final_newline_on_save = true;
    app.tabs[active].set_text("keep   \ntrailing   ".into());

    app.save_active();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep\ntrailing\n");
    assert_eq!(
        app.tabs[active].text, "keep\ntrailing\n",
        "the cleaned text must be reflected back into the live buffer"
    );
    assert!(
        !app.tabs[active].is_dirty(),
        "buffer and file agree => clean"
    );
}

#[test]
fn save_active_leaves_the_text_alone_when_hygiene_is_off() {
    // The default: what the user typed is what is written, byte for byte.
    let (mut app, path) = app_with_file("h.md", "x");
    let active = app.active;
    app.config.editor.trim_trailing_whitespace_on_save = false;
    app.config.editor.final_newline_on_save = false;
    app.tabs[active].set_text("keep   \nno newline".into());

    app.save_active();

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "keep   \nno newline",
        "hygiene off must not touch the bytes"
    );
}

#[test]
fn save_active_out_of_range_is_a_noop() {
    let (mut app, _) = app_with_file("n.md", "x");
    app.active = 999;
    app.save_active(); // must not panic on the tabs[active] index
}

// ---- Save-As: everything except the dialog ----

use scribe_core::config::DefaultSaveFormat;

#[test]
fn save_as_prompt_is_none_without_an_active_tab() {
    let (mut app, _) = app_with_file("n.md", "x");
    app.active = 999;
    assert!(
        app.save_as_prompt().is_none(),
        "no active tab means nothing to ask about — and tabs[999] must not be indexed"
    );
}

#[test]
fn save_as_prompt_suggests_the_current_stem_in_the_configured_format() {
    let (mut app, _) = app_with_file("notes.txt", "x");
    app.config.integration.default_save_format = DefaultSaveFormat::Markdown;

    let p = app.save_as_prompt().expect("an active tab");

    assert_eq!(
        p.suggested, "notes.md",
        "the stem is kept, the configured format supplies the extension"
    );
    assert_eq!(p.fmt, DefaultSaveFormat::Markdown);
}

#[test]
fn save_as_prompt_offers_the_configured_format_first_and_every_other_exactly_once() {
    // The `other != fmt` loop exists so the configured format is not listed
    // twice. Inverting it duplicated the configured format AND dropped every
    // other one, with nothing to notice.
    let (mut app, _) = app_with_file("n.md", "x");
    app.config.integration.default_save_format = DefaultSaveFormat::PlainText;

    let p = app.save_as_prompt().expect("an active tab");

    assert_eq!(
        p.filters[0],
        (
            DefaultSaveFormat::PlainText.filter_label(),
            DefaultSaveFormat::PlainText.extension()
        ),
        "the configured format must be the dialog's default filter"
    );
    assert_eq!(
        *p.filters.last().expect("filters are never empty"),
        ("All files", "*"),
        "the catch-all comes last"
    );
    for f in DefaultSaveFormat::ALL {
        let n = p
            .filters
            .iter()
            .filter(|(label, _)| *label == f.filter_label())
            .count();
        assert_eq!(n, 1, "{f:?} must be offered exactly once, got {n}");
    }
}

#[test]
fn commit_save_as_appends_the_configured_extension_to_a_bare_name() {
    let (mut app, _) = app_with_file("orig.md", "body");
    let dir = temp_dir("saveas-bare");
    let chosen = dir.join("notes"); // the user typed no extension

    app.commit_save_as(&chosen, DefaultSaveFormat::Markdown);

    assert_eq!(
        std::fs::read_to_string(dir.join("notes.md")).unwrap(),
        "body",
        "`notes` must be written as `notes.md`"
    );
    assert!(
        app.status.contains("notes.md"),
        "the save is reported with the real path, got: {:?}",
        app.status
    );
}

#[test]
fn commit_save_as_respects_an_extension_the_user_typed() {
    let (mut app, _) = app_with_file("orig.md", "body");
    let dir = temp_dir("saveas-explicit");
    let chosen = dir.join("notes.txt"); // deliberate, despite Markdown default

    app.commit_save_as(&chosen, DefaultSaveFormat::Markdown);

    assert!(chosen.exists(), "the user's chosen name is used verbatim");
    assert!(
        !dir.join("notes.txt.md").exists(),
        "the default extension must NOT be stacked onto an explicit one"
    );
}

#[test]
fn save_as_active_runs_prompt_then_dialog_then_commit() {
    // The whole flow, for real: the REAL save_as_prompt decides the name and
    // filters, the dialog's ANSWER is injected (that is the OS boundary, and the
    // only part a test cannot drive), and the REAL commit_save_as writes. Until
    // this existed, `replace save_as_active with ()` was MISSED — a fn nothing
    // can call is a fn whose body can be deleted with the suite still green.
    let (mut app, _) = app_with_file("orig.md", "on disk");
    let active = app.active;
    app.tabs[active].set_text("edited".into());

    let dir = temp_dir("save-as-active");
    // No extension: proves commit_save_as's ensure_extension really ran.
    super::dialogs::test_hooks::set_next_save_path(dir.join("picked"));

    app.save_as_active();

    assert_eq!(
        std::fs::read_to_string(dir.join("picked.md")).unwrap(),
        "edited",
        "Save-As must write the buffer to the picked path, extension appended"
    );
    assert!(
        !app.tabs[active].is_dirty(),
        "a completed Save-As leaves the buffer clean"
    );
}

#[test]
fn save_as_active_writes_nothing_when_the_user_cancels() {
    // Nothing injected => the dialog reports cancel. The buffer must be left
    // exactly as it was: no write, and still dirty.
    let (mut app, path) = app_with_file("orig.md", "on disk");
    let active = app.active;
    app.tabs[active].set_text("edited".into());

    app.save_as_active();

    assert!(
        app.tabs[active].is_dirty(),
        "a cancelled Save-As must NOT mark the buffer saved"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "on disk",
        "a cancelled Save-As must not touch the original file either"
    );
}

#[test]
fn commit_save_as_leaves_the_buffer_clean() {
    let (mut app, _) = app_with_file("orig.md", "on disk");
    let active = app.active;
    app.tabs[active].set_text("edited".into());
    assert!(app.tabs[active].is_dirty(), "precondition: dirty");

    let dir = temp_dir("saveas-clean");
    app.commit_save_as(&dir.join("out.md"), DefaultSaveFormat::Markdown);

    assert_eq!(
        std::fs::read_to_string(dir.join("out.md")).unwrap(),
        "edited"
    );
    assert!(
        !app.tabs[active].is_dirty(),
        "a saved buffer is clean — the change bar must flip from unsaved to saved"
    );
}

// ---- snapshot_session_backups (hot exit) ----

#[test]
fn snapshot_backs_up_dirty_and_untitled_content_but_not_clean_tabs() {
    use scribe_core::session;
    let (mut app, _) = app_with_file("dirty.md", "on disk");
    app.tabs[app.active].set_text("UNSAVED EDIT".into());
    // A second, clean, file-backed tab.
    let clean = temp_dir("clean").join("clean.md");
    std::fs::write(&clean, "clean content").unwrap();
    app.open_path(clean);

    app.snapshot_session_backups();

    let dir = app.config_dir.clone().expect("new_test sets a config dir");
    let manifest = session::load_manifest(&dir).expect("a manifest is written");
    // Every open tab is recorded — including the empty untitled scratch tab the
    // app starts with. Look entries up by path rather than position.
    let find = |needle: &str| {
        manifest
            .tabs
            .iter()
            .find(|t| t.path.as_deref().is_some_and(|p| p.ends_with(needle)))
            .unwrap_or_else(|| panic!("{needle} must be in the manifest"))
    };

    let dirty = find("dirty.md");
    assert!(dirty.dirty, "the edited tab is recorded dirty");
    let name = dirty.backup.as_ref().expect("dirty content is backed up");
    assert_eq!(
        session::read_backup(&session::backup_dir(&dir), name).unwrap(),
        "UNSAVED EDIT",
        "the backup holds the UNSAVED text, not the on-disk text"
    );

    let clean_snap = find("clean.md");
    assert!(!clean_snap.dirty);
    assert!(
        clean_snap.backup.is_none(),
        "a clean tab is recorded by path only — backing it up would duplicate \
         the file for nothing"
    );
    assert!(
        app.last_backup_at.is_some(),
        "the snapshot timestamp is set"
    );
}

#[test]
fn snapshot_backs_up_an_untitled_tab_with_content() {
    use scribe_core::session;
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    let mut app = ScribeApp::new_test(cfg);
    // The default untitled scratch tab, with something typed into it: no path,
    // so a crash would lose it entirely if it were not backed up.
    app.tabs[app.active].set_text("scratch note".into());

    app.snapshot_session_backups();

    let dir = app.config_dir.clone().unwrap();
    let manifest = session::load_manifest(&dir).unwrap();
    let snap = &manifest.tabs[app.active];
    assert!(snap.path.is_none(), "untitled => no path");
    let name = snap
        .backup
        .as_ref()
        .expect("untitled-with-content MUST be backed up — nothing else holds it");
    assert_eq!(
        session::read_backup(&session::backup_dir(&dir), name).unwrap(),
        "scratch note"
    );
}

#[test]
fn snapshot_skips_an_empty_untitled_tab() {
    use scribe_core::session;
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    let mut app = ScribeApp::new_test(cfg);

    app.snapshot_session_backups();

    let dir = app.config_dir.clone().unwrap();
    let manifest = session::load_manifest(&dir).unwrap();
    assert!(
        manifest.tabs[0].backup.is_none(),
        "an empty untitled tab has nothing to recover — no backup file"
    );
}

#[test]
fn snapshot_without_a_config_dir_is_a_noop() {
    // The guard that keeps a test (or a config-dir-less environment) from
    // writing into the real user session store.
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    let mut app = ScribeApp::new_test(cfg);
    app.config_dir = None;
    app.tabs[app.active].set_text("must not be persisted".into());

    app.snapshot_session_backups();

    assert!(
        app.last_backup_at.is_none(),
        "no config dir => nothing was snapshotted"
    );
}

#[test]
fn snapshot_prunes_a_backup_left_by_a_closed_tab() {
    use scribe_core::session;
    let (mut app, _) = app_with_file("keep.md", "x");
    app.tabs[app.active].set_text("edited".into());
    app.snapshot_session_backups();

    let dir = app.config_dir.clone().unwrap();
    let bdir = session::backup_dir(&dir);
    // Plant an orphan: a backup file no live tab refers to (what a closed tab
    // leaves behind). Without pruning these accumulate forever.
    session::write_backup(&bdir, "orphan-999.bak", "stale").unwrap();
    assert!(bdir.join("orphan-999.bak").exists());

    app.snapshot_session_backups();

    assert!(
        !bdir.join("orphan-999.bak").exists(),
        "a backup no manifest entry references must be pruned"
    );
}

// ---- restore_tabs_from_manifest ----

/// Write a manifest + backups into `dir` and restore from it.
fn restore_from(
    dir: &Path,
    snaps: Vec<scribe_core::session::TabSnapshot>,
    restore_session: bool,
) -> Option<(Vec<EditorTab>, usize)> {
    restore_from_active(dir, snaps, restore_session, 0)
}

/// `restore_from` with control over the manifest's persisted active index.
fn restore_from_active(
    dir: &Path,
    snaps: Vec<scribe_core::session::TabSnapshot>,
    restore_session: bool,
    active: usize,
) -> Option<(Vec<EditorTab>, usize)> {
    use scribe_core::session;
    let manifest = session::SessionManifest::new(snaps, active);
    session::save_manifest(dir, &manifest).unwrap();
    with_config_dir(dir, || {
        ScribeApp::restore_tabs_from_manifest(restore_session)
    })
}

fn snap(
    path: Option<String>,
    dirty: bool,
    backup: Option<String>,
) -> scribe_core::session::TabSnapshot {
    scribe_core::session::TabSnapshot {
        path,
        dirty,
        backup,
        cursor: 0,
    }
}

#[test]
fn restore_returns_none_without_a_manifest() {
    let dir = temp_dir("no-manifest");
    let got = with_config_dir(&dir, || ScribeApp::restore_tabs_from_manifest(true));
    assert!(got.is_none(), "no manifest => nothing to restore");
}

#[test]
fn restore_recovers_unsaved_content_even_with_restore_session_off() {
    // The two features are separate: "restore session" (reopen my files) is OFF,
    // but unsaved scratch work must STILL come back — losing it is the failure
    // this whole subsystem exists to prevent.
    use scribe_core::session;
    let dir = temp_dir("restore-unsaved");
    let file = dir.join("note.md");
    std::fs::write(&file, "on disk").unwrap();
    session::write_backup(&session::backup_dir(&dir), "b0.bak", "UNSAVED").unwrap();

    let (tabs, _) = restore_from(
        &dir,
        vec![snap(
            Some(file.display().to_string()),
            true,
            Some("b0.bak".into()),
        )],
        false,
    )
    .expect("unsaved content restores regardless of restore_session");

    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].text, "UNSAVED", "the backup content wins over disk");
    assert!(
        tabs[0].is_dirty(),
        "restored unsaved content is still dirty"
    );
}

#[test]
fn restore_session_off_does_not_reopen_clean_files() {
    // The toggle is authoritative: with it off, a clean file-backed tab from the
    // last session must NOT be reopened. (It used to be reopened anyway, which
    // made the setting a no-op.)
    let dir = temp_dir("restore-clean-off");
    let file = dir.join("clean.md");
    std::fs::write(&file, "clean").unwrap();

    let got = restore_from(
        &dir,
        vec![snap(Some(file.display().to_string()), false, None)],
        false,
    );
    assert!(
        got.is_none(),
        "restore_session off => previously-open clean files stay closed"
    );
}

#[test]
fn restore_session_on_reopens_clean_files_from_disk() {
    let dir = temp_dir("restore-clean-on");
    let file = dir.join("clean.md");
    std::fs::write(&file, "clean from disk").unwrap();

    let (tabs, active) = restore_from(
        &dir,
        vec![snap(Some(file.display().to_string()), false, None)],
        true,
    )
    .expect("restore_session on => the file reopens");
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].text, "clean from disk");
    assert_eq!(active, 0);
}

#[test]
fn restore_collapses_two_manifest_entries_for_one_file_into_one_tab() {
    // A prior session could open one file twice; without dedup the duplicate
    // COMPOUNDS every restart and the two copies silently diverge.
    use scribe_core::session;
    let dir = temp_dir("restore-dedup");
    let file = dir.join("dup.md");
    std::fs::write(&file, "on disk").unwrap();
    session::write_backup(&session::backup_dir(&dir), "d0.bak", "unsaved copy").unwrap();
    let p = file.display().to_string();

    let (tabs, _) = restore_from(
        &dir,
        vec![
            snap(Some(p.clone()), true, Some("d0.bak".into())),
            snap(Some(p), false, None),
        ],
        true,
    )
    .expect("restores");

    assert_eq!(
        tabs.len(),
        1,
        "one file must never restore into two tabs, got {} tabs",
        tabs.len()
    );
}

#[test]
fn restore_remaps_the_active_index_when_dedup_collapses_entries() {
    // Two manifest entries for ONE file collapse into a single tab. A persisted
    // active of 1 pointed at the SECOND entry, so it has to follow that entry to
    // the tab it merged into (0) rather than dangle past the end.
    let dir = temp_dir("restore-active-remap");
    let file = dir.join("dup.md");
    std::fs::write(&file, "on disk").unwrap();
    let p = file.display().to_string();
    scribe_core::session::write_backup(
        &scribe_core::session::backup_dir(&dir),
        "d0.bak",
        "unsaved",
    )
    .unwrap();

    let (tabs, active) = restore_from_active(
        &dir,
        vec![
            snap(Some(p.clone()), true, Some("d0.bak".into())),
            snap(Some(p), false, None),
        ],
        true,
        1,
    )
    .expect("restores");

    assert_eq!(tabs.len(), 1, "one file, one tab");
    assert_eq!(
        active, 0,
        "the active pointer follows its entry through dedup"
    );
}

#[test]
fn restore_falls_back_to_the_first_tab_when_the_manifest_active_is_out_of_range() {
    // `session.json` is user-writable, so `active` is untrusted input. An index
    // naming no entry must land on the first tab, never past the end.
    let dir = temp_dir("restore-active-bogus");
    let file = dir.join("one.md");
    std::fs::write(&file, "on disk").unwrap();

    let (tabs, active) = restore_from_active(
        &dir,
        vec![snap(Some(file.display().to_string()), false, None)],
        true,
        999,
    )
    .expect("restores");

    assert_eq!(tabs.len(), 1);
    assert_eq!(
        active, 0,
        "a bogus active index falls back to the first tab"
    );
}

#[test]
fn restore_skips_a_tampered_unc_path_but_keeps_its_unsaved_content() {
    // S-04: `session.json` is user-writable. A tampered UNC path must never be
    // auto-opened (SMB/NTLM credential leak) — but the user's unsaved content is
    // still recovered, as a PATHLESS scratch buffer so the attacker-chosen path
    // can't become a silent save target.
    use scribe_core::session;
    let dir = temp_dir("restore-unc");
    session::write_backup(&session::backup_dir(&dir), "u0.bak", "my work").unwrap();

    let (tabs, _) = restore_from(
        &dir,
        vec![snap(
            Some(r"\\attacker\share\evil.md".into()),
            true,
            Some("u0.bak".into()),
        )],
        true,
    )
    .expect("the content still restores");

    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].text, "my work", "unsaved work is never lost");
    assert!(
        tabs[0].doc.path().is_none(),
        "the untrusted path MUST be stripped: it must not become a save target"
    );
}

#[test]
fn restore_skips_a_clean_tab_whose_path_is_untrusted() {
    // Same guard, no backup to save: an untrusted path with nothing to recover
    // yields no tab at all.
    let dir = temp_dir("restore-unc-clean");
    let got = restore_from(
        &dir,
        vec![snap(Some(r"\\attacker\share\evil.md".into()), false, None)],
        true,
    );
    assert!(got.is_none(), "a UNC path is never auto-opened");
}

#[test]
fn restore_falls_back_to_disk_when_the_backup_is_unreadable() {
    // The manifest names a backup that isn't there (partial write / pruned).
    // With restore_session on, the file still reopens from disk rather than
    // vanishing.
    let dir = temp_dir("restore-badbackup");
    let file = dir.join("f.md");
    std::fs::write(&file, "disk copy").unwrap();

    let (tabs, _) = restore_from(
        &dir,
        vec![snap(
            Some(file.display().to_string()),
            true,
            Some("missing.bak".into()),
        )],
        true,
    )
    .expect("falls back to the file on disk");
    assert_eq!(tabs[0].text, "disk copy");
}

#[test]
fn an_unreadable_backup_does_not_reopen_from_disk_when_restore_session_is_off() {
    // The OFF mirror of the test above. The fall-through is gated on
    // `restore_session`, and only the ON side was ever asserted — so forcing
    // that guard to `true` changed nothing any test could see.
    let dir = temp_dir("restore-badbackup-off");
    let file = dir.join("f.md");
    std::fs::write(&file, "disk copy").unwrap();

    let got = restore_from(
        &dir,
        vec![snap(
            Some(file.display().to_string()),
            true,
            Some("missing.bak".into()),
        )],
        false,
    );
    assert!(
        got.is_none(),
        "restore_session is OFF: an unreadable backup must not silently reopen \
         the file from disk"
    );
}

/// Point `link` at `target`, or fail with the reason.
///
/// Windows needs Developer Mode (or an elevated shell) for `symlink_file`. This
/// refuses to skip on failure: the symlink is the only fixture that exercises
/// the S-04 restore guard at all (see below), and a security test that silently
/// skips is indistinguishable from one that passes.
fn symlink_file_or_explain(target: &Path, link: &Path) {
    #[cfg(windows)]
    let r = std::os::windows::fs::symlink_file(target, link);
    #[cfg(unix)]
    let r = std::os::unix::fs::symlink(target, link);
    if let Err(e) = r {
        panic!(
            "could not create the symlink fixture {} -> {}: {e}\n\
             On Windows this needs Developer Mode (Settings ▸ System ▸ For \
             developers) or an elevated shell.",
            link.display(),
            target.display()
        );
    }
}

#[test]
fn restore_refuses_a_symlink_whose_target_escapes_the_prior_session_roots() {
    // The wiring test that `restore_skips_a_clean_tab_whose_path_is_untrusted`
    // only LOOKS like it is. That one declares `\\attacker\share\evil.md`, which
    // does not exist — so `EditorTab::from_path` fails to open it whether or not
    // the guard runs, and it passes with the guard REMOVED. cargo-mutants proved
    // that: forcing the `is_safe_restore_path` guard to `true` left all nine
    // restore tests green, and `allowed_roots` merely became an unused variable.
    //
    // A symlink is the discriminating fixture, because `Document::open` FOLLOWS
    // it and succeeds. The guard is then the only thing between a tampered
    // `session.json` and the file it aims at.
    let dir = temp_dir("restore-symlink-escape");
    let inside = dir.join("inside");
    std::fs::create_dir_all(&inside).unwrap();
    let outside = temp_dir("restore-symlink-escape-outside");
    let secret = outside.join("secret.md");
    std::fs::write(&secret, "top secret").unwrap();

    // The manifest declares ONLY the link, so the sole allowed root is
    // `inside/` — and the link's canonical target lands in `outside/`.
    let link = inside.join("notes.md");
    symlink_file_or_explain(&secret, &link);

    let got = restore_from(
        &dir,
        vec![snap(Some(link.display().to_string()), false, None)],
        true,
    );
    assert!(
        got.is_none(),
        "a symlink whose canonical target escapes the prior session roots must \
         never be auto-opened"
    );
}

#[test]
fn restore_never_makes_an_escaping_symlink_a_save_target() {
    // The same escape, but with unsaved work to recover: the content must
    // survive (we never lose the user's work) while the attacker-chosen path is
    // stripped — otherwise a later Ctrl+S writes straight through the link into
    // the outside file.
    use scribe_core::session;
    let dir = temp_dir("restore-symlink-target");
    let inside = dir.join("inside");
    std::fs::create_dir_all(&inside).unwrap();
    let outside = temp_dir("restore-symlink-target-outside");
    let secret = outside.join("secret.md");
    std::fs::write(&secret, "top secret").unwrap();
    let link = inside.join("notes.md");
    symlink_file_or_explain(&secret, &link);

    session::write_backup(&session::backup_dir(&dir), "u0.bak", "my work").unwrap();
    let (tabs, _) = restore_from(
        &dir,
        vec![snap(
            Some(link.display().to_string()),
            true,
            Some("u0.bak".into()),
        )],
        true,
    )
    .expect("the unsaved content still restores");

    assert_eq!(tabs[0].text, "my work", "unsaved work is never lost");
    assert!(
        tabs[0].doc.path().is_none(),
        "the escaping path MUST be stripped: saving through it would write to \
         the symlink's target outside every allowed root"
    );
    assert_eq!(
        std::fs::read_to_string(&secret).unwrap(),
        "top secret",
        "the outside file must be untouched by the restore"
    );
}

#[test]
fn restore_round_trips_a_real_snapshot() {
    // End-to-end: snapshot a live app, then restore from what it wrote. This is
    // the actual hot-exit path, and it catches a snapshot/restore disagreement
    // that testing either half alone would miss.
    let (mut app, _) = app_with_file("rt.md", "on disk");
    app.tabs[app.active].set_text("unsaved edit".into());
    app.snapshot_session_backups();
    let dir = app.config_dir.clone().unwrap();

    let (tabs, _) = with_config_dir(&dir, || ScribeApp::restore_tabs_from_manifest(true))
        .expect("what snapshot_session_backups wrote must be restorable");
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].text, "unsaved edit");
    assert!(tabs[0].is_dirty());
}

// ---- poll_external_disk_changes (F-022) ----

/// Force the next poll to actually run (the throttle is frame-based).
fn force_poll(app: &mut ScribeApp) {
    app.last_disk_poll_frame = u64::MAX; // "never polled" sentinel
    app.poll_external_disk_changes(0);
}

/// Write `text` to `path` with an mtime the poll is guaranteed to see as newer.
/// A same-second write can land on an unchanged mtime (filesystem timestamp
/// granularity), which would make these tests flaky rather than wrong.
fn write_with_newer_mtime(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(future).unwrap();
}

#[test]
fn poll_reloads_a_clean_tab_changed_on_disk() {
    let (mut app, path) = app_with_file("ext.md", "original");
    let active = app.active;
    assert!(!app.tabs[active].is_dirty());

    write_with_newer_mtime(&path, "changed by git pull");
    force_poll(&mut app);

    assert_eq!(
        app.tabs[active].text, "changed by git pull",
        "a clean tab silently picks up the external edit"
    );
    assert_eq!(app.tabs[active].disk_text, "changed by git pull");
    assert!(!app.tabs[active].external_change, "nothing to resolve");
    assert!(app.status.contains("reloaded"), "the reload is reported");
}

#[test]
fn poll_flags_a_dirty_tab_instead_of_clobbering_local_edits() {
    // The tab has unsaved edits AND the file changed underneath. Reloading would
    // destroy the user's work, so the poll must raise the persistent flag that
    // drives the Reload / Keep-mine banner.
    let (mut app, path) = app_with_file("ext.md", "original");
    let active = app.active;
    app.tabs[active].set_text("my local edits".into());

    write_with_newer_mtime(&path, "someone else's version");
    force_poll(&mut app);

    assert_eq!(
        app.tabs[active].text, "my local edits",
        "the user's unsaved edits MUST survive the poll"
    );
    assert!(
        app.tabs[active].external_change,
        "the conflict must be flagged for the banner"
    );
}

#[test]
fn poll_leaves_the_conflict_flag_set_until_it_is_resolved() {
    // The flag must not clear itself on the next poll: `disk_mtime` is
    // deliberately NOT refreshed on the warn path, so a second poll re-flags
    // rather than forgetting a conflict the user never resolved.
    let (mut app, path) = app_with_file("ext.md", "original");
    let active = app.active;
    app.tabs[active].set_text("my local edits".into());
    write_with_newer_mtime(&path, "theirs");

    force_poll(&mut app);
    force_poll(&mut app);

    assert!(
        app.tabs[active].external_change,
        "an unresolved conflict must stay flagged across polls"
    );
}

#[test]
fn poll_does_nothing_when_the_file_is_untouched() {
    let (mut app, _) = app_with_file("ext.md", "original");
    let active = app.active;
    force_poll(&mut app);
    assert_eq!(app.tabs[active].text, "original");
    assert!(!app.tabs[active].external_change);
}

#[test]
fn poll_is_throttled_between_intervals() {
    // The throttle is what keeps this off the per-frame hot path: it must NOT
    // stat every open file every frame.
    let (mut app, path) = app_with_file("ext.md", "original");
    let active = app.active;
    app.last_disk_poll_frame = 100;
    write_with_newer_mtime(&path, "changed");

    app.poll_external_disk_changes(101); // 1 frame later — under the interval
    assert_eq!(
        app.tabs[active].text, "original",
        "an in-interval frame must not poll"
    );

    app.poll_external_disk_changes(100 + DISK_POLL_INTERVAL_FRAMES);
    assert_eq!(
        app.tabs[active].text, "changed",
        "the change is picked up on the next poll tick"
    );
}

#[test]
fn poll_ignores_untitled_tabs() {
    // No path => nothing to stat; must not panic or misfire.
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[app.active].set_text("scratch".into());
    force_poll(&mut app);
    assert_eq!(app.tabs[app.active].text, "scratch");
}
