//! Per-frame find cache for the in-buffer find bar (P-01 / 4-02 R2).
//!
//! # Why
//!
//! While the find bar is open, the editor render path calls
//! `scribe_core::search::find_all(&text, &query)` on the active buffer **every
//! frame** — from the find-bar counter (`{i}/{n}`), the highlight-all overlay,
//! and navigation. That is a full-document rescan AND a fresh regex compile per
//! frame, even when neither the query nor the document changed since the last
//! frame. On a large buffer that pegs a core for nothing.
//!
//! # Fix
//!
//! Memoize the match list keyed by `(query, edit_gen, doc_id)` — the SAME
//! generation-keyed idiom the spell-check and change-bar caches already use
//! (`edit_gen` is bumped on every edit; `doc_id` disambiguates tabs that happen
//! to share an `edit_gen` in the single-slot cache). The matches are recomputed
//! ONLY when the cache key moves; on an idle frame the cached `Vec<Match>` is
//! cloned out, so `find_all` is never re-invoked.
//!
//! The recompute *decision* is factored into [`FindCacheKey`] +
//! [`should_recompute`] so it is a pure, side-effect-free predicate the unit
//! tests can drive without standing up an egui frame.

use scribe_core::search::Match;

/// The identity of a cached find result. Two keys are equal iff the cached
/// match list is still valid: same query text, same per-tab edit generation,
/// same document id. Any change to one of these invalidates the memo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindCacheKey {
    /// The find-bar query string the matches were computed for.
    pub query: String,
    /// The active tab's monotonic edit generation at compute time. Bumped on
    /// every edit, so an edit invalidates the cache.
    pub edit_gen: u64,
    /// The active document id, so switching tabs (which can share an `edit_gen`)
    /// invalidates the single-slot cache.
    pub doc_id: u64,
}

impl FindCacheKey {
    /// Build a key from the live find/tab state.
    pub fn new(query: &str, edit_gen: u64, doc_id: u64) -> Self {
        Self {
            query: query.to_string(),
            edit_gen,
            doc_id,
        }
    }
}

/// A cached find result: the key it was computed for, plus the matches.
#[derive(Debug, Clone)]
pub struct FindCacheEntry {
    pub key: FindCacheKey,
    pub matches: Vec<Match>,
}

/// Pure predicate: does the cached entry need recomputing for `key`?
///
/// `None` (no entry yet) always recomputes; otherwise recompute iff the key
/// changed. This is the load-bearing "no recompute on idle frame" decision —
/// when the query, edit generation, and document are all unchanged, it returns
/// `false` and the caller reuses the cached matches without calling `find_all`.
pub fn should_recompute(cached: Option<&FindCacheKey>, key: &FindCacheKey) -> bool {
    match cached {
        None => true,
        Some(prev) => prev != key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(start: usize, end: usize) -> Match {
        Match { start, end }
    }

    #[test]
    fn no_entry_recomputes() {
        let key = FindCacheKey::new("foo", 0, 0);
        assert!(
            should_recompute(None, &key),
            "an empty cache must recompute"
        );
    }

    #[test]
    fn identical_key_does_not_recompute() {
        // The core P-01 proof at the predicate level: same query, same
        // edit_gen, same doc -> NO recompute across idle frames.
        let key = FindCacheKey::new("foo", 7, 3);
        let cached = key.clone();
        assert!(
            !should_recompute(Some(&cached), &key),
            "an idle frame (unchanged query+edit_gen+doc) must NOT recompute"
        );
    }

    #[test]
    fn query_change_invalidates() {
        let cached = FindCacheKey::new("foo", 7, 3);
        let key = FindCacheKey::new("bar", 7, 3);
        assert!(
            should_recompute(Some(&cached), &key),
            "changing the query must recompute"
        );
    }

    #[test]
    fn edit_gen_bump_invalidates() {
        // An edit bumps edit_gen -> the cache must recompute even though the
        // query string is identical.
        let cached = FindCacheKey::new("foo", 7, 3);
        let key = FindCacheKey::new("foo", 8, 3);
        assert!(
            should_recompute(Some(&cached), &key),
            "an edit (edit_gen bump) must recompute"
        );
    }

    #[test]
    fn tab_switch_invalidates() {
        // Two tabs can share an edit_gen; the doc_id is what disambiguates the
        // single-slot cache on a tab switch.
        let cached = FindCacheKey::new("foo", 7, 3);
        let key = FindCacheKey::new("foo", 7, 4);
        assert!(
            should_recompute(Some(&cached), &key),
            "switching tabs (doc_id change) must recompute"
        );
    }

    #[test]
    fn entry_round_trips_matches() {
        // Sanity: the entry carries both the key and the matches it cached.
        let key = FindCacheKey::new("foo", 1, 2);
        let entry = FindCacheEntry {
            key: key.clone(),
            matches: vec![m(0, 3), m(10, 13)],
        };
        assert_eq!(entry.key, key);
        assert_eq!(entry.matches.len(), 2);
        assert!(!should_recompute(Some(&entry.key), &key));
    }
}
