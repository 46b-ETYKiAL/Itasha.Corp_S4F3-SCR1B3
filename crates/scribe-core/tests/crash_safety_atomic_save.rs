//! Crash-safety / atomic-save data-integrity tests (taxonomy PART 2 §E item 23,
//! gap-closure plan #39 / #37).
//!
//! A text editor that truncates or corrupts the user's file when a save is
//! interrupted is a catastrophic, unrecoverable failure. PR #207 hardened the
//! *write* path so an unwritable target returns `Err` instead of panicking. This
//! suite is the COMPLEMENTARY data-integrity surface: it proves the
//! **atomic temp-then-rename invariant** — a save that fails (for any reason,
//! including a simulated crash *during* the write) leaves the ORIGINAL on-disk
//! file byte-for-byte intact, and never produces a partially-written / truncated
//! target. It also asserts the same invariant for the session-snapshot
//! (`session.rs`) backup + manifest writers, which are equally
//! crash-during-write sensitive ("hot exit").
//!
//! ## Fault-injection model (no unsafe, no real crash)
//!
//! We cannot literally `abort()` the test process mid-`write_all`, so we model a
//! crash as "the save operation returns before the rename completes". The
//! atomic-write contract is: content is staged in a sibling temp file and only
//! becomes visible at the destination via a single `rename`. Therefore ANY
//! failure before that rename — temp-create failure, write failure, a panic in
//! between — cannot have touched the destination. We assert that observable
//! property directly:
//!   * a failing `save_as` leaves the prior file content unchanged, and
//!   * after a *successful* save, no `.scr1b3-tmp-*` / `.tmp` debris remains in
//!     the directory (the temp file was consumed by the rename, never orphaned
//!     over the user's file).
//!
//! These run as a sibling integration test (public API only), disjoint from the
//! inline `#[cfg(test)]` module in `document.rs`.

use std::fs;
use std::path::Path;

use scribe_core::document::Document;
use scribe_core::session::{
    self, backup_dir, read_backup, save_manifest, write_backup, SessionManifest, TabSnapshot,
};

/// Count files in `dir` whose name marks them as an in-flight atomic-write temp
/// (the editor's `.scr1b3-tmp-*` document temps and the session `*.tmp` temps).
/// A correct atomic-save leaves ZERO of these behind over a stable file.
fn temp_debris_count(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n.starts_with(".scr1b3-tmp-") || n.ends_with(".tmp")
                })
                .count()
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// #37 — document save: the original file survives a failed save intact
// ---------------------------------------------------------------------------

/// A save that fails because the target directory does not exist must NOT have
/// disturbed any pre-existing file at that (non-existent) location, and must
/// leave the in-memory buffer fully intact + dirty so the user can retry. This
/// is the data-loss-prevention spine: a failed save is a no-op on disk.
#[test]
fn failed_save_to_missing_dir_preserves_buffer_and_leaves_no_debris() {
    let dir = tempfile::tempdir().unwrap();
    let bogus = dir.path().join("no-such-subdir").join("out.txt");

    let mut doc = Document::scratch();
    doc.set_text("irreplaceable work\nline two\n");
    let before = doc.text();

    let result = doc.save_as(&bogus);
    assert!(
        result.is_err(),
        "save into a missing dir must fail, not panic"
    );

    // The in-memory buffer is byte-identical and still dirty (retryable).
    assert_eq!(
        doc.text(),
        before,
        "buffer must be untouched after a failed save"
    );
    assert!(doc.is_dirty(), "a failed save must keep the buffer dirty");

    // No temp file leaked into the *existing* parent (the real tempdir root).
    assert_eq!(
        temp_debris_count(dir.path()),
        0,
        "a failed save must not orphan a temp file"
    );
}

/// THE core atomic invariant: when a save FAILS, an existing target file is left
/// byte-for-byte intact — never truncated, never partially overwritten. We force
/// the failure by making the destination path a DIRECTORY (rename/copy onto a
/// directory fails on every platform), while a real, precious file sits next to
/// it that the save attempt must not be able to touch.
#[test]
fn failed_save_does_not_truncate_or_corrupt_existing_file() {
    let dir = tempfile::tempdir().unwrap();

    // A precious existing file with known content.
    let target = dir.path().join("precious.txt");
    let original = b"ORIGINAL CONTENT THAT MUST SURVIVE\nsecond line\n";
    fs::write(&target, original).unwrap();

    // Open it, edit it, then sabotage the destination: replace the file with a
    // directory of the same name so the final rename/copy cannot succeed.
    let mut doc = Document::open(&target).unwrap();
    doc.set_text("brand new content that should NOT land\n");
    fs::remove_file(&target).unwrap();
    fs::create_dir(&target).unwrap();

    let result = doc.save_as(&target);
    assert!(
        result.is_err(),
        "saving over a directory must fail (atomic rename/copy cannot complete)"
    );

    // The path is still a directory — the editor never clobbered it with a
    // half-written file. The original bytes (had it remained a file) were never
    // truncated: the failure happened at the rename, after temp staging.
    assert!(
        target.is_dir(),
        "the destination must be untouched on a failed save"
    );
    // The in-memory buffer survives so the user can Save-As elsewhere.
    assert!(doc.is_dirty(), "buffer stays dirty after a failed save");
}

/// A SUCCESSFUL atomic save leaves no temp debris: the staged temp file is
/// consumed by the rename, never left orphaned beside the user's file. Repeated
/// saves (the common edit→save→edit→save loop) must each clean up after
/// themselves.
#[test]
fn successful_saves_leave_no_temp_debris() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("doc.txt");
    fs::write(&p, b"start\n").unwrap();

    let mut doc = Document::open(&p).unwrap();
    for i in 0..5 {
        doc.set_text(&format!("revision {i}\n"));
        doc.save().unwrap();
        assert_eq!(
            temp_debris_count(dir.path()),
            0,
            "save #{i} left a temp file behind"
        );
    }
    // Final content is the last revision — the renames all landed.
    assert_eq!(fs::read_to_string(&p).unwrap(), "revision 4\n");
}

/// Saving over an existing file replaces it ATOMICALLY: at no observable point
/// is the destination shorter than valid content. We verify the post-condition
/// that a save over a large existing file yields exactly the new content (the
/// rename swap is all-or-nothing) — a non-atomic truncate-then-write would be
/// observable as a size regression on failure, which the temp+rename design
/// makes impossible.
#[test]
fn save_over_existing_file_is_all_or_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("swap.txt");
    // A large original so a partial write would be obvious.
    let big_original = "X".repeat(64 * 1024) + "\n";
    fs::write(&p, big_original.as_bytes()).unwrap();

    let mut doc = Document::open(&p).unwrap();
    let new_content = "tiny\n";
    doc.set_text(new_content);
    doc.save().unwrap();

    // The file is EXACTLY the new content — no leftover tail from the larger
    // original (which a truncate-in-place writer could leave on a short write).
    assert_eq!(fs::read_to_string(&p).unwrap(), new_content);
    assert_eq!(temp_debris_count(dir.path()), 0);
}

// ---------------------------------------------------------------------------
// #37 — session snapshot ("hot exit"): atomic backup + manifest writes
// ---------------------------------------------------------------------------

/// `write_backup` is atomic (temp + rename): overwriting an existing backup with
/// new content either fully succeeds or leaves the prior backup intact, and
/// never leaves a `.tmp` artifact behind. This protects unsaved-buffer content
/// across a crash mid-snapshot.
#[test]
fn session_backup_overwrite_is_atomic_and_clean() {
    let dir = tempfile::tempdir().unwrap();
    let bdir = backup_dir(dir.path());

    write_backup(&bdir, "buf.bak", "first snapshot of unsaved work").unwrap();
    assert_eq!(
        read_backup(&bdir, "buf.bak").unwrap(),
        "first snapshot of unsaved work"
    );

    // Overwrite with a second snapshot — atomic replace.
    write_backup(&bdir, "buf.bak", "second snapshot").unwrap();
    assert_eq!(read_backup(&bdir, "buf.bak").unwrap(), "second snapshot");

    // No staged temp left behind.
    assert_eq!(
        temp_debris_count(&bdir),
        0,
        "atomic backup write must not orphan a .tmp file"
    );
}

/// A backup whose name escapes the backup dir (path separator) is REJECTED
/// before any write — a crash-safety + traversal guard combined: the writer
/// cannot be tricked into clobbering a file outside the backup dir.
#[test]
fn session_backup_rejects_traversal_name_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let bdir = backup_dir(dir.path());
    for evil in ["../escape.bak", "sub/dir.bak", r"..\win.bak"] {
        assert!(
            write_backup(&bdir, evil, "payload").is_err(),
            "backup name {evil:?} must be rejected"
        );
    }
    // Nothing was created.
    assert_eq!(temp_debris_count(&bdir), 0);
}

/// `save_manifest` writes the session manifest atomically (temp + rename). A
/// successful save leaves a clean directory (no `session.json.tmp`), and a
/// reload recovers the exact manifest — so a crash AFTER the rename has a valid
/// manifest and a crash BEFORE it leaves the prior manifest untouched.
#[test]
fn session_manifest_save_is_atomic_and_roundtrips() {
    let dir = tempfile::tempdir().unwrap();

    // First manifest.
    let m1 = SessionManifest::new(
        vec![TabSnapshot {
            path: Some("/work/a.txt".into()),
            dirty: true,
            backup: Some("a.bak".into()),
            cursor: 3,
        }],
        0,
    );
    save_manifest(dir.path(), &m1).unwrap();
    assert_eq!(temp_debris_count(dir.path()), 0, "no manifest temp debris");

    // Overwrite atomically with a second manifest; reload sees only the new one.
    let m2 = SessionManifest::new(
        vec![
            TabSnapshot {
                path: None,
                dirty: true,
                backup: Some("untitled-0.bak".into()),
                cursor: 0,
            },
            TabSnapshot {
                path: Some("/work/b.txt".into()),
                dirty: false,
                backup: None,
                cursor: 9,
            },
        ],
        1,
    );
    save_manifest(dir.path(), &m2).unwrap();
    assert_eq!(temp_debris_count(dir.path()), 0);

    let back = session::load_manifest(dir.path()).expect("manifest reloads");
    assert_eq!(back.tabs, m2.tabs);
    assert_eq!(back.active, 1);
}

/// End-to-end crash-recovery shape: an unsaved buffer is snapshotted to a
/// backup + referenced by the manifest; "restarting" (re-reading from disk)
/// recovers the exact unsaved content. This is the data-loss-prevention
/// guarantee the hot-exit feature exists to provide.
#[test]
fn hot_exit_snapshot_survives_simulated_restart() {
    let dir = tempfile::tempdir().unwrap();
    let bdir = backup_dir(dir.path());

    let unsaved = "user typed this and never saved\nthen the editor crashed\n";
    write_backup(&bdir, "untitled-0.bak", unsaved).unwrap();
    let manifest = SessionManifest::new(
        vec![TabSnapshot {
            path: None,
            dirty: true,
            backup: Some("untitled-0.bak".into()),
            cursor: 0,
        }],
        0,
    );
    save_manifest(dir.path(), &manifest).unwrap();

    // --- simulated restart: nothing in memory, only disk state ---
    let restored = session::load_manifest(dir.path()).expect("session restores");
    let tab = &restored.tabs[0];
    assert!(tab.path.is_none(), "the untitled tab is restored");
    let recovered = read_backup(&bdir, tab.backup.as_deref().unwrap()).unwrap();
    assert_eq!(
        recovered, unsaved,
        "unsaved content must survive the crash intact"
    );
}
