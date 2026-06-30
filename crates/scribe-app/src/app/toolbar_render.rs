//! Quick-access toolbar rendering — extracted from `mod.rs` (A-01 wave 2).
#![allow(clippy::wildcard_imports)]

use super::*;

impl ScribeApp {
    /// Render one quick-access toolbar entry by action id and apply its effect.
    /// Buttons set the pending-action flags; toggles flip the live config/state
    /// and request a config save. The id `"sep"` draws a divider.
    // The explicit `=> { if widget.clicked() { effect } }` per arm is clearer
    // than clippy's suggested match-guard form, which would render the widget as
    // a side effect inside the guard condition.
    #[allow(clippy::collapsible_match)]
    /// Render the quick-access toolbar contents (command-palette button, the
    /// user-ordered items, and the optional user-curated "⋯" dropdown) into
    /// `ui`. Shared by the toolbar panel and the in-titlebar placement
    /// (`appearance.toolbar_in_titlebar`). The settings gear lives in the window
    /// caption row (left of Minimize), not here.
    pub(super) fn toolbar_contents(
        &mut self,
        ui: &mut egui::Ui,
        act: &mut Pending,
        save_cfg: &mut bool,
        start_lsp: &mut bool,
    ) {
        // NOTE: the settings "gear" used to lead this toolbar; it was RELOCATED
        // into the window caption row (left of Minimize) — see `caption_btn` in
        // `chrome.rs` + the titlebar caption block in `frame_tick.rs`. The
        // command-palette ">_" button stays here as the toolbar's lead control.
        if ui
            .button(">_")
            .on_hover_text("Command palette (Ctrl+Shift+P)")
            .clicked()
        {
            self.palette_open = true;
            self.focus_palette = true;
            self.palette_query.clear();
            // BUG-APP-01: fresh open starts the keyboard highlight at the top.
            self.palette_selected = 0;
        }
        ui.separator();
        // User-customizable quick-access items (membership + order from
        // config.toolbar; editable in Settings → Toolbar). When the bar is
        // narrow (notably the in-titlebar toolbar on a small window), render as
        // many items as FIT and fold the rest into the "⋯ more actions" dropdown
        // — the user's "compress up to a point, keep the contents legible/
        // reachable" intent — instead of clipping them off the edge where they
        // become invisible AND unclickable. `available_width()` is egui's real
        // remaining width at this cursor (after the pinned gear/palette/wordmark),
        // so on a wide bar everything fits and nothing folds (unchanged).
        let items = self.config.toolbar.items.clone();
        let bspace = self.config.toolbar.clamped_button_spacing();
        let item_w = (self.config.toolbar.clamped_button_size() + bspace).max(1.0);
        let dropdown_w = 22.0 + bspace;
        let visible = toolbar_visible_count(ui.available_width(), item_w, dropdown_w, items.len());
        for id in &items[..visible] {
            self.toolbar_item(ui, id, act, save_cfg, start_lsp);
        }
        let overflow: Vec<String> = items[visible..].to_vec();
        // User-curated overflow dropdown — actions the user parked here to keep
        // the bar clean — PLUS any items that didn't fit above (folded so they
        // stay reachable). Shown whenever the toggle is ON (even with an empty
        // menu, so the toggle has a VISIBLE effect — previously it was gated on a
        // non-empty menu, so turning it on with the default empty menu looked
        // inert, the "toggle does nothing" report) OR whenever there is forced
        // overflow to surface. An otherwise-empty menu opens to a hint pointing
        // at the editor. The trigger is PAINTED three dots, NOT the "⋯" glyph:
        // U+22EF renders as a tofu □ in this build's font atlas (the same
        // egui-phosphor .notdef footgun that forced the grip to paint its dots).
        // Painted dots are font-independent and read as a clean "more actions"
        // affordance.
        let menu = self.config.toolbar.menu.clone();
        if self.config.toolbar.show_dropdown || !overflow.is_empty() {
            let dot = ui.visuals().weak_text_color();
            let btn_h = ui.spacing().interact_size.y;
            let btn = egui::Button::new("").min_size(egui::vec2(22.0, btn_h));
            let resp = egui::menu::menu_custom_button(ui, btn, |ui| {
                let mut any = false;
                // Forced overflow first (the items that didn't fit the bar).
                for id in &overflow {
                    self.toolbar_item(ui, id, act, save_cfg, start_lsp);
                    any = true;
                }
                // Then the user-parked menu, separated when both are present.
                if !menu.is_empty() {
                    if any {
                        ui.separator();
                    }
                    for id in &menu {
                        self.toolbar_item(ui, id, act, save_cfg, start_lsp);
                    }
                    any = true;
                }
                // Empty-state hint only when there is genuinely nothing to show
                // (the toggle is on but no overflow and no parked actions).
                if !any {
                    ui.set_min_width(180.0);
                    ui.label(egui::RichText::new("No actions added yet").strong());
                    ui.label(
                        egui::RichText::new(
                            "Add actions in Settings → Toolbar → More-actions menu.",
                        )
                        .weak(),
                    );
                    if ui.button("Open toolbar settings").clicked() {
                        self.settings_open = true;
                        ui.close_menu();
                    }
                }
            })
            .response
            .on_hover_text("More actions");
            // Expose an accessible name for the icon-only (painted-dots) trigger.
            // The `Button::new("")` above carries no text, so without this the
            // node reaches the AccessKit tree as an UNNAMED interactive Button —
            // a screen-reader dead end (WCAG 4.1.2 Name/Role/Value). `on_hover_text`
            // sets a tooltip/description, NOT the accessible name, so it is set
            // explicitly here.
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "More actions")
            });
            // Paint a horizontal 3-dot "⋯" centered in the button rect.
            let c = resp.rect.center();
            let painter = ui.painter();
            for dx in [-4.0_f32, 0.0, 4.0] {
                painter.circle_filled(egui::pos2(c.x + dx, c.y), 1.6, dot);
            }
        }
    }

    // clippy 1.95's `collapsible_match` wants each `"id" => { if ui.button(..)
    // .clicked() { .. } }` arm rewritten with the button call as a match GUARD.
    // That would move a side-effecting widget call into the guard — worse style
    // (guards should be pure) — so the per-arm `if` is intentional here.
    #[allow(clippy::collapsible_match)]
    fn toolbar_item(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        act: &mut Pending,
        save_cfg: &mut bool,
        start_lsp: &mut bool,
    ) {
        // Phase 16 T16.3: every toolbar label routes through `toolbar_widget(id, icons, jp, size)`
        // so flipping `appearance.toolbar_icons` swaps every entry between its text
        // form and its Phosphor (Thin) glyph in one place. Phase 17 T17.5: the
        // same helper also appends a verified-canonical kanji "instrument plate"
        // when `appearance.jp_glyph_labels` is on (English-redundant, dimmed, smaller).
        let icons = self.config.appearance.toolbar_icons;
        let jp = self.config.appearance.jp_glyph_labels;
        // Phase 18 T18.5: the icon-size slider drives every toolbar glyph/label.
        let size = self.config.toolbar.clamped_icon_size();
        // #22 — a SELECTED toggle's label is pinned to the theme accent (so it
        // reads as "on" in both kanji-on and kanji-off states). Plain buttons and
        // unselected toggles pass `Color32::PLACEHOLDER` to follow the widget's
        // own fg (preserving hover/active brightening). `sel(on)` is the helper.
        let accent = ui_color(&self.theme, "accent", Rgba::new(0, 255, 254, 255));
        let sel = |on: bool| if on { accent } else { Color32::PLACEHOLDER };
        match id {
            "sep" => {
                ui.separator();
            }
            "new" => {
                if ui
                    .button(toolbar_widget("new", icons, jp, size, Color32::PLACEHOLDER))
                    .on_hover_text("New file (Ctrl+N)")
                    .clicked()
                {
                    act.new = true;
                }
            }
            "open" => {
                if ui
                    .button(toolbar_widget(
                        "open",
                        icons,
                        jp,
                        size,
                        Color32::PLACEHOLDER,
                    ))
                    .on_hover_text("Open file (Ctrl+O)")
                    .clicked()
                {
                    act.open = true;
                }
            }
            "openfolder" => {
                if ui
                    .button(toolbar_widget(
                        "openfolder",
                        icons,
                        jp,
                        size,
                        Color32::PLACEHOLDER,
                    ))
                    .on_hover_text("Open folder")
                    .clicked()
                {
                    act.open_folder = true;
                }
            }
            "save" => {
                if ui
                    .button(toolbar_widget(
                        "save",
                        icons,
                        jp,
                        size,
                        Color32::PLACEHOLDER,
                    ))
                    .on_hover_text("Save (Ctrl+S)")
                    .clicked()
                {
                    act.save = true;
                }
            }
            "saveas" => {
                if ui
                    .button(toolbar_widget(
                        "saveas",
                        icons,
                        jp,
                        size,
                        Color32::PLACEHOLDER,
                    ))
                    .on_hover_text("Save As…")
                    .clicked()
                {
                    self.save_as_active();
                }
            }
            "find" => {
                if ui
                    .button(toolbar_widget(
                        "find",
                        icons,
                        jp,
                        size,
                        Color32::PLACEHOLDER,
                    ))
                    .on_hover_text("Find (Ctrl+F)")
                    .clicked()
                {
                    self.find_open = true;
                    self.focus_find = true;
                }
            }
            "palette" => {
                if ui
                    .button(toolbar_widget(
                        "palette",
                        icons,
                        jp,
                        size,
                        Color32::PLACEHOLDER,
                    ))
                    .on_hover_text("Command palette")
                    .clicked()
                {
                    self.palette_open = true;
                    self.focus_palette = true;
                    self.palette_query.clear();
                    // BUG-APP-01: fresh open starts the highlight at the top.
                    self.palette_selected = 0;
                }
            }
            "split" => {
                // Split and grid are one feature: this toggles the multi-pane
                // view, which lays the OPEN TABS out as panes — two tabs read as
                // a side-by-side split, and it grows into a grid as more tabs
                // open. (Same `editor.grid_enabled` the grid command toggles.)
                if ui
                    .selectable_label(
                        self.config.editor.grid_enabled,
                        toolbar_widget(
                            "split",
                            icons,
                            jp,
                            size,
                            sel(self.config.editor.grid_enabled),
                        ),
                    )
                    .on_hover_text(
                        "Split / grid view — show the open notes side by side. \
                         Opening more notes grows the split into a grid.",
                    )
                    .clicked()
                {
                    self.config.editor.grid_enabled = !self.config.editor.grid_enabled;
                    *save_cfg = true;
                }
            }
            "minimap" => {
                if ui
                    .selectable_label(
                        self.config.editor.show_minimap,
                        toolbar_widget(
                            "minimap",
                            icons,
                            jp,
                            size,
                            sel(self.config.editor.show_minimap),
                        ),
                    )
                    .on_hover_text("Minimap")
                    .clicked()
                {
                    self.config.editor.show_minimap = !self.config.editor.show_minimap;
                    *save_cfg = true;
                }
            }
            "wrap" => {
                if ui
                    .selectable_label(
                        self.config.editor.word_wrap,
                        toolbar_widget("wrap", icons, jp, size, sel(self.config.editor.word_wrap)),
                    )
                    .on_hover_text("Word wrap")
                    .clicked()
                {
                    self.config.editor.word_wrap = !self.config.editor.word_wrap;
                    *save_cfg = true;
                }
            }
            "fold" => {
                if ui
                    .selectable_label(
                        self.fold_view,
                        toolbar_widget("fold", icons, jp, size, sel(self.fold_view)),
                    )
                    .on_hover_text("Folded view")
                    .clicked()
                {
                    self.fold_view = !self.fold_view;
                }
            }
            "linenumbers" => {
                if ui
                    .selectable_label(
                        self.config.editor.show_line_numbers,
                        toolbar_widget(
                            "linenumbers",
                            icons,
                            jp,
                            size,
                            sel(self.config.editor.show_line_numbers),
                        ),
                    )
                    .on_hover_text("Line numbers")
                    .clicked()
                {
                    self.config.editor.show_line_numbers = !self.config.editor.show_line_numbers;
                    *save_cfg = true;
                }
            }
            "spellcheck" => {
                if ui
                    .selectable_label(
                        self.config.spellcheck.enabled,
                        toolbar_widget(
                            "spellcheck",
                            icons,
                            jp,
                            size,
                            sel(self.config.spellcheck.enabled),
                        ),
                    )
                    .on_hover_text("Spellcheck (offline)")
                    .clicked()
                {
                    self.config.spellcheck.enabled = !self.config.spellcheck.enabled;
                    *save_cfg = true;
                }
            }
            "lsp" => {
                if ui
                    .button(toolbar_widget("lsp", icons, jp, size, Color32::PLACEHOLDER))
                    .on_hover_text("Start language server")
                    .clicked()
                {
                    *start_lsp = true;
                }
            }
            _ => {}
        }
    }
}
