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
            let chip_resp = chip.show(ui, |ui| {
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
            });
            // #82 — record the FULL chip frame rect (grip · rotated label · pin ·
            // close · margins), NOT the inner rotated-label rect, so the
            // drop-insertion indicator and the nearest-chip drop resolution use the
            // tab's real outline. Pushing the inner label rect made the inter-tab
            // "gap" midpoint land INSIDE a neighbouring chip's outline (over its
            // grip/close glyphs) — the drop line appeared inside the tab. Mirrors
            // the full-chip rect pushed by `draw_tab_strip`.
            rects.push((i, chip_resp.response.rect));
            ui.add_space(2.0);
        }
        // Centre the + add-tab button in the note-tab column (it used to hug the
        // left edge of the bar). `vertical_centered` centres the single button
        // horizontally in the column.
        ui.vertical_centered(|ui| {
            if ui
                .small_button("+")
                .on_hover_text("New tab (Ctrl+N)")
                .clicked()
            {
                add_tab = true;
            }
        });
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
            let chip_resp = chip.show(ui, |ui| {
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
                    let label =
                        RichText::new(shown.clone()).color(if selected { accent } else { muted });
                    let resp = ui.add(
                        egui::Label::new(label)
                            .selectable(false)
                            .sense(egui::Sense::click_and_drag()),
                    );
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
            });
            // Hit-test the FULL chip rect (label + pin + close + margins), not the
            // bare name-label — that was why a drop in the gap, or over the
            // pin/close area, or past the last tab silently did nothing.
            rects.push((i, chip_resp.response.rect));
        }

        // "+" — add a new tab at the end of the strip (same as Ctrl+N).
        if ui
            .small_button("+")
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
            // Infer strip orientation from the first two tab rects: when tabs
            // advance mostly in X the strip is horizontal (top/bottom); mostly
            // in Y means a vertical side strip.
            let horizontal = rects.len() < 2
                || (rects[1].1.center().x - rects[0].1.center().x).abs()
                    >= (rects[1].1.center().y - rects[0].1.center().y).abs();

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
            let horizontal = rects.len() < 2
                || (rects[1].1.center().x - rects[0].1.center().x).abs()
                    >= (rects[1].1.center().y - rects[0].1.center().y).abs();
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
        // the strip is laid out, using the same horizontal/vertical inference.
        if rects.len() > 1 {
            let horizontal = (rects[1].1.center().x - rects[0].1.center().x).abs()
                >= (rects[1].1.center().y - rects[0].1.center().y).abs();
            let painter = ui.painter();
            let hairline = egui::Stroke::new(1.0, muted.linear_multiply(0.30));
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
