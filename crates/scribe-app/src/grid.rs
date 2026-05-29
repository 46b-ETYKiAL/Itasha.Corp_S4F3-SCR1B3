//! Phase 18 T18.2 — multi-note grid foundation.
//!
//! Wraps `egui_tiles::Tree<Pane>` over the existing per-tab document model so
//! the user can open up to **6 documents side-by-side** in a resizable,
//! drag-rearrangeable grid. The grid is OPT-IN — when disabled, the central
//! editor renders today's single-pane path unchanged. When enabled, the
//! grid is the central renderer and tabs are routed to the focused pane.
//!
//! ## Concepts
//!
//! - [`DocId`] — stable monotonic identifier for each open document. Survives
//!   tree round-trip through TOML.
//! - [`Pane`] — thin handle wrapping a `DocId`. NOT a wrapper over the heavy
//!   `EditorTab` because `egui_tiles::Behavior::pane_ui` takes `&mut Pane`,
//!   and the renderer needs `&mut ScribeApp` reachable (syntax cache, LSP
//!   state, completion popups). The pane stores the id only; the look-up
//!   is `O(n)` over `Vec<EditorTab>` where `n ≤ 20`.
//! - [`MAX_PANES`] — hard cap (six). Enforced post-frame via undo-snapshot:
//!   clone the `Tree` before each frame, snap back to the pre-frame copy if
//!   `count > MAX_PANES`, toast the user. `Tree` clone at `n ≤ 12` tile-graph
//!   entries is microseconds.

use serde::{Deserialize, Serialize};

/// Hard upper bound on simultaneously visible panes. Enforced post-`on_edit`
/// via undo-snapshot. Above this, splitting/adding refuses and the editor
/// toasts a hint.
pub const MAX_PANES: usize = 6;

/// Stable, monotonically-allocated identifier for an open document. Survives
/// the egui_tiles round-trip through TOML — the type is a transparent
/// newtype over `u64` so serde encodes it as a plain integer.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocId(pub u64);

impl DocId {
    /// Pluck the integer for direct use in egui id-stack scopes.
    #[inline]
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// A leaf in the `egui_tiles::Tree`. Just a handle into the existing
/// `Vec<EditorTab>`. The pane carries no editor state of its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pane {
    pub doc_id: DocId,
}

impl Pane {
    pub fn new(doc_id: DocId) -> Self {
        Self { doc_id }
    }
}

/// Monotonic `DocId` allocator. Held by the app; bumped on every new doc.
/// The high bit will not overflow within the lifetime of any reasonable
/// session (would need `2^63` document opens).
#[derive(Debug, Default)]
pub struct DocIdAllocator {
    next: u64,
}

impl DocIdAllocator {
    pub fn next(&mut self) -> DocId {
        let id = DocId(self.next);
        self.next = self.next.wrapping_add(1);
        id
    }

    /// After loading a persisted layout, advance the allocator past any
    /// observed id so we never collide with a restored pane.
    pub fn observe(&mut self, seen: DocId) {
        if seen.0 >= self.next {
            self.next = seen.0.saturating_add(1);
        }
    }
}

/// Count the leaf panes in an `egui_tiles::Tree`. Used by the 6-pane cap
/// enforcement after `tree.ui()` runs each frame. Counts EVERY pane in the
/// tree's storage — not just the currently-visible tab; a tab container
/// holding 5 docs counts as 5 panes for cap purposes, even though only one
/// is rendered at a time.
pub fn count_panes(tree: &egui_tiles::Tree<Pane>) -> usize {
    tree.tiles
        .iter()
        .filter(|(_, tile)| matches!(tile, egui_tiles::Tile::Pane(_)))
        .count()
}

/// Build a default grid layout from a list of doc ids. Every doc becomes
/// a leaf pane inside a single tab container — visually identical to the
/// existing single-pane mode until the user splits. The id-stack key
/// `"scribe-grid"` is fixed so persistence is stable across versions.
pub fn build_default_grid(docs: &[DocId]) -> egui_tiles::Tree<Pane> {
    let mut tiles = egui_tiles::Tiles::default();
    let pane_ids: Vec<egui_tiles::TileId> = docs
        .iter()
        .map(|d| tiles.insert_pane(Pane::new(*d)))
        .collect();
    if pane_ids.is_empty() {
        return egui_tiles::Tree::empty("scribe-grid");
    }
    let root = tiles.insert_tab_tile(pane_ids);
    egui_tiles::Tree::new("scribe-grid", root, tiles)
}

// ---- Behavior<Pane> implementation ----
//
// `egui_tiles::Behavior` is the trait the tree consults to render each
// pane + decide layout policy. The MVP holds a `PaneCallbacks` struct of
// references to whatever the renderer needs to draw a pane body. The
// caller (`ScribeApp::update`) populates it on each frame and feeds it
// to `tree.ui(&mut behavior, ui)`. This avoids the "hold `&mut self`
// across the closure" problem the dossier flagged.

/// Callbacks the grid renderer dispatches to. The host app passes
/// closures that close over its own state; the `Behavior` impl below
/// only forwards method calls.
///
/// The lifetimes are explicit so the borrow checker can see that the
/// callbacks live exactly as long as the borrow of the tab vector held
/// by the host.
pub struct AppGridBehavior<'a> {
    /// `(doc_id, title)` pairs for every open doc. Used by
    /// `tab_title_for_pane`.
    pub titles: &'a [(DocId, String)],
    /// Renderer hook: paint the pane body for the given doc id into the
    /// supplied `Ui`. Returns true if the pane reported it wants to be
    /// dragged this frame (which the host can use to start a drag).
    pub render_body: &'a mut dyn FnMut(&mut egui::Ui, DocId) -> bool,
    /// Drained by the host after `tree.ui(...)` returns: doc ids the
    /// pane chrome (close button etc.) requested be closed this frame.
    pub close_requests: &'a mut Vec<DocId>,
}

impl<'a> egui_tiles::Behavior<Pane> for AppGridBehavior<'a> {
    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        let label = self
            .titles
            .iter()
            .find(|(id, _)| *id == pane.doc_id)
            .map(|(_, t)| t.as_str())
            .unwrap_or("(closed)");
        label.into()
    }

    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        _tile_id: egui_tiles::TileId,
        pane: &mut Pane,
    ) -> egui_tiles::UiResponse {
        // Small close affordance in the top-right of every pane body so
        // a tab-container of 1 (no tab strip) still has a way to dismiss.
        let drag_started = (self.render_body)(ui, pane.doc_id);
        if drag_started {
            egui_tiles::UiResponse::DragStarted
        } else {
            egui_tiles::UiResponse::None
        }
    }

    fn min_size(&self) -> f32 {
        120.0
    }

    fn gap_width(&self, _style: &egui::Style) -> f32 {
        4.0
    }

    fn retain_pane(&mut self, pane: &Pane) -> bool {
        !self.close_requests.contains(&pane.doc_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_id_allocator_monotonic() {
        let mut a = DocIdAllocator::default();
        let a0 = a.next();
        let a1 = a.next();
        let a2 = a.next();
        assert!(a0.0 < a1.0);
        assert!(a1.0 < a2.0);
    }

    #[test]
    fn doc_id_allocator_observes_higher_ids() {
        let mut a = DocIdAllocator::default();
        a.observe(DocId(42));
        let next = a.next();
        assert_eq!(next, DocId(43));
        // Observing a lower id is a no-op.
        a.observe(DocId(5));
        assert_eq!(a.next(), DocId(44));
    }

    #[test]
    fn doc_id_serialises_as_transparent_integer_via_toml() {
        #[derive(Serialize, Deserialize)]
        struct W {
            id: DocId,
        }
        let s = toml::to_string(&W { id: DocId(7) }).unwrap();
        // `#[serde(transparent)]` over a `u64` newtype → emits as a plain
        // integer, not as a `[id]` table or a `{ id = ... }` struct map.
        assert!(s.contains("id = 7"), "got {s:?}");
        let back: W = toml::from_str(&s).unwrap();
        assert_eq!(back.id, DocId(7));
    }

    #[test]
    fn build_default_grid_with_three_docs_has_three_panes() {
        let docs = [DocId(0), DocId(1), DocId(2)];
        let tree = build_default_grid(&docs);
        assert_eq!(count_panes(&tree), 3);
    }

    #[test]
    fn build_default_grid_empty_is_empty_tree() {
        let tree = build_default_grid(&[]);
        assert_eq!(count_panes(&tree), 0);
    }

    /// `Tree<Pane>` MUST clone losslessly — the 6-pane cap enforcement
    /// pattern is "clone the tree before each frame; if `count > MAX_PANES`
    /// after `tree.ui()`, snap back to the pre-frame copy". The cap is the
    /// load-bearing invariant of the foundation; if Clone regresses, the
    /// cap can't be enforced safely.
    #[test]
    fn grid_layout_clones_losslessly() {
        let docs = [DocId(10), DocId(20), DocId(30)];
        let tree = build_default_grid(&docs);
        let snapshot = tree.clone();
        assert_eq!(count_panes(&snapshot), 3);
        // The clone is independent — mutating one doesn't touch the other.
        let mut original = tree;
        original.tiles.remove(original.root().unwrap());
        // snapshot still has its panes.
        assert_eq!(count_panes(&snapshot), 3);
    }

    /// Direct TOML serialisation of `Tree<Pane>` currently fails because
    /// `egui_tiles::TileId(pub u64)` carries values that exceed `i64::MAX`
    /// (TOML's only integer width), and `toml` 0.8 rejects them at
    /// serialise time. This is a KNOWN follow-up: persistence wiring in
    /// the next PR routes through a JSON-string-in-TOML wrapper. The
    /// in-memory `Tree<Pane>` itself round-trips through `Clone` (see
    /// `grid_layout_clones_losslessly` above), which is what the 6-pane
    /// cap enforcement actually depends on at runtime.
    #[test]
    fn tree_direct_toml_serialisation_is_a_known_gap() {
        let docs = [DocId(10), DocId(20), DocId(30)];
        let tree = build_default_grid(&docs);
        #[derive(Serialize, Deserialize)]
        struct Wrap {
            tree: egui_tiles::Tree<Pane>,
        }
        let w = Wrap { tree };
        // Asserts the documented limitation. When the follow-up PR adds
        // the JSON-string-in-TOML wrapper, this test gets re-pointed.
        let r = toml::to_string(&w);
        assert!(
            r.is_err(),
            "expected direct toml serialise to fail until \
             JSON-string-in-TOML wrapper lands, got Ok with {:?}",
            r.ok()
        );
    }
}
