//! Find-navigation, scroll-to-offset, and bookmarks — extracted from `mod.rs` (A-01 wave 2).
#![allow(clippy::wildcard_imports)]

use super::*;

impl ScribeApp {
    /// F-015 — Scroll the active buffer so the given 1-based line is in the
    /// viewport. The minimap renderer already drives `pending_scroll` for
    /// click-jump; we reuse that pipe by computing the approximate Y of
    /// `line` from the current per-line gutter heights (one-frame lag is
    /// fine — same lag the minimap accepts).
    pub(super) fn goto_line(&mut self, line_1based: usize) {
        if self.active >= self.tabs.len() {
            return;
        }
        let line0 = line_1based.saturating_sub(1);
        // Prefer the captured per-line gutter Ys (most accurate; populated
        // each frame when line numbers render). Fall back to a simple
        // line-height * index estimate otherwise.
        if let Some(&y) = self.line_gutter.get(line0) {
            // line_gutter Ys are screen-Y; the editor scroll-pipe wants the
            // vertical offset INSIDE the scroll area. The minimap already
            // assumes scroll-area = full window vertically — keep that.
            self.pending_scroll = Some(y.max(0.0));
        } else {
            let lh =
                self.config.fonts.clamped_editor_size() * self.config.fonts.clamped_line_height();
            self.pending_scroll = Some((line0 as f32) * lh);
        }
        self.status = format!("go to line {line_1based}");
    }

    /// #R6 — all matches of the current find query in the active buffer (empty
    /// when there is no query / no buffer / the regex is invalid).
    ///
    /// P-01 / 4-02 R2 — memoized in `find_cache`, keyed by
    /// `(query, active tab edit_gen, doc_id)`. This function is called every
    /// frame the find bar is open (counter, highlight-all overlay, navigation);
    /// the cache makes the full-document rescan + regex recompile happen ONLY
    /// when the query, the buffer (`edit_gen`), or the active tab (`doc_id`)
    /// actually changed. On an idle frame the cached matches are cloned out and
    /// `find_all` is never re-invoked. Mirrors the `spell_cache` /
    /// `ensure_change_states` generation-keyed idiom.
    pub(super) fn find_matches_active(&self) -> Vec<scribe_core::search::Match> {
        if self.find_query.is_empty() || self.active >= self.tabs.len() {
            return Vec::new();
        }
        let tab = &self.tabs[self.active];
        let key =
            crate::find_cache::FindCacheKey::new(&self.find_query, tab.edit_gen, tab.doc_id.raw());
        // Cache HIT: query, edit generation, and active document all unchanged
        // since the cached entry — reuse the matches, no rescan, no recompile.
        if let Some(entry) = self.find_cache.borrow().as_ref() {
            if !crate::find_cache::should_recompute(Some(&entry.key), &key) {
                return entry.matches.clone();
            }
        }
        // Cache MISS: recompute once and store under the new key.
        self.find_recompute_count
            .set(self.find_recompute_count.get().wrapping_add(1));
        let q = scribe_core::search::Query {
            pattern: self.find_query.clone(),
            ..Default::default()
        };
        let matches = scribe_core::search::find_all(&tab.text, &q).unwrap_or_default();
        *self.find_cache.borrow_mut() = Some(crate::find_cache::FindCacheEntry {
            key,
            matches: matches.clone(),
        });
        matches
    }

    /// Scroll the editor so the byte offset `start` is in view, reusing the
    /// gutter-Y scroll pipe `goto_line` uses (without its status message).
    fn scroll_to_offset(&mut self, start: usize) {
        if self.active >= self.tabs.len() {
            return;
        }
        let line0 = {
            let text = &self.tabs[self.active].text;
            let clamped = start.min(text.len());
            text.as_bytes()[..clamped]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
        };
        if let Some(&y) = self.line_gutter.get(line0) {
            self.pending_scroll = Some(y.max(0.0));
        } else {
            let lh =
                self.config.fonts.clamped_editor_size() * self.config.fonts.clamped_line_height();
            self.pending_scroll = Some((line0 as f32) * lh);
        }
    }

    /// Move to the next (`forward`) or previous find match, wrapping around, and
    /// scroll it into view. No-op when there are no matches.
    pub(super) fn find_navigate(&mut self, forward: bool) {
        let matches = self.find_matches_active();
        if matches.is_empty() {
            return;
        }
        let n = matches.len();
        // Clamp first (the buffer or query may have changed since last frame).
        self.find_match_idx = self.find_match_idx.min(n - 1);
        self.find_match_idx = if forward {
            (self.find_match_idx + 1) % n
        } else {
            (self.find_match_idx + n - 1) % n
        };
        let start = matches[self.find_match_idx].start;
        self.scroll_to_offset(start);
        self.status = format!("match {} of {}", self.find_match_idx + 1, n);
    }

    /// 0-based cursor line of the active tab (from `last_cursor_line_col`,
    /// which is 1-based; defaults to line 0 when no caret has been seen yet).
    fn cursor_line0(&self) -> usize {
        self.last_cursor_line_col
            .map(|(l, _)| l.saturating_sub(1))
            .unwrap_or(0)
    }

    /// Toggle a bookmark on the active tab's cursor line.
    pub(super) fn toggle_bookmark(&mut self) {
        if self.active >= self.tabs.len() {
            return;
        }
        let line0 = self.cursor_line0();
        let bm = &mut self.tabs[self.active].bookmarks;
        if bm.remove(&line0) {
            self.status = format!("bookmark removed: line {}", line0 + 1);
        } else {
            bm.insert(line0);
            self.status = format!("bookmark added: line {}", line0 + 1);
        }
    }

    /// Jump to the next (`dir = 1`) or previous (`dir = -1`) bookmark on the
    /// active tab, wrapping around the buffer. No-op (with a status hint) when
    /// the tab has no bookmarks.
    pub(super) fn navigate_bookmark(&mut self, dir: i32) {
        if self.active >= self.tabs.len() {
            return;
        }
        let from = self.cursor_line0();
        let target = pick_bookmark(&self.tabs[self.active].bookmarks, from, dir);
        match target {
            Some(line0) => self.goto_line(line0 + 1),
            None => self.status = "no bookmarks in this buffer".to_string(),
        }
    }
}
