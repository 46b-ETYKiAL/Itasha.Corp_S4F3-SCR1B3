//! Session snapshot + unsaved-buffer backup — the "hot exit" feature.
//!
//! Persists the CONTENT of unsaved buffers (including never-saved "untitled"
//! scratch notes) so they survive a restart or crash WITHOUT an explicit save,
//! matching Notepad++'s "session snapshot + periodic backup" and VS Code's
//! "Hot Exit". On launch the host restores each tab from its backup; a backup
//! is deleted once its buffer is saved.
//!
//! Design (best-in-class synthesis):
//! - A JSON **manifest** (`session.json`) records one [`TabSnapshot`] per open
//!   tab: original path (or `None` for untitled), dirty flag, the backup file
//!   name holding the unsaved content, and the caret position.
//! - Each unsaved buffer's content lives in its own **backup file** under
//!   `backup/`, written **atomically** (temp + rename) so a crash mid-write
//!   never corrupts a snapshot.
//! - Triggers are the host's concern (debounced-on-change + on-exit); this
//!   module is pure I/O + types so it is unit-testable without a UI.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Manifest schema version. Bumped only on an incompatible shape change; a
/// newer manifest is ignored by an older build (treated as "no session").
pub const MANIFEST_VERSION: u32 = 1;

/// One open tab's restorable state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TabSnapshot {
    /// Original file path on disk; `None` for a never-saved untitled buffer.
    #[serde(default)]
    pub path: Option<String>,
    /// Whether the buffer had unsaved edits at snapshot time.
    #[serde(default)]
    pub dirty: bool,
    /// Backup file name (inside the backup dir) holding the unsaved content.
    /// `None` when the tab was clean — restore from `path` instead.
    #[serde(default)]
    pub backup: Option<String>,
    /// Caret char index, restored on reopen (best-effort).
    #[serde(default)]
    pub cursor: usize,
}

/// The whole persisted session.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionManifest {
    pub version: u32,
    #[serde(default)]
    pub tabs: Vec<TabSnapshot>,
    /// Index of the active tab on restore.
    #[serde(default)]
    pub active: usize,
}

impl SessionManifest {
    pub fn new(tabs: Vec<TabSnapshot>, active: usize) -> Self {
        Self {
            version: MANIFEST_VERSION,
            tabs,
            active,
        }
    }
}

/// Path to the JSON session manifest inside the config dir.
pub fn manifest_path(config_dir: &Path) -> PathBuf {
    config_dir.join("session.json")
}

/// Directory holding the per-tab content backups.
pub fn backup_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("backup")
}

/// A deterministic, filesystem-safe backup file name for a tab. A saved file
/// derives a stable name from its path (so the same file reuses one backup
/// across sessions); an untitled buffer uses its tab index. Never contains a
/// path separator, so it can't escape the backup dir.
pub fn backup_name(path: Option<&str>, index: usize) -> String {
    match path {
        Some(p) => format!("f{:016x}.bak", fnv1a(p)),
        None => format!("untitled-{index}.bak"),
    }
}

/// FNV-1a 64-bit hash — small, dependency-free, good enough for naming.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Write `bytes` to `path`, OWNER-ONLY on Unix (mode 0600). Unsaved-buffer
/// backups and the session manifest hold buffer CONTENT (possibly secrets in a
/// scratch note), so they must not be world-readable on a shared multi-user
/// host. On Windows the default ACL is already owner-scoped, so a plain write is
/// correct there.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        fs::write(path, bytes)
    }
}

/// SEC-1 path-traversal guard. A backup `name` is ALWAYS a bare file name by
/// construction ([`backup_name`]), but it re-enters from the untrusted,
/// user-writable `session.json` manifest on every restore, so the
/// "always a bare filename" invariant must be re-validated at EVERY boundary
/// that joins it to a directory — not just the write side.
///
/// The strongest, OS-uniform primitive is: `Path::new(name).components()` must
/// be EXACTLY one [`std::path::Component::Normal`]. This uniformly rejects `..`
/// (`ParentDir`), an absolute path / root (`RootDir`), a Windows drive or UNC
/// prefix (`Prefix`), a current-dir token (`CurDir`), an empty name, and any
/// `/` or `\` separator (which would split into >1 component) on both Windows
/// and POSIX. It mirrors — and subsumes — [`write_backup`]'s separator check so
/// the read and write sides are symmetric.
fn validate_backup_name(name: &str) -> io::Result<()> {
    use std::path::Component;
    // A rejection here is a security decision on a restore: the backup `name`
    // arrives verbatim from the untrusted, user-writable `session.json`, so a
    // non-bare name is an attempted path escape. Record the REASON kind at warn
    // (the path itself only at debug — it is untrusted content).
    fn reject(name: &str, reason: &'static str, msg: &'static str) -> io::Error {
        tracing::warn!(
            target: "scribe::session",
            reason,
            "restore path rejected — refusing a non-bare backup name from the session manifest"
        );
        tracing::debug!(target: "scribe::session", name = %name, "rejected backup name");
        io::Error::new(io::ErrorKind::InvalidInput, msg)
    }
    // A `session.json` manifest is portable across OSes, so a malicious
    // Windows-authored `backup: "..\\..\\secret"` could be opened on POSIX
    // (where `\` is a legal filename char, NOT a separator, so `components()`
    // alone would treat it as one Normal component and accept it). Reject BOTH
    // separators explicitly on every platform so the guard is OS-independent.
    if name.contains('/') || name.contains('\\') {
        return Err(reject(
            name,
            "separator",
            "backup name must not contain a path separator",
        ));
    }
    const BARE_MSG: &str =
        "backup name must be a single bare file component (no separators, `..`, or root/drive prefix)";
    let mut comps = Path::new(name).components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        // `..` traversal.
        (Some(Component::ParentDir), _) => Err(reject(name, "parent-dir-escape", BARE_MSG)),
        // Absolute path, Windows drive, or a `\\?\` / UNC prefix.
        (Some(Component::Prefix(_)), _) | (Some(Component::RootDir), _) => {
            Err(reject(name, "absolute-or-unc", BARE_MSG))
        }
        // Empty, `.`, or a multi-component name.
        _ => Err(reject(name, "non-bare-name", BARE_MSG)),
    }
}

/// Atomically write `content` to `<dir>/<name>` (write temp, then rename).
/// Creates `dir` if needed. `name` MUST be a bare file name (no separators).
pub fn write_backup(dir: &Path, name: &str, content: &str) -> io::Result<()> {
    validate_backup_name(name)?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{name}.tmp"));
    let dst = dir.join(name);
    write_private(&tmp, content.as_bytes())?;
    // rename is atomic on the same volume; replaces any existing backup.
    fs::rename(&tmp, &dst)
}

/// Read a backup's content. SEC-1: the `name` arrives verbatim from the
/// untrusted `session.json` manifest, so it MUST be re-validated as a single
/// bare file component before joining it to `dir` — otherwise a tampered
/// manifest (`backup: "../../../secret"`) reads an arbitrary file into a
/// restored buffer (CWE-22 path traversal / arbitrary-file-read).
pub fn read_backup(dir: &Path, name: &str) -> io::Result<String> {
    validate_backup_name(name)?;
    fs::read_to_string(dir.join(name))
}

/// Delete a backup (best-effort; missing file is not an error). SEC-1: the same
/// path-traversal guard applies — a tampered manifest name must not be able to
/// `remove_file` outside the backup dir.
pub fn delete_backup(dir: &Path, name: &str) {
    if validate_backup_name(name).is_err() {
        return;
    }
    let _ = fs::remove_file(dir.join(name));
}

/// Remove ALL on-disk session state that can hold buffer CONTENT or file paths:
/// the restore manifest (`session.json`) and every backup file under `backup/`
/// (each is a snapshot of an unsaved buffer's text). Best-effort — a missing
/// file is not an error — returning the number of files removed. Does NOT touch
/// the user's config, themes, or any saved document. Used by the app's
/// "Clear local data" action (privacy).
pub fn clear_session_state(config_dir: &Path) -> usize {
    let mut removed = 0usize;
    if fs::remove_file(manifest_path(config_dir)).is_ok() {
        removed += 1;
    }
    let bdir = backup_dir(config_dir);
    if let Ok(entries) = fs::read_dir(&bdir) {
        for entry in entries.flatten() {
            if entry.path().is_file() && fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    // Drop the (now-empty) backup directory too; ignored if non-empty/absent.
    let _ = fs::remove_dir(&bdir);
    removed
}

/// Atomically persist the manifest as pretty JSON.
pub fn save_manifest(config_dir: &Path, manifest: &SessionManifest) -> io::Result<()> {
    fs::create_dir_all(config_dir)?;
    let body = serde_json::to_string_pretty(manifest)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let path = manifest_path(config_dir);
    let tmp = path.with_extension("json.tmp");
    write_private(&tmp, body.as_bytes())?;
    fs::rename(&tmp, &path)
}

/// Load the manifest, or `None` when absent / unreadable / a newer schema.
pub fn load_manifest(config_dir: &Path) -> Option<SessionManifest> {
    let path = manifest_path(config_dir);
    let body = match fs::read_to_string(&path) {
        Ok(b) => b,
        // Absent manifest is the normal "no prior session" case — silent.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        // The manifest EXISTS but cannot be read (permissions, IO error, a
        // partially-written file). The caller only sees `None` either way, so
        // without this it is indistinguishable from "no session" and the
        // unsaved-work recovery is silently lost. Log the error KIND only (the
        // path is untrusted content → debug).
        Err(e) => {
            tracing::warn!(
                target: "scribe::session",
                error_kind = ?e.kind(),
                "session manifest unreadable — unsaved-work recovery skipped"
            );
            tracing::debug!(target: "scribe::session", path = %path.display(), "unreadable session manifest path");
            return None;
        }
    };
    let manifest: SessionManifest = match serde_json::from_str(&body) {
        Ok(m) => m,
        // Corrupt JSON (disk corruption, partial write, hand-edit). Recovery is
        // lost; the caller cannot tell this from "absent". Never log the serde
        // error or the body — both can echo buffer CONTENT.
        Err(_) => {
            tracing::warn!(
                target: "scribe::session",
                "session manifest corrupt (unparseable JSON) — unsaved-work recovery skipped"
            );
            return None;
        }
    };
    if manifest.version > MANIFEST_VERSION {
        return None;
    }
    Some(manifest)
}

/// Remove backup files in `dir` that the manifest no longer references. Keeps
/// the backup dir from accumulating snapshots of closed/saved buffers. Returns
/// the number of files pruned.
pub fn prune_orphan_backups(dir: &Path, manifest: &SessionManifest) -> usize {
    let live: std::collections::HashSet<&str> = manifest
        .tabs
        .iter()
        .filter_map(|t| t.backup.as_deref())
        .collect();
    let mut pruned = 0;
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(stem) = name.strip_suffix(".bak.tmp") {
            // CORR-02: a crash mid-`write_backup` leaves a `<name>.bak.tmp`
            // (the atomic-rename source) that `clear_session_state` would
            // reclaim but `prune` previously skipped, leaking it forever.
            // Reclaim it conservatively: only when no LIVE backup shares its
            // stem (i.e. the rename never completed for a referenced tab).
            // `live` holds full `*.bak` names, so reconstruct the sibling.
            let live_sibling = format!("{stem}.bak");
            if !live.contains(live_sibling.as_str()) {
                let _ = fs::remove_file(entry.path());
                pruned += 1;
            }
            continue;
        }
        // Never touch non-backup files.
        if !name.ends_with(".bak") {
            continue;
        }
        if !live.contains(name.as_ref()) {
            let _ = fs::remove_file(entry.path());
            pruned += 1;
        }
    }
    pruned
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn backup_name_is_stable_and_separator_free() {
        let a = backup_name(Some("/proj/notes.txt"), 0);
        let b = backup_name(Some("/proj/notes.txt"), 9);
        assert_eq!(a, b, "same path → same backup name regardless of index");
        assert!(!a.contains('/') && !a.contains('\\'));
        let u0 = backup_name(None, 0);
        let u1 = backup_name(None, 1);
        assert_ne!(u0, u1, "untitled buffers are distinguished by index");
    }

    #[test]
    fn backup_name_pins_exact_fnv1a_hash() {
        // Known-answer test that pins the FNV-1a 64-bit hash byte-for-byte.
        // A saved file's backup name is `f{hash:016x}.bak`; the hash is a STABLE
        // identity key reused across sessions, so a change to the hash function
        // (constant, fold operator, or seed) would silently orphan every existing
        // backup. Vectors computed from the canonical FNV-1a-64 reference.
        // FNV-1a("a")              = 0xaf63dc4c8601ec8c
        // FNV-1a("test")           = 0xf9e6e6ef197c2b25
        // FNV-1a("/proj/notes.txt")= 0xddc29c6c66027a15
        assert_eq!(backup_name(Some("a"), 0), "faf63dc4c8601ec8c.bak");
        assert_eq!(backup_name(Some("test"), 7), "ff9e6e6ef197c2b25.bak");
        assert_eq!(
            backup_name(Some("/proj/notes.txt"), 3),
            "fddc29c6c66027a15.bak",
        );
        // The index is ignored for a saved path (hash-derived name), so two
        // different indices for the SAME path yield the SAME, exact name. This
        // pins that the `^=` fold (not `|=`/`&=`) and the literal seed are intact:
        // a `0`/`1` return or a different operator changes every digit above.
        assert_eq!(
            backup_name(Some("a"), 99),
            "faf63dc4c8601ec8c.bak",
            "the saved-path name is the hash, never the seed or a constant",
        );
    }

    #[test]
    fn backup_roundtrip_and_delete() {
        let dir = tempdir().unwrap();
        let bdir = backup_dir(dir.path());
        write_backup(&bdir, "f0.bak", "hello").unwrap();
        assert_eq!(read_backup(&bdir, "f0.bak").unwrap(), "hello");
        // Overwrite is atomic + replaces.
        write_backup(&bdir, "f0.bak", "world").unwrap();
        assert_eq!(read_backup(&bdir, "f0.bak").unwrap(), "world");
        delete_backup(&bdir, "f0.bak");
        assert!(read_backup(&bdir, "f0.bak").is_err());
    }

    #[test]
    fn write_backup_rejects_path_separator() {
        let dir = tempdir().unwrap();
        assert!(write_backup(dir.path(), "../escape.bak", "x").is_err());
    }

    #[test]
    fn read_backup_rejects_path_traversal() {
        // SEC-1: a tampered `session.json` `backup` field that contains `..`, an
        // absolute path, or a separator must be REJECTED before any file is
        // read — otherwise it reads an arbitrary file into a restored buffer.
        let dir = tempdir().unwrap();
        let bdir = backup_dir(dir.path());
        // Plant a real "secret" OUTSIDE the backup dir (sibling of bdir).
        let secret = dir.path().join("secret.txt");
        fs::create_dir_all(&bdir).unwrap();
        fs::write(&secret, "TOP SECRET").unwrap();

        // `..`-relative escape (the audit's exact attack shape).
        assert!(
            read_backup(&bdir, "../secret.txt").is_err(),
            "`..` traversal must be rejected (no file read)"
        );
        // Forward-slash separator.
        assert!(
            read_backup(&bdir, "sub/secret.txt").is_err(),
            "`/` separator must be rejected"
        );
        // Backslash separator (Windows).
        assert!(
            read_backup(&bdir, "sub\\secret.txt").is_err(),
            "`\\` separator must be rejected"
        );
        // Absolute path.
        let abs = secret.to_string_lossy().to_string();
        assert!(
            read_backup(&bdir, &abs).is_err(),
            "absolute path must be rejected"
        );
        // Empty / current-dir tokens.
        assert!(read_backup(&bdir, "").is_err(), "empty name rejected");
        assert!(read_backup(&bdir, ".").is_err(), "`.` rejected");

        // A plain bare name still works (round-trip preserved).
        write_backup(&bdir, "f0.bak", "ok").unwrap();
        assert_eq!(read_backup(&bdir, "f0.bak").unwrap(), "ok");
    }

    #[test]
    fn delete_backup_rejects_path_traversal() {
        // SEC-1: `delete_backup` joins the same untrusted name → must not be
        // able to `remove_file` outside the backup dir.
        let dir = tempdir().unwrap();
        let bdir = backup_dir(dir.path());
        let secret = dir.path().join("keep.txt");
        fs::create_dir_all(&bdir).unwrap();
        fs::write(&secret, "do not delete").unwrap();
        delete_backup(&bdir, "../keep.txt");
        assert!(secret.exists(), "traversal delete must be a no-op");
        // A valid name still deletes.
        write_backup(&bdir, "f0.bak", "x").unwrap();
        delete_backup(&bdir, "f0.bak");
        assert!(read_backup(&bdir, "f0.bak").is_err());
    }

    #[test]
    fn prune_reclaims_orphan_bak_tmp() {
        // CORR-02: a crash mid-write leaves a `*.bak.tmp` with no completed
        // `.bak`. Prune must reclaim it; a `*.bak.tmp` whose `.bak` sibling is
        // LIVE must be left alone (a write may be in flight for it).
        let dir = tempdir().unwrap();
        let bdir = backup_dir(dir.path());
        fs::create_dir_all(&bdir).unwrap();
        // Orphan temp: no matching live backup.
        fs::write(bdir.join("orphan.bak.tmp"), "crash residue").unwrap();
        // Live backup + an in-flight temp for that same live name.
        write_backup(&bdir, "live.bak", "a").unwrap();
        fs::write(bdir.join("live.bak.tmp"), "in flight").unwrap();

        let m = SessionManifest::new(
            vec![TabSnapshot {
                path: None,
                dirty: true,
                backup: Some("live.bak".into()),
                cursor: 0,
            }],
            0,
        );
        let pruned = prune_orphan_backups(&bdir, &m);
        assert_eq!(pruned, 1, "only the orphan temp is reclaimed");
        assert!(!bdir.join("orphan.bak.tmp").exists(), "orphan temp gone");
        assert!(
            bdir.join("live.bak.tmp").exists(),
            "in-flight temp for a live backup is preserved"
        );
        assert!(read_backup(&bdir, "live.bak").is_ok(), "live backup intact");
    }

    #[test]
    fn clear_session_state_removes_manifest_and_backups() {
        let dir = tempdir().unwrap();
        // Lay down a manifest + two backups (unsaved-buffer content).
        let bdir = backup_dir(dir.path());
        write_backup(&bdir, "f0.bak", "secret one").unwrap();
        write_backup(&bdir, "f1.bak", "secret two").unwrap();
        let m = SessionManifest::new(
            vec![TabSnapshot {
                path: Some("/secret/path.txt".into()),
                dirty: true,
                backup: Some("f0.bak".into()),
                cursor: 0,
            }],
            0,
        );
        save_manifest(dir.path(), &m).unwrap();
        assert!(manifest_path(dir.path()).exists());

        let removed = clear_session_state(dir.path());
        assert!(removed >= 3, "manifest + 2 backups removed, got {removed}");
        assert!(!manifest_path(dir.path()).exists(), "manifest gone");
        assert!(read_backup(&bdir, "f0.bak").is_err(), "backup content gone");
        assert!(read_backup(&bdir, "f1.bak").is_err(), "backup content gone");
        // Idempotent: a second clear on an already-clean dir is a no-op, no panic.
        assert_eq!(clear_session_state(dir.path()), 0);
    }

    #[test]
    fn manifest_roundtrip() {
        let dir = tempdir().unwrap();
        let m = SessionManifest::new(
            vec![
                TabSnapshot {
                    path: Some("/a.txt".into()),
                    dirty: true,
                    backup: Some("f1.bak".into()),
                    cursor: 12,
                },
                TabSnapshot {
                    path: None,
                    dirty: true,
                    backup: Some("untitled-0.bak".into()),
                    cursor: 0,
                },
            ],
            1,
        );
        save_manifest(dir.path(), &m).unwrap();
        let back = load_manifest(dir.path()).unwrap();
        assert_eq!(back.tabs, m.tabs);
        assert_eq!(back.active, 1);
    }

    #[test]
    fn load_manifest_ignores_newer_schema() {
        let dir = tempdir().unwrap();
        let body = format!("{{\"version\": {}, \"tabs\": []}}", MANIFEST_VERSION + 1);
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(manifest_path(dir.path()), body).unwrap();
        assert!(load_manifest(dir.path()).is_none());
    }

    #[test]
    fn load_manifest_absent_is_none() {
        let dir = tempdir().unwrap();
        assert!(load_manifest(dir.path()).is_none());
    }

    #[test]
    fn prune_removes_unreferenced_backups() {
        let dir = tempdir().unwrap();
        let bdir = backup_dir(dir.path());
        write_backup(&bdir, "live.bak", "a").unwrap();
        write_backup(&bdir, "orphan.bak", "b").unwrap();
        let m = SessionManifest::new(
            vec![TabSnapshot {
                path: None,
                dirty: true,
                backup: Some("live.bak".into()),
                cursor: 0,
            }],
            0,
        );
        assert_eq!(prune_orphan_backups(&bdir, &m), 1);
        assert!(read_backup(&bdir, "live.bak").is_ok());
        assert!(read_backup(&bdir, "orphan.bak").is_err());
    }

    // ---- silent-failure logging ----

    use crate::test_log_capture::with_captured_logs;
    use tracing::Level;

    #[test]
    fn absent_manifest_loads_silently_without_a_warn() {
        let dir = tempdir().unwrap();
        with_captured_logs(|logs| {
            assert!(load_manifest(dir.path()).is_none());
            // A missing session is the normal first-run case — no warn noise.
            assert!(
                !logs.records().iter().any(|(lvl, _)| *lvl <= Level::WARN),
                "an absent manifest must not emit a warn/error: {:?}",
                logs.records()
            );
        });
    }

    #[test]
    fn corrupt_manifest_emits_warn_and_recovery_is_skipped() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path()).unwrap();
        // Plant a manifest that EXISTS but is unparseable JSON.
        fs::write(manifest_path(dir.path()), b"{ this is not valid json ]]]").unwrap();
        with_captured_logs(|logs| {
            assert!(load_manifest(dir.path()).is_none());
            assert!(
                logs.has(Level::WARN, "session manifest corrupt"),
                "expected a corrupt-manifest WARN, got: {:?}",
                logs.records()
            );
            assert!(
                logs.warn_plus_text().contains("recovery skipped"),
                "the warn must state recovery was skipped"
            );
        });
    }

    #[test]
    fn unreadable_manifest_emits_warn_without_leaking_the_path() {
        let dir = tempdir().unwrap();
        // A DIRECTORY at the manifest path makes `read_to_string` fail with a
        // non-NotFound error (the "exists but unreadable" branch).
        fs::create_dir_all(manifest_path(dir.path())).unwrap();
        with_captured_logs(|logs| {
            assert!(load_manifest(dir.path()).is_none());
            assert!(
                logs.has(Level::WARN, "session manifest unreadable"),
                "expected an unreadable-manifest WARN, got: {:?}",
                logs.records()
            );
            // The path is logged only at debug — never at warn+.
            assert!(
                !logs.warn_plus_text().contains("session.json"),
                "the manifest path must not appear at warn+"
            );
        });
    }

    #[test]
    fn rejected_restore_path_emits_warn_without_leaking_the_name() {
        let dir = tempdir().unwrap();
        let bdir = backup_dir(dir.path());
        with_captured_logs(|logs| {
            // A tampered manifest name that escapes the backup dir is rejected.
            assert!(read_backup(&bdir, "../SECRET_TARGET.txt").is_err());
            assert!(
                logs.has(Level::WARN, "restore path rejected"),
                "expected a restore-path-reject WARN, got: {:?}",
                logs.records()
            );
            // The untrusted name is debug-only; it must not surface at warn+.
            assert!(
                !logs.warn_plus_text().contains("SECRET_TARGET"),
                "the rejected name must not appear at warn+"
            );
        });
    }
}
