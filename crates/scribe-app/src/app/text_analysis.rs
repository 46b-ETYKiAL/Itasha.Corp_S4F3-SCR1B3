//! Text-analysis methods for `ScribeApp`: spellcheck memoization (count,
//! per-frame borrow, cache key, compute), symbol-scope and document-count
//! caches, plus spell-engine reload. Extracted from `mod.rs` (A-01 wave 3 —
//! behavior-preserving move; methods widened to `pub(super)` for the parent
//! and sibling call-sites, matching the `text_ops_methods` extraction).
#![allow(clippy::wildcard_imports)]

use super::*;

impl ScribeApp {
    /// Count misspellings in the active buffer when spellcheck is enabled.
    ///
    /// P-08: reads the memoized vec length through a borrow -- it does NOT
    /// clone the cached `Vec<Misspelling>` (the status-bar count runs every
    /// frame, so the per-frame clone-just-to-call-`.len()` was pure waste).
    pub(super) fn spell_count(&self) -> usize {
        self.with_active_misspellings(|m| m.len())
    }

    /// Ensure the active buffer misspelling memo is current and run `f` over a
    /// BORROW of the cached slice (no clone). Shared by the status-bar count
    /// (`spell_count`, which only needs `.len()`) and the squiggle painter
    /// (`misspellings_for_active`, which clones exactly once because its owned
    /// snapshot has to outlive a later `&mut self` borrow). `f` sees an empty
    /// slice when spellcheck is off or there is no active buffer.
    pub(super) fn with_active_misspellings<R>(
        &self,
        f: impl FnOnce(&[spell::Misspelling]) -> R,
    ) -> R {
        if !self.config.spellcheck.enabled {
            return f(&[]);
        }
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        if self.tabs.get(active).is_none() {
            return f(&[]);
        }
        let key = self.spell_cache_key(active);
        // Cache hit: borrow the cached vec and hand the slice to `f` (no clone).
        if let Some((k, v)) = self.spell_cache.borrow().as_ref() {
            if *k == key {
                return f(v);
            }
        }
        // Miss: recompute, store, then hand the stored slice to `f` via borrow.
        let result = self.compute_misspellings(active);
        *self.spell_cache.borrow_mut() = Some((key, result));
        let slot = self.spell_cache.borrow();
        f(&slot.as_ref().expect("just stored above").1)
    }

    /// Content+config cache key for the active buffer misspelling memo: the
    /// per-tab `edit_gen` (so the scan re-runs only on a real edit, not every
    /// frame), the `doc_id` (disambiguates tabs sharing an `edit_gen` in the
    /// single-slot cache), the three scope toggles, and the language hint.
    pub(super) fn spell_cache_key(&self, active: usize) -> u64 {
        use std::hash::{Hash, Hasher};
        let Some(tab) = self.tabs.get(active) else {
            return 0;
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        tab.edit_gen.hash(&mut h);
        tab.doc_id.raw().hash(&mut h);
        self.config.spellcheck.check_comments.hash(&mut h);
        self.config.spellcheck.check_strings.hash(&mut h);
        self.config.spellcheck.check_identifiers.hash(&mut h);
        tab.doc.language_hint().hash(&mut h);
        h.finish()
    }

    /// Misspellings in the active buffer (#78), memoized by a content+config
    /// hash so the dictionary scan runs once per changed frame and is shared by
    /// the status-bar count and the editor underline painter. Empty when
    /// spellcheck is off or there is no active buffer.
    ///
    /// P-08: this returns an OWNED snapshot because its caller (the editor
    /// closure) holds the result across a later `&mut self` borrow, so a
    /// `Ref`/`&[..]` cannot be used there. The clone is now confined to this
    /// one call site -- `spell_count` reads the cache via
    /// `with_active_misspellings` without cloning.
    pub(super) fn misspellings_for_active(&self) -> Vec<spell::Misspelling> {
        self.with_active_misspellings(|m| m.to_vec())
    }

    /// Run the dictionary scan for the active buffer (the cache-miss body,
    /// factored out of `with_active_misspellings`).
    pub(super) fn compute_misspellings(&self, active: usize) -> Vec<spell::Misspelling> {
        let Some(tab) = self.tabs.get(active) else {
            return Vec::new();
        };
        // Scope the check to the requested token classes (comments / strings /
        // identifiers) using the highlighter's classified spans, so the three
        // Settings toggles actually constrain what gets flagged. With all three
        // off, or no derivable syntax, `check_text_scoped` falls back to the
        // whole-text behavior (no regression).
        let scope = spell::SpellScope::new(
            self.config.spellcheck.check_comments,
            self.config.spellcheck.check_strings,
            self.config.spellcheck.check_identifiers,
        );
        let ext = tab.doc.language_hint();
        let spans = self.hl.classify_document(&tab.text, ext.as_deref());
        // Scoping (comments / strings / identifiers) is a CODE concept. When the
        // buffer has no code structure — an untitled note, plain text, markdown —
        // those classes don't apply, so check the whole document as prose. Only
        // when there are real comment/string/identifier spans do the toggles
        // constrain the check.
        let has_code_structure = spans
            .iter()
            .any(|s| !matches!(s.class, spell::SpanClass::Other));
        if has_code_structure {
            spell::check_text_scoped(&self.spell, &tab.text, &spans, scope)
        } else {
            spell::check_text(&self.spell, &tab.text, true)
        }
    }

    /// P-05: brace-delimited definition scopes for the active buffer, memoized
    /// by `(edit_gen, doc_id)` so the O(n) `symbol_scopes` char scan that drives
    /// the breadcrumb bar + sticky-scroll headers runs ONLY on an edit or a tab
    /// switch, not every frame. A 1-frame-stale breadcrumb after a keystroke is
    /// visually harmless (same rationale as the spell + minimap memos). Returns
    /// an owned snapshot because the caller holds it across a later `&mut self`
    /// borrow. Buffers over `MAX_SYMBOL_SCAN_BYTES` are not scanned (the scan
    /// stays bounded), matching the prior inline guard.
    pub(super) fn symbol_scopes_for_active(&self) -> Vec<crate::editor_features::SymbolScope> {
        /// Upper buffer size for the breadcrumb/sticky symbol scan.
        const MAX_SYMBOL_SCAN_BYTES: usize = 500_000;
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        let Some(tab) = self.tabs.get(active) else {
            return Vec::new();
        };
        if tab.text.len() > MAX_SYMBOL_SCAN_BYTES {
            return Vec::new();
        }
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            tab.edit_gen.hash(&mut h);
            tab.doc_id.raw().hash(&mut h);
            h.finish()
        };
        if let Some((k, v)) = self.symbol_cache.borrow().as_ref() {
            if *k == key {
                return v.clone();
            }
        }
        // Cache miss: run the scan, record that it re-ran (proof counter), store.
        self.symbol_scan_count
            .set(self.symbol_scan_count.get().wrapping_add(1));
        let scopes = crate::editor_features::symbol_scopes(&tab.text);
        *self.symbol_cache.borrow_mut() = Some((key, scopes.clone()));
        scopes
    }

    /// PA-04 / PA-05 — memoized `(lines, words, chars)` for the tab at `active`,
    /// keyed by `(edit_gen, doc_id)`. The status bar (line/word/char readout) and
    /// the sticky line-number gutter (digit-width) both walked the WHOLE buffer
    /// every frame; this caches the three `O(n)` passes so they recompute ONLY on
    /// a real edit or a tab switch (a 1-frame-stale count after a keystroke is
    /// harmless — `edit_gen` moves on the next frame), not on every idle frame.
    /// Word/char are 0 for `is_read_only_large()` buffers (the multi-GB rope-
    /// browser path), exactly as the un-memoized status bar short-circuited.
    /// Mirrors the `symbol_scopes_for_active` / `spell_count` memo idiom.
    pub(super) fn doc_counts_active(&self, active: usize) -> DocCounts {
        let Some(tab) = self.tabs.get(active) else {
            return (1, 0, 0);
        };
        if let Some((gen, doc, counts)) = self.count_cache.borrow().as_ref() {
            if *gen == tab.edit_gen && *doc == tab.doc_id {
                return *counts;
            }
        }
        // Cache miss: re-walk the buffer once, record the re-walk (proof counter),
        // store keyed on (edit_gen, doc_id).
        self.count_recompute_count
            .set(self.count_recompute_count.get().wrapping_add(1));
        let lines = tab.text.lines().count().max(1);
        let (words, chars) = if tab.doc.is_read_only_large() {
            (0, 0)
        } else {
            (
                tab.text.split_whitespace().count(),
                tab.text.chars().count(),
            )
        };
        let counts = (lines, words, chars);
        *self.count_cache.borrow_mut() = Some((tab.edit_gen, tab.doc_id, counts));
        counts
    }

    /// Rebuild the spell engine from the current config — called after the user
    /// changes the spellcheck language or custom dictionary in Settings so the
    /// new dictionary takes effect without a restart.
    pub(super) fn reload_spell_engine(&mut self) {
        self.spell = build_spell_engine(&self.config);
    }
}
