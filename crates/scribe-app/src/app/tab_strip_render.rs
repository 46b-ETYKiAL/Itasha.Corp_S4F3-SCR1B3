//! Tab-strip rendering: the top horizontal tab strip plus the left/right side tab bar (standard + rotated-label variants) and its width metric. Bodies moved verbatim from the `app` god-module (A-01 decomposition); `use super::*` re-exports the types these methods touch.
#![allow(clippy::wildcard_imports)]
use super::*;

#[cfg(test)]
thread_local! {
    /// Test hook (#82): when `Some`, `draw_rotated_side_tabs` treats this as the
    /// live drag pointer, so the drop-insertion indicator paints deterministically
    /// (no event-timing flake) for the regression + visual-QA tests.
    pub(crate) static TEST_FORCE_SIDE_TAB_DRAG: std::cell::Cell<Option<egui::Pos2>> =
        const { std::cell::Cell::new(None) };
    /// Test hook (#82): the FULL chip rects `draw_rotated_side_tabs` fed to the
    /// drop indicator this frame — a test asserts the insertion line lands in an
    /// inter-chip GAP and never inside a chip outline (the bug this guards).
    pub(crate) static TEST_ROTATED_TAB_RECTS: std::cell::RefCell<Vec<egui::Rect>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The faint hover-highlight fill for a NON-selected note tab (Fix 3). Lower
/// alpha than the selected chip's `accent @ 0.20`, so a hovered inactive tab
/// reads as a gentle affordance clearly distinct from — and lighter than — the
/// active tab. Painted BEHIND the tab content (via `Frame::begin`/`end`) so it
/// never washes over the label text. Shared by BOTH draw functions so the hover
/// feel is identical in every dock position and side variant.
fn tab_hover_fill(accent: Color32) -> Color32 {
    accent.linear_multiply(0.09)
}

/// Wrap parameters for a horizontal LEFT/RIGHT side-bar tab title (Fixes for the
/// two follow-up requirements). Given the content width available to the chip's
/// row, whether the tab is selected (which carries a trailing pin+close vs just
/// a close), and the opt-in 2-line setting, returns `(max_width, max_rows)` for
/// the title galley:
///   * `max_width` reserves room for the trailing pin/close affordances so the
///     truncated/wrapped title never collides with them — AND, because the
///     galley then reports only this BOUNDED width, it lets the resizable side
///     panel shrink BELOW the widest title (egui otherwise floors the panel at
///     the un-truncated label width, so the divider couldn't drag narrower).
///   * `max_rows` is 1 (single-line, ellipsis on overflow) or 2 (opt-in wrap;
///     the 2nd row still elides with … when even two lines don't fit).
///
/// Pure + unit-tested so the "reserve + row-cap" rule can't silently regress.
fn side_tab_label_wrap(avail_width: f32, selected: bool, two_line: bool) -> (f32, usize) {
    // Selected tabs show BOTH a pin toggle and a close ✕; other tabs show at
    // most a close ✕. Reserve accordingly (glyph + inter-item spacing).
    let reserve = if selected { 46.0 } else { 28.0 };
    let max_width = (avail_width - reserve).max(24.0);
    let max_rows = if two_line { 2 } else { 1 };
    (max_width, max_rows)
}

/// Draw the note-tab-strip "+" (new-tab) button with the SAME frameless-until-
/// hover chrome as the top-bar toolbar buttons: transparent idle fill + border,
/// egui's default weak hover/active fill preserved (so it lights up ONLY on
/// hover — matching a top-bar button, not the old grey `small_button` slab), and
/// a fixed SQUARE min-size with the "+" glyph optically centred in both axes.
/// Mirrors the visuals override in `toolbar_contents`, so a note-tab "+" is
/// visually identical to a top-bar toolbar button. Used by BOTH `draw_tab_strip`
/// (top/bottom + non-rotated side) and `draw_rotated_side_tabs` (rotated side),
/// so every dock position and side variant gets the same button.
fn tab_add_button(ui: &mut egui::Ui) -> egui::Response {
    // A square sized to the standard interactive height so the button reads as a
    // proper box (matching the top-bar buttons' footprint) in every position.
    let side = ui.spacing().interact_size.y.max(22.0);
    // A CENTER×CENTER child layout makes the Button's atom (the "+" glyph) sit
    // dead-centre within the square min-size: egui aligns button content by the
    // ui's horizontal_align × vertical_align, and `top_down(Align::Center)`
    // yields Center on BOTH axes. The Phosphor PLUS glyph is centred in its own
    // em-box (unlike the bare "+" text char, which sits optically high), so the
    // mark lands pixel-centred regardless of dock position.
    ui.allocate_ui_with_layout(
        egui::vec2(side, side),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            let v = ui.visuals_mut();
            v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            v.widgets.inactive.bg_fill = Color32::TRANSPARENT;
            v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
            ui.add(
                egui::Button::new(egui::RichText::new(egui_phosphor::thin::PLUS))
                    .min_size(egui::vec2(side, side)),
            )
        },
    )
    .inner
}

impl ScribeApp {
    /// Render the tab strip inside a Left/Right side panel, honouring the
    /// `side_tabs_rotated` orientation option (#82). A side tab bar is always a
    /// single vertical column; when `_rotated` is on, each tab's label is drawn
    /// rotated 90° (vertical text) via [`Self::draw_rotated_side_tabs`],
    /// otherwise the standard horizontal-label rows. Scrolls so no tab becomes
    /// unreachable in a small window.
    pub(super) fn draw_side_tab_strip(
        &mut self,
        ui: &mut egui::Ui,
        accent: Color32,
        muted: Color32,
        _rotated: bool,
    ) {
        // A side tab bar is ALWAYS a single vertical column of tabs (one per
        // row). The earlier horizontal-wrap experiment was wrong — the user
        // wants the column preserved; the orientation option (#82) only rotates
        // each tab's TEXT, not the stacking. Scrolls so no tab is unreachable.
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if _rotated {
                    ui.vertical(|ui| self.draw_rotated_side_tabs(ui, accent, muted));
                } else {
                    ui.vertical(|ui| self.draw_tab_strip(ui, accent, muted));
                }
            });
    }

    /// Natural width for a left/right tab bar so it HUGS the tab content instead
    /// of a fixed 180px slab. The bar should be only as wide as the widest note
    /// tab needs — a short tab name must not leave a big empty bar beside it.
    /// Rotated tabs are a narrow vertical-text strip; horizontal tabs are
    /// measured from the widest label plus the grip/pin/close affordances, then
    /// clamped so a very long filename can't make the bar enormous.
    pub(super) fn side_tab_bar_width(&self, ctx: &egui::Context, rotated: bool) -> f32 {
        if rotated {
            // Vertical-text column + the grip/pin/close stacked above it. Kept
            // snug to the content (was a fat 44) so a rotated tab's thickness is
            // close to a horizontal tab's height instead of floating in a wide bar.
            return 30.0;
        }
        let font = ctx
            .style()
            .text_styles
            .get(&egui::TextStyle::Body)
            .cloned()
            .unwrap_or_else(|| egui::FontId::proportional(14.0));
        // Measure via a Painter (its `layout_no_wrap` is the `&self` form egui
        // exposes for text measurement; `Fonts::layout_no_wrap` needs `&mut`).
        // No layer is painted — this only measures galley widths.
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Background,
            egui::Id::new("scr1b3_side_tab_width_probe"),
        ));
        let mut max_label = 0.0_f32;
        for tab in &self.tabs {
            let shown = tab_display_label(&tab.title(), tab.pinned);
            let w = painter
                .layout_no_wrap(shown, font.clone(), Color32::WHITE)
                .size()
                .x;
            max_label = max_label.max(w);
        }
        // grip(~12) + pin(~18) + close(~18) + inter-item spacing + chip inner
        // margin(16) + panel inner margin(8): the fixed affordances a tab row
        // carries beside its label. Clamped: never narrower than a couple of
        // buttons, never wider than a readable cap.
        (max_label + 74.0).clamp(96.0, 280.0)
    }

    /// Render the side tab bar with each tab's label ROTATED 90° (vertical text,
    /// reading top-to-bottom), still stacked in a single column (#82). The close
    /// button sits ABOVE each tab (with the pin toggle on the active tab); the
    /// rotated label below is the click/drag target. Drag-reorder is resolved
    /// against the tab rects exactly like the horizontal strip.
    pub(super) fn draw_rotated_side_tabs(
        &mut self,
        ui: &mut egui::Ui,
        accent: Color32,
        muted: Color32,
    ) {
        let active = self.active;
        let mut switch_to = None;
        let mut close = None;
        let mut close_others = None;
        let mut close_to_right = None;
        let mut close_all = false;
        let mut toggle_pin: Option<usize> = None;
        let mut reorder: Option<(usize, usize)> = None;
        let mut drag_src: Option<usize> = None;
        let mut drop_pos: Option<egui::Pos2> = None;
        // #59-parity live drag feedback for the rotated column: the in-flight
        // pointer position so an insertion hairline can be painted at the drop gap.
        let mut dragging: Option<egui::Pos2> = None;
        let mut rects: Vec<(usize, egui::Rect)> = Vec::with_capacity(self.tabs.len());
        let mut add_tab = false;
        // `pad.x` is the cross-axis (screen left/right) padding around the
        // vertical text — kept tight so the rotated tab's thickness matches a
        // horizontal tab rather than looking chunky. `pad.y` is along the reading
        // direction (the word's ends).
        let pad = egui::vec2(6.0, 8.0);
        let font = egui::TextStyle::Button.resolve(ui.style());

        for i in 0..self.tabs.len() {
            let selected = i == active;
            let pinned = self.tabs[i].pinned;
            let shown = tab_display_label(&self.tabs[i].title(), pinned);
            let pin_label = if pinned { "Unpin tab" } else { "Pin tab" };
            // The active tab is a filled chip spanning the whole vertical cell
            // (icon row + rotated label), so it reads as a real tab.
            // Tighten the coloured chip to the text (like the top bar): a snug
            // inner margin + a thin accent outline on the active tab so the fill
            // HUGS the rotated label/grip stack instead of floating in the column.
            let chip = egui::Frame::default()
                .inner_margin(egui::Margin::symmetric(2, 3))
                .corner_radius(egui::CornerRadius::same(5))
                .stroke(if selected {
                    egui::Stroke::new(1.0, accent.linear_multiply(0.5))
                } else {
                    egui::Stroke::NONE
                })
                .fill(if selected {
                    accent.linear_multiply(0.20)
                } else {
                    Color32::TRANSPARENT
                });
            let mut prepared = chip.begin(ui);
            {
                let ui = &mut prepared.content_ui;
                ui.vertical_centered(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    // #30 column order: grip · rotated-name · pin · close (top→bottom).
                    // Drag GRIP at the head — painted dots (`grip_handle`), never a
                    // phosphor glyph, so the handle can't tofu. Pinned tabs show a
                    // dimmed, drag-disabled grip. `rotated = true` turns the grip on
                    // its side (3×2 dots) so it matches the vertical-text tab.
                    if pinned {
                        grip_handle(ui, false, muted, true).on_hover_text("Pinned — drag disabled");
                    } else {
                        let g = grip_handle(ui, true, muted, true)
                            .on_hover_text("Drag to reorder")
                            .on_hover_cursor(egui::CursorIcon::Grab);
                        if g.dragged() {
                            if let Some(p) = g.interact_pointer_pos() {
                                dragging = Some(p);
                            }
                        }
                        if g.drag_stopped() {
                            if let Some(p) = g.interact_pointer_pos() {
                                drag_src = Some(i);
                                drop_pos = Some(p);
                            }
                        }
                    }
                    let color = if selected { accent } else { muted };
                    let galley = ui
                        .painter()
                        .layout_no_wrap(shown.clone(), font.clone(), color);
                    let size = rotated_tab_size(galley.size(), pad);
                    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
                    // Paint the label rotated 90° clockwise (reads top-to-bottom).
                    let pos = rotated_tab_text_pos(rect, galley.size(), pad);
                    ui.painter().add(egui::Shape::Text(
                        egui::epaint::TextShape::new(pos, galley, color)
                            .with_angle(std::f32::consts::FRAC_PI_2),
                    ));
                    if resp.clicked() {
                        switch_to = Some(i);
                    }
                    if resp.clicked_by(egui::PointerButton::Middle) && !pinned {
                        close = Some(i);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Close").clicked() {
                            close = Some(i);
                            ui.close_menu();
                        }
                        if ui.button("Close Others").clicked() {
                            close_others = Some(i);
                            ui.close_menu();
                        }
                        if ui.button("Close All to the Right").clicked() {
                            close_to_right = Some(i);
                            ui.close_menu();
                        }
                        if ui.button("Close All").clicked() {
                            close_all = true;
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button(pin_label).clicked() {
                            toggle_pin = Some(i);
                            ui.close_menu();
                        }
                    });
                    // The rotated name is also a drag target (in addition to the
                    // grip) so either reorders; pinned tabs never initiate a drag.
                    if resp.dragged() && !pinned {
                        if let Some(p) = resp.interact_pointer_pos() {
                            dragging = Some(p);
                        }
                    }
                    if resp.drag_stopped() && !pinned {
                        if let Some(p) = resp.interact_pointer_pos() {
                            drag_src = Some(i);
                            drop_pos = Some(p);
                        }
                    }
                    // Pin toggle (active only), then close (unpinned), BELOW the name.
                    if selected {
                        let glyph = if pinned {
                            egui_phosphor::thin::PUSH_PIN_SLASH
                        } else {
                            egui_phosphor::thin::PUSH_PIN
                        };
                        if ui
                            .add(egui::Button::new(glyph).frame(false).small())
                            .on_hover_text(pin_label)
                            .clicked()
                        {
                            toggle_pin = Some(i);
                        }
                    }
                    if !pinned
                        && ui
                            .add(
                                egui::Button::new(egui_phosphor::thin::X)
                                    .frame(false)
                                    .small(),
                            )
                            .on_hover_text("Close tab (or middle-click)")
                            .clicked()
                    {
                        close = Some(i);
                    }
                });
            }
            // Fix 3 — faint hover fill on a NON-selected rotated tab, written into
            // the frame's RESERVED background slot (from `begin`) so it paints
            // BEHIND the chip content and never washes over the rotated
            // label/grip/close glyphs. Selected tabs keep their accent fill.
            let chip_rect = prepared.frame.outer_rect(prepared.content_ui.min_rect());
            if !selected && ui.rect_contains_pointer(chip_rect) {
                prepared.frame.fill = tab_hover_fill(accent);
            }
            let chip_resp = prepared.end(ui);
            // #82 — record the FULL chip frame rect (grip · rotated label · pin ·
            // close · margins), NOT the inner rotated-label rect, so the
            // drop-insertion indicator and the nearest-chip drop resolution use the
            // tab's real outline. Pushing the inner label rect made the inter-tab
            // "gap" midpoint land INSIDE a neighbouring chip's outline (over its
            // grip/close glyphs) — the drop line appeared inside the tab. Mirrors
            // the full-chip rect pushed by `draw_tab_strip`.
            rects.push((i, chip_resp.rect));
            ui.add_space(2.0);
        }
        // Centre the + add-tab button in the note-tab column (it used to hug the
        // left edge of the bar). `vertical_centered` centres the single button
        // horizontally in the column.
        ui.vertical_centered(|ui| {
            if tab_add_button(ui)
                .on_hover_text("New tab (Ctrl+N)")
                .clicked()
            {
                add_tab = true;
            }
        });
        // Subtle dividers between adjacent notes — a faint 1px hairline in each
        // inter-chip gap so the rotated note tabs read as distinct without a heavy
        // separator. The rotated column is always vertical, so each divider is a
        // horizontal line at the gap midpoint spanning the chip width. Painted as
        // a stroke (never a panel FILL) so it stays visible in transparency mode.
        // Mirrors the divider block in `draw_tab_strip`.
        if rects.len() > 1 {
            let painter = ui.painter();
            // Fix 4 — the rotated column is ALWAYS vertical, so every divider is a
            // horizontal hairline at the inter-chip gap midpoint. Bumped from the
            // old 0.30 alpha (near-invisible on the narrow side bar against the
            // dark panel fill) to a clearly legible theme-tinted 1px line.
            let hairline = egui::Stroke::new(1.0, muted.linear_multiply(0.55));
            for pair in rects.windows(2) {
                let (a, b) = (pair[0].1, pair[1].1);
                let y = (a.bottom() + b.top()) * 0.5;
                let x = egui::Rangef::new(a.left() + 3.0, a.right() - 3.0);
                painter.hline(x, y, hairline);
            }
        }
        // #82 test hooks (cfg(test) only): record the full chip rects the indicator
        // consumes, and let a test force the in-flight drag pointer so the insertion
        // line paints deterministically for the regression + visual-QA checks.
        #[cfg(test)]
        TEST_ROTATED_TAB_RECTS.with(|r| {
            *r.borrow_mut() = rects.iter().map(|(_, rect)| *rect).collect();
        });
        #[cfg(test)]
        let dragging = dragging.or_else(|| TEST_FORCE_SIDE_TAB_DRAG.with(|c| c.get()));
        // #59-parity insertion indicator: while a tab is in flight, paint an
        // accent hairline in the GAP the drop will land in. The rotated column is
        // always vertical, so the boundary is a horizontal line — drawn at the
        // inter-tab gap midpoint and spanning the FULL column width so it reads
        // as a separator BETWEEN rows, never as a mark inside a chip.
        if let Some(pointer) = dragging {
            if let Some((_, last_rect)) = rects.last().copied() {
                let x_range = ui.max_rect().x_range();
                let painter = ui.painter();
                let accent_line = egui::Stroke::new(2.0, accent);
                let mut drawn = false;
                for (idx, (_, rect)) in rects.iter().enumerate() {
                    if pointer.y < rect.center().y {
                        // Drop lands ABOVE this row: paint in the gap above it.
                        // Shared pure geometry with the non-rotated side strip so
                        // the inter-chip-midpoint rule lives in exactly one place.
                        let prev_bottom = (idx > 0).then(|| rects[idx - 1].1.bottom());
                        let y = side_tab_insertion_y(idx, rect.top(), prev_bottom);
                        painter.hline(x_range, y, accent_line);
                        drawn = true;
                        break;
                    }
                }
                if !drawn {
                    painter.hline(x_range, last_rect.bottom() + 1.0, accent_line);
                }
            }
        }
        // Drag-reorder drop resolution — the SAME nearest-chip model as the
        // horizontal strip: a direct hit wins; otherwise snap to the NEAREST chip
        // centre along the column's vertical axis, so a release in an inter-tab
        // gap, over the pin/close glyphs, or past either end still reorders. (The
        // old `contains`-only test silently no-op'd in all those cases — the
        // batch-1 fix only covered `draw_tab_strip`.)
        if let (Some(src), Some(pos)) = (drag_src, drop_pos) {
            let mut target: Option<usize> = rects
                .iter()
                .find(|(_, rect)| rect.contains(pos))
                .map(|(j, _)| *j);
            if target.is_none() {
                let mut best: Option<(usize, f32)> = None;
                for (j, rect) in &rects {
                    let d = (pos.y - rect.center().y).abs();
                    if best.is_none_or(|(_, bd)| d < bd) {
                        best = Some((*j, d));
                    }
                }
                target = best.map(|(j, _)| j);
            }
            if let Some(t) = target {
                if t != src {
                    reorder = Some((src, t));
                }
            }
        }
        if let Some(i) = switch_to {
            // Per-tab scroll + selection are preserved by the doc_id-salted editor
            // ScrollArea / TextEdit Ids (see frame_tick.rs), which egui retains per
            // note across EVERY switch path — so the switch itself only moves the
            // active index. (An earlier explicit scroll_y save/restore lived here
            // but was removed: it went stale on keyboard/palette/"already open"
            // switches that bypass this handler, then clobbered egui's correct
            // retained offset on the next tab-strip click. See the 2026-07-01
            // render/persistence audit.)
            self.active = i;
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
        if let Some(keep) = close_others {
            self.close_all_tabs_except(keep);
        }
        if let Some(after) = close_to_right {
            self.close_tabs_after(after);
        }
        if close_all {
            self.close_all_tabs();
        }
        if let Some(i) = toggle_pin {
            if i < self.tabs.len() {
                self.tabs[i].pinned = !self.tabs[i].pinned;
            }
        }
        if let Some((src, target)) = reorder {
            self.move_tab(src, target);
        }
        if add_tab {
            self.new_tab();
        }
    }

    /// Render the tab strip — the row (or column, for side positions) of open
    /// documents with the active one accented and an `×` close button on it.
    /// Extracted from the toolbar (T18.4) so the same widget can live inline at
    /// the top OR in a dedicated bottom / left / right panel. Mouse ergonomics:
    ///
    /// - **Click** → switch to that tab
    /// - **Middle-click** → close that tab (universal editor convention)
    /// - **Right-click** → context menu: Close · Close Others · Close All to the Right · Close All · Pin
    /// - **`×` button on the active tab** → close (back-compat with pre-audit behavior)
    /// - **Drag** → rearrange. Each tab is ONE `click_and_drag` widget (click
    ///   switches, drag reorders); the drop target is resolved AFTER the loop by
    ///   hit-testing the release position against every tab's full rect, so a
    ///   drop onto a tab to the RIGHT of the dragged one is no longer missed and
    ///   the extra `dnd_drop_zone` interaction that used to swallow the click is
    ///   gone. The index arithmetic lives in [`tab_index_after_move`] (unit-tested).
    ///   Closes F-001 / F-043 from `docs/audits/overlooked-surfaces-2026-05-29.md`.
    pub(super) fn draw_tab_strip(&mut self, ui: &mut egui::Ui, accent: Color32, muted: Color32) {
        let active = self.active;
        // Strip orientation is KNOWN from the parent layout, not inferred from
        // the tab rects: top/bottom wrap this strip in `ui.horizontal(...)`
        // (main_dir horizontal); a non-rotated SIDE bar wraps it in
        // `ui.vertical(...)` (main_dir top-down). The old center-delta inference
        // (Δx vs Δy of the first two chips) MISFIRED on a side bar whenever two
        // stacked tabs had very different widths — |Δx| could exceed the one-row
        // |Δy|, so the code drew VERTICAL dividers/insertion lines for a VERTICAL
        // bar (i.e. inside a tab, invisible as a separator). That was the "no
        // divider on left/right" bug. Deriving it from the layout is exact.
        let horizontal = ui.layout().main_dir().is_horizontal();
        // A LEFT/RIGHT bar in HORIZONTAL orientation (this fn's side variant):
        // titles truncate (or, opt-in, wrap to 2 lines) so the resizable panel
        // can shrink below the widest title. Top/bottom scroll instead.
        let side_bar = !horizontal;
        let two_line = side_bar && self.config.editor.side_tabs_wrap_two_lines;
        let label_font = egui::TextStyle::Body.resolve(ui.style());
        let mut switch_to = None;
        let mut close = None;
        let mut close_others = None;
        let mut close_to_right = None;
        let mut close_all = false;
        let mut toggle_pin: Option<usize> = None;
        // Reorder is resolved AFTER the loop from the dragged tab's release
        // position against the full set of tab rects. (The original code
        // hit-tested a half-built vector and missed drop targets to the right;
        // a later rewrite wrapped each tab in dnd_drop_zone/dnd_drag_source,
        // whose extra interaction swallowed the click so tabs couldn't be
        // switched. This uses ONE click_and_drag widget per tab — click switches,
        // drag reorders — with the drop resolved here against every rect.)
        let mut reorder: Option<(usize, usize)> = None;
        let mut drag_src: Option<usize> = None;
        let mut drop_pos: Option<egui::Pos2> = None;
        let mut rects: Vec<(usize, egui::Rect)> = Vec::with_capacity(self.tabs.len());
        let mut add_tab = false;
        // #59 live drag feedback: the in-flight (index, label, current pointer)
        // while a tab is being dragged, so we can paint a ghost following the
        // cursor and an insertion indicator at the drop gap.
        let mut dragging: Option<(usize, String, egui::Pos2)> = None;

        for i in 0..self.tabs.len() {
            let selected = i == active;
            let pinned = self.tabs[i].pinned;
            let shown = tab_display_label(&self.tabs[i].title(), pinned);
            let pin_label = if pinned { "Unpin tab" } else { "Pin tab" };
            // Each tab is a cohesive CHIP: the active tab gets a filled, rounded
            // accent background + a thin accent outline spanning the grip · label ·
            // pin · close so it reads as a real tab; inactive tabs are dimmed text.
            // Click = switch, drag = reorder, middle-click / ✕ = close. The fill +
            // outline + margins MATCH `draw_rotated_side_tabs` so a tab looks the
            // same size/shape in every orientation (horizontal and rotated).
            let chip = egui::Frame::default()
                .inner_margin(egui::Margin::symmetric(8, 4))
                .corner_radius(egui::CornerRadius::same(5))
                .stroke(if selected {
                    egui::Stroke::new(1.0, accent.linear_multiply(0.5))
                } else {
                    egui::Stroke::NONE
                })
                .fill(if selected {
                    accent.linear_multiply(0.20)
                } else {
                    Color32::TRANSPARENT
                });
            let mut prepared = chip.begin(ui);
            {
                let ui = &mut prepared.content_ui;
                // Force a single horizontal ROW for the chip contents (grip ·
                // name · pin · close). Top/bottom strips are already in a
                // horizontal parent so this is a no-op there; a SIDE strip's
                // parent is vertical, where without this wrap the name/pin/close
                // would stack into a ragged column per tab (#30).
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    // EVERY tab — top/bottom AND side — leads with an explicit drag
                    // GRIP so the row reads grip · name · pin · close, identical in
                    // all orientations and mirroring the split-pane header. Painted
                    // dots (`grip_handle`), never a phosphor glyph, so the handle is
                    // a clean grab affordance and can't tofu into an empty square (it
                    // is the single left-of-title affordance — there is no separate
                    // pin-glyph box). A pinned tab shows the grip DIMMED + drag-
                    // disabled, which is how pinned state now reads.
                    {
                        if pinned {
                            grip_handle(ui, false, muted, false)
                                .on_hover_text("Pinned — drag disabled");
                        } else {
                            let g = grip_handle(ui, true, muted, false)
                                .on_hover_text("Drag to reorder")
                                .on_hover_cursor(egui::CursorIcon::Grab);
                            if g.dragged() {
                                if let Some(p) = g.interact_pointer_pos() {
                                    dragging = Some((i, shown.clone(), p));
                                }
                            }
                            if g.drag_stopped() {
                                if let Some(p) = g.interact_pointer_pos() {
                                    drag_src = Some(i);
                                    drop_pos = Some(p);
                                }
                            }
                        }
                    }
                    let label_color = if selected { accent } else { muted };
                    let resp = if side_bar {
                        // Requirement 1/2 — LEFT/RIGHT horizontal side bar: the
                        // title truncates with … (single line), or wraps to at
                        // most 2 lines (opt-in) with the 2nd row elided when even
                        // two won't fit. A pre-layouted galley reports only its
                        // BOUNDED width, so the resizable side panel can shrink
                        // BELOW the longest title (egui would otherwise floor the
                        // panel width at the widest un-truncated label). Reserve
                        // room for the trailing pin/close so the title never
                        // collides with them as the bar narrows.
                        let (max_width, max_rows) =
                            side_tab_label_wrap(ui.available_width(), selected, two_line);
                        let mut job = egui::text::LayoutJob::single_section(
                            shown.clone(),
                            egui::text::TextFormat {
                                font_id: label_font.clone(),
                                color: label_color,
                                ..Default::default()
                            },
                        );
                        job.wrap = egui::text::TextWrapping {
                            max_width,
                            max_rows,
                            // Single line: break anywhere for a clean mid-word
                            // elision. Two lines: prefer word breaks; the 2nd row
                            // still elides with … via `max_rows`.
                            break_anywhere: !two_line,
                            overflow_character: Some('…'),
                        };
                        let galley = ui.fonts_mut(|f| f.layout_job(job));
                        ui.add(
                            egui::Label::new(galley)
                                .selectable(false)
                                .sense(egui::Sense::click_and_drag()),
                        )
                    } else {
                        // Top/bottom (and any non-side use): keep the full title on
                        // one line — the strip scrolls horizontally instead.
                        let label = RichText::new(shown.clone()).color(label_color);
                        ui.add(
                            egui::Label::new(label)
                                .selectable(false)
                                .sense(egui::Sense::click_and_drag()),
                        )
                    };
                    if resp.clicked() {
                        switch_to = Some(i);
                    }
                    if resp.clicked_by(egui::PointerButton::Middle) && !pinned {
                        close = Some(i);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Close").clicked() {
                            close = Some(i);
                            ui.close_menu();
                        }
                        if ui.button("Close Others").clicked() {
                            close_others = Some(i);
                            ui.close_menu();
                        }
                        if ui.button("Close All to the Right").clicked() {
                            close_to_right = Some(i);
                            ui.close_menu();
                        }
                        if ui.button("Close All").clicked() {
                            close_all = true;
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button(pin_label).clicked() {
                            toggle_pin = Some(i);
                            ui.close_menu();
                        }
                    });
                    // #R5: pinned notes are anchored — they switch on click but
                    // never initiate a drag-reorder (no ghost, no drop resolution).
                    if resp.dragged() && !pinned {
                        if let Some(p) = resp.interact_pointer_pos() {
                            dragging = Some((i, shown.clone(), p));
                        }
                    }
                    if resp.drag_stopped() && !pinned {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            drag_src = Some(i);
                            drop_pos = Some(pos);
                        }
                    }
                    // Pin TOGGLE — only on the active tab (the pin GLYPH in the
                    // label still marks any pinned tab, so non-active pins stay
                    // visible).
                    if selected {
                        let glyph = if pinned {
                            egui_phosphor::thin::PUSH_PIN_SLASH
                        } else {
                            egui_phosphor::thin::PUSH_PIN
                        };
                        if ui
                            .add(egui::Button::new(glyph).frame(false).small())
                            .on_hover_text(pin_label)
                            .clicked()
                        {
                            toggle_pin = Some(i);
                        }
                    }
                    // Close — on every UNPINNED tab. A pinned note hides the ✕ (and
                    // refuses middle-click / context Close) so it can't be closed by
                    // accident; unpin first (#R5).
                    if !pinned
                        && ui
                            .add(
                                egui::Button::new(egui_phosphor::thin::X)
                                    .frame(false)
                                    .small(),
                            )
                            .on_hover_text("Close tab (or middle-click)")
                            .clicked()
                    {
                        close = Some(i);
                    }
                });
            }
            // Fix 3 — faint hover fill on a NON-selected tab, written into the
            // frame's RESERVED background slot (from `begin`) so it paints BEHIND
            // the chip content and never washes over the label text. Selected
            // tabs keep their accent fill; this only lights up an inactive tab
            // the pointer is over.
            let chip_rect = prepared.frame.outer_rect(prepared.content_ui.min_rect());
            if !selected && ui.rect_contains_pointer(chip_rect) {
                prepared.frame.fill = tab_hover_fill(accent);
            }
            let chip_resp = prepared.end(ui);
            // Hit-test the FULL chip rect (label + pin + close + margins), not the
            // bare name-label — that was why a drop in the gap, or over the
            // pin/close area, or past the last tab silently did nothing.
            rects.push((i, chip_resp.rect));
        }

        // "+" — add a new tab at the end of the strip (same as Ctrl+N). Frameless-
        // until-hover + centred glyph, identical to a top-bar toolbar button.
        if tab_add_button(ui)
            .on_hover_text("New tab (Ctrl+N)")
            .clicked()
        {
            add_tab = true;
        }

        // #59 live drag feedback — paint while a tab is in flight:
        //  * an insertion indicator (accent line) at the gap the drop will land
        //  * a ghost of the dragged label following the cursor
        // Both are painted on the foreground (paint-only, never interactable —
        // a `layer_painter`, not an `Area`, so it cannot swallow clicks).
        if let Some((src, ref label, pointer)) = dragging {
            // Orientation is the KNOWN strip orientation (from the parent layout),
            // not a rect-delta guess — see the note at the top of the fn.
            // Insertion gap: the boundary nearest the pointer along the main
            // axis. We draw the line on the leading edge of the first tab whose
            // center is past the pointer (or the trailing edge of the last).
            //
            // On a VERTICAL side strip the indicator must span the FULL column
            // width and sit in the inter-row GAP — not `rect.x_range()` (the
            // chip's narrow content width) at `rect.top()`, which painted the
            // hairline ACROSS the tab's own grip/label/close widgets so it read
            // as a mark INSIDE the tab. Mirror `draw_rotated_side_tabs`: full
            // `ui.max_rect()` width, y at the midpoint of the gap between rows.
            // (Captured before the painter borrow to avoid aliasing `ui`.)
            let strip_x_range = ui.max_rect().x_range();
            let painter = ui.painter();
            let accent_line = egui::Stroke::new(2.0, accent);
            if let Some((_, last_rect)) = rects.last().copied().map(|r| (r.0, r.1)) {
                let mut drawn = false;
                for (idx, (_, rect)) in rects.iter().enumerate() {
                    let past = if horizontal {
                        pointer.x < rect.center().x
                    } else {
                        pointer.y < rect.center().y
                    };
                    if past {
                        if horizontal {
                            painter.vline(rect.left(), rect.y_range(), accent_line);
                        } else {
                            // Gap above this row: midpoint between the previous
                            // row's bottom and this row's top (or just above the
                            // first row), spanning the full column width.
                            let prev_bottom = (idx > 0).then(|| rects[idx - 1].1.bottom());
                            let y = side_tab_insertion_y(idx, rect.top(), prev_bottom);
                            painter.hline(strip_x_range, y, accent_line);
                        }
                        drawn = true;
                        break;
                    }
                }
                if !drawn {
                    // Pointer is beyond the last tab — indicate append-at-end.
                    if horizontal {
                        painter.vline(last_rect.right(), last_rect.y_range(), accent_line);
                    } else {
                        painter.hline(strip_x_range, last_rect.bottom() + 1.0, accent_line);
                    }
                }
            }

            // Ghost label trailing the cursor (slightly offset so it doesn't sit
            // under the pointer). Drawn on the Tooltip layer so it floats above
            // the strip without taking input.
            let ghost = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::new("tab-drag-ghost"),
            ));
            let font = egui::TextStyle::Button.resolve(ui.style());
            let ghost_pos = pointer + egui::vec2(12.0, 6.0);
            let galley = ghost.layout_no_wrap(label.clone(), font, accent);
            // Soft backing chip for legibility against any background.
            let bg =
                egui::Rect::from_min_size(ghost_pos, galley.size()).expand2(egui::vec2(6.0, 3.0));
            ghost.rect_filled(bg, 4.0, muted.linear_multiply(0.25));
            ghost.galley(ghost_pos, galley, accent);
            let _ = src;
        }

        // Drag-reorder drop resolution. Use the SAME model as the live indicator:
        // a direct hit on a chip wins; otherwise snap to the NEAREST chip centre
        // along the strip's main axis. This means a release in an inter-tab gap,
        // over the pin/close area, or past either end still reorders (the old
        // name-label-`contains` test silently no-op'd in all those cases).
        if let (Some(src), Some(pos)) = (drag_src, drop_pos) {
            // Same KNOWN orientation as the divider/indicator (from the parent
            // layout) — no fragile rect-delta guess.
            let mut target: Option<usize> = rects
                .iter()
                .find(|(_, rect)| rect.contains(pos))
                .map(|(j, _)| *j);
            if target.is_none() {
                let mut best: Option<(usize, f32)> = None;
                for (j, rect) in &rects {
                    let d = if horizontal {
                        (pos.x - rect.center().x).abs()
                    } else {
                        (pos.y - rect.center().y).abs()
                    };
                    if best.is_none_or(|(_, bd)| d < bd) {
                        best = Some((*j, d));
                    }
                }
                target = best.map(|(j, _)| j);
            }
            if let Some(t) = target {
                if t != src {
                    reorder = Some((src, t));
                }
            }
        }

        // Subtle dividers between tabs — a faint hairline in each inter-chip gap
        // so the tabs read as distinct without heavy separators. Painted after
        // the strip is laid out, using the KNOWN strip orientation (see the note
        // at the top of the fn): a horizontal strip → vertical dividers between
        // columns; a vertical SIDE bar → horizontal dividers between rows. This
        // is the actual left/right-divider fix — the old center-delta guess drew
        // vertical lines inside a side bar's tabs, so no separator was visible.
        if rects.len() > 1 {
            let painter = ui.painter();
            // Bumped from the old 0.30 alpha (near-invisible on the narrow side
            // bar against the dark panel) to a clearly legible theme-tinted line.
            let hairline = egui::Stroke::new(1.0, muted.linear_multiply(0.55));
            for pair in rects.windows(2) {
                let (a, b) = (pair[0].1, pair[1].1);
                if horizontal {
                    let x = (a.right() + b.left()) * 0.5;
                    let y = egui::Rangef::new(a.top() + 3.0, a.bottom() - 3.0);
                    painter.vline(x, y, hairline);
                } else {
                    let y = (a.bottom() + b.top()) * 0.5;
                    let x = egui::Rangef::new(a.left() + 3.0, a.right() - 3.0);
                    painter.hline(x, y, hairline);
                }
            }
        }

        if let Some(i) = switch_to {
            // Per-tab scroll + selection are preserved by the doc_id-salted editor
            // ScrollArea / TextEdit Ids (see frame_tick.rs), which egui retains per
            // note across EVERY switch path — so the switch itself only moves the
            // active index. (An earlier explicit scroll_y save/restore lived here
            // but was removed: it went stale on keyboard/palette/"already open"
            // switches that bypass this handler, then clobbered egui's correct
            // retained offset on the next tab-strip click. See the 2026-07-01
            // render/persistence audit.)
            self.active = i;
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
        if let Some(keep) = close_others {
            self.close_all_tabs_except(keep);
        }
        if let Some(after) = close_to_right {
            self.close_tabs_after(after);
        }
        if close_all {
            self.close_all_tabs();
        }
        if let Some(i) = toggle_pin {
            if i < self.tabs.len() {
                self.tabs[i].pinned = !self.tabs[i].pinned;
            }
        }
        if let Some((src, target)) = reorder {
            self.move_tab(src, target);
        }
        if add_tab {
            self.new_tab();
        }
    }
}

#[cfg(test)]
mod side_tab_label_wrap_tests {
    use super::side_tab_label_wrap;

    #[test]
    fn single_line_by_default_and_reserves_room_for_trailing_controls() {
        // OFF → exactly one row (truncate with ellipsis), never a wrap.
        let (w, rows) = side_tab_label_wrap(200.0, false, false);
        assert_eq!(rows, 1, "2-line OFF ⇒ single-line truncation");
        // The label max-width is BELOW the available width (room reserved for the
        // trailing close ✕) — this is what lets the panel shrink below the title.
        assert!(
            w < 200.0,
            "must reserve trailing space so the bar can shrink"
        );
        assert!((w - (200.0 - 28.0)).abs() < f32::EPSILON);
    }

    #[test]
    fn selected_reserves_more_for_pin_plus_close() {
        // A selected tab carries pin + close, so it reserves MORE than a plain tab.
        let (sel, _) = side_tab_label_wrap(200.0, true, false);
        let (plain, _) = side_tab_label_wrap(200.0, false, false);
        assert!(
            sel < plain,
            "selected reserves for pin + close, not just close"
        );
    }

    #[test]
    fn two_line_option_wires_through_to_max_rows() {
        // The config toggle is honoured: ON ⇒ up to TWO rows.
        let (_, rows_on) = side_tab_label_wrap(200.0, false, true);
        assert_eq!(rows_on, 2, "2-line ON ⇒ max two rows");
        let (_, rows_off) = side_tab_label_wrap(200.0, false, false);
        assert_eq!(rows_off, 1, "2-line OFF ⇒ one row");
    }

    #[test]
    fn width_floors_at_a_small_positive_on_a_very_narrow_bar() {
        // Dragged narrower than the reserve → the galley width never goes to
        // zero/negative (which would panic or vanish); it floors at 24px so a
        // sliver of the elided title still shows.
        let (w, _) = side_tab_label_wrap(10.0, true, false);
        assert!(w >= 24.0, "max-width floors at 24px on an ultra-narrow bar");
    }
}
