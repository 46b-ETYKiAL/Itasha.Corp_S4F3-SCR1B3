//! R6 / S-04 (CWE-59 link-following, CWE-610 untrusted-resource, CWE-22
//! path-traversal) — session-restore path validation.
//!
//! The session manifest (`session.json` + the legacy paths file) is an
//! ON-DISK, USER-WRITABLE artifact. On restore the app re-opens whatever
//! absolute path it finds there. A tampered manifest can therefore point the
//! editor at:
//!   * a UNC path (`\\attacker\share\…`, `\\?\UNC\…`) — opening it makes
//!     Windows authenticate to the remote SMB host, leaking the user's
//!     NTLM credentials to an attacker-controlled server, OR
//!   * a symlink whose target escapes the user's prior working set onto a
//!     new volume / a sensitive system location.
//!
//! Defense: a pure predicate [`is_safe_restore_path`] that fails CLOSED. A
//! restore path is opened ONLY when it:
//!   1. is NOT a UNC path,
//!   2. EXISTS on disk (a vanished/non-existent target is skipped), and
//!   3. resolves (canonicalizes) to stay UNDER at least one allowed root
//!      (the parent directories of the prior session's own declared paths,
//!      plus any opened-folder roots). A symlink whose canonical target
//!      escapes every allowed root is rejected — we never auto-follow a
//!      symlink onto a new volume.
//!
//! Paths that fail are silently skipped (and logged by the caller); a
//! tampered/attacker-chosen path is NEVER auto-opened.

use std::path::{Component, Path, PathBuf};

/// Is `raw` a UNC network-share path or a Windows device-namespace path —
/// i.e. a "remote / non-ordinary-file" path that must NEVER be auto-opened
/// on restore? Covers:
///   * verbatim-UNC (`\\?\UNC\server\share`),
///   * plain UNC (`\\server\share`, `//server/share`) — the SMB/NTLM
///     credential-leak vector, and
///   * device-namespace (`\\.\PhysicalDrive0`, `\\?\C:` style raw devices) —
///     not an ordinary file.
///
/// IMPORTANT: a Windows *verbatim-disk* path (`\\?\C:\Users\…\notes.md`, the
/// form `std::fs::canonicalize` returns) is an ORDINARY LOCAL FILE and MUST
/// NOT be flagged — else every canonicalized restore path would be falsely
/// rejected. We distinguish verbatim-disk (allowed) from verbatim-UNC /
/// device (rejected) precisely.
///
/// Pure + cheap; defends the highest-value vector (SMB/NTLM credential leak)
/// independently of whether the target exists. The name is kept as
/// `is_unc_path` for call-site brevity; the doc above is the precise scope.
pub(crate) fn is_unc_path(raw: &Path) -> bool {
    // First, ask the path API directly. On Windows this is authoritative and
    // cleanly separates network/device prefixes from local disk ones.
    if let Some(Component::Prefix(p)) = raw.components().next() {
        use std::path::Prefix::*;
        return match p.kind() {
            // Network shares — the credential-leak vector.
            UNC(..) | VerbatimUNC(..) => true,
            // Raw device namespace (`\\.\PhysicalDrive0`) — not an ordinary file.
            DeviceNS(..) => true,
            // Ordinary local disk (incl. the `\\?\C:` verbatim-disk that
            // `canonicalize` returns) — allowed.
            Verbatim(..) | VerbatimDisk(..) | Disk(..) => false,
        };
    }

    // Fallback for non-Windows hosts (our tests run there too): a
    // `std::path::Component` does NOT classify a `\\server\share` string as a
    // Prefix on Unix, so pattern-match the raw bytes. A leading double
    // separator (`\\` or `//`) denotes a UNC share or a device path.
    let s = raw.to_string_lossy();
    let chars: Vec<char> = s.chars().collect();
    let starts_double_sep = chars.len() >= 2
        && (chars[0] == '\\' || chars[0] == '/')
        && (chars[1] == '\\' || chars[1] == '/');
    if !starts_double_sep {
        return false;
    }

    // `\\?\…` (verbatim) or `\\.\…` (device) prefix.
    let third = chars.get(2).copied();
    match third {
        // `\\.\…` device namespace → always rejected (raw device).
        Some('.') => true,
        // `\\?\…` verbatim → local (allowed) UNLESS the explicit
        // `\\?\UNC\…` network spelling.
        Some('?') => {
            let head: String = s.chars().take(8).collect::<String>().to_ascii_uppercase();
            head.replace('/', "\\").starts_with("\\\\?\\UNC")
        }
        // Plain `\\server\share` / `//server/share` → UNC.
        _ => true,
    }
}

/// Canonicalize `root` for prefix comparison, best-effort. A root that does
/// not canonicalize (vanished folder) yields `None` and simply contributes
/// no coverage — fail-closed: fewer allowed roots, never more.
fn canon_root(root: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(root).ok()
}

/// Build the set of canonical allowed roots from a collection of candidate
/// roots (prior-session path parents + opened-folder roots). Non-existent
/// roots are dropped.
pub(crate) fn allowed_roots<'a, I>(candidates: I) -> Vec<PathBuf>
where
    I: IntoIterator<Item = &'a Path>,
{
    let mut roots: Vec<PathBuf> = candidates.into_iter().filter_map(canon_root).collect();
    roots.sort();
    roots.dedup();
    roots
}

/// The pure decision. `path` is a restore candidate; `roots` are the
/// canonical allowed roots (from [`allowed_roots`]). Returns `true` ONLY
/// when the path is safe to auto-open.
///
/// Fail-CLOSED on every uncertainty: UNC → reject; nonexistent → reject;
/// un-canonicalizable → reject; canonical target outside every root →
/// reject. An EMPTY root set rejects everything (no prior working set means
/// nothing is trusted to auto-open).
pub(crate) fn is_safe_restore_path(path: &Path, roots: &[PathBuf]) -> bool {
    // (1) UNC paths are rejected unconditionally — the SMB/NTLM leak vector
    // is the highest-value one and is independent of on-disk existence.
    if is_unc_path(path) {
        return false;
    }

    // (2) The target must EXIST. A tampered manifest pointing at a path that
    // isn't there is skipped (and `canonicalize` would fail anyway).
    //   `symlink_metadata` does NOT follow the final symlink; we use the
    //   canonical form below for the escape check, but existence is the
    //   cheap first gate.
    if std::fs::symlink_metadata(path).is_err() {
        return false;
    }

    // (3) Resolve the REAL target (following symlinks) and require it to stay
    // under an allowed root. `canonicalize` follows links + normalises
    // `..`, so a symlink whose target escapes the prior working set — or a
    // `../../..`-style traversal — lands outside every root and is rejected.
    let Ok(canon) = std::fs::canonicalize(path) else {
        return false;
    };

    // A canonical UNC (resolved onto a network volume via a junction) is also
    // rejected here — defense in depth against a symlink→UNC pivot.
    if is_unc_path(&canon) {
        return false;
    }

    roots.iter().any(|root| canon.starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn unc_paths_are_detected() {
        assert!(is_unc_path(Path::new(r"\\attacker\share\evil.txt")));
        assert!(is_unc_path(Path::new(r"\\?\UNC\server\share\x")));
        assert!(is_unc_path(Path::new(r"\\.\PhysicalDrive0")));
        assert!(is_unc_path(Path::new("//server/share/x")));
        // A normal local path is NOT a UNC path.
        assert!(!is_unc_path(Path::new("/home/user/notes.md")));
        assert!(!is_unc_path(Path::new(r"C:\Users\me\notes.md")));
        assert!(!is_unc_path(Path::new("relative/notes.md")));
        // CRITICAL: a Windows *verbatim-disk* path (`\\?\C:\…`, the form
        // `canonicalize` returns) is LOCAL — it must NOT be flagged, or every
        // canonicalized restore path would be falsely rejected.
        assert!(!is_unc_path(Path::new(r"\\?\C:\Users\me\notes.md")));
        // …but the explicit verbatim-UNC spelling IS rejected, and a `\\.\`
        // device-namespace path is rejected too (it is a raw device, not an
        // ordinary file we should auto-open).
        assert!(is_unc_path(Path::new(r"\\?\UNC\server\share\x")));
        assert!(is_unc_path(Path::new(r"\\.\C:\Users\me\notes.md")));
    }

    #[test]
    fn unc_path_is_rejected_even_if_roots_would_allow() {
        // The UNC reject is unconditional — it does not depend on the root
        // set or on-disk existence. This is the credential-leak vector.
        let unc = PathBuf::from(r"\\attacker\share\notes.md");
        // Even with a permissive (but irrelevant) root, UNC is rejected.
        let roots = vec![PathBuf::from("/")];
        assert!(!is_safe_restore_path(&unc, &roots));
    }

    #[test]
    fn normal_local_file_under_root_is_accepted() {
        let dir = tempdir().expect("tempdir");
        let root = canon_root(dir.path()).expect("canon root");
        let file = dir.path().join("notes.md");
        fs::write(&file, b"hello").expect("write");
        let roots = vec![root];
        assert!(
            is_safe_restore_path(&file, &roots),
            "a real file under an allowed root is restorable"
        );
    }

    #[test]
    fn nonexistent_path_is_skipped() {
        let dir = tempdir().expect("tempdir");
        let root = canon_root(dir.path()).expect("canon root");
        let ghost = dir.path().join("does-not-exist.md");
        let roots = vec![root];
        assert!(
            !is_safe_restore_path(&ghost, &roots),
            "a vanished/tampered target must be skipped, not auto-created"
        );
    }

    #[test]
    fn empty_root_set_rejects_everything() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("notes.md");
        fs::write(&file, b"x").expect("write");
        // No prior working set → nothing is trusted to auto-open.
        assert!(!is_safe_restore_path(&file, &[]));
    }

    #[test]
    fn path_outside_every_root_is_rejected() {
        let allowed = tempdir().expect("tempdir");
        let other = tempdir().expect("tempdir");
        let root = canon_root(allowed.path()).expect("canon root");
        // A real file, but in a DIFFERENT directory than the allowed root.
        let outside = other.path().join("notes.md");
        fs::write(&outside, b"x").expect("write");
        assert!(
            !is_safe_restore_path(&outside, &[root]),
            "a path outside every allowed root must be rejected"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_the_root_is_rejected() {
        use std::os::unix::fs::symlink;
        let allowed = tempdir().expect("tempdir");
        let outside = tempdir().expect("tempdir");
        let root = canon_root(allowed.path()).expect("canon root");

        // The real secret lives OUTSIDE the allowed root.
        let secret = outside.path().join("secret.txt");
        fs::write(&secret, b"top secret").expect("write secret");

        // A symlink INSIDE the allowed root that points at the outside secret.
        let link = allowed.path().join("link.md");
        symlink(&secret, &link).expect("symlink");

        // The link itself is under the root, but its canonical TARGET escapes
        // — `canonicalize` follows the link and lands outside, so it's rejected.
        assert!(
            !is_safe_restore_path(&link, &[root]),
            "a symlink whose target escapes the allowed root must be rejected"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_staying_inside_the_root_is_accepted() {
        use std::os::unix::fs::symlink;
        let dir = tempdir().expect("tempdir");
        let root = canon_root(dir.path()).expect("canon root");

        let real = dir.path().join("real.md");
        fs::write(&real, b"x").expect("write");
        let link = dir.path().join("link.md");
        symlink(&real, &link).expect("symlink");

        // Both link and target are under the root → safe.
        assert!(is_safe_restore_path(&link, &[root]));
    }

    #[test]
    fn parent_traversal_outside_root_is_rejected() {
        let allowed = tempdir().expect("tempdir");
        let root = canon_root(allowed.path()).expect("canon root");
        // A real file two levels up from the allowed root (its grandparent),
        // reached via `..` — canonicalize normalises it OUT of the root.
        let escaper = allowed.path().join("..").join("..");
        // `escaper` exists (it's an ancestor dir), but it is not under `root`.
        assert!(!is_safe_restore_path(&escaper, &[root]));
    }

    #[test]
    fn allowed_roots_drops_nonexistent_and_dedups() {
        let dir = tempdir().expect("tempdir");
        let ghost = dir.path().join("nope");
        let real = dir.path();
        let cands: Vec<&Path> = vec![real, real, ghost.as_path()];
        let roots = allowed_roots(cands);
        // Only the real dir survives, deduped to one entry.
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0], canon_root(real).unwrap());
    }
}
