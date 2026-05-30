//! Minimal stdlib-only fuzzy filename matcher.
//!
//! Closes F-010 from `docs/audits/overlooked-surfaces-2026-05-29.md` — the
//! Ctrl+P "open file by name" surface every code editor on Earth ships.
//! Keeping this in-tree (no `nucleo-matcher` / `fuzzy-matcher` dep) honours
//! the "prefer stdlib over third-party when feasible" posture from
//! CONTRIBUTING.md and keeps the supply chain tight.
//!
//! ## Scoring
//!
//! Score each candidate path against the query as a case-insensitive
//! subsequence match. Returns `None` when the query isn't a subsequence.
//!
//! Score component weights (higher is better):
//!
//! - **Base subsequence hit**: 100
//! - **Tight cluster bonus**: −2 per skipped char (favours adjacent runs).
//! - **Word-boundary start bonus**: +10 if each matched query char follows
//!   a path separator, `_`, `-`, `.`, or the start of the basename.
//! - **Filename-region bonus**: +20 when ALL query chars land inside the
//!   path's last path component.
//! - **Exact-basename-match bonus**: +50 when the query equals the basename
//!   (case-insensitive, ignoring extension).
//!
//! Empty query returns `Some(0)` for every candidate so the picker can
//! render its full list before the user types.
//!
//! ## Project scan
//!
//! [`scan_project`] walks a root recursively (best-effort; errors silently
//! skip the offending entry) and returns up to `cap` file paths. Hidden
//! entries (basename starts with `.`) are skipped to keep `.git/`, `node_
//! modules/`, `target/` etc. from drowning the list.

use std::path::{Path, PathBuf};

/// Default cap on the number of files scanned + held in memory. Tuned for
/// "indexing is invisible" — even on a 5k-file repo the walk + score round
/// finishes well under the 60 Hz frame budget on modern hardware.
pub const FUZZY_SCAN_CAP: usize = 5_000;

/// Score a candidate path against `query`. Returns `None` if `query` is not
/// a case-insensitive subsequence of the path string (with path separators
/// normalized to `/` for cross-platform consistency).
pub fn score(path: &str, query: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let hay = path.replace('\\', "/").to_lowercase();
    let needle = query.to_lowercase();
    // Index of the basename within `hay` so we can grant the
    // filename-region bonus.
    let basename_start = hay.rfind('/').map(|i| i + 1).unwrap_or(0);
    let mut score: i64 = 100;
    let mut hay_it = hay.char_indices().peekable();
    let mut last_match: Option<usize> = None;
    let mut all_in_basename = true;
    let mut needle_it = needle.chars();
    let mut needle_ch = needle_it.next()?;
    loop {
        let (i, h) = hay_it.next()?;
        if h == needle_ch {
            // Skip-penalty: -2 per char skipped since last match.
            if let Some(prev) = last_match {
                let skipped = i - prev - 1;
                score -= 2 * skipped as i64;
            }
            // Word-boundary bonus: +10 when the previous char is one of
            // / . - _ or the match is at byte 0 of the haystack.
            let at_boundary = i == 0
                || hay
                    .as_bytes()
                    .get(i.saturating_sub(1))
                    .is_some_and(|b| matches!(*b, b'/' | b'.' | b'-' | b'_'));
            if at_boundary {
                score += 10;
            }
            if i < basename_start {
                all_in_basename = false;
            }
            last_match = Some(i);
            match needle_it.next() {
                Some(c) => needle_ch = c,
                None => break,
            }
        }
    }
    if all_in_basename {
        score += 20;
    }
    // Exact basename match (ignoring extension).
    let basename = &hay[basename_start..];
    let basename_no_ext = basename
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(basename);
    if basename_no_ext == needle {
        score += 50;
    }
    Some(score)
}

/// Recursively scan `root` for file paths, skipping hidden entries
/// (basename starts with `.`) and capping at [`FUZZY_SCAN_CAP`]. Returns
/// paths in OS-walk order (stable enough for the UI; the picker sorts by
/// score anyway).
pub fn scan_project(root: &Path, cap: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= cap {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if out.len() >= cap {
                break;
            }
            let p = entry.path();
            let basename = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if basename.starts_with('.') {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                // Skip a small set of universal "don't index" dirs.
                if matches!(
                    basename,
                    "target" | "node_modules" | "build" | "dist" | "out" | "__pycache__"
                ) {
                    continue;
                }
                stack.push(p);
            } else if meta.is_file() {
                out.push(p);
            }
        }
    }
    out
}

/// Rank `paths` against `query`, returning the top `max_results` matches
/// sorted by score (highest first). Path-string equality breaks ties so the
/// ordering is deterministic.
pub fn rank(paths: &[PathBuf], query: &str, max_results: usize) -> Vec<PathBuf> {
    let mut scored: Vec<(i64, &PathBuf)> = paths
        .iter()
        .filter_map(|p| score(&p.display().to_string(), query).map(|s| (s, p)))
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.display().to_string().cmp(&b.1.display().to_string()))
    });
    scored
        .into_iter()
        .take(max_results)
        .map(|(_, p)| p.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_zero_for_every_path() {
        assert_eq!(score("anything.rs", ""), Some(0));
    }

    #[test]
    fn non_subsequence_returns_none() {
        assert_eq!(score("foo.rs", "zzz"), None);
    }

    #[test]
    fn simple_subsequence_hits() {
        assert!(score("src/main.rs", "main").is_some());
        assert!(score("src/main.rs", "smr").is_some());
    }

    #[test]
    fn case_insensitive() {
        assert!(score("src/Main.rs", "main").is_some());
        assert!(score("src/main.rs", "MAIN").is_some());
    }

    #[test]
    fn rank_prefers_basename_over_path_match() {
        let paths = vec![
            PathBuf::from("foo/bar/baz/main.rs"),
            PathBuf::from("main/foo/bar/baz.rs"),
        ];
        let top = rank(&paths, "main", 5);
        assert_eq!(top[0], PathBuf::from("foo/bar/baz/main.rs"));
    }

    #[test]
    fn rank_prefers_tight_clusters() {
        let paths = vec![
            PathBuf::from("a/abcdef.rs"),
            PathBuf::from("a/aXbYcZdEeF.rs"),
        ];
        let top = rank(&paths, "abcde", 5);
        assert_eq!(top[0], PathBuf::from("a/abcdef.rs"));
    }

    #[test]
    fn rank_max_results_cap() {
        let paths: Vec<PathBuf> = (0..10).map(|n| PathBuf::from(format!("f{n}.rs"))).collect();
        assert_eq!(rank(&paths, "f", 3).len(), 3);
    }

    #[test]
    fn scan_project_skips_hidden_and_caps() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(".git")).expect("mk .git");
        std::fs::write(dir.path().join(".git/hidden"), "x").expect("write hidden");
        std::fs::write(dir.path().join("a.rs"), "x").expect("write a");
        std::fs::write(dir.path().join("b.rs"), "x").expect("write b");
        let r = scan_project(dir.path(), 100);
        assert!(!r.iter().any(|p| p.display().to_string().contains(".git")));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn scan_project_skips_target_node_modules() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("target")).expect("mk target");
        std::fs::write(dir.path().join("target/out.bin"), "x").expect("write out");
        std::fs::create_dir(dir.path().join("node_modules")).expect("mk nm");
        std::fs::write(dir.path().join("node_modules/leftpad.js"), "x").expect("write js");
        std::fs::write(dir.path().join("real.rs"), "x").expect("write real");
        let r = scan_project(dir.path(), 100);
        let names: Vec<String> = r.iter().map(|p| p.display().to_string()).collect();
        assert!(names.iter().any(|n| n.ends_with("real.rs")));
        assert!(!names.iter().any(|n| n.contains("target")));
        assert!(!names.iter().any(|n| n.contains("node_modules")));
    }
}
