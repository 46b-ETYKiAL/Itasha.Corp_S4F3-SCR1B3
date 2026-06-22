//! Project-wide find: walk the open folder, run the existing search engine over
//! each text file, and collect file:line matches for a navigable results pane.
//! Pure-Rust, on-device, zero network. Reuses `scribe_core::search::Query` so the
//! regex / case / whole-word semantics match the in-buffer find bar exactly.

use scribe_core::search::{self, Query};
use std::path::{Path, PathBuf};

/// One match in one file: the path, 1-based line number + column, the full line
/// text (display-trimmed) so the results pane can show context without
/// re-reading, and the byte offset of the match start for click-to-open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatch {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
    pub line_text: String,
    pub byte_start: usize,
}

/// Caps so a huge tree / huge file can't stall the UI. Conservative —
/// find-in-files is interactive.
pub const MAX_FILES_SCANNED: usize = 20_000;
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024; // skip files > 8 MiB
pub const MAX_TOTAL_MATCHES: usize = 5_000;
pub const MAX_LINE_DISPLAY: usize = 240;

/// Directories never indexed (mirrors `fuzzy`'s skip list).
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "build",
    "dist",
    "out",
    "__pycache__",
    ".git",
];

/// Walk `root` and collect every match of `query`. Hidden entries (basename
/// starting with `.`) and the `SKIP_DIRS` are pruned; binary/oversized files are
/// skipped. Stops at the caps above.
pub fn search_project(root: &Path, query: &Query) -> Vec<FileMatch> {
    let mut out = Vec::new();
    if query.pattern.is_empty() {
        return out;
    }
    let mut files_seen = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if files_seen >= MAX_FILES_SCANNED || out.len() >= MAX_TOTAL_MATCHES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if out.len() >= MAX_TOTAL_MATCHES {
                break;
            }
            let p = entry.path();
            let base = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if base.starts_with('.') {
                continue;
            }
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                if !SKIP_DIRS.contains(&base) {
                    stack.push(p);
                }
            } else if meta.is_file() {
                if meta.len() > MAX_FILE_BYTES {
                    continue;
                }
                files_seen += 1;
                search_one_file(&p, query, &mut out);
            }
        }
    }
    out
}

/// Read one file as UTF-8 (lossy) and append its matches. Skips files that are
/// not valid text (a NUL byte in the first 1 KiB → treated as binary).
fn search_one_file(path: &Path, query: &Query, out: &mut Vec<FileMatch>) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let probe = &bytes[..bytes.len().min(1024)];
    if probe.contains(&0) {
        return; // binary
    }
    let text = String::from_utf8_lossy(&bytes);
    let matches = match search::find_all(&text, query) {
        Ok(m) => m,
        Err(_) => return, // bad regex — surfaced once by the caller, not per file
    };
    for m in matches {
        if out.len() >= MAX_TOTAL_MATCHES {
            break;
        }
        // Map byte offset → (line, col) 1-based.
        let prefix = &text[..m.start];
        let line = prefix.matches('\n').count() + 1;
        let line_start = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col = text[line_start..m.start].chars().count() + 1;
        let line_end = text[m.start..]
            .find('\n')
            .map(|i| m.start + i)
            .unwrap_or(text.len());
        let mut line_text = text[line_start..line_end].to_string();
        if line_text.chars().count() > MAX_LINE_DISPLAY {
            line_text = line_text.chars().take(MAX_LINE_DISPLAY).collect::<String>() + "…";
        }
        out.push(FileMatch {
            path: path.to_path_buf(),
            line,
            col,
            line_text,
            byte_start: m.start,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn q(p: &str) -> Query {
        Query {
            pattern: p.into(),
            ..Default::default()
        }
    }

    #[test]
    fn finds_matches_across_files_with_line_numbers() {
        let dir = std::env::temp_dir().join(format!("sib_fif_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let mut f = std::fs::File::create(dir.join("a.txt")).unwrap();
        writeln!(f, "alpha\nbeta TODO\ngamma").unwrap();
        let mut g = std::fs::File::create(dir.join("b.txt")).unwrap();
        writeln!(g, "no match here\nTODO again").unwrap();
        let hits = search_project(&dir, &q("TODO"));
        assert_eq!(hits.len(), 2);
        assert!(hits
            .iter()
            .any(|h| h.line == 2 && h.path.ends_with("a.txt")));
        assert!(hits
            .iter()
            .any(|h| h.line == 2 && h.path.ends_with("b.txt")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_binary_files() {
        let dir = std::env::temp_dir().join(format!("sib_fif_bin_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("bin"), [0u8, 1, 2, b'T', b'O', b'D', b'O']).unwrap();
        let hits = search_project(&dir, &q("TODO"));
        assert!(hits.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_pattern_yields_nothing() {
        let dir = std::env::temp_dir().join(format!("sib_fif_empty_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert!(search_project(&dir, &q("")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_hidden_entries_and_skip_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // A hidden file and a file under a SKIP_DIR must NOT be searched; a normal
        // sibling file must be.
        std::fs::write(dir.path().join(".hidden.txt"), "TODO in hidden").unwrap();
        let skip = dir.path().join("node_modules");
        std::fs::create_dir_all(&skip).unwrap();
        std::fs::write(skip.join("dep.txt"), "TODO in node_modules").unwrap();
        std::fs::write(dir.path().join("real.txt"), "TODO in real file").unwrap();

        let hits = search_project(dir.path(), &q("TODO"));
        assert_eq!(hits.len(), 1, "only the visible, non-skipped file matches");
        assert!(hits[0].path.ends_with("real.txt"));
    }

    #[test]
    fn skips_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        // A file just over the 8 MiB cap is skipped even though it contains the
        // pattern; a small file is searched.
        let mut big = "x".repeat((MAX_FILE_BYTES as usize) + 16);
        big.push_str("\nTODO big\n");
        std::fs::write(dir.path().join("big.txt"), big).unwrap();
        std::fs::write(dir.path().join("small.txt"), "TODO small").unwrap();

        let hits = search_project(dir.path(), &q("TODO"));
        assert_eq!(hits.len(), 1, "the oversized file is skipped");
        assert!(hits[0].path.ends_with("small.txt"));
    }

    #[test]
    fn reports_one_based_line_and_column() {
        let dir = tempfile::tempdir().unwrap();
        // A match NOT at column 1 on a later line exercises the line/col mapping.
        std::fs::write(dir.path().join("c.txt"), "first\nab TODO cd\n").unwrap();
        let hits = search_project(dir.path(), &q("TODO"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2, "1-based line");
        assert_eq!(hits[0].col, 4, "1-based column past 'ab '");
        assert_eq!(hits[0].line_text, "ab TODO cd");
    }

    #[test]
    fn truncates_very_long_match_lines() {
        let dir = tempfile::tempdir().unwrap();
        // A line far longer than MAX_LINE_DISPLAY with a match is truncated with
        // an ellipsis so the results pane stays bounded.
        let mut long = String::from("TODO");
        long.push_str(&"-".repeat(MAX_LINE_DISPLAY + 100));
        std::fs::write(dir.path().join("long.txt"), long).unwrap();
        let hits = search_project(dir.path(), &q("TODO"));
        assert_eq!(hits.len(), 1);
        assert!(
            hits[0].line_text.ends_with('…'),
            "an over-long line is ellipsis-truncated"
        );
        assert_eq!(
            hits[0].line_text.chars().count(),
            MAX_LINE_DISPLAY + 1,
            "truncated to the display cap plus the ellipsis"
        );
    }
}
