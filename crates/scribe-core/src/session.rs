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

/// Atomically write `content` to `<dir>/<name>` (write temp, then rename).
/// Creates `dir` if needed. `name` MUST be a bare file name (no separators).
pub fn write_backup(dir: &Path, name: &str, content: &str) -> io::Result<()> {
    if name.contains('/') || name.contains('\\') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backup name must not contain a path separator",
        ));
    }
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{name}.tmp"));
    let dst = dir.join(name);
    fs::write(&tmp, content)?;
    // rename is atomic on the same volume; replaces any existing backup.
    fs::rename(&tmp, &dst)
}

/// Read a backup's content.
pub fn read_backup(dir: &Path, name: &str) -> io::Result<String> {
    fs::read_to_string(dir.join(name))
}

/// Delete a backup (best-effort; missing file is not an error).
pub fn delete_backup(dir: &Path, name: &str) {
    let _ = fs::remove_file(dir.join(name));
}

/// Atomically persist the manifest as pretty JSON.
pub fn save_manifest(config_dir: &Path, manifest: &SessionManifest) -> io::Result<()> {
    fs::create_dir_all(config_dir)?;
    let body = serde_json::to_string_pretty(manifest)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let path = manifest_path(config_dir);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)
}

/// Load the manifest, or `None` when absent / unreadable / a newer schema.
pub fn load_manifest(config_dir: &Path) -> Option<SessionManifest> {
    let body = fs::read_to_string(manifest_path(config_dir)).ok()?;
    let manifest: SessionManifest = serde_json::from_str(&body).ok()?;
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
        // Never touch in-flight temp files or non-backup files.
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
        let a = backup_name(Some("/home/x/notes.txt"), 0);
        let b = backup_name(Some("/home/x/notes.txt"), 9);
        assert_eq!(a, b, "same path → same backup name regardless of index");
        assert!(!a.contains('/') && !a.contains('\\'));
        let u0 = backup_name(None, 0);
        let u1 = backup_name(None, 1);
        assert_ne!(u0, u1, "untitled buffers are distinguished by index");
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
}
