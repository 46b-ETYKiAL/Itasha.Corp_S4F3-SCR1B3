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

/// Walk `root` and collect every match of `query` synchronously into one Vec.
/// Hidden entries (basename starting with `.`) and the `SKIP_DIRS` are pruned;
/// binary/oversized files are skipped; stops at the caps above.
///
/// 4-02 — the interactive UI no longer calls this on the frame thread; it drives
/// the project search OFF-thread via [`spawn_search`], which reuses the shared
/// [`walk_project`] core with a streaming callback. This synchronous variant is
/// retained as the test oracle the streaming worker's output is validated
/// against (so it is `#[cfg(test)]`-only — keeping it in the production binary
/// would be dead code now that the UI streams).
#[cfg(test)]
pub fn search_project(root: &Path, query: &Query) -> Vec<FileMatch> {
    let mut out = Vec::new();
    if query.pattern.is_empty() {
        return out;
    }
    // Collect every per-file batch into one flat Vec, preserving the caps.
    walk_project(root, query, &mut |batch| {
        out.extend(batch);
        true // keep walking until the caps in `walk_project` stop us
    });
    out
}

/// Core directory walk. For each file with at least one match, the file's match
/// batch is handed to `on_batch`; the walk continues only while `on_batch`
/// returns `true` (so a streaming worker can stop early when its receiver is
/// gone). Hidden entries / `SKIP_DIRS` are pruned; binary/oversized files are
/// skipped; the `MAX_*` caps bound total work. The per-file batch granularity is
/// what lets the off-thread search stream partial results to the UI.
fn walk_project(root: &Path, query: &Query, on_batch: &mut dyn FnMut(Vec<FileMatch>) -> bool) {
    if query.pattern.is_empty() {
        return;
    }
    let mut files_seen = 0usize;
    let mut total_matches = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if files_seen >= MAX_FILES_SCANNED || total_matches >= MAX_TOTAL_MATCHES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if total_matches >= MAX_TOTAL_MATCHES {
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
                // Scan one file into a fresh batch; respect the global match cap.
                let mut batch = Vec::new();
                let remaining = MAX_TOTAL_MATCHES - total_matches;
                search_one_file(&p, query, &mut batch);
                if batch.len() > remaining {
                    batch.truncate(remaining);
                }
                if !batch.is_empty() {
                    total_matches += batch.len();
                    if !on_batch(batch) {
                        return; // consumer asked us to stop (receiver dropped)
                    }
                }
            }
        }
    }
}

/// A message streamed from the off-thread project-find worker back to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchMsg {
    /// A batch of matches from one scanned file. Sent progressively as the walk
    /// proceeds so the results pane fills in without the UI ever blocking.
    Batch(Vec<FileMatch>),
    /// The walk finished (sent exactly once, after the last batch).
    Done,
}

/// Spawn the project-wide search on a background thread, streaming
/// [`SearchMsg`] batches back over the returned receiver and calling
/// `on_progress` after each send so the caller can request a repaint. The frame
/// thread NEVER blocks on the fs walk (4-02). Dropping the receiver makes the
/// next send fail, which stops the walk early — so a superseding search just
/// drops the old receiver, no cancellation flag needed.
pub fn spawn_search(
    root: PathBuf,
    query: Query,
    on_progress: impl Fn() + Send + 'static,
) -> std::sync::mpsc::Receiver<SearchMsg> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("scr1b3-find-in-files".into())
        .spawn(move || {
            walk_project(&root, &query, &mut |batch| {
                // `send` fails iff the receiver was dropped (search superseded /
                // pane closed) — return false to stop the walk immediately.
                let alive = tx.send(SearchMsg::Batch(batch)).is_ok();
                if alive {
                    on_progress();
                }
                alive
            });
            // Signal completion (ignore the error if the receiver is gone).
            if tx.send(SearchMsg::Done).is_ok() {
                on_progress();
            }
        })
        .expect("spawning the find-in-files worker thread should not fail");
    rx
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

    // --- 4-02: off-thread streaming worker -----------------------------------

    #[test]
    fn spawn_search_streams_all_matches_off_thread() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha\nbeta TODO\ngamma").unwrap();
        std::fs::write(dir.path().join("b.txt"), "no match\nTODO again").unwrap();

        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p = progress.clone();
        let rx = spawn_search(dir.path().to_path_buf(), q("TODO"), move || {
            p.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });

        // Collect every streamed batch until Done, off the calling thread.
        let mut all = Vec::new();
        let mut saw_done = false;
        while let Ok(msg) = rx.recv() {
            match msg {
                SearchMsg::Batch(b) => all.extend(b),
                SearchMsg::Done => {
                    saw_done = true;
                    break;
                }
            }
        }
        assert!(saw_done, "the worker must emit a terminal Done");
        assert_eq!(all.len(), 2, "both TODO matches arrive via the channel");
        // The streamed results equal what the synchronous walk would produce.
        let sync = search_project(dir.path(), &q("TODO"));
        assert_eq!(all.len(), sync.len());
        for m in &sync {
            assert!(all.contains(m), "streamed results match the sync walk");
        }
        assert!(
            progress.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "on_progress fired at least once (repaint requested)"
        );
    }

    #[test]
    fn spawn_search_dropping_receiver_stops_worker() {
        // A superseding search drops the old receiver; the worker's next send
        // then fails and the walk stops. Dropping must not panic; the worker
        // thread exits cleanly. We can't observe the early stop directly, but we
        // assert the contract holds (no panic, the new search still works).
        let dir = tempfile::tempdir().unwrap();
        for i in 0..50 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "TODO line").unwrap();
        }
        let rx = spawn_search(dir.path().to_path_buf(), q("TODO"), || {});
        drop(rx); // supersede immediately

        // A fresh search after the drop still returns correct results.
        let rx2 = spawn_search(dir.path().to_path_buf(), q("TODO"), || {});
        let mut count = 0;
        while let Ok(msg) = rx2.recv() {
            match msg {
                SearchMsg::Batch(b) => count += b.len(),
                SearchMsg::Done => break,
            }
        }
        assert_eq!(count, 50, "the second search streams all 50 matches");
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
