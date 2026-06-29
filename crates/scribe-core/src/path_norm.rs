//! Filesystem-identity path normalization for **comparison only**.
//!
//! Path dedup (recent-files MRU, one-tab-per-file, session-restore) must match
//! the *host filesystem's* notion of file identity, not byte-exact `PathBuf`
//! equality. On Windows the FS is case-insensitive and accepts both `/` and
//! `\` separators, so `C:\Data\f.txt` and `c:\data\F.TXT` name the SAME file —
//! byte-exact equality wrongly treats them as two files and produces duplicate
//! recent-files entries / a second tab for one note. POSIX is case-SENSITIVE
//! and must stay so: `/srv/F` and `/srv/f` are genuinely distinct files.
//!
//! These helpers derive a *comparison key* — never a display or persisted
//! string. The folded/lowercased form is lossy and MUST NOT be shown to the
//! user or written to the session manifest.

use std::path::Path;

/// Normalize `path` into a comparison key matching the host FS's identity
/// semantics.
///
/// - **Windows:** strips a leading `\\?\` verbatim-disk prefix (`\\?\C:\…` →
///   `C:\…`), folds `/` → `\`, then ASCII-lowercases (case-insensitive FS).
///   A `\\?\UNC\…` verbatim-UNC prefix is left intact (only separator-folded +
///   lowercased) so it normalizes consistently with itself but is never
///   mistaken for a disk path.
/// - **Non-Windows (POSIX):** returns the path string unchanged — case- AND
///   separator-SENSITIVE, preserving genuine file identity.
///
/// For equality/dedup ONLY — never for display or persistence.
pub fn normalize_for_compare(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        // Strip a verbatim DISK prefix `\\?\C:\…` -> `C:\…`. Keep the verbatim
        // UNC prefix `\\?\UNC\…` (it is not a drive-letter path; stripping it
        // would conflate it with a disk path).
        let stripped: &str = if let Some(rest) = s.strip_prefix(r"\\?\") {
            if rest.starts_with(r"UNC\") || rest.starts_with("UNC/") {
                // Verbatim UNC — leave the `\\?\` in place so it stays distinct.
                &s
            } else {
                rest
            }
        } else {
            &s
        };
        stripped.replace('/', "\\").to_ascii_lowercase()
    } else {
        s.into_owned()
    }
}

/// True when `a` and `b` name the same file under the host FS's identity
/// semantics. Equivalent to comparing their [`normalize_for_compare`] keys.
pub fn paths_equal_for_compare(a: &Path, b: &Path) -> bool {
    normalize_for_compare(a) == normalize_for_compare(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // --- platform-agnostic: verbatim-disk-prefix strip is windows-only behavior,
    // but the helper is pure-string-testable. These run on every platform. ---

    #[test]
    fn posix_paths_are_case_sensitive() {
        // POSIX file identity is case-sensitive: /srv/F and /srv/f are DISTINCT.
        // (On non-Windows this is the real behavior; on Windows the inputs would
        // fold, so we gate the inequality assertion to non-Windows.)
        let upper = PathBuf::from("/srv/proj/F");
        let lower = PathBuf::from("/srv/proj/f");
        #[cfg(not(windows))]
        {
            assert_ne!(
                normalize_for_compare(&upper),
                normalize_for_compare(&lower),
                "POSIX paths must stay case-sensitive"
            );
            assert!(!paths_equal_for_compare(&upper, &lower));
        }
        // A path equals itself on every platform.
        assert!(paths_equal_for_compare(&upper, &upper));
        let _ = &lower;
    }

    #[test]
    fn identical_paths_always_equal() {
        let p = PathBuf::from("/srv/data/note.txt");
        assert_eq!(normalize_for_compare(&p), normalize_for_compare(&p));
        assert!(paths_equal_for_compare(&p, &p));
    }

    #[cfg(windows)]
    #[test]
    fn windows_case_and_separator_insensitive() {
        // The A-07 defect: the SAME file reached via two casings / separators.
        let a = PathBuf::from(r"C:\Data\f.txt");
        let b = PathBuf::from(r"c:\data\F.TXT");
        let c = PathBuf::from("C:/Data/f.txt"); // forward slashes
        assert_eq!(normalize_for_compare(&a), normalize_for_compare(&b));
        assert_eq!(normalize_for_compare(&a), normalize_for_compare(&c));
        assert!(paths_equal_for_compare(&a, &b));
        assert!(paths_equal_for_compare(&a, &c));
    }

    #[cfg(windows)]
    #[test]
    fn windows_verbatim_disk_prefix_stripped() {
        // `\\?\C:\…` (verbatim long-path form, e.g. from canonicalize) names the
        // same file as the plain `C:\…` form.
        let verbatim = PathBuf::from(r"\\?\C:\Data\f.txt");
        let plain = PathBuf::from(r"C:\Data\f.txt");
        assert_eq!(
            normalize_for_compare(&verbatim),
            normalize_for_compare(&plain)
        );
        assert!(paths_equal_for_compare(&verbatim, &plain));
    }

    #[cfg(windows)]
    #[test]
    fn windows_distinct_files_stay_distinct() {
        let a = PathBuf::from(r"C:\Data\one.txt");
        let b = PathBuf::from(r"C:\Data\two.txt");
        assert_ne!(normalize_for_compare(&a), normalize_for_compare(&b));
        assert!(!paths_equal_for_compare(&a, &b));
    }

    #[cfg(windows)]
    #[test]
    fn windows_verbatim_unc_not_conflated_with_disk() {
        // A verbatim-UNC path keeps its `\\?\` so it never collides with a disk
        // path. It still self-normalizes consistently.
        let unc = PathBuf::from(r"\\?\UNC\server\share\f.txt");
        let unc2 = PathBuf::from(r"\\?\UNC\server\share\F.TXT");
        assert_eq!(normalize_for_compare(&unc), normalize_for_compare(&unc2));
        // It is NOT equal to a disk path of similar tail.
        let diskish = PathBuf::from(r"C:\server\share\f.txt");
        assert_ne!(normalize_for_compare(&unc), normalize_for_compare(&diskish));
    }

    #[cfg(windows)]
    #[test]
    fn windows_verbatim_unc_detected_via_forward_slash_too() {
        // The verbatim-UNC detector is `rest.starts_with("UNC\\") || starts_with("UNC/")`.
        // The `||` is load-bearing: a `\\?\` prefix can be followed by EITHER
        // separator after the literal "UNC". This pins the OR against an `&&`
        // mutation — with `&&`, a `UNC/`-form prefix (which does NOT start with
        // `UNC\`) would fail the guard, get its `\\?\` STRIPPED, and be wrongly
        // conflated with a disk path. Under correct `||`, the `\\?\` is retained
        // so the verbatim-UNC key stays distinct from the disk-path key.
        let unc_fwd = PathBuf::from(r"\\?\UNC/server/share/f.txt");
        let key = normalize_for_compare(&unc_fwd);
        // The `\\?\` verbatim marker must survive (folded to `\\?\` after the
        // separator pass) — it must NOT have been stripped to a bare `unc\...`.
        assert!(
            key.starts_with(r"\\?\"),
            "a forward-slash verbatim-UNC must keep its \\\\?\\ marker, got: {key}"
        );
        // It must equal the back-slash verbatim-UNC form for the same target (both
        // are the SAME file), and stay distinct from a disk-path key.
        let unc_back = PathBuf::from(r"\\?\UNC\server\share\f.txt");
        assert_eq!(key, normalize_for_compare(&unc_back));
        let diskish = PathBuf::from(r"C:\unc\server\share\f.txt");
        assert_ne!(key, normalize_for_compare(&diskish));
    }
}
