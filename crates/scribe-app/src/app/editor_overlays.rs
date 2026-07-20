//! Editor overlay/popup methods for `ScribeApp`: modal keyboard ownership,
//! the autocomplete popup (open/accept), the minimap, and the fold view.
//! Extracted from `mod.rs` (A-01 wave 3 — behavior-preserving move; methods
//! widened to `pub(super)` for the parent + sibling call-sites incl
//! `frame_tick`).
#![allow(clippy::wildcard_imports)]

use super::*;

/// Base point-size the minimap galley is laid out at to MEASURE the document's
/// intrinsic minimap height before fit-to-height scaling.
const MINIMAP_BASE_PT: f32 = 3.0;
/// Floor on the *drawn* minimap font size. Below this, glyphs stop carrying
/// useful information. Documents so tall that fit-to-height would shrink the
/// font under this floor keep the floor and switch to the co-scrolling
/// proportional-slider model so the minimap stays legible (the threshold is
/// `natural_h > (BASE/MIN) * panel_h`, i.e. ~3× the panel height).
const MINIMAP_MIN_PT: f32 = 1.0;

/// Geometry of the minimap viewport indicator + content offset, in panel-local
/// pixels. Returned by [`minimap_geometry`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct MinimapGeom {
    /// Indicator-box top, as an offset from the panel top (px).
    pub ind_top: f32,
    /// Indicator-box height (px).
    pub ind_h: f32,
    /// Vertical offset (≤ 0) to translate the drawn galley by so its rows
    /// co-scroll with the indicator when the document is taller than the panel.
    /// `0.0` whenever the whole document fits the panel.
    pub map_offset: f32,
}

/// Compute the minimap viewport-indicator geometry from the editor's real scroll
/// metrics and the ACTUAL drawn minimap-galley height.
///
/// This is the load-bearing accuracy invariant, kept as a pure free function (no
/// egui state, no GPU) so the scale-unification can be unit-tested: the indicator
/// is `view_h/content_h` of `drawn_h` at offset `off_y/content_h` — the SAME
/// `drawn_h` the content is painted at. The historical bug multiplied the
/// indicator by an extra `scale` factor the content draw never applied; that can
/// never reappear without this function failing its tests.
///
/// * `scroll` — editor metrics `(off_y, content_h, view_h)` in editor px.
/// * `panel_h` — minimap panel inner height (px).
/// * `drawn_h` — actual pixel height of the minimap galley as painted (already
///   scaled to fit, or floored-and-taller for huge files).
pub(super) fn minimap_geometry(scroll: (f32, f32, f32), panel_h: f32, drawn_h: f32) -> MinimapGeom {
    let (off_y, content_h, view_h) = scroll;
    let content_h = content_h.max(1.0);
    let panel_h = panel_h.max(1.0);
    let drawn_h = drawn_h.max(1.0);
    let off_y = off_y.max(0.0);
    // Indicator height = editor's visible fraction of the document mapped onto
    // the drawn minimap content — the SAME scale as the content.
    let ind_h = ((view_h / content_h) * drawn_h).clamp(0.0, drawn_h);
    if drawn_h <= panel_h + 0.5 {
        // Fit-to-height (the normal case after scaling, and all short docs): the
        // whole document occupies the panel; the box maps 1:1 onto the content.
        let ind_top = ((off_y / content_h) * drawn_h).clamp(0.0, (drawn_h - ind_h).max(0.0));
        MinimapGeom {
            ind_top,
            ind_h,
            map_offset: 0.0,
        }
    } else {
        // Huge file: drawn content is taller than the panel even at the floored
        // font. Co-scroll content + slider (VS Code proportional model) so the
        // slider stays in view and overlays the matching rows.
        let scroll_range = (content_h - view_h).max(1.0);
        let f = (off_y / scroll_range).clamp(0.0, 1.0);
        let slider_travel = (panel_h - ind_h).max(0.0);
        let map_travel = (drawn_h - panel_h).max(0.0);
        MinimapGeom {
            ind_top: f * slider_travel,
            ind_h,
            map_offset: -(f * map_travel),
        }
    }
}

impl ScribeApp {
    /// True when a modal with a focused text field or arrow-key navigation
    /// currently owns the keyboard (#72). The editor-surface completion popup
    /// must defer to these so its ↑↓/Enter interception cannot steal the modal
    /// field's keys. Kept as one method so the set is defined in exactly one
    /// place. NOTE: the passive display modals (welcome, cheatsheet, recent —
    /// no text entry, no arrow navigation) are deliberately EXCLUDED; they have
    /// no keys for completion to conflict with, and the first-run welcome flag
    /// must not suppress completion in the editor behind it.
    pub(super) fn modal_owns_keyboard(&self) -> bool {
        self.find_open
            || self.palette_open
            || self.settings_open
            || self.fuzzy_open
            || self.goto_open
            || self.goto_symbol_open
    }

    /// Open the identifier-completion popup for the prefix ending at `char_idx`
    /// in the active buffer. Sources suggestions from the buffer's own words
    /// (zero network / LSP dependency).
    pub(super) fn open_completion(&mut self, active: usize, char_idx: Option<usize>) {
        let Some(ci) = char_idx else {
            self.completion = None;
            return;
        };
        let text = &self.tabs[active].text;
        let byte = char_to_byte(text, ci);
        let (start, prefix) = crate::editor_features::prefix_before(text, byte);
        let items = crate::editor_features::word_completions(text, &prefix, 8);
        self.completion = (!items.is_empty()).then_some(Completion {
            prefix_start: start,
            items,
            selected: 0,
        });
    }

    /// Insert the selected completion, replacing the typed prefix.
    pub(super) fn accept_completion(&mut self, active: usize, char_idx: Option<usize>) {
        let Some(c) = self.completion.take() else {
            return;
        };
        let Some(ci) = char_idx else { return };
        let Some(item) = c.items.get(c.selected).cloned() else {
            return;
        };
        let text = &mut self.tabs[active].text;
        let byte = char_to_byte(text, ci);
        // `c.prefix_start` is a byte offset captured a frame EARLIER; the buffer
        // may have mutated since (e.g. an async edit between popup-open and
        // accept), leaving it mid-multibyte-char. `replace_range` panics on a
        // non-boundary offset → `panic = "abort"`. `char_to_byte` already clamps
        // `byte` to a boundary; re-validate `prefix_start` the same way before
        // splicing. On a stale offset we drop the completion rather than crash.
        if c.prefix_start <= byte && byte <= text.len() && text.is_char_boundary(c.prefix_start) {
            text.replace_range(c.prefix_start..byte, &item);
        }
        self.tabs[active].edit_gen = self.tabs[active].edit_gen.wrapping_add(1);
    }

    /// Render the minimap strip (rightmost): a memoized fit-to-height overview of
    /// the active document with an accurate viewport indicator; click/drag scrolls
    /// the editor.
    ///
    /// Accuracy invariant (the fix): the minimap CONTENT and the viewport
    /// INDICATOR share ONE scale. The document is squished to the panel height
    /// (fit-to-height, the user's request) by laying the drawn galley out at a
    /// scaled font; the indicator is then `view_h/content_h` of the SAME drawn
    /// height at offset `off_y/content_h`. Previously the content was drawn at its
    /// natural height while the indicator carried an extra `* scale` factor, so for
    /// any document taller than the panel the highlight sat over the wrong rows.
    pub(super) fn show_minimap(&mut self, ctx: &egui::Context, panel: Color32, accent: Color32) {
        egui::SidePanel::right("minimap")
            // #86 — the Map view is now user-resizable (was a fixed exact_width).
            // The minimap galley re-lays out to `available_size` each frame, so
            // it tracks the dragged width. Floor keeps it legible; ceiling stops
            // it eating the editor.
            .default_width(110.0)
            .width_range(48.0..=260.0)
            .resizable(true)
            .frame(egui::Frame::default().fill(panel).inner_margin(4.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("MAP").color(accent).small().monospace());
                let avail = ui.available_size();
                let map_fg = Color32::from_rgb(0x8a, 0x88, 0x99);
                // P2: match the minimap's wrap behaviour to the editor's so a
                // logical line maps to the right minimap row. Editor word_wrap OFF
                // → NO wrap (one minimap row per logical line, exactly like the
                // editor, which scrolls horizontally), so the editor-pixel scroll
                // fraction maps proportionally onto the minimap. word_wrap ON →
                // wrap at the panel width (both galleys wrap).
                let word_wrap = self.config.editor.word_wrap;
                let wrap_w = if word_wrap { avail.x } else { f32::INFINITY };
                // (1) NATURAL galley at MINIMAP_BASE_PT — used to MEASURE the
                // document's intrinsic minimap height, and drawn directly for
                // short documents. Memoized (edit_gen, doc_id, width, word_wrap):
                // the owned String is built ONLY on a cache miss.
                let natural = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    self.tabs[self.active].edit_gen.hash(&mut h);
                    self.tabs[self.active].doc_id.raw().hash(&mut h);
                    avail.x.to_bits().hash(&mut h);
                    (word_wrap as u8).hash(&mut h);
                    let key = h.finish();
                    let mut slot = self.minimap_cache.borrow_mut();
                    match slot.as_ref() {
                        Some((k, g)) if *k == key => g.clone(),
                        _ => {
                            // egui 0.34: layout caches into the FontsView so it now
                            // needs `&mut`; use fonts_mut(...) instead of fonts(...).
                            let g = ui.fonts_mut(|f| {
                                f.layout(
                                    self.tabs[self.active].text.clone(),
                                    FontId::monospace(MINIMAP_BASE_PT),
                                    map_fg,
                                    wrap_w,
                                )
                            });
                            *slot = Some((key, g.clone()));
                            g
                        }
                    }
                };
                let natural_h = natural.size().y.max(1.0);
                let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
                let panel_h = rect.height().max(1.0);
                let panel_h_q = panel_h.round().max(1.0);
                // (2)+(3) DRAWN galley. Short docs (already ≤ panel) draw at the
                // natural size. Tall docs are squished to the panel height
                // (fit-to-height — the user's request): lay out at a scaled font,
                // then apply ONE correction toward the panel height because egui's
                // per-row height is sub-linear at tiny sizes (a pure linear guess
                // under-fills). The font is floored at `MINIMAP_MIN_PT`; genuinely
                // huge files (taller than the floor allows) keep the floor and
                // co-scroll via the proportional-slider branch in `minimap_geometry`.
                // The accuracy invariant does NOT depend on the fill being exact —
                // the indicator always uses the ACTUAL `drawn_h` below.
                let drawn = if natural_h <= panel_h_q {
                    natural.clone()
                } else {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    self.tabs[self.active].edit_gen.hash(&mut h);
                    self.tabs[self.active].doc_id.raw().hash(&mut h);
                    avail.x.to_bits().hash(&mut h);
                    (word_wrap as u8).hash(&mut h);
                    panel_h_q.to_bits().hash(&mut h);
                    let key = h.finish();
                    let mut slot = self.minimap_draw_cache.borrow_mut();
                    match slot.as_ref() {
                        Some((k, g)) if *k == key => g.clone(),
                        _ => {
                            let text = self.tabs[self.active].text.clone();
                            let lay = |pt: f32, ui: &egui::Ui| {
                                ui.fonts_mut(|f| {
                                    f.layout(text.clone(), FontId::monospace(pt), map_fg, wrap_w)
                                })
                            };
                            // Linear first guess for the font that makes
                            // `drawn_h == panel_h`, clamped to the legibility floor.
                            let mut pt = (MINIMAP_BASE_PT * panel_h_q / natural_h)
                                .clamp(MINIMAP_MIN_PT, MINIMAP_BASE_PT);
                            let mut g = lay(pt, ui);
                            // One correction toward the panel height — but only when
                            // not floored (a floored huge file co-scrolls instead).
                            if pt > MINIMAP_MIN_PT {
                                let dh = g.size().y.max(1.0);
                                if (dh - panel_h_q).abs() > panel_h_q * 0.03 {
                                    let corrected = (pt * panel_h_q / dh)
                                        .clamp(MINIMAP_MIN_PT, MINIMAP_BASE_PT);
                                    if (corrected - pt).abs() > f32::EPSILON {
                                        pt = corrected;
                                        g = lay(pt, ui);
                                    }
                                }
                            }
                            *slot = Some((key, g.clone()));
                            g
                        }
                    }
                };
                let drawn_h = drawn.size().y.max(1.0);
                // (4) ONE shared geometry for content offset + indicator box.
                let geom = minimap_geometry(self.scroll_metrics, panel_h, drawn_h);
                // (5) Draw the content, translated by `map_offset` (co-scroll for
                // huge files), clipped to the panel rect.
                let painter = ui.painter_at(rect);
                painter.add(egui::epaint::TextShape::new(
                    egui::pos2(rect.left(), rect.top() + geom.map_offset),
                    drawn.clone(),
                    map_fg,
                ));
                // (6) Viewport indicator — same scale as the content above.
                let ind_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.left(), rect.top() + geom.ind_top),
                    egui::vec2(rect.width(), geom.ind_h.max(6.0)),
                );
                painter.rect_filled(
                    ind_rect,
                    2.0,
                    Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 40),
                );
                // (7) Click/drag → scroll the editor. P3: a drag that BEGINS on the
                // indicator box grabs it and moves by pointer delta (mapped through
                // the shared scale); a press elsewhere is an absolute scrub that
                // centres the viewport on the clicked fraction.
                let (off_y, content_h, view_h) = self.scroll_metrics;
                let max_off = (content_h - view_h).max(0.0);
                if resp.drag_started() {
                    self.minimap_drag_box = resp
                        .interact_pointer_pos()
                        .is_some_and(|p| ind_rect.contains(p));
                }
                if resp.dragged() && self.minimap_drag_box {
                    // Relative: pointer Δ in panel px → minimap fraction → editor px.
                    let editor_dy = resp.drag_delta().y / drawn_h * content_h;
                    let base = self.pending_scroll.unwrap_or(off_y);
                    self.pending_scroll = Some((base + editor_dy).clamp(0.0, max_off));
                } else if let Some(p) = resp.interact_pointer_pos() {
                    let frac = ((p.y - rect.top()) / panel_h).clamp(0.0, 1.0);
                    self.pending_scroll =
                        Some((frac * content_h - view_h * 0.5).clamp(0.0, max_off));
                }
                if resp.drag_stopped() {
                    self.minimap_drag_box = false;
                }
            });
    }

    /// Render the folded read-only preview: per-region toggles plus the
    /// brace-collapsed projection of the active buffer.
    pub(super) fn show_fold_view(&mut self, ui: &mut egui::Ui, font: FontId, ext: Option<&str>) {
        // Wave-3: borrow instead of cloning the whole buffer every frame the
        // fold view is shown. `fold_regions`/`project_folded` take &str, and the
        // toolbar closure below only captures `self.folds` (disjoint from
        // `self.tabs` under edition-2021 closure capture), so the borrow holds.
        let text = &self.tabs[self.active].text;
        // P2-4: markdown/text notes fold by heading section; code by braces.
        let regions = crate::editor_features::fold_regions_for(text, ext);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("FOLDS").small().monospace());
            if ui.small_button("fold all").clicked() {
                self.folds = regions.iter().map(|r| r.start_line).collect();
            }
            if ui.small_button("expand all").clicked() {
                self.folds.clear();
            }
            for r in &regions {
                let folded = self.folds.contains(&r.start_line);
                let label = format!(
                    "{} L{} ({})",
                    if folded { "▸" } else { "▾" },
                    r.start_line + 1,
                    r.hidden_len()
                );
                if ui.small_button(label).clicked() {
                    if folded {
                        self.folds.remove(&r.start_line);
                    } else {
                        self.folds.insert(r.start_line);
                    }
                }
            }
        });
        ui.separator();
        let (mut projected, _map) =
            crate::editor_features::project_folded(text, &regions, &self.folds);
        let line_height = self.config.fonts.clamped_line_height();
        let hl = &self.hl;
        let word_wrap = self.config.editor.word_wrap;
        let layout_fg = ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
        // #D — themeable URL colour + link-detection toggle (fold view).
        let detect_links = self.config.editor.detect_links;
        let url_color = scribe_render::color32(self.theme.syntax_color(
            "url",
            self.theme.ui("accent", Rgba::new(0x4c, 0xc2, 0xff, 255)),
        ));
        let mut layouter = make_layouter(
            hl,
            &self.hl_cache,
            &self.hl_galley_cache,
            &self.hl_inc_cache,
            ext,
            font,
            line_height,
            word_wrap,
            layout_fg,
            url_color,
            detect_links,
        );
        egui::ScrollArea::both()
            .id_salt("fold-scroll")
            .show(ui, |ui| {
                let editor = egui::TextEdit::multiline(&mut projected)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(30)
                    .interactive(false)
                    .layouter(&mut layouter);
                ui.add_sized(ui.available_size(), editor);
            });
    }
}

#[cfg(test)]
mod minimap_geom_tests {
    use super::{minimap_geometry, MINIMAP_BASE_PT, MINIMAP_MIN_PT};

    const EPS: f32 = 1e-3;

    #[test]
    fn geometry_kills_branch_and_arith_mutants() {
        // Targets the four un-covered arithmetic/branch mutants no existing test
        // exercised: the FIT/HUGE selector `drawn_h <= panel_h + 0.5`, the FIT
        // ind_top clamp bound `(drawn_h - ind_h)`, and the HUGE `scroll_range`/`f`.
        // (1) FIT/HUGE selector. panel=100, drawn=100.4 -> FIT -> map_offset==0.
        //   `+ -> *`: 100.4<=50 false -> HUGE -> map_offset=-0.2 (killed);
        //   `+ -> -`: 100.4<=99.5 false -> HUGE -> map_offset=-0.2 (killed).
        let g = minimap_geometry((450.0, 1000.0, 100.0), 100.0, 100.4);
        assert!((g.map_offset - 0.0).abs() < EPS);

        // (2) FIT ind_top clamp upper bound `(drawn_h - ind_h)`. Scroll to the
        //   bottom so the raw top exceeds the bound and the clamp bites: ind_h=10,
        //   orig ind_top=90.0; `- -> +` widens the bound to 110 -> ind_top=100.0.
        let g = minimap_geometry((1000.0, 1000.0, 100.0), 200.0, 100.0);
        assert!((g.ind_h - 10.0).abs() < EPS);
        assert!((g.ind_top - 90.0).abs() < EPS);

        // (3) HUGE scroll_range + f. panel=100, drawn=300 -> HUGE. ind_h=30,
        //   orig scroll_range=900, f=0.5, ind_top=35.0, map_offset=-100.0.
        //   `- -> /`: scroll_range=1000/100=10 -> f clamps to 1 -> ind_top=70 (killed);
        //   `/ -> *`: f=450*900 -> clamps to 1 -> ind_top=70 (killed).
        let g = minimap_geometry((450.0, 1000.0, 100.0), 100.0, 300.0);
        assert!((g.ind_h - 30.0).abs() < EPS);
        assert!((g.ind_top - 35.0).abs() < EPS);
        assert!((g.map_offset + 100.0).abs() < EPS);
    }

    /// The core regression guard for the original bug: the indicator's
    /// fraction-of-drawn-content MUST equal the editor's visible fraction. The
    /// pre-fix code multiplied the indicator by an extra `scale` while the
    /// content was drawn un-scaled, so `ind_top/drawn_h` diverged from
    /// `off_y/content_h` for any document taller than the panel.
    #[test]
    fn indicator_fraction_matches_editor_when_fit() {
        // Tall document squished to fit a 700px panel: drawn_h == panel_h.
        let panel_h = 700.0;
        let drawn_h = 700.0; // after fit-to-height scaling
        let content_h = 12_000.0; // editor px (tall doc)
        let view_h = 800.0;
        // Scroll to the MIDDLE of the document.
        let off_y = (content_h - view_h) * 0.5;
        let g = minimap_geometry((off_y, content_h, view_h), panel_h, drawn_h);
        // Box top fraction of drawn content == scroll-offset fraction of doc.
        assert!(
            (g.ind_top / drawn_h - off_y / content_h).abs() < EPS,
            "ind_top/drawn_h={} must equal off_y/content_h={}",
            g.ind_top / drawn_h,
            off_y / content_h
        );
        // Box height fraction == visible fraction.
        assert!(
            (g.ind_h / drawn_h - view_h / content_h).abs() < EPS,
            "ind_h/drawn_h={} must equal view_h/content_h={}",
            g.ind_h / drawn_h,
            view_h / content_h
        );
        assert!(g.map_offset.abs() < EPS, "fit case must not co-scroll");
        // The box stays fully inside the panel.
        assert!(g.ind_top >= 0.0 && g.ind_top + g.ind_h <= panel_h + 0.5);
    }

    /// The pre-fix arithmetic, reproduced, must DISAGREE with the editor's true
    /// fraction for a tall doc — proving the test would have caught the bug.
    #[test]
    fn buggy_extra_scale_factor_would_fail() {
        let panel_h = 700.0_f32;
        let content_h = 12_000.0_f32;
        let view_h = 800.0_f32;
        let off_y = (content_h - view_h) * 0.5;
        // Natural (un-scaled) minimap height for this doc, > panel (the bug regime).
        let map_h = 3600.0_f32;
        let scale = (panel_h / map_h).min(1.0); // < 1
                                                // Old buggy indicator top in panel space:
        let buggy_ind_top = (off_y / content_h) * map_h * scale;
        let buggy_drawn_h = map_h; // content was DRAWN at natural height
        let editor_frac = off_y / content_h;
        let buggy_frac = buggy_ind_top / buggy_drawn_h;
        assert!(
            (buggy_frac - editor_frac).abs() > 0.1,
            "the old extra-scale formula must visibly diverge from the editor fraction"
        );
    }

    #[test]
    fn short_doc_top_and_bottom() {
        // Short doc: drawn_h < panel_h, scale clamps to 1.
        let panel_h = 700.0;
        let drawn_h = 200.0;
        let content_h = 1_000.0;
        let view_h = 700.0;
        // Top of document.
        let top = minimap_geometry((0.0, content_h, view_h), panel_h, drawn_h);
        assert!(top.ind_top.abs() < EPS);
        assert!(top.map_offset.abs() < EPS);
        // Bottom of document — box bottom flush with content bottom, never past it.
        let max_off = content_h - view_h;
        let bot = minimap_geometry((max_off, content_h, view_h), panel_h, drawn_h);
        assert!(bot.ind_top + bot.ind_h <= drawn_h + EPS);
    }

    #[test]
    fn huge_file_coscrolls_and_keeps_slider_in_panel() {
        // Drawn content taller than panel even after the font floor.
        let panel_h = 700.0;
        let drawn_h = 2_100.0; // 3× the panel → proportional-slider regime
        let content_h = 200_000.0;
        let view_h = 800.0;
        let max_off = content_h - view_h;
        // Top: no offset, slider at panel top.
        let top = minimap_geometry((0.0, content_h, view_h), panel_h, drawn_h);
        assert!(top.ind_top.abs() < EPS);
        assert!(top.map_offset.abs() < EPS);
        // Bottom: slider pinned at panel bottom, content scrolled fully up.
        let bot = minimap_geometry((max_off, content_h, view_h), panel_h, drawn_h);
        assert!(
            (bot.ind_top + bot.ind_h - panel_h).abs() < 1.0,
            "slider bottom must reach the panel bottom"
        );
        assert!(
            (bot.map_offset + (drawn_h - panel_h)).abs() < 1.0,
            "content must scroll up by (drawn_h - panel_h)"
        );
        // Slider always within the panel.
        for frac in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
            let g = minimap_geometry((frac * max_off, content_h, view_h), panel_h, drawn_h);
            assert!(g.ind_top >= -EPS && g.ind_top + g.ind_h <= panel_h + 1.0);
            assert!(g.map_offset <= EPS);
        }
    }

    #[test]
    fn font_floor_threshold_is_three_panels() {
        // Sanity on the constants that pick fit-vs-coscroll: the floor kicks in
        // at natural_h ≈ (BASE/MIN) × panel_h.
        let ratio = MINIMAP_BASE_PT / MINIMAP_MIN_PT;
        assert!((ratio - 3.0).abs() < EPS);
    }
}
