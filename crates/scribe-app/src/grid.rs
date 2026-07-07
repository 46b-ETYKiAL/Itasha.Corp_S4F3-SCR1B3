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
//! - [`MAX_PANES`] — hard cap (six), enforced at BUILD time:
//!   [`build_default_grid`] only ever lays out the first `MAX_PANES` documents,
//!   so the tree never holds more than six panes. Extra documents stay open as
//!   tabs and the editor toasts to say so.

use serde::{Deserialize, Serialize};

/// Hard upper bound on simultaneously visible panes. Enforced by
/// [`build_default_grid`], which caps the doc list it lays out. Above this,
/// extra documents remain open as tabs (not shown in the grid) and the editor
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
///
/// `DocId(0)` is RESERVED as the never-allocated "unassigned" sentinel — a tab
/// constructed before it joins the grid carries id 0, and `sync_grid_state`
/// reassigns any id-0 tab a real id. So the allocator starts at 1; if it ever
/// handed out 0 it would collide with that sentinel and the grid's
/// pane->tab look-up could map several panes onto the same tab.
#[derive(Debug)]
pub struct DocIdAllocator {
    next: u64,
}

impl Default for DocIdAllocator {
    fn default() -> Self {
        Self { next: 1 }
    }
}

impl DocIdAllocator {
    pub fn next(&mut self) -> DocId {
        // Never hand out 0 (the reserved sentinel), even across the practically
        // unreachable u64 wrap.
        let id = DocId(self.next.max(1));
        self.next = self.next.wrapping_add(1).max(1);
        id
    }

    /// After loading a persisted layout, advance the allocator past any
    /// observed id so we never collide with a restored pane.
    pub fn observe(&mut self, seen: DocId) {
        if seen.0 >= self.next {
            self.next = seen.0.saturating_add(1).max(1);
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

/// Build a default grid layout from a list of doc ids. Every doc becomes a
/// leaf pane inside a single **grid** container, so the open documents render
/// side-by-side (egui_tiles auto-arranges rows×columns and lets the user drag
/// to rearrange). Crucially this is NOT a *tab* container: a tab container
/// paints its own tab strip inside the editor area, which is the "second row
/// of tabs" the user reported. A grid container has no tab strip — the top-bar
/// tab list stays the single source of truth for which documents are open, and
/// the grid just shows them all at once. The id-stack key `"scribe-grid"` is
/// fixed so persistence is stable across versions.
///
/// The doc list is capped at [`MAX_PANES`] — the grid never builds more than
/// six panes (the enforcement the module promised but previously only toasted
/// about). Extra documents stay open as tabs; they just aren't shown in the
/// grid until a pane frees up.
pub fn build_default_grid(docs: &[DocId]) -> egui_tiles::Tree<Pane> {
    let docs = &docs[..docs.len().min(MAX_PANES)];
    let mut tiles = egui_tiles::Tiles::default();
    let pane_ids: Vec<egui_tiles::TileId> = docs
        .iter()
        .map(|d| tiles.insert_pane(Pane::new(*d)))
        .collect();
    if pane_ids.is_empty() {
        return egui_tiles::Tree::empty("scribe-grid");
    }
    let root = tiles.insert_grid_tile(pane_ids);
    egui_tiles::Tree::new("scribe-grid", root, tiles)
}

/// Serialise the grid layout to a JSON string for persistence. A direct TOML
/// round-trip fails on egui_tiles' `Container::Tabs.height` serialise/
/// deserialise asymmetry; JSON of the Grid-container trees this module builds
/// round-trips cleanly (see [`from_json`]). Returns `None` on serialisation
/// failure (treated by callers as "no saved layout").
pub fn to_json(tree: &egui_tiles::Tree<Pane>) -> Option<String> {
    serde_json::to_string(tree).ok()
}

/// Rebuild a grid layout from a [`to_json`] string. Returns `None` on any parse
/// error so a corrupt or stale saved layout degrades to a freshly-built default
/// rather than refusing to open.
pub fn from_json(s: &str) -> Option<egui_tiles::Tree<Pane>> {
    serde_json::from_str(s).ok()
}

/// The set of doc ids currently represented by a pane in the tree. Used to
/// reconcile the grid against the open-tab set: a tab with no pane needs one
/// added; a pane whose tab was closed is pruned via `retain_pane`.
pub fn pane_doc_ids(tree: &egui_tiles::Tree<Pane>) -> std::collections::BTreeSet<DocId> {
    tree.tiles
        .iter()
        .filter_map(|(_, tile)| match tile {
            egui_tiles::Tile::Pane(p) => Some(p.doc_id),
            _ => None,
        })
        .collect()
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
    /// Shared close-request buffer. The pane chrome (the `✕` button drawn by
    /// `render_body`) pushes a doc id here, and `retain_pane` reads it back in
    /// the SAME frame so egui_tiles prunes exactly that pane while leaving the
    /// rest of the user's arrangement intact — no full-tree rebuild. A
    /// `RefCell` (not `&mut Vec`) because `render_body` and `retain_pane` both
    /// need to touch it during one `tree.ui()` call. Drained by the host
    /// afterwards to drop the matching tabs.
    pub close_requests: &'a std::cell::RefCell<Vec<DocId>>,
    /// Colour of the thin divider line egui_tiles paints in the gap BETWEEN
    /// adjacent panes (see [`AppGridBehavior::resize_stroke`]). Driven from the
    /// active theme accent (muted via [`divider_color`]) so split view has a
    /// calm, theme-consistent boundary instead of an empty gap. Recomputed each
    /// frame in `render_grid_central_panel`, so it tracks a live theme change.
    pub divider: egui::Color32,
}

/// The colour of the split-view divider line: the theme **accent**, dropped to a
/// low alpha so it reads as a calm separator rather than a harsh bright rule —
/// consistent with the app's "calm, legible" surface. Pulled out as a pure
/// function so the muting is unit-testable without driving a frame.
pub fn divider_color(accent: egui::Color32) -> egui::Color32 {
    // ~28% alpha: visible enough to divide the panes, quiet enough not to
    // compete with the note text on either side.
    accent.gamma_multiply(0.28)
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

    /// Paint a thin theme-accent line in the gap between adjacent panes.
    ///
    /// egui_tiles draws the inter-pane boundary (for horizontal, vertical AND
    /// grid layouts) by stroking a line down the centre of each internal gap
    /// with `resize_stroke(_, ResizeState::Idle)` — the default returns a
    /// `gap_width`-wide stroke in the (near-invisible) `tab_bar_color`, which is
    /// why split view read as just an empty 4 px gap with no visible boundary.
    /// Overriding the IDLE state gives a crisp 1 px accent rule instead. The
    /// line is centred in the gap and only drawn BETWEEN children (egui_tiles
    /// iterates `tuple_windows` over adjacent panes), never on the container's
    /// outer edge. The `Hovering`/`Dragging` states keep egui's default resize
    /// highlight so the resize handle still lights up when the user grabs it.
    fn resize_stroke(
        &self,
        style: &egui::Style,
        resize_state: egui_tiles::ResizeState,
    ) -> egui::Stroke {
        match resize_state {
            egui_tiles::ResizeState::Idle => egui::Stroke::new(1.0, self.divider),
            egui_tiles::ResizeState::Hovering => style.visuals.widgets.hovered.fg_stroke,
            egui_tiles::ResizeState::Dragging => style.visuals.widgets.active.fg_stroke,
        }
    }

    fn retain_pane(&mut self, pane: &Pane) -> bool {
        !self.close_requests.borrow().contains(&pane.doc_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The grid behaviour is exercised through the egui_tiles `Behavior` trait
    // (tab_title_for_pane / min_size / gap_width / retain_pane); bring it into
    // scope so the tests can call those trait methods.
    use egui_tiles::Behavior as _;

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
    fn doc_id_allocator_never_yields_the_zero_sentinel() {
        // DocId(0) is the reserved "unassigned" sentinel; the allocator must
        // never hand it out (else grid pane->tab look-up aliases tabs).
        let mut a = DocIdAllocator::default();
        for _ in 0..1000 {
            assert_ne!(a.next(), DocId(0));
        }
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

    /// Direct TOML round-trip of `Tree<Pane>` remains a known gap, with the
    /// failure point shifted by the `toml` 0.8 → 1.x bump:
    ///
    /// - `toml` 0.8: serialise itself failed because `egui_tiles::TileId(pub u64)`
    ///   carries values exceeding `i64::MAX` (TOML's only integer width) and the
    ///   0.8 serializer rejected them.
    /// - `toml` 1.x: serialise now succeeds (1.x relaxed the integer-width
    ///   check and handles newtype tuple structs gracefully), but deserialise
    ///   fails because `egui_tiles::Container::Tabs` declines to emit its
    ///   `height` field on serialise and then requires it on deserialise — a
    ///   round-trip asymmetry in egui_tiles itself, not in toml.
    ///
    /// The persistence wiring in the next PR routes through a JSON-string-in-
    /// TOML wrapper (the JSON serializer doesn't depend on egui_tiles' field
    /// ordering). The in-memory `Tree<Pane>` itself round-trips through
    /// `Clone` (see `grid_layout_clones_losslessly` above), which is what the
    /// 6-pane cap enforcement actually depends on at runtime.
    #[test]
    fn grid_layout_round_trips_through_json() {
        // #R6 — the grid layout now persists via a JSON string. The
        // Grid-container trees this module builds round-trip cleanly through
        // `to_json`/`from_json`, preserving the panes + their doc ids.
        let docs = [DocId(10), DocId(20), DocId(30)];
        let tree = build_default_grid(&docs);
        let json = to_json(&tree).expect("serialise grid to JSON");
        let back = from_json(&json).expect("deserialise grid from JSON");
        assert_eq!(count_panes(&back), 3);
        assert_eq!(pane_doc_ids(&back), pane_doc_ids(&tree));
        // Garbage in -> None (degrade to a default layout), never a panic.
        assert!(from_json("not json").is_none());
    }

    #[test]
    fn tree_direct_toml_round_trip_remains_a_known_gap() {
        // Documents WHY persistence routes through JSON rather than TOML:
        // egui_tiles drops `Container::Tabs.height` on serialise and then
        // requires it on deserialise, so a direct TOML round-trip fails.
        let docs = [DocId(10), DocId(20), DocId(30)];
        let tree = build_default_grid(&docs);
        #[derive(Serialize, Deserialize)]
        struct Wrap {
            tree: egui_tiles::Tree<Pane>,
        }
        let s = toml::to_string(&Wrap { tree }).expect("toml serialises (lossily)");
        let back: Result<Wrap, _> = toml::from_str(&s);
        assert!(
            back.is_err(),
            "direct TOML round-trip is the gap the JSON wrapper avoids",
        );
    }

    #[test]
    fn build_default_grid_caps_panes_at_max() {
        // #R6 — the 6-pane cap is enforced at build time: more docs than
        // MAX_PANES still produce only MAX_PANES panes.
        let docs: Vec<DocId> = (0..(MAX_PANES as u64 + 4)).map(DocId).collect();
        let tree = build_default_grid(&docs);
        assert_eq!(count_panes(&tree), MAX_PANES);
    }

    // ----------------------------------------------------------------------
    // AppGridBehavior trait surface.
    //
    // The `Behavior<Pane>` methods that DON'T need a live `Ui` (everything but
    // `pane_ui`, which paints into a frame) are pure dispatch — title lookup,
    // layout constants, and the close-request retain check. Exercising them
    // directly pins the grid's layout policy + the close-pane plumbing without
    // driving a wgpu frame, so a refactor can't silently change the min pane
    // size, the gap, the "(closed)" title fallback, or the retain semantics.
    // ----------------------------------------------------------------------

    /// Build an `AppGridBehavior` over the given titles + close-request buffer.
    /// `render_body` is never invoked by the methods under test here, so a stub
    /// that returns `false` is sufficient.
    fn behavior<'a>(
        titles: &'a [(DocId, String)],
        render: &'a mut dyn FnMut(&mut egui::Ui, DocId) -> bool,
        closes: &'a std::cell::RefCell<Vec<DocId>>,
    ) -> AppGridBehavior<'a> {
        AppGridBehavior {
            titles,
            render_body: render,
            close_requests: closes,
            // A fully-opaque teal stand-in so the divider assertions can check
            // the muting/stroke without depending on a live theme.
            divider: divider_color(egui::Color32::from_rgb(0, 255, 254)),
        }
    }

    #[test]
    fn tab_title_for_pane_uses_the_matching_title() {
        let titles = vec![
            (DocId(7), "lib.rs".to_string()),
            (DocId(8), "main.rs".to_string()),
        ];
        let mut render = |_: &mut egui::Ui, _: DocId| false;
        let closes = std::cell::RefCell::new(Vec::new());
        let mut b = behavior(&titles, &mut render, &closes);
        let text = b.tab_title_for_pane(&Pane::new(DocId(8)));
        // WidgetText renders to the underlying string; assert it carries the title.
        assert_eq!(text.text(), "main.rs");
    }

    #[test]
    fn tab_title_for_pane_falls_back_to_closed_for_an_unknown_doc() {
        let titles = vec![(DocId(7), "lib.rs".to_string())];
        let mut render = |_: &mut egui::Ui, _: DocId| false;
        let closes = std::cell::RefCell::new(Vec::new());
        let mut b = behavior(&titles, &mut render, &closes);
        // A pane whose doc id is not in the title list shows the "(closed)"
        // sentinel rather than panicking or rendering an empty tab.
        let text = b.tab_title_for_pane(&Pane::new(DocId(999)));
        assert_eq!(text.text(), "(closed)");
    }

    #[test]
    fn layout_constants_are_stable() {
        let titles: Vec<(DocId, String)> = Vec::new();
        let mut render = |_: &mut egui::Ui, _: DocId| false;
        let closes = std::cell::RefCell::new(Vec::new());
        let b = behavior(&titles, &mut render, &closes);
        assert_eq!(b.min_size(), 120.0, "min pane size pins the grid layout");
        // gap_width ignores the style argument; a default style is fine.
        assert_eq!(b.gap_width(&egui::Style::default()), 4.0);
    }

    #[test]
    fn divider_color_mutes_the_accent_alpha() {
        // The split-view divider is the theme accent dropped to a low alpha so
        // it reads as a calm separator, not a harsh bright rule.
        let accent = egui::Color32::from_rgb(0, 255, 254);
        let d = divider_color(accent);
        assert!(
            d.a() < accent.a(),
            "divider must be more transparent than the opaque accent"
        );
        assert!(d.a() > 0, "divider must still be visible (non-zero alpha)");
        assert_ne!(
            d,
            egui::Color32::TRANSPARENT,
            "divider must not be fully invisible"
        );
        // Color32 is premultiplied, so gamma_multiply scales every channel; the
        // ACCENT HUE is what's preserved — recover it by un-premultiplying and
        // confirm red stays absent while green/blue dominate (the teal accent),
        // i.e. the divider is a muted accent, not a washed-out grey rule.
        let [r, g, b, _a] = d.to_srgba_unmultiplied();
        assert_eq!(r, 0, "accent hue preserved: no red in the teal divider");
        assert!(
            g > 200 && b > 200,
            "accent hue preserved: green/blue dominate"
        );
    }

    #[test]
    fn resize_stroke_idle_paints_the_thin_accent_divider() {
        // egui_tiles strokes the inter-pane gap with `resize_stroke(_, Idle)`.
        // Our override returns a 1 px line in the (muted) divider colour, so the
        // boundary between split panes is visible instead of an empty gap. The
        // Hovering/Dragging states keep egui's default resize highlight.
        use egui_tiles::ResizeState;
        let titles: Vec<(DocId, String)> = Vec::new();
        let mut render = |_: &mut egui::Ui, _: DocId| false;
        let closes = std::cell::RefCell::new(Vec::new());
        let b = behavior(&titles, &mut render, &closes);
        let style = egui::Style::default();
        let idle = b.resize_stroke(&style, ResizeState::Idle);
        assert_eq!(idle.width, 1.0, "divider line is a thin 1 px rule");
        assert_eq!(
            idle.color, b.divider,
            "idle stroke uses the muted accent divider"
        );
        // Interactive states are untouched — the resize handle still highlights.
        assert_eq!(
            b.resize_stroke(&style, ResizeState::Hovering),
            style.visuals.widgets.hovered.fg_stroke
        );
        assert_eq!(
            b.resize_stroke(&style, ResizeState::Dragging),
            style.visuals.widgets.active.fg_stroke
        );
    }

    #[test]
    fn retain_pane_drops_only_panes_with_a_pending_close_request() {
        let titles: Vec<(DocId, String)> = Vec::new();
        let mut render = |_: &mut egui::Ui, _: DocId| false;
        // The host queued a close for DocId(5) this frame; retain_pane must drop
        // exactly that pane (return false) and keep every other (return true).
        let closes = std::cell::RefCell::new(vec![DocId(5)]);
        let mut b = behavior(&titles, &mut render, &closes);
        assert!(
            !b.retain_pane(&Pane::new(DocId(5))),
            "a pane with a pending close request must NOT be retained"
        );
        assert!(
            b.retain_pane(&Pane::new(DocId(6))),
            "a pane with no close request must be retained"
        );
    }
}
