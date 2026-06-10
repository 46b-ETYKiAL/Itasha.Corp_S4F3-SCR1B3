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
}
