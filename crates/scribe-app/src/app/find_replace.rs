//! Find/replace surface: in-buffer Replace (F-008) plus the find-in-files run/drain/open-result trio. Bodies moved verbatim from the `app` god-module (A-01 decomposition); `use super::*` re-exports the types these methods touch.
#![allow(clippy::wildcard_imports)]
use super::*;

impl ScribeApp {
    /// F-008 — Replace `find_query` with `replace_query` in the active
    /// buffer. `all=true` replaces every match; `all=false` replaces only the
    /// first. Matches with the SAME engine the find bar highlights with
    /// ([`find_matches_active`] → a default literal, case-insensitive
    /// [`scribe_core::search::Query`]), so Replace touches exactly what the
    /// user sees highlighted. Skips when the find field is empty.
    ///
    /// Root cause of the prior divergence: this used `str::replace` /
    /// `str::find`, which are case-SENSITIVE, while the find bar highlights
    /// case-INSENSITIVELY — so "Replace All" silently skipped the case-variant
    /// matches the find bar had highlighted (find said 3, replace changed 1).
    /// Unifying both surfaces on `find_all` eliminates the divergence. The
    /// replacement is spliced LITERALLY (the find bar exposes no regex toggle,
    /// so `$1`-style regex expansion must not leak in).
    pub(super) fn replace_in_active(&mut self, all: bool) {
        if self.find_query.is_empty() || self.active >= self.tabs.len() {
            return;
        }
        let pat = self.find_query.clone();
        let rep = self.replace_query.clone();
        let q = scribe_core::search::Query {
            pattern: pat.clone(),
            ..Default::default()
        };
        let matches =
            scribe_core::search::find_all(&self.tabs[self.active].text, &q).unwrap_or_default();
        if matches.is_empty() {
            self.status = format!("no match for '{pat}'");
            return;
        }
        // Splice right-to-left so earlier byte offsets stay valid as the text
        // shifts. Match spans are UTF-8 char boundaries (regex guarantee), so
        // `replace_range` cannot panic on a boundary.
        let text = &mut self.tabs[self.active].text;
        let spans = if all { &matches[..] } else { &matches[..1] };
        for m in spans.iter().rev() {
            text.replace_range(m.start..m.end, &rep);
        }
        self.status = if all {
            format!("replaced {} x '{pat}' -> '{rep}'", matches.len())
        } else {
            format!("replaced '{pat}' -> '{rep}'")
        };
        // Wave-3: invalidate the gen-keyed minimap/spell caches.
        let i = self.active;
        self.tabs[i].edit_gen = self.tabs[i].edit_gen.wrapping_add(1);
    }

    /// Wave-5 / 4-02: run the project-wide search over the open folder into the
    /// results pane. Reuses the in-buffer find engine + the open file-tree root.
    ///
    /// 4-02 — the fs walk + per-file scan runs OFF the egui frame thread on a
    /// spawned worker (`find_in_files::spawn_search`), streaming results back
    /// over `find_in_files_rx`. The UI shows partial results as they arrive and
    /// never blocks on a big tree. Starting a new search drops the previous
    /// receiver (assigning `Some(rx)` over the old one), which makes the orphaned
    /// worker's next send fail and stop the walk — the latest query supersedes
    /// the old one without an explicit cancellation flag.
    pub(super) fn run_find_in_files(&mut self, ctx: &egui::Context) {
        self.find_in_files_error = None;
        self.find_in_files_results.clear();
        // PA-02: a fresh search invalidates the old keyboard-selection index.
        self.find_in_files_selected = 0;
        // Dropping any in-flight receiver supersedes the previous search.
        self.find_in_files_rx = None;
        self.find_in_files_running = false;
        let Some(root) = self.file_tree_root.clone() else {
            self.find_in_files_error = Some("open a folder first".into());
            return;
        };
        let query = scribe_core::search::Query {
            pattern: self.find_in_files_query.clone(),
            regex: self.find_in_files_regex,
            case_sensitive: false,
            whole_word: false,
        };
        if query.pattern.is_empty() {
            return;
        }
        // Surface a bad regex once (the per-file search swallows it silently).
        if query.regex {
            if let Err(e) = scribe_core::search::find_all("", &query) {
                self.find_in_files_error = Some(format!("bad regex: {e}"));
                return;
            }
        }
        // Spawn the walk off-thread; each streamed batch requests a repaint so
        // the partial results land promptly even while the user is idle.
        let ctx = ctx.clone();
        self.find_in_files_rx = Some(crate::find_in_files::spawn_search(root, query, move || {
            ctx.request_repaint();
        }));
        self.find_in_files_running = true;
        self.status = "searching…".into();
    }

    /// 4-02 — drain any batches the off-thread project-find worker streamed back,
    /// appending them to the results pane. Called once per frame from
    /// `frame_tick`. Non-blocking (`try_recv`); when `Done` arrives (or the
    /// channel disconnects) the receiver is dropped and the running flag clears.
    pub(super) fn drain_find_in_files(&mut self) {
        let Some(rx) = self.find_in_files_rx.as_ref() else {
            return;
        };
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(crate::find_in_files::SearchMsg::Batch(batch)) => {
                    self.find_in_files_results.extend(batch);
                }
                Ok(crate::find_in_files::SearchMsg::Done) => {
                    finished = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.find_in_files_rx = None;
            self.find_in_files_running = false;
            self.status = format!("{} match(es)", self.find_in_files_results.len());
        }
    }

    /// Wave-5: open `path` in a tab (reusing the normal open path) then scroll to
    /// 1-based `line` (the click-to-open target from the results pane).
    pub(super) fn open_find_in_files_result(&mut self, path: PathBuf, line: usize) {
        self.open_path(path);
        self.goto_line(line.max(1));
    }
}
