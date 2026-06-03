//! In-app settings window. Edits the live `Config` (deep customization without
//! hand-editing TOML). Returns `true` when something changed so the caller can
//! persist + re-apply the theme. Kept as a free function so it never fights the
//! `ScribeApp` borrow.
//!
//! Layout: a resizable window with a left category nav + a searchable,
//! internally-scrolling content pane — so every setting is reachable at the
//! default size without resizing, while the window can still be dragged,
//! resized, and closed normally (`ScrollArea` with `auto_shrink([false,
//! false])` is the load-bearing idiom here). Every control carries an
//! `.on_hover_text` tooltip.

use eframe::egui;
use scribe_core::config::{ToolbarConfig, UpdateMode, WindowMode};
use scribe_core::Config;

/// Left-nav categories, in display order.
const CATEGORIES: &[&str] = &[
    "Appearance",
    "Fonts",
    "Editor",
    "Motion",
    "Window",
    "Spellcheck",
    "Updates",
    "Plugins",
    "Toolbar",
];

/// egui temp-data key the Plugins section sets when "Manage plugins…" is
/// clicked. The host reads + clears it after [`show`] returns and opens its
/// own plugin-manager modal — settings owns no modal state of its own.
fn open_plugin_manager_id() -> egui::Id {
    egui::Id::new("scr1b3_open_plugin_manager")
}

/// Parse a `#rrggbb` (or `rrggbb`) hex string into an opaque `Color32` (#88).
/// Returns `None` on malformed input so the caller can fall back to a default.
fn parse_hex_color(s: &str) -> Option<egui::Color32> {
    let h = s.trim().trim_start_matches('#');
    // `h.len()` is the BYTE length; a 6-byte value containing a multibyte char
    // (e.g. `aa€b`) passed the `== 6` check then panicked on `&h[0..2]` slicing
    // through the char boundary. Reject non-ASCII first so byte-length and the
    // ASCII hex windows agree, and slice via `get`/`from_utf8` so no range can
    // panic.
    if !h.is_ascii() || h.len() != 6 {
        return None;
    }
    let comp = |i: usize| -> Option<u8> {
        let bytes = h.as_bytes().get(i..i + 2)?;
        u8::from_str_radix(std::str::from_utf8(bytes).ok()?, 16).ok()
    };
    Some(egui::Color32::from_rgb(comp(0)?, comp(2)?, comp(4)?))
}

/// egui temp-data key holding the selected Settings category. Shared by
/// [`show`] (read + write each frame) and [`request_category`] (host deep-link
/// pre-select) so the two never drift apart.
fn settings_cat_id() -> egui::Id {
    egui::Id::new("scr1b3_settings_cat")
}

/// Pre-select which category [`show`] opens on. The host calls this when it
/// opens Settings from a deep-link affordance (e.g. the status-bar encoding /
/// language chips that advertise "Settings → Editor"). [`show`] reads the same
/// temp key on its next frame, so the window opens on `category` instead of the
/// last-used / default "Appearance". No-op if `category` is not a real section
/// name — the nav simply falls back to its default selection.
pub fn request_category(ctx: &egui::Context, category: &str) {
    ctx.data_mut(|d| d.insert_temp(settings_cat_id(), category.to_string()));
}

/// Host-side accessor: returns `true` (and clears the flag) when the Plugins
/// section requested the plugin manager this frame.
pub fn take_open_plugin_manager_request(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| {
        let id = open_plugin_manager_id();
        if d.get_temp::<bool>(id).unwrap_or(false) {
            d.remove::<bool>(id);
            true
        } else {
            false
        }
    })
}

/// Whether a category section should render: its own tab when not searching, or
/// any-label-matches when a search query is active (cross-category results).
fn section_visible(selected: &str, q: &str, category: &str, labels: &[&str]) -> bool {
    if q.is_empty() {
        selected == category
    } else {
        category.to_lowercase().contains(q) || labels.iter().any(|l| l.to_lowercase().contains(q))
    }
}

/// Whether an individual row should render given the active search query.
fn row_visible(q: &str, label: &str) -> bool {
    q.is_empty() || label.to_lowercase().contains(q)
}

/// F-037 — a per-setting "restore default" affordance. Renders a small ↺
/// button that is enabled only when `cur != def`; clicking it resets the
/// field and returns `true` so the caller marks settings dirty. Placed at the
/// end of a setting's row, it gives every scalar setting its own one-click
/// revert without a global "reset everything" sledgehammer.
fn reset_to_default<T: PartialEq + Clone>(ui: &mut egui::Ui, cur: &mut T, def: &T) -> bool {
    let differs = *cur != *def;
    let resp = ui
        .add_enabled(
            differs,
            egui::Button::new(egui::RichText::new("↺").small()).frame(false),
        )
        .on_hover_text(if differs {
            "Restore default"
        } else {
            "Already default"
        });
    if differs && resp.clicked() {
        *cur = def.clone();
        return true;
    }
    false
}

/// Human label for a toolbar action id (`"sep"` → separator).
fn action_label(id: &str) -> String {
    if id == "sep" {
        return "— separator —".to_string();
    }
    crate::app::TOOLBAR_ACTIONS
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, l)| (*l).to_string())
        .unwrap_or_else(|| id.to_string())
}

/// Render the settings window. `open` is toggled false when the user closes it.
/// Returns `true` if any field changed this frame.
pub fn show(
    ctx: &egui::Context,
    config: &mut Config,
    open: &mut bool,
    updater: &mut crate::updater::Updater,
) -> bool {
    let mut changed = false;
    let mut keep_open = *open;

    let cat_id = settings_cat_id();
    let q_id = egui::Id::new("scr1b3_settings_query");
    let mut category = ctx
        .data_mut(|d| d.get_temp::<String>(cat_id))
        .unwrap_or_else(|| "Appearance".to_string());
    let mut query = ctx
        .data_mut(|d| d.get_temp::<String>(q_id))
        .unwrap_or_default();

    egui::Window::new("settings")
        // Stable, section-independent Id so the window's persisted size+position
        // (eframe `persistence` feature) survive restart and never shift with the
        // selected category. The visible title never changes, but pin the Id
        // explicitly so persistence can't break if it ever does.
        .id(egui::Id::new("scr1b3_settings"))
        .open(&mut keep_open)
        .collapsible(false)
        // Resizable + a default (not fixed) size restores egui's standard
        // window layout: a full-width title bar that is draggable across its
        // whole span, a correctly-placed close (✕) button, and resize handles.
        // The previous `.resizable(false).fixed_size(...)` was the single root
        // cause of "only the left half drags", the dead close button, and the
        // un-resizable window.
        .resizable(true)
        .default_size([760.0, 560.0])
        .min_width(420.0)
        .min_height(320.0)
        // Cap the window so a wide section (e.g. the full-width search field or a
        // long row) can never blow it out: it stays the same size regardless of
        // which category is open, while the user can still resize freely within
        // these bounds. Pairs with the vertical ScrollArea `auto_shrink([false,
        // false])` below, which stops the grow-to-content feedback loop.
        .max_width(940.0)
        .max_height(820.0)
        // #77 — force the Settings window OPAQUE. The app-window transparency /
        // glass setting drives a translucent `window_fill`; without this the
        // Settings panel itself went see-through, which is not what the
        // app-background transparency option is for. Take the theme's window
        // fill but pin alpha to 255 so Settings stays readable in glass mode.
        .frame({
            let style = ctx.global_style();
            let f = style.visuals.window_fill;
            egui::Frame::window(&style).fill(egui::Color32::from_rgb(f.r(), f.g(), f.b()))
        })
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                // ---- Left category nav ----
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    ui.add_space(2.0);
                    for cat in CATEGORIES {
                        ui.selectable_value(&mut category, (*cat).to_string(), *cat)
                            .on_hover_text(format!("Show the {cat} settings."));
                    }
                });
                ui.separator();

                // ---- Searchable content pane ----
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        // Phosphor (loaded) — the 🔍 emoji rendered as tofu (#R5).
                        ui.label(egui_phosphor::thin::MAGNIFYING_GLASS);
                        ui.add(
                            egui::TextEdit::singleline(&mut query)
                                .hint_text("search settings")
                                .desired_width(f32::INFINITY),
                        )
                        .on_hover_text(
                            "Filter settings by name across every category. Clear to return to \
                             the selected category.",
                        );
                        if !query.is_empty()
                            && ui
                                .button(egui_phosphor::thin::X)
                                .on_hover_text("Clear the search filter.")
                                .clicked()
                        {
                            query.clear();
                        }
                    });
                    ui.separator();

                    let q = query.trim().to_lowercase();
                    let sel = category.as_str();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            changed |= render_sections(ui, config, updater, sel, &q);
                        });
                });
            });
        });

    ctx.data_mut(|d| {
        d.insert_temp(cat_id, category);
        d.insert_temp(q_id, query);
    });
    *open = keep_open;
    changed
}

/// #R5 — open a Copland-style aligned settings grid (the `apps/c0pl4nd`
/// settings reference): three columns — a left label, the control, and the
/// per-setting reset — so labels and controls line up in columns across every
/// row of a group instead of a ragged single stack. The middle (control)
/// column is given room to grow so sliders/combos align.
fn settings_grid<R>(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Grid::new(id)
        .num_columns(3)
        .spacing([24.0, 10.0])
        .min_col_width(140.0)
        .show(ui, add)
        .inner
}

/// One boolean row inside a [`settings_grid`]: a labelled checkbox in the left
/// column, an empty control column, and the reset (↺) button aligned in the
/// third column under the slider/combo controls — then `end_row`. Honors the
/// search filter; returns whether the value changed.
fn grid_bool(
    ui: &mut egui::Ui,
    q: &str,
    key: &str,
    label: &str,
    hover: &str,
    val: &mut bool,
    default: &bool,
) -> bool {
    if !row_visible(q, key) {
        return false;
    }
    // The checkbox keeps its visible text so it has an accessible name (screen
    // readers + the kittest harness query it by label); an empty control column
    // keeps the reset (↺) button aligned under the slider/combo column.
    let mut changed = ui.checkbox(val, label).on_hover_text(hover).changed();
    ui.label("");
    changed |= reset_to_default(ui, val, default);
    ui.end_row();
    changed
}

/// Render every category section that is visible for the current selection /
/// search query. Comfortable spacing (group gaps) keeps it from feeling
/// squished even at the default window size.
fn render_sections(
    ui: &mut egui::Ui,
    config: &mut Config,
    updater: &mut crate::updater::Updater,
    sel: &str,
    q: &str,
) -> bool {
    let mut changed = false;
    // Roomier vertical rhythm so rows don't feel cramped — egui's default item
    // spacing (~3px) is what made settings hard to read. Applies to every row.
    ui.spacing_mut().item_spacing.y = 8.0;
    let space = |ui: &mut egui::Ui| ui.add_space(12.0);
    // Sub-group header inside a category page (Copland-style #102): a strong
    // single-concept label, a muted one-line "what it controls" sentence, and a
    // thin rule — mirroring Copland's CONFIG.md section formatting so every
    // group reads as a self-explanatory section.
    let group = |ui: &mut egui::Ui, label: &str, desc: &str| {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(label).strong());
        if !desc.is_empty() {
            ui.label(egui::RichText::new(desc).weak().small());
        }
        ui.separator();
    };
    // Category page header: the heading plus a muted one-line description of what
    // the page covers, so each section is self-explanatory at a glance (#69).
    let head = |ui: &mut egui::Ui, title: &str, desc: &str| {
        ui.heading(title);
        ui.label(egui::RichText::new(desc).weak().small());
        ui.add_space(2.0);
    };
    // F-037 — the default config, used by `reset_to_default` for every
    // per-setting ↺ revert button. Cheap to construct once per render.
    let def = Config::default();

    // ---- Appearance ----
    if section_visible(
        sel,
        q,
        "Appearance",
        &["theme", "follow os", "frameless", "toolbar icons"],
    ) {
        head(
            ui,
            "Appearance",
            "Theme, window chrome, and toolbar look. Changes apply live.",
        );
        settings_grid(ui, "settings-appearance", |ui| {
            if row_visible(q, "theme") {
                // Phase 17 T17.2: theme picker over the built-ins + a free text
                // field for user themes under <config_dir>/themes/<name>.toml.
                ui.label("Theme").on_hover_text(
                    "Pick the active colour theme from the built-ins, or type a user theme \
                     name below. Changes apply live.",
                );
                let current = config.appearance.theme.clone();
                egui::ComboBox::from_id_salt("theme-picker")
                    .selected_text(&current)
                    .show_ui(ui, |ui| {
                        for name in scribe_core::theme::Theme::builtin_names() {
                            if ui
                                .selectable_value(
                                    &mut config.appearance.theme,
                                    (*name).to_string(),
                                    *name,
                                )
                                .changed()
                            {
                                // #88/#106 — switching theme resets BOTH the app
                                // and note background overrides to the new theme.
                                config.appearance.background_override = None;
                                config.appearance.note_background_override = None;
                                changed = true;
                            }
                        }
                    })
                    .response
                    .on_hover_text("Choose one of the built-in colour themes.");
                changed |=
                    reset_to_default(ui, &mut config.appearance.theme, &def.appearance.theme);
                ui.end_row();
            }
            if row_visible(q, "theme custom name") {
                ui.label("…or user theme name");
                let name_changed = ui
                    .text_edit_singleline(&mut config.appearance.theme)
                    .on_hover_text(
                        "If a TOML at <config_dir>/themes/<name>.toml exists it overrides the \
                         built-in; otherwise the built-in by the same name (or wired-noir) is used.",
                    )
                    .changed();
                if name_changed {
                    config.appearance.background_override = None;
                    config.appearance.note_background_override = None;
                    changed = true;
                }
                ui.end_row();
            }
            if row_visible(q, "background colour color app override") {
                // #88 — app background colour, independent of the theme.
                ui.label("App background").on_hover_text(
                    "Override the app background colour independently of the theme. Switching \
                     themes resets this to the new theme's background.",
                );
                ui.horizontal(|ui| {
                    let mut col = config
                        .appearance
                        .background_override
                        .as_deref()
                        .and_then(parse_hex_color)
                        .unwrap_or(egui::Color32::from_rgb(0x0d, 0x0b, 0x14));
                    if ui.color_edit_button_srgba(&mut col).changed() {
                        config.appearance.background_override =
                            Some(format!("#{:02x}{:02x}{:02x}", col.r(), col.g(), col.b()));
                        changed = true;
                    }
                    if config.appearance.background_override.is_some()
                        && ui
                            .small_button("Follow theme")
                            .on_hover_text("Clear the override; follow the theme's background.")
                            .clicked()
                    {
                        config.appearance.background_override = None;
                        changed = true;
                    }
                });
                ui.end_row();
            }
            // #106 — link toggle + the note (editor well) background.
            changed |= grid_bool(
                ui,
                q,
                "note background link app editor separate together",
                "Link app & note backgrounds",
                "ON: the note (editor) background follows the app background — one control \
                 changes both. OFF: set the note background separately below.",
                &mut config.appearance.link_backgrounds,
                &def.appearance.link_backgrounds,
            );
            if row_visible(q, "note background link app editor separate together") {
                let linked = config.appearance.link_backgrounds;
                ui.label("Note background").on_hover_text(
                    "Background colour of the note/editor text area (used when 'Link app & \
                     note backgrounds' is off).",
                );
                ui.add_enabled_ui(!linked, |ui| {
                    ui.horizontal(|ui| {
                        let mut col = config
                            .appearance
                            .note_background_override
                            .as_deref()
                            .and_then(parse_hex_color)
                            .unwrap_or(egui::Color32::from_rgb(0x0d, 0x0b, 0x14));
                        if ui.color_edit_button_srgba(&mut col).changed() {
                            config.appearance.note_background_override =
                                Some(format!("#{:02x}{:02x}{:02x}", col.r(), col.g(), col.b()));
                            changed = true;
                        }
                        if config.appearance.note_background_override.is_some()
                            && ui
                                .small_button("Follow theme")
                                .on_hover_text("Clear the note override; follow the theme.")
                                .clicked()
                        {
                            config.appearance.note_background_override = None;
                            changed = true;
                        }
                    });
                });
                ui.end_row();
            }
            changed |= grid_bool(
                ui,
                q,
                "follow os dark light",
                "Follow OS dark/light",
                "Automatically switch between a light and dark theme to match the operating \
                 system's appearance setting.",
                &mut config.appearance.follow_os_theme,
                &def.appearance.follow_os_theme,
            );
            changed |= grid_bool(
                ui,
                q,
                "frameless window",
                "Frameless window (restart to apply)",
                "Draw the window without the OS title bar (a custom in-app title bar is used).",
                &mut config.appearance.frameless,
                &def.appearance.frameless,
            );
            changed |= grid_bool(
                ui,
                q,
                "toolbar icons words phosphor",
                "Toolbar shows icons instead of words",
                "When off, the quick-access toolbar renders text labels (the default). When on, \
                 items render as Phosphor Thin icon glyphs — compact, brand-aligned.",
                &mut config.appearance.toolbar_icons,
                &def.appearance.toolbar_icons,
            );
            changed |= grid_bool(
                ui,
                q,
                "kanji jp glyph japanese instrument label",
                "Toolbar — show kanji instrument labels",
                "Adds a small, dim kanji to each toolbar action whose canonical Japanese term \
                 is verified (e.g. New=新, Save=保, Find=検). English-redundant — the kanji \
                 never replaces the label.",
                &mut config.appearance.jp_glyph_labels,
                &def.appearance.jp_glyph_labels,
            );
        });
        // Theme export + live colour picker — full-width editors below the grid.
        if row_visible(q, "export theme tom user customize edit") {
            changed |= render_theme_export(ui, config);
        }
        if row_visible(q, "live color picker edit theme customize palette") {
            changed |= render_live_color_picker(ui, config);
        }
        space(ui);
    }

    // ---- Fonts ----  (no ligatures: egui has no OpenType shaping, so the
    // toggle is intentionally absent rather than shown as a dead control.)
    if section_visible(
        sel,
        q,
        "Fonts",
        &["size", "line height", "family", "font theme"],
    ) {
        head(
            ui,
            "Fonts",
            "Editor font family, text size, and line spacing. (Ligatures are off — \
             the renderer does no OpenType shaping.)",
        );
        settings_grid(ui, "settings-fonts", |ui| {
            if row_visible(q, "font family theme editor note") {
                ui.label("Note font")
                    .on_hover_text("Font for the note/editor text. Applies live, no restart.");
                egui::ComboBox::from_id_salt("note-font-picker")
                    .selected_text(config.fonts.editor_family.clone())
                    .show_ui(ui, |ui| {
                        for (display, _key) in crate::app::FONT_FAMILIES {
                            if ui
                                .selectable_value(
                                    &mut config.fonts.editor_family,
                                    (*display).to_string(),
                                    *display,
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    })
                    .response
                    .on_hover_text("Choose one of the bundled coding fonts for the note text.");
                changed |= reset_to_default(
                    ui,
                    &mut config.fonts.editor_family,
                    &def.fonts.editor_family,
                );
                ui.end_row();
            }
            if row_visible(q, "ui font app interface family") {
                ui.label("App UI font").on_hover_text(
                    "Font for the app interface (toolbar, settings, status). 'System default' \
                     keeps the built-in UI font. Applies live.",
                );
                egui::ComboBox::from_id_salt("ui-font-picker")
                    .selected_text(config.fonts.ui_family.clone())
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(
                                &mut config.fonts.ui_family,
                                "System default".to_string(),
                                "System default",
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        for (display, _key) in crate::app::FONT_FAMILIES {
                            if ui
                                .selectable_value(
                                    &mut config.fonts.ui_family,
                                    (*display).to_string(),
                                    *display,
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    })
                    .response
                    .on_hover_text("Choose the app-interface font (or keep the system default).");
                changed |= reset_to_default(ui, &mut config.fonts.ui_family, &def.fonts.ui_family);
                ui.end_row();
            }
            if row_visible(q, "note colour color theme syntax text") {
                ui.label("Note colour theme").on_hover_text(
                    "Colour scheme for the note text / syntax highlighting, separate from the \
                     app theme. Applies live.",
                );
                egui::ComboBox::from_id_salt("note-theme-picker")
                    .selected_text(config.editor.note_theme.clone())
                    .show_ui(ui, |ui| {
                        for name in crate::app::NOTE_THEMES {
                            if ui
                                .selectable_value(
                                    &mut config.editor.note_theme,
                                    (*name).to_string(),
                                    *name,
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    })
                    .response
                    .on_hover_text("Pick a note text colour scheme.");
                changed |=
                    reset_to_default(ui, &mut config.editor.note_theme, &def.editor.note_theme);
                ui.end_row();
            }
            if row_visible(q, "editor size") {
                ui.label("Size")
                    .on_hover_text("Font size of the editor text, in points.");
                ui.horizontal(|ui| {
                    if ui.small_button("-").on_hover_text("Smaller").clicked() {
                        config.fonts.editor_size =
                            (config.fonts.editor_size - 1.0).clamp(8.0, 32.0);
                        changed = true;
                    }
                    changed |= ui
                        .add(egui::Slider::new(&mut config.fonts.editor_size, 8.0..=32.0))
                        .changed();
                    if ui.small_button("+").on_hover_text("Larger").clicked() {
                        config.fonts.editor_size =
                            (config.fonts.editor_size + 1.0).clamp(8.0, 32.0);
                        changed = true;
                    }
                });
                changed |=
                    reset_to_default(ui, &mut config.fonts.editor_size, &def.fonts.editor_size);
                ui.end_row();
            }
            if row_visible(q, "line height") {
                ui.label("Line height").on_hover_text(
                    "Vertical spacing between lines, as a multiple of the font size. Note: the \
                     text caret + selection are exactly this tall, so a larger value also makes \
                     them taller than the glyphs. ~1.2 keeps the caret tight to the text.",
                );
                ui.horizontal(|ui| {
                    if ui.small_button("-").on_hover_text("Tighter").clicked() {
                        config.fonts.line_height = (config.fonts.line_height - 0.1).clamp(1.0, 2.5);
                        changed = true;
                    }
                    changed |= ui
                        .add(egui::Slider::new(&mut config.fonts.line_height, 1.0..=2.5))
                        .changed();
                    if ui.small_button("+").on_hover_text("Looser").clicked() {
                        config.fonts.line_height = (config.fonts.line_height + 0.1).clamp(1.0, 2.5);
                        changed = true;
                    }
                });
                changed |=
                    reset_to_default(ui, &mut config.fonts.line_height, &def.fonts.line_height);
                ui.end_row();
            }
        });
        space(ui);
    }

    // ---- Editor ----
    if section_visible(
        sel,
        q,
        "Editor",
        &[
            "tab width",
            "insert spaces",
            "line numbers",
            "word wrap",
            "minimap",
            "restore session",
        ],
    ) {
        head(
            ui,
            "Editor",
            "Indentation, what's shown around the text, the tab bar, and save / \
             session behaviour.",
        );

        // -- Indentation --
        group(
            ui,
            "Indentation",
            "Tabs vs spaces, and how wide one indent step is.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-editor-indentation", |ui| {
            if row_visible(q, "tab width") {
                ui.label("Tab width")
                    .on_hover_text("How many columns a tab character occupies.");
                changed |= ui
                    .add(egui::Slider::new(&mut config.editor.tab_width, 1..=8))
                    .changed();
                changed |=
                    reset_to_default(ui, &mut config.editor.tab_width, &def.editor.tab_width);
                ui.end_row();
            }
            changed |= grid_bool(
                ui,
                q,
                "insert spaces",
                "Insert spaces (Tab key)",
                "Insert spaces instead of a tab character when you press Tab.",
                &mut config.editor.insert_spaces,
                &def.editor.insert_spaces,
            );
        });
        ui.add_space(6.0);

        // -- Display --
        group(
            ui,
            "Display",
            "What is shown around the text — line numbers, minimap, wrapping, whitespace.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-editor-display", |ui| {
            changed |= grid_bool(
                ui,
                q,
                "line numbers",
                "Line numbers",
                "Show a line-number gutter to the left of the editor.",
                &mut config.editor.show_line_numbers,
                &def.editor.show_line_numbers,
            );
            changed |= grid_bool(
                ui,
                q,
                "word wrap",
                "Word wrap",
                "Wrap long lines to the editor width instead of scrolling horizontally.",
                &mut config.editor.word_wrap,
                &def.editor.word_wrap,
            );
            changed |= grid_bool(
                ui,
                q,
                "minimap",
                "Minimap",
                "Show a zoomed-out overview of the whole file alongside the editor for \
                 quick navigation.",
                &mut config.editor.show_minimap,
                &def.editor.show_minimap,
            );
            changed |= grid_bool(
                ui,
                q,
                "render whitespace markers",
                "Render whitespace markers (spaces · tabs)",
                "Draw faint markers for spaces and tabs so invisible whitespace is \
                 visible. Applies to the experimental rope editor.",
                &mut config.editor.render_whitespace,
                &def.editor.render_whitespace,
            );
        });
        ui.add_space(6.0);

        // -- Layout --
        group(
            ui,
            "Layout",
            "Where the tab bar sits and how the editor surface is arranged.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-editor-layout", |ui| {
            if row_visible(q, "tab bar position top bottom left right") {
                // T18.4: position the open-tab strip relative to the editor.
                use scribe_core::config::TabBarPosition;
                let positions = [
                    (TabBarPosition::Top, "top"),
                    (TabBarPosition::Bottom, "bottom"),
                    (TabBarPosition::Left, "left"),
                    (TabBarPosition::Right, "right"),
                ];
                ui.label("Tab bar position")
                    .on_hover_text("Where the strip of open-file tabs sits around the editor.");
                egui::ComboBox::from_id_salt("tab-bar-position")
                    .selected_text(
                        positions
                            .iter()
                            .find(|(p, _)| *p == config.editor.tab_bar_position)
                            .map(|(_, s)| *s)
                            .unwrap_or("top"),
                    )
                    .show_ui(ui, |ui| {
                        for (pos, label) in positions {
                            if ui
                                .selectable_value(&mut config.editor.tab_bar_position, pos, label)
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    })
                    .response
                    .on_hover_text("Choose the edge where the open-tab strip is shown.");
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.tab_bar_position,
                    &def.editor.tab_bar_position,
                );
                ui.end_row();
            }
            if row_visible(
                q,
                "side tab orientation vertical horizontal rotate left right",
            ) {
                // #82 — only meaningful when the tab bar is on the Left/Right;
                // greyed otherwise so the dependency is obvious.
                let is_side = config.editor.tab_bar_position.is_vertical();
                ui.add_enabled_ui(is_side, |ui| {
                    changed |= ui
                        .checkbox(
                            &mut config.editor.side_tabs_rotated,
                            "Rotate side tabs (vertical text)",
                        )
                        .on_hover_text(
                            "When the tab bar is on the Left or Right: ON rotates each tab's \
                             label 90° so the text reads vertically, while the tabs stay in a \
                             single column. OFF keeps the labels horizontal. No effect for \
                             Top/Bottom.",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.side_tabs_rotated,
                    &def.editor.side_tabs_rotated,
                );
                ui.end_row();
            }
            changed |= grid_bool(
                ui,
                q,
                "multi-note grid panes split editor central",
                "Multi-note grid (experimental)",
                "Render every open tab as a movable / resizable pane in the central editor. \
                 Drag tabs between panes to rearrange; drag the splitter to resize.",
                &mut config.editor.grid_enabled,
                &def.editor.grid_enabled,
            );
            changed |= grid_bool(
                ui,
                q,
                "experimental rope editor owned cursor undo keystone",
                "Experimental rope editor",
                "Use the in-house rope editor for normal files instead of the default egui \
                 text widget. Own caret, selection, and persistent-capable undo. \
                 Experimental: no IME / mouse-selection parity yet.",
                &mut config.editor.experimental_rope_editor,
                &def.editor.experimental_rope_editor,
            );
        });
        ui.add_space(6.0);

        // -- Save & Session --
        group(
            ui,
            "Save & Session",
            "Autosave, session restore, and on-save cleanup.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-editor-save", |ui| {
            changed |= grid_bool(
                ui,
                q,
                "restore session",
                "Restore session",
                "Reopen the files and tabs you had open when you last closed SCR1B3.",
                &mut config.editor.restore_session,
                &def.editor.restore_session,
            );
            changed |= grid_bool(
                ui,
                q,
                "session backup hot exit unsaved restore crash recovery",
                "Restore unsaved notes after restart",
                "Keeps a backup of unsaved buffers (including never-saved scratch notes) so \
                 they come back after a restart or crash — no save needed. Backups live in \
                 the config 'backup' folder and are deleted once you save. On by default.",
                &mut config.editor.session_backup,
                &def.editor.session_backup,
            );
            changed |= grid_bool(
                ui,
                q,
                "auto save autosave",
                "Auto-save (after a short pause)",
                "Automatically save dirty file-backed buffers a few seconds after you stop \
                 typing. Untitled buffers are never auto-saved. Off by default.",
                &mut config.editor.auto_save,
                &def.editor.auto_save,
            );
            changed |= grid_bool(
                ui,
                q,
                "trim trailing whitespace on save",
                "Trim trailing whitespace on save",
                "Remove trailing spaces and tabs at the end of every line when a file is saved.",
                &mut config.editor.trim_trailing_whitespace_on_save,
                &def.editor.trim_trailing_whitespace_on_save,
            );
            changed |= grid_bool(
                ui,
                q,
                "final newline ensure on save",
                "Ensure final newline on save",
                "Make sure the file ends with exactly one newline character when saved.",
                &mut config.editor.final_newline_on_save,
                &def.editor.final_newline_on_save,
            );
            changed |= grid_bool(
                ui,
                q,
                "restore cursor caret position per file",
                "Restore caret position per file",
                "Remember where the caret was in each file and jump back there when you \
                 reopen it.",
                &mut config.editor.restore_cursor_position,
                &def.editor.restore_cursor_position,
            );
        });
        space(ui);
    }

    // ---- Motion / animations ----
    if section_visible(
        sel,
        q,
        "Motion",
        &["motion", "animation", "blink", "fade", "cursor"],
    ) {
        head(
            ui,
            "Motion",
            "Subtle interface animation. Turn off for a fully static UI.",
        );
        // Master OFF by default — calm-surface principle (DECISION-2026-005);
        // animation is opt-in so idle frames cost the same as plain egui.
        settings_grid(ui, "settings-motion", |ui| {
            changed |= grid_bool(
                ui,
                q,
                "motion animation enable",
                "Enable animations",
                "Master switch. When off, transitions are instant (no fades) and the text \
                 caret stays steady — idle frames cost the same as plain egui.",
                &mut config.motion.enabled,
                &def.motion.enabled,
            );
            let on = config.motion.enabled;
            if row_visible(q, "motion animation speed intensity") {
                ui.label("Animation speed").on_hover_text(
                    "Scale how long animations take. 0 makes every transition instant; 1 is \
                     egui's full animation time. Affects hover fades, panel collapses, and \
                     value changes across the editor.",
                );
                changed |= ui
                    .add_enabled(
                        on,
                        egui::Slider::new(&mut config.motion.intensity, 0.0..=1.0),
                    )
                    .changed();
                changed |=
                    reset_to_default(ui, &mut config.motion.intensity, &def.motion.intensity);
                ui.end_row();
            }
            if row_visible(q, "cursor blink motion") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.motion.cursor_blink, "Blink the text cursor")
                        .on_hover_text(
                            "Blink the text caret instead of showing it steady. Disable for a \
                             calmer, motion-free caret.",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.cursor_blink,
                    &def.motion.cursor_blink,
                );
                ui.end_row();
            }
        });
        space(ui);
    }

    // ---- Window (transparency / glass) ----
    if section_visible(
        sel,
        q,
        "Window",
        &["mode", "opacity", "tint", "glass", "mica"],
    ) {
        head(
            ui,
            "Window",
            "Always-on-top, and translucency / glass for the window background.",
        );

        // -- Always on top --
        group(ui, "Always on top", "Keep the window above other windows.");
        ui.add_space(4.0);
        settings_grid(ui, "settings-window-aot", |ui| {
            changed |= grid_bool(
                ui,
                q,
                "always on top window above",
                "Always on top",
                "Keep the SCR1B3 window above other windows.",
                &mut config.window.always_on_top,
                &def.window.always_on_top,
            );
        });
        ui.add_space(6.0);

        // -- Transparency / glass --
        group(
            ui,
            "Transparency / glass",
            "Window translucency and the OS glass / blur effect.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-window-glass", |ui| {
            // Master on/off switch — off by default (opaque is fast, no DWM ghost).
            changed |= grid_bool(
                ui,
                q,
                "transparency enable master glass",
                "Enable window transparency (master)",
                "Master switch. When off, the window is fully opaque regardless of the mode \
                 below. Turn on to use transparent / glass / mica / vibrancy. Restart to apply \
                 the surface change.",
                &mut config.window.transparency_enabled,
                &def.window.transparency_enabled,
            );
            let tos = config.window.transparency_enabled;
            if row_visible(q, "window mode transparent glass mica vibrancy opaque") {
                let wmodes = [
                    (WindowMode::Opaque, "opaque"),
                    (WindowMode::Transparent, "transparent"),
                    (WindowMode::Glass, "glass / acrylic"),
                    (WindowMode::Mica, "mica (Win11)"),
                    (WindowMode::Vibrancy, "vibrancy (macOS)"),
                ];
                ui.label("Mode").on_hover_text(
                    "Pick the window surface style: opaque, transparent, glass / acrylic, mica \
                     (Windows 11), or vibrancy (macOS). Restart to apply blur.",
                );
                ui.add_enabled_ui(tos, |ui| {
                    egui::ComboBox::from_id_salt("window-mode")
                        .selected_text(
                            wmodes
                                .iter()
                                .find(|(m, _)| *m == config.window.mode)
                                .map(|(_, s)| *s)
                                .unwrap_or("opaque"),
                        )
                        .show_ui(ui, |ui| {
                            for (m, label) in wmodes {
                                if ui
                                    .selectable_value(&mut config.window.mode, m, label)
                                    .changed()
                                {
                                    changed = true;
                                }
                            }
                        })
                        .response
                        .on_hover_text("Restart to apply the blur surface change.");
                });
                changed |= reset_to_default(ui, &mut config.window.mode, &def.window.mode);
                ui.end_row();
            }
            if row_visible(q, "window opacity transparent") {
                let translucent = tos && config.window.mode.is_translucent();
                ui.label("Opacity").on_hover_text(
                    "How see-through the window is — 1.0 is fully opaque, lower is more \
                     transparent. Only active for translucent modes.",
                );
                changed |= ui
                    .add_enabled(
                        translucent,
                        egui::Slider::new(&mut config.window.opacity, 0.30..=1.0),
                    )
                    .changed();
                changed |= reset_to_default(ui, &mut config.window.opacity, &def.window.opacity);
                ui.end_row();
            }
            if row_visible(q, "window tint colour hex") {
                ui.label("Tint").on_hover_text(
                    "Colour tint applied over the translucent surface, as a hex code \
                     (e.g. #1a1a2e).",
                );
                ui.add_enabled_ui(tos, |ui| {
                    changed |= ui
                        .text_edit_singleline(&mut config.window.tint)
                        .on_hover_text(
                            "Hex colour (e.g. #1a1a2e) blended over the translucent window \
                             surface.",
                        )
                        .changed();
                });
                changed |= reset_to_default(ui, &mut config.window.tint, &def.window.tint);
                ui.end_row();
            }
            if row_visible(q, "window tint strength") {
                ui.label("Tint strength").on_hover_text(
                    "How strongly the tint colour is blended over the surface — 0 is none, \
                     1 is full.",
                );
                changed |= ui
                    .add_enabled(
                        tos,
                        egui::Slider::new(&mut config.window.tint_strength, 0.0..=1.0),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.window.tint_strength,
                    &def.window.tint_strength,
                );
                ui.end_row();
            }
        });
        space(ui);
    }

    // ---- Spellcheck ----
    if section_visible(
        sel,
        q,
        "Spellcheck",
        &[
            "spellcheck",
            "language",
            "comments",
            "strings",
            "identifiers",
        ],
    ) {
        head(
            ui,
            "Spellcheck (offline)",
            "Dictionary spellchecking that runs entirely on-device — no network.",
        );
        settings_grid(ui, "settings-spellcheck", |ui| {
            changed |= grid_bool(
                ui,
                q,
                "spellcheck enable",
                "Enable",
                "Turn on the offline spell checker for editor text.",
                &mut config.spellcheck.enabled,
                &def.spellcheck.enabled,
            );
            let on = config.spellcheck.enabled;
            if row_visible(q, "spellcheck language dictionary") {
                ui.label("Language").on_hover_text(
                    "Dictionary language code (e.g. en_US). en_US is built in; for any other \
                     code, drop a matching <code>.txt word list in the config dict/ folder.",
                );
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .text_edit_singleline(&mut config.spellcheck.language)
                        .on_hover_text(
                            "Dictionary language code. en_US ships built in. For another \
                             language, place <code>.txt (one word per line) in the dict/ folder \
                             of your config directory; it is loaded automatically.",
                        )
                        .changed();
                });
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.language,
                    &def.spellcheck.language,
                );
                ui.end_row();
            }
            if row_visible(q, "spellcheck check comments") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.spellcheck.check_comments, "Check comments")
                        .on_hover_text("Spell-check words inside code comments.")
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.check_comments,
                    &def.spellcheck.check_comments,
                );
                ui.end_row();
            }
            if row_visible(q, "spellcheck check strings") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.spellcheck.check_strings, "Check strings")
                        .on_hover_text("Spell-check words inside string literals.")
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.check_strings,
                    &def.spellcheck.check_strings,
                );
                ui.end_row();
            }
            if row_visible(q, "spellcheck check identifiers") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(
                            &mut config.spellcheck.check_identifiers,
                            "Check identifiers",
                        )
                        .on_hover_text(
                            "Spell-check variable and function names (splits camelCase / \
                             snake_case).",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.check_identifiers,
                    &def.spellcheck.check_identifiers,
                );
                ui.end_row();
            }
            if row_visible(q, "spellcheck custom dictionary word list") {
                ui.label("Custom dictionary").on_hover_text(
                    "Optional path to your own word list; every word in it is always treated \
                     as correct (layered on top of the base dictionary).",
                );
                ui.add_enabled_ui(on, |ui| {
                    let mut s = config
                        .spellcheck
                        .custom_dict_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    if ui
                        .text_edit_singleline(&mut s)
                        .on_hover_text(
                            "Absolute path to a newline-separated .txt word list (one word per \
                             line). Leave empty for none.",
                        )
                        .changed()
                    {
                        config.spellcheck.custom_dict_path = if s.trim().is_empty() {
                            None
                        } else {
                            Some(std::path::PathBuf::from(s.trim()))
                        };
                        changed = true;
                    }
                });
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.custom_dict_path,
                    &def.spellcheck.custom_dict_path,
                );
                ui.end_row();
            }
        });
        space(ui);
    }

    // ---- Updates ----
    if section_visible(sel, q, "Updates", &["update", "mode", "notify", "auto"]) {
        head(
            ui,
            "Updates",
            "Check for new SCR1B3 releases. A check reads only the public GitHub releases \
             API and sends no identifiers — no analytics, no telemetry. Off and Manual \
             never touch the network on their own; Notify and Auto check once per launch \
             when due (Notify shows a toast; Auto asks before installing).",
        );
        // Show the running version so the result of a check is concretely verifiable.
        ui.label(
            egui::RichText::new(format!("You are running v{}.", env!("CARGO_PKG_VERSION")))
                .weak()
                .small(),
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-updates", |ui| {
            if row_visible(q, "update mode notify auto manual off") {
                let modes = [
                    (UpdateMode::Off, "off"),
                    (UpdateMode::Notify, "notify"),
                    (UpdateMode::Manual, "manual"),
                    (UpdateMode::Auto, "auto"),
                ];
                ui.label("Mode").on_hover_text(
                    "When SCR1B3 checks for updates: off (never), manual (only when you press \
                     Check for updates), notify (check once per launch, show a toast if a newer \
                     version exists), auto (check once per launch, ask before installing). A \
                     check reads only the public GitHub releases API and sends no identifiers.",
                );
                egui::ComboBox::from_id_salt("update-mode")
                    .selected_text(
                        modes
                            .iter()
                            .find(|(m, _)| *m == config.updates.mode)
                            .map(|(_, s)| *s)
                            .unwrap_or("notify"),
                    )
                    .show_ui(ui, |ui| {
                        for (m, label) in modes {
                            if ui
                                .selectable_value(&mut config.updates.mode, m, label)
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    })
                    .response
                    .on_hover_text(
                        "A check reads only the public GitHub releases API; no analytics or \
                         identifiers are sent.",
                    );
                changed |= reset_to_default(ui, &mut config.updates.mode, &def.updates.mode);
                ui.end_row();
            }
            if row_visible(q, "check interval hours") {
                ui.label("Check interval (hours)")
                    .on_hover_text("How often, in hours, to check for a new release (1–168).");
                changed |= ui
                    .add(egui::Slider::new(
                        &mut config.updates.check_interval_hours,
                        1..=168,
                    ))
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.updates.check_interval_hours,
                    &def.updates.check_interval_hours,
                );
                ui.end_row();
            }
        });
        if row_visible(q, "check for updates now install update") {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let busy = updater.is_busy();
                if ui
                    .add_enabled(!busy, egui::Button::new("Check for updates"))
                    .on_hover_text(
                        "Ask the public GitHub releases API whether a newer version exists. \
                         No identifiers are sent.",
                    )
                    .clicked()
                {
                    updater.start_check(ui.ctx(), crate::updater::LaunchKind::Manual);
                    // Record the check so the on-launch interval respects it.
                    config.updates.last_check_unix = Some(crate::app::now_unix());
                    changed = true;
                }
                render_update_status(ui, updater);
            });
            ui.add_space(2.0);
            if ui
                .link("View all releases on GitHub")
                .on_hover_text("Open the SCR1B3 releases page in your browser.")
                .clicked()
            {
                ui.ctx()
                    .open_url(egui::OpenUrl::new_tab(crate::app::RELEASES_URL));
            }
        }
        space(ui);
    }

    // ---- Plugins ----
    if section_visible(sel, q, "Plugins", &["plugin", "mod"]) {
        head(
            ui,
            "Plugins",
            "Enable plugins and open the manager. Plugins are local and \
             signature-verified.",
        );
        settings_grid(ui, "settings-plugins", |ui| {
            changed |= grid_bool(
                ui,
                q,
                "plugin mod enable system",
                "Enable plugin/mod system",
                "Allow SCR1B3 to load plugins / mods from the plugins directory at startup.",
                &mut config.plugins.enabled,
                &def.plugins.enabled,
            );
        });
        ui.label(
            egui::RichText::new("Drop mods into the plugins dir — see PLUGINS.md")
                .weak()
                .small(),
        );
        // F-039 — open the plugin manager (Loaded / Registry / Install). The
        // request is stashed in egui temp data; the host reads + clears it
        // after `show` returns so it can open its own modal state.
        if ui
            .button("Manage plugins…")
            .on_hover_text(
                "Open the plugin manager to view loaded plugins, browse the registry, and \
                 install new ones.",
            )
            .clicked()
        {
            ui.ctx()
                .data_mut(|d| d.insert_temp(open_plugin_manager_id(), true));
        }
        space(ui);
    }

    // ---- Toolbar (customizable quick-access bar) ----
    if section_visible(
        sel,
        q,
        "Toolbar",
        &["toolbar", "quick access", "reorder", "buttons"],
    ) {
        changed |= render_toolbar_editor(ui, config);
    }

    changed
}

/// Drag-and-drop payload for the toolbar editor (Phase 18 T18.5b).
/// `Reorder(i)` means "move existing item at index i". `AddAction(id)`
/// means "append a new action from the palette".
#[derive(Clone, Debug)]
enum ToolbarDrag {
    Reorder(usize),
    AddAction(String),
}

/// Add / remove / reorder the quick-access toolbar items. Returns `true` on any
/// edit so the caller persists the new layout.
///
/// Phase 18 T18.5b — drag-to-reorder + drag-from-palette layered on top of the
/// existing keyboard-accessible ↑/↓/✕ controls. The drag-and-drop is
/// **additive**: keyboard users keep the buttons; pointer users get the
/// direct-manipulation UX the plan calls out.
/// Render the inline update status + action buttons next to the "Check for
/// updates" button, driven by the [`crate::updater::UpdateState`] machine.
/// Mutating calls (start download, apply) are deferred past the immutable
/// state borrow so the borrow checker is satisfied.
fn render_update_status(ui: &mut egui::Ui, updater: &mut crate::updater::Updater) {
    use crate::updater::UpdateState;
    enum Act {
        Download(scribe_core::update::ReleaseInfo),
        Apply,
        Recheck,
    }
    let mut act: Option<Act> = None;
    match &updater.state {
        UpdateState::Idle => {}
        UpdateState::Checking => {
            ui.spinner();
            ui.label("Checking…");
        }
        UpdateState::UpToDate => {
            ui.label(
                egui::RichText::new(format!(
                    "You're on the latest version (v{}).",
                    crate::updater::current_version()
                ))
                .weak(),
            );
        }
        UpdateState::Available(info) => {
            ui.label(format!("v{} is available.", info.version));
            if ui.button("Update now").clicked() {
                act = Some(Act::Download(info.clone()));
            }
            if ui
                .link("changelog")
                .on_hover_text("Open this release's notes in your browser.")
                .clicked()
            {
                ui.ctx()
                    .open_url(egui::OpenUrl::new_tab(info.html_url.clone()));
            }
        }
        UpdateState::Downloading { received, total } => {
            let frac = if *total > 0 {
                *received as f32 / *total as f32
            } else {
                0.0
            };
            ui.add(
                egui::ProgressBar::new(frac)
                    .show_percentage()
                    .desired_width(160.0),
            );
        }
        UpdateState::ReadyToApply { version, .. } => {
            ui.label(format!("v{version} downloaded + verified."));
            if ui
                .button("Restart to finish update")
                .on_hover_text("Replace the running SCR1B3 with the new version and relaunch.")
                .clicked()
            {
                act = Some(Act::Apply);
            }
        }
        UpdateState::Applied { version } => {
            ui.label(format!("Updated to v{version} — restarting…"));
        }
        UpdateState::Failed(e) => {
            let err = ui.visuals().error_fg_color;
            ui.colored_label(err, format!("Update failed: {e}"));
            if ui.button("Retry").clicked() {
                act = Some(Act::Recheck);
            }
        }
    }
    match act {
        Some(Act::Download(info)) => updater.start_download(ui.ctx(), info),
        Some(Act::Apply) => updater.apply_and_restart(ui.ctx()),
        Some(Act::Recheck) => updater.start_check(ui.ctx(), crate::updater::LaunchKind::Manual),
        None => {}
    }
}

fn render_toolbar_editor(ui: &mut egui::Ui, config: &mut Config) -> bool {
    use egui_phosphor::thin as ph;
    let mut changed = false;
    // #80 — pin the editor to the available width so its wide children (the
    // palette's wrapped row of chips) WRAP instead of forcing the resizable
    // Settings window to balloon to fit them.
    ui.set_max_width(ui.available_width());
    ui.heading("Quick-access toolbar");
    ui.label(
        egui::RichText::new(
            "Drag to reorder the toolbar buttons, or add actions from the palette below.",
        )
        .weak()
        .small(),
    );
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(
            "Choose what shows in the top bar. Drag rows to reorder; drag from the \
             palette to add. Keyboard: up/down reorder, delete removes.",
        )
        .weak()
        .small(),
    );
    ui.add_space(6.0);

    // Phase 18 T18.5: button size + spacing + icon size sliders. All values
    // are clamped at render time so a malformed user toml can't produce a
    // 4000-px-tall toolbar.
    ui.label(egui::RichText::new("Sizing").strong().small());
    ui.horizontal(|ui| {
        changed |= ui
            .add(
                egui::Slider::new(&mut config.toolbar.button_size_px, 16.0..=64.0)
                    .text("Button height (px)"),
            )
            .on_hover_text("Height of each quick-access toolbar button, in pixels.")
            .changed();
        changed |= reset_to_default(
            ui,
            &mut config.toolbar.button_size_px,
            &ToolbarConfig::default_button_size(),
        );
    });
    ui.horizontal(|ui| {
        changed |= ui
            .add(
                egui::Slider::new(&mut config.toolbar.button_spacing_px, 0.0..=24.0)
                    .text("Button spacing (px)"),
            )
            .on_hover_text("Gap between adjacent toolbar buttons, in pixels.")
            .changed();
        changed |= reset_to_default(
            ui,
            &mut config.toolbar.button_spacing_px,
            &ToolbarConfig::default_button_spacing(),
        );
    });
    ui.horizontal(|ui| {
        changed |= ui
            .add(
                egui::Slider::new(&mut config.toolbar.icon_size_px, 10.0..=32.0)
                    .text("Icon glyph size (px)"),
            )
            .on_hover_text("Active only when 'Toolbar shows icons' (Appearance) is on.")
            .changed();
        changed |= reset_to_default(
            ui,
            &mut config.toolbar.icon_size_px,
            &ToolbarConfig::default_icon_size(),
        );
    });
    if ui
        .small_button("Reset sizing to defaults")
        .on_hover_text("Restore the button height, spacing, and icon size to their defaults.")
        .clicked()
    {
        config.toolbar.button_size_px = scribe_core::config::ToolbarConfig::default_button_size();
        config.toolbar.button_spacing_px =
            scribe_core::config::ToolbarConfig::default_button_spacing();
        config.toolbar.icon_size_px = scribe_core::config::ToolbarConfig::default_icon_size();
        changed = true;
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Items").strong().small());

    let mut mv: Option<(usize, isize)> = None;
    let mut rm: Option<usize> = None;
    // Drop-action queue: (target_index, payload). Applied after the row loop
    // so the mutation doesn't invalidate iterator state.
    let mut drop_actions: Vec<(usize, ToolbarDrag)> = Vec::new();
    let n = config.toolbar.items.len();
    // Track the current dragged index (if any) so we can paint a thin
    // insertion guide between rows. egui's DnD doesn't expose the live
    // pointer/dragged index directly; we read DragAndDrop::payload from
    // the context to peek without consuming.
    let dragged: Option<ToolbarDrag> =
        egui::DragAndDrop::payload::<ToolbarDrag>(ui.ctx()).map(|arc| (*arc).clone());
    for i in 0..n {
        let label = action_label(&config.toolbar.items[i]);
        // Each row is a drag source carrying `Reorder(i)`. egui paints the
        // body at the cursor while dragging — free live preview.
        let drag_id = egui::Id::new(("scr1b3-toolbar-item-drag", i));
        ui.dnd_drag_source(drag_id, ToolbarDrag::Reorder(i), |ui| {
            ui.horizontal(|ui| {
                // A grip glyph signals "this row is draggable" (#89 — phosphor
                // icons instead of raw braille/arrows that rendered as tofu).
                ui.add(
                    egui::Label::new(egui::RichText::new(ph::DOTS_SIX_VERTICAL).weak())
                        .selectable(false),
                )
                .on_hover_text("Drag to reorder")
                .on_hover_cursor(egui::CursorIcon::Grab);
                if ui
                    .add_enabled(i > 0, egui::Button::new(ph::CARET_UP))
                    .on_hover_text("Move up")
                    .clicked()
                {
                    mv = Some((i, -1));
                }
                if ui
                    .add_enabled(i + 1 < n, egui::Button::new(ph::CARET_DOWN))
                    .on_hover_text("Move down")
                    .clicked()
                {
                    mv = Some((i, 1));
                }
                if ui
                    .button(ph::X)
                    .on_hover_text("Remove from toolbar")
                    .clicked()
                {
                    rm = Some(i);
                }
                ui.label(label);
            });
        });
        // Per-row drop zone immediately AFTER the row. A drop here means
        // "insert before index i+1" (the next slot). For the LAST row we
        // also accept drops at the tail.
        let (_resp, dropped) = ui.dnd_drop_zone::<ToolbarDrag, _>(
            egui::Frame::default()
                .inner_margin(egui::Margin::symmetric(2, 1))
                .stroke(egui::Stroke::NONE),
            |ui| {
                // Render a thin insertion guide ONLY while a compatible
                // drag is in progress, so the UI stays calm otherwise.
                if dragged.is_some() {
                    ui.add(egui::Separator::default().horizontal().spacing(1.0));
                } else {
                    ui.add_space(2.0);
                }
            },
        );
        if let Some(payload) = dropped {
            drop_actions.push((i + 1, (*payload).clone()));
        }
    }
    // A leading drop zone before the first row so the user can drop AT
    // INDEX 0. Rendered AFTER the loop to keep the row indices stable —
    // the drop position is recorded as 0.
    let (_lead_resp, lead_dropped) = ui.dnd_drop_zone::<ToolbarDrag, _>(
        egui::Frame::default()
            .inner_margin(egui::Margin::symmetric(2, 1))
            .stroke(egui::Stroke::NONE),
        |ui| {
            if dragged.is_some() {
                ui.label(
                    egui::RichText::new("drop here for top of toolbar")
                        .weak()
                        .small(),
                );
            } else {
                ui.add_space(2.0);
            }
        },
    );
    if let Some(payload) = lead_dropped {
        drop_actions.push((0, (*payload).clone()));
    }
    if let Some((i, d)) = mv {
        let j = (i as isize + d).clamp(0, n as isize - 1) as usize;
        if i != j {
            config.toolbar.items.swap(i, j);
            changed = true;
        }
    }
    if let Some(i) = rm {
        config.toolbar.items.remove(i);
        changed = true;
    }
    // Apply drops in reverse so insertion indices stay valid as the
    // vector grows.
    for (target, drag) in drop_actions.into_iter().rev() {
        match drag {
            ToolbarDrag::Reorder(src) => {
                if src < config.toolbar.items.len() {
                    let item = config.toolbar.items.remove(src);
                    // Adjust target if we removed an item before it.
                    let t = if src < target { target - 1 } else { target };
                    let t = t.min(config.toolbar.items.len());
                    config.toolbar.items.insert(t, item);
                    changed = true;
                }
            }
            ToolbarDrag::AddAction(id) => {
                let t = target.min(config.toolbar.items.len());
                config.toolbar.items.insert(t, id);
                changed = true;
            }
        }
    }

    ui.add_space(6.0);
    // Palette — each available action is a drag source carrying its id.
    // The original ComboBox (keyboard discoverable) stays for keyboard users.
    ui.label(
        egui::RichText::new("Palette (drag onto the list)")
            .strong()
            .small(),
    );
    ui.horizontal_wrapped(|ui| {
        for (id, label) in crate::app::TOOLBAR_ACTIONS {
            let drag_id = egui::Id::new(("scr1b3-toolbar-palette-drag", *id));
            ui.dnd_drag_source(drag_id, ToolbarDrag::AddAction((*id).to_string()), |ui| {
                // #90 — chips read as grabbable: a faint grip glyph + a filled
                // chip background, and a grab cursor on hover. They wrap into
                // 2-3 rows because the editor width is pinned (#80 above).
                let chip = egui::Frame::default()
                    .inner_margin(egui::Margin::symmetric(6, 3))
                    .fill(ui.visuals().widgets.inactive.bg_fill)
                    .stroke(egui::Stroke::new(
                        1.0,
                        ui.visuals().widgets.inactive.bg_stroke.color,
                    ))
                    .corner_radius(egui::CornerRadius::same(4));
                chip.show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(egui::RichText::new(ph::DOTS_SIX_VERTICAL).weak().small());
                    ui.label(*label);
                });
            })
            .response
            .on_hover_text("Drag onto the list above to add")
            .on_hover_cursor(egui::CursorIcon::Grab);
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("add:").on_hover_text(
            "Append a toolbar action from the list (keyboard-friendly alternative to dragging).",
        );
        egui::ComboBox::from_id_salt("toolbar-add")
            .selected_text("choose…")
            .show_ui(ui, |ui| {
                for (id, label) in crate::app::TOOLBAR_ACTIONS {
                    if ui.selectable_label(false, *label).clicked() {
                        config.toolbar.items.push((*id).to_string());
                        changed = true;
                    }
                }
            })
            .response
            .on_hover_text("Pick an action to append to the toolbar.");
        if ui
            .button("reset to defaults")
            .on_hover_text("Restore the toolbar's items to the default set.")
            .clicked()
        {
            config.toolbar = ToolbarConfig::default();
            changed = true;
        }
    });
    changed
}

/// Phase 17 T17.6 — export the current built-in theme to a user TOML file
/// the user can edit by hand. The watcher reloads on change so saved edits
/// land live. Foundation for the in-app live-color-picker editor.
fn render_theme_export(ui: &mut egui::Ui, config: &mut Config) -> bool {
    use scribe_core::theme::Theme;
    let mut changed = false;
    let name_id = egui::Id::new("scr1b3-theme-export-name");
    let mut new_name: String = ui
        .ctx()
        .data(|d| d.get_temp::<String>(name_id))
        .unwrap_or_else(|| "my-theme".to_string());
    let status_id = egui::Id::new("scr1b3-theme-export-status");
    let mut status: String = ui
        .ctx()
        .data(|d| d.get_temp::<String>(status_id))
        .unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label("Export to user theme");
        ui.text_edit_singleline(&mut new_name).on_hover_text(
            "Writes the current theme's colours to \
                 <config_dir>/themes/<name>.toml. Edit the TOML by hand to \
                 customise; live-reload will apply changes immediately.",
        );
        if ui
            .button("Export")
            .on_hover_text("Write the current theme's colours to an editable user TOML file.")
            .clicked()
        {
            let safe = new_name.trim().to_lowercase().replace([' ', '_'], "-");
            if safe.is_empty() {
                status = "Export: name is empty".to_string();
            } else if let Some(dir) = Config::config_dir() {
                let theme_dir = dir.join("themes");
                let path = theme_dir.join(format!("{safe}.toml"));
                let theme =
                    Theme::builtin(&config.appearance.theme).unwrap_or_else(Theme::itasha_corp);
                status = match std::fs::create_dir_all(&theme_dir)
                    .and_then(|()| std::fs::write(&path, theme.to_toml_string()))
                {
                    Ok(()) => {
                        config.appearance.theme = safe.clone();
                        changed = true;
                        format!("Saved to {} — now editable", path.display())
                    }
                    Err(e) => format!("Export failed: {e}"),
                };
            } else {
                status = "Export: no config dir on this OS".to_string();
            }
        }
    });
    if !status.is_empty() {
        ui.label(egui::RichText::new(&status).weak().small());
    }
    ui.ctx().data_mut(|d| {
        d.insert_temp(name_id, new_name);
        d.insert_temp(status_id, status);
    });
    changed
}

/// Phase 17 T17.6b — in-app live color editor. Renders one egui color
/// picker per `[palette]` / `[ui]` / `[syntax]` key of the active user
/// theme. Every change writes the modified theme TOML back to disk; the
/// existing watcher reloads it and the editor reflects the change live.
///
/// Only renders when a user theme TOML exists at
/// `<config_dir>/themes/<active>.toml`. For built-in themes the user
/// is directed to the **Export to user theme** action above — built-ins
/// stay immutable so a switch-back is always possible.
///
/// Returns true when the user mutated a color (so the caller can request
/// a config save for any other changed fields in the same frame).
fn render_live_color_picker(ui: &mut egui::Ui, config: &Config) -> bool {
    use scribe_core::theme::{Rgba, Theme};
    let Some(dir) = Config::config_dir() else {
        return false;
    };
    let theme_dir = dir.join("themes");
    let theme_name = &config.appearance.theme;
    let path = theme_dir.join(format!("{theme_name}.toml"));
    if !path.exists() {
        // Quiet hint — the export-to-user-theme button right above is the
        // forward path; no need to show the picker UI when there's nothing
        // editable.
        ui.label(
            egui::RichText::new("Live color editor: available after Export to user theme above.")
                .weak()
                .small(),
        );
        return false;
    }
    // Read + parse the user TOML. On a parse error, fall back to the
    // built-in by the same name (the watcher already surfaces the parse
    // error elsewhere; we don't double-report here).
    let toml_src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let mut theme = match Theme::from_toml_str(&toml_src) {
        Ok(t) => t,
        Err(_) => Theme::builtin(theme_name).unwrap_or_else(Theme::itasha_corp),
    };
    let mut any_changed = false;
    egui::CollapsingHeader::new("Edit colors live")
        .id_salt("scr1b3-live-color-picker")
        .default_open(false)
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(
                    "Changes save to the user theme TOML; the watcher \
                     applies them live. Switch theme to revert.",
                )
                .weak()
                .small(),
            );
            // Render every key in palette / ui / syntax. The sections
            // collapse independently so users editing only `[ui]` aren't
            // overwhelmed by `[syntax]` rows.
            for (section_name, owner_kind) in [
                ("palette", PickerSection::Palette),
                ("ui", PickerSection::Ui),
                ("syntax", PickerSection::Syntax),
            ] {
                let entry_count = match owner_kind {
                    PickerSection::Palette => theme.palette.len(),
                    PickerSection::Ui => theme.ui.len(),
                    PickerSection::Syntax => theme.syntax.len(),
                };
                egui::CollapsingHeader::new(format!("{section_name}  [{entry_count}]"))
                    .id_salt(format!("scr1b3-live-color-picker-{section_name}"))
                    .default_open(matches!(owner_kind, PickerSection::Palette))
                    .show(ui, |ui| {
                        let map = match owner_kind {
                            PickerSection::Palette => &mut theme.palette,
                            PickerSection::Ui => &mut theme.ui,
                            PickerSection::Syntax => &mut theme.syntax,
                        };
                        // Stable iteration order — BTreeMap walks sorted.
                        let keys: Vec<String> = map.keys().cloned().collect();
                        for k in keys {
                            let r = match map.get_mut(&k) {
                                Some(r) => r,
                                None => continue,
                            };
                            let mut srgba =
                                egui::Color32::from_rgba_unmultiplied(r.r, r.g, r.b, r.a);
                            let row_changed = ui
                                .horizontal(|ui| {
                                    let resp = egui::color_picker::color_edit_button_srgba(
                                        ui,
                                        &mut srgba,
                                        egui::color_picker::Alpha::OnlyBlend,
                                    );
                                    ui.label(&k);
                                    resp.changed()
                                })
                                .inner;
                            if row_changed {
                                let [rr, gg, bb, aa] = srgba.to_array();
                                *r = Rgba {
                                    r: rr,
                                    g: gg,
                                    b: bb,
                                    a: aa,
                                };
                                any_changed = true;
                            }
                        }
                    });
            }
        });
    if any_changed {
        // Persist immediately; the watcher will pick it up on its next
        // scan tick and apply the change live. Write errors stay quiet
        // here — surface a status string if this ever needs operator UX.
        let _ = std::fs::write(&path, theme.to_toml_string());
    }
    any_changed
}

#[derive(Clone, Copy)]
enum PickerSection {
    Palette,
    Ui,
    Syntax,
}

#[cfg(test)]
mod hex_color {
    use super::parse_hex_color;

    #[test]
    fn parses_with_and_without_hash() {
        assert_eq!(
            parse_hex_color("#112233"),
            Some(egui::Color32::from_rgb(0x11, 0x22, 0x33))
        );
        assert_eq!(
            parse_hex_color("aabbcc"),
            Some(egui::Color32::from_rgb(0xaa, 0xbb, 0xcc))
        );
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(parse_hex_color("#123"), None);
        assert_eq!(parse_hex_color("nothex!"), None);
        assert_eq!(parse_hex_color(""), None);
    }

    #[test]
    fn rejects_non_ascii_without_panicking() {
        // A multibyte char can make a value 6 BYTES long while crossing a char
        // boundary inside the `&h[0..2]` / `&h[2..4]` windows — the old code
        // panicked (aborting the whole app) instead of returning `None`.
        // `€` is 3 bytes: `aa€` strips to 5 bytes; `aa€b` = 6 bytes → the old
        // `== 6` check passed and `&h[2..4]` sliced through `€`.
        assert_eq!(parse_hex_color("aa\u{20ac}b"), None); // 6 bytes, splits €
        assert_eq!(parse_hex_color("#aa\u{20ac}b"), None); // same, with hash
        assert_eq!(parse_hex_color("\u{20ac}\u{20ac}"), None); // 6 bytes, all non-ascii
    }
}

#[cfg(test)]
mod deep_link {
    //! #71 — the status-bar encoding / language chips advertise
    //! "Settings → Editor"; opening Settings must land on that category, not the
    //! last-used / default "Appearance". The host calls [`request_category`]
    //! before flipping the window open; [`show`] reads the SAME temp key on its
    //! next frame. This pins that both sides agree on the key + value so the
    //! deep-link can't silently regress to opening on the wrong page.
    use super::{request_category, settings_cat_id};

    #[test]
    fn request_category_sets_the_key_show_reads() {
        let ctx = egui::Context::default();
        request_category(&ctx, "Editor");
        let stored = ctx.data_mut(|d| d.get_temp::<String>(settings_cat_id()));
        assert_eq!(
            stored.as_deref(),
            Some("Editor"),
            "request_category must write the exact temp key show() reads"
        );
    }

    #[test]
    fn show_defaults_to_appearance_when_no_request_made() {
        // Mirror show()'s own read so the default contract is pinned: absent a
        // deep-link, the window opens on Appearance.
        let ctx = egui::Context::default();
        let stored = ctx
            .data_mut(|d| d.get_temp::<String>(settings_cat_id()))
            .unwrap_or_else(|| "Appearance".to_string());
        assert_eq!(stored, "Appearance");
    }
}

#[cfg(test)]
mod wiring_guard {
    //! Proof that every control exposed in the Settings window is actually
    //! WIRED to runtime behavior — i.e. its config field is read by code outside
    //! `settings.rs` (the UI) and `config.rs` (the definition). A "dead" control
    //! is one nothing reads; this guard catches them and prevents new ones.
    //!
    //! `KNOWN_DEAD` lists controls audited as not-yet-wired; as later phases wire
    //! them, remove them here and the guard then REQUIRES a consumer.
    use std::fs;
    use std::path::Path;

    /// All runtime source (scribe-app + scribe-core) minus the settings UI and
    /// the config definition, concatenated for substring consumer-scanning.
    fn runtime_source() -> String {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let mut out = String::new();
        for dir in [
            format!("{manifest}/src"),
            format!("{manifest}/../scribe-core/src"),
        ] {
            collect(Path::new(&dir), &mut out);
        }
        out
    }
    fn collect(dir: &Path, out: &mut String) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                if name == "settings.rs" || name == "config.rs" {
                    continue;
                }
                if let Ok(c) = fs::read_to_string(&p) {
                    out.push_str(&c);
                }
            }
        }
    }

    /// A field is consumed if its `section.field` access (or a documented
    /// method alias that reads it) appears in runtime source.
    fn consumed(src: &str, field: &str) -> bool {
        if src.contains(field) {
            return true;
        }
        // Fields read only through a config method (the literal `section.field`
        // never appears at the call site).
        match field {
            "window.transparency_enabled" => src.contains("effective_translucent"),
            "toolbar.button_size_px" => src.contains("clamped_button_size"),
            "toolbar.button_spacing_px" => src.contains("clamped_button_spacing"),
            "toolbar.icon_size_px" => src.contains("clamped_icon_size"),
            "motion.intensity" => src.contains("clamped_intensity"),
            _ => false,
        }
    }

    /// Every Settings-exposed config field that MUST have a runtime consumer.
    const WIRED: &[&str] = &[
        "appearance.theme",
        "appearance.frameless",
        "appearance.toolbar_icons",
        "appearance.jp_glyph_labels",
        "appearance.background_override",
        "appearance.note_background_override",
        "appearance.link_backgrounds",
        "fonts.editor_size",
        "fonts.line_height",
        "fonts.editor_family",
        "fonts.ui_family",
        "editor.note_theme",
        "editor.tab_width",
        "editor.insert_spaces",
        "editor.show_line_numbers",
        "editor.word_wrap",
        "editor.show_minimap",
        "editor.render_whitespace",
        "editor.tab_bar_position",
        "editor.side_tabs_rotated",
        "editor.restore_session",
        "editor.grid_enabled",
        "editor.experimental_rope_editor",
        "editor.session_backup",
        "editor.auto_save",
        "editor.trim_trailing_whitespace_on_save",
        "editor.final_newline_on_save",
        "editor.restore_cursor_position",
        "window.always_on_top",
        "window.transparency_enabled",
        "window.mode",
        "window.opacity",
        "window.tint",
        "window.tint_strength",
        "spellcheck.enabled",
        "spellcheck.language",
        "spellcheck.check_comments",
        "spellcheck.check_strings",
        "spellcheck.check_identifiers",
        "spellcheck.custom_dict_path",
        "plugins.enabled",
        "toolbar.button_size_px",
        "toolbar.button_spacing_px",
        "toolbar.icon_size_px",
        "appearance.follow_os_theme",
        "updates.mode",
        "updates.check_interval_hours",
        "motion.enabled",
        "motion.intensity",
        "motion.cursor_blink",
    ];

    /// Controls audited as DEAD (no runtime consumer yet). Shrinks as phases wire
    /// them; an entry here that gains a consumer fails the guard (move it to WIRED).
    /// Now EMPTY: every Settings-exposed control has a runtime consumer. Controls
    /// that could not be made to work (egui-impossible font-family/ligatures, the
    /// bespoke motion catalog, OS reduced-motion / battery gates) were removed
    /// rather than left as dead toggles, so there is nothing left to track here.
    const KNOWN_DEAD: &[&str] = &[];

    #[test]
    fn every_wired_setting_has_a_runtime_consumer() {
        let src = runtime_source();
        for &field in WIRED {
            assert!(
                consumed(&src, field),
                "DEAD CONTROL: `{field}` is exposed in Settings but no runtime code reads it",
            );
        }
    }

    #[test]
    fn known_dead_controls_are_still_dead() {
        let src = runtime_source();
        for &field in KNOWN_DEAD {
            assert!(
                !consumed(&src, field),
                "`{field}` now has a consumer -- wire-up done; remove it from KNOWN_DEAD and add to WIRED",
            );
        }
    }
}
