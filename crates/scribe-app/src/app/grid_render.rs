//! Per-pane grid header chip rendering, extracted from
//! `ScribeApp::render_grid_central_panel` (A-01).
//!
//! Behavior-neutral split: the chip layout (wide one-row vs narrow centered
//! column, pin toggle, close button, drag handle) is moved verbatim out of the
//! `render_body` closure. The caller passes the focused-pane `tab`, whether the
//! pane is active, the two theme colours, and the shared per-frame `closes`
//! buffer; the helper returns whether a tile drag started this frame.

use eframe::egui;
use egui::{Color32, RichText};
use std::cell::RefCell;

use super::{grip_handle, EditorTab};
use crate::grid::DocId;

/// Render one grid pane's header chip and return whether a tile drag started.
///
/// Moved verbatim from the inline `render_body` closure: same wide/narrow
/// layout split, same pin/close controls, same drag-handle behaviour. The only
/// mechanical change is `tabs[idx]` -> `tab` and `is_active` arriving as a
/// parameter instead of being recomputed from `active_doc`.
pub(super) fn render_pane_header(
    ui: &mut egui::Ui,
    tab: &mut EditorTab,
    doc_id: DocId,
    is_active: bool,
    accent: Color32,
    muted: Color32,
    render_closes: &RefCell<Vec<DocId>>,
) -> bool {
    let mut drag_started = false;
    // #R5 — per-pane header rendered as a tab CHIP that mirrors the
    // top tab strip: a filled accent chip on the focused pane,
    // transparent otherwise; drag-handle ICON on the left, note name,
    // pin toggle, close ✕ on the far right. Pinned notes drop the
    // drag handle + ✕ (anchored, can't be moved/closed). All glyphs
    // are phosphor (the old ✕ / ⠿ were tofu).
    let pane_title = tab.title();
    let pinned = tab.pinned;
    let chip = egui::Frame::default()
        .inner_margin(egui::Margin::symmetric(8, 3))
        .corner_radius(egui::CornerRadius::same(5))
        .fill(if is_active {
            accent.linear_multiply(0.20)
        } else {
            Color32::TRANSPARENT
        });
    // Header layout adapts to pane width. A WIDE pane gets ONE row
    // (handle · name · pin, with the close ✕ on the far right); a
    // NARROW pane gets a single CENTERED column (name, then pin, then
    // close) so the controls never wrap into a ragged stack. All
    // glyphs are phosphor (now resolved in monospace too, so no tofu).
    let pin_glyph = if pinned {
        egui_phosphor::thin::PUSH_PIN_SLASH
    } else {
        egui_phosphor::thin::PUSH_PIN
    };
    if ui.available_width() >= 220.0 {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            chip.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    // Drag handle — a neutral `muted` grip so it stays
                    // visible on BOTH the active (accent-tinted) and
                    // inactive chip backgrounds (an accent-coloured grip
                    // vanished against the active chip's accent fill).
                    let grip_color = muted;
                    if pinned {
                        grip_handle(ui, false, grip_color, false)
                            .on_hover_text("Pinned — drag disabled");
                    } else {
                        // `drag_started()` fires ONCE on drag start (egui_tiles
                        // expects a single `DragStarted`); a held-button check
                        // would re-fire every frame and wedge the tile drag.
                        let handle = grip_handle(ui, true, grip_color, false)
                            .on_hover_text("Drag to rearrange")
                            .on_hover_cursor(egui::CursorIcon::Grab);
                        if handle.drag_started() {
                            drag_started = true;
                        }
                    }
                    ui.label(RichText::new(&pane_title).monospace().color(if is_active {
                        accent
                    } else {
                        muted
                    }))
                    .on_hover_text(&pane_title);
                    if ui
                        .add(egui::Button::new(pin_glyph).frame(false).small())
                        .on_hover_text(if pinned { "Unpin note" } else { "Pin note" })
                        .clicked()
                    {
                        tab.pinned = !pinned;
                    }
                });
            });
            // Close at the far right — hidden on pinned notes.
            if !pinned {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(egui_phosphor::thin::X)
                                .frame(false)
                                .small(),
                        )
                        .on_hover_text("Close pane")
                        .clicked()
                    {
                        render_closes.borrow_mut().push(doc_id);
                    }
                });
            }
        });
    } else {
        // NARROW pane: a single centered column — name, pin, close.
        // The name doubles as the drag handle (no room for a grip).
        chip.show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let name = ui.add(
                    egui::Label::new(RichText::new(&pane_title).monospace().color(if is_active {
                        accent
                    } else {
                        muted
                    }))
                    .sense(if pinned {
                        egui::Sense::hover()
                    } else {
                        egui::Sense::click_and_drag()
                    }),
                );
                if !pinned && name.drag_started() {
                    drag_started = true;
                }
                if ui
                    .add(egui::Button::new(pin_glyph).frame(false).small())
                    .on_hover_text(if pinned { "Unpin note" } else { "Pin note" })
                    .clicked()
                {
                    tab.pinned = !pinned;
                }
                if !pinned
                    && ui
                        .add(
                            egui::Button::new(egui_phosphor::thin::X)
                                .frame(false)
                                .small(),
                        )
                        .on_hover_text("Close pane")
                        .clicked()
                {
                    render_closes.borrow_mut().push(doc_id);
                }
            });
        });
    }
    drag_started
}
