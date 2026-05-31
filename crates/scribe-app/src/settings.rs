//! In-app settings window. Edits the live `Config` (deep customization without
//! hand-editing TOML). Returns `true` when something changed so the caller can
//! persist + re-apply the theme. Kept as a free function so it never fights the
//! `ScribeApp` borrow.
//!
//! Layout: a fixed-size window with a left category nav + a searchable,
//! internally-scrolling content pane — so every setting is reachable at the
//! default size without ever resizing the window (`ScrollArea` with
//! `auto_shrink([false, false])` is the load-bearing idiom here).

use eframe::egui;
use scribe_core::config::{ToolbarConfig, UpdateMode, WindowMode};
use scribe_core::Config;

/// Left-nav categories, in display order.
const CATEGORIES: &[&str] = &[
    "Appearance",
    "Fonts",
    "Editor",
    "Effects",
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
pub fn show(ctx: &egui::Context, config: &mut Config, open: &mut bool) -> bool {
    let mut changed = false;
    let mut keep_open = *open;

    let cat_id = egui::Id::new("scr1b3_settings_cat");
    let q_id = egui::Id::new("scr1b3_settings_query");
    let mut category = ctx
        .data_mut(|d| d.get_temp::<String>(cat_id))
        .unwrap_or_else(|| "Appearance".to_string());
    let mut query = ctx
        .data_mut(|d| d.get_temp::<String>(q_id))
        .unwrap_or_default();

    egui::Window::new("settings")
        .open(&mut keep_open)
        .collapsible(false)
        .resizable(false)
        .fixed_size([760.0, 560.0])
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                // ---- Left category nav ----
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    ui.add_space(2.0);
                    for cat in CATEGORIES {
                        ui.selectable_value(&mut category, (*cat).to_string(), *cat);
                    }
                });
                ui.separator();

                // ---- Searchable content pane ----
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("🔍");
                        ui.add(
                            egui::TextEdit::singleline(&mut query)
                                .hint_text("search settings")
                                .desired_width(f32::INFINITY),
                        );
                        if !query.is_empty() && ui.button("✕").clicked() {
                            query.clear();
                        }
                    });
                    ui.separator();

                    let q = query.trim().to_lowercase();
                    let sel = category.as_str();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            changed |= render_sections(ui, config, sel, &q);
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

/// Render every category section that is visible for the current selection /
/// search query. Comfortable spacing (group gaps) keeps it from feeling
/// squished even at the default window size.
fn render_sections(ui: &mut egui::Ui, config: &mut Config, sel: &str, q: &str) -> bool {
    let mut changed = false;
    let space = |ui: &mut egui::Ui| ui.add_space(10.0);
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
        ui.heading("Appearance");
        if row_visible(q, "theme") {
            // Phase 17 T17.2: theme picker over the 4 built-ins (wired-noir,
            // phosphor-amber, lain-mauve, ghost-paper) + a free text field for
            // user themes stored under <config_dir>/themes/<name>.toml.
            ui.horizontal(|ui| {
                ui.label("Theme");
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
                                changed = true;
                            }
                        }
                    });
                changed |=
                    reset_to_default(ui, &mut config.appearance.theme, &def.appearance.theme);
            });
            if row_visible(q, "theme custom name") {
                changed |= ui
                    .horizontal(|ui| {
                        ui.label("…or user theme name");
                        ui.text_edit_singleline(&mut config.appearance.theme)
                            .on_hover_text(
                                "If a TOML at <config_dir>/themes/<name>.toml exists \
                                 it overrides the built-in; otherwise the built-in by \
                                 the same name (or wired-noir) is used.",
                            )
                            .changed()
                    })
                    .inner;
            }
            if row_visible(q, "export theme tom user customize edit") {
                // Phase 17 T17.6: export the CURRENT built-in (or wired-noir
                // fallback) to <config_dir>/themes/<name>.toml so the user can
                // edit the colours by hand and the live-reload watcher picks
                // it up. Foundation for the live-color-picker editor.
                changed |= render_theme_export(ui, config);
            }
            if row_visible(q, "live color picker edit theme customize palette") {
                // Phase 17 T17.6b — in-app live color editor. Only renders
                // when the active theme has a user TOML on disk (Export
                // first if not). Pickers write changes back to the TOML;
                // the watcher reloads + applies them live.
                changed |= render_live_color_picker(ui, config);
            }
        }
        if row_visible(q, "follow os dark light") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.appearance.follow_os_theme,
                        "Follow OS dark/light",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.appearance.follow_os_theme,
                    &def.appearance.follow_os_theme,
                );
            });
        }
        if row_visible(q, "frameless window") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.appearance.frameless,
                        "Frameless window (restart to apply)",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.appearance.frameless,
                    &def.appearance.frameless,
                );
            });
        }
        if row_visible(q, "toolbar icons words phosphor") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.appearance.toolbar_icons,
                        "Toolbar shows icons (Phosphor Thin) instead of words",
                    )
                    .on_hover_text(
                        "When off, the quick-access toolbar renders text labels (the default). \
                         When on, items render as Phosphor Thin icon glyphs — compact, brand-aligned.",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.appearance.toolbar_icons,
                    &def.appearance.toolbar_icons,
                );
            });
        }
        if row_visible(q, "kanji jp glyph japanese instrument label") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.appearance.jp_glyph_labels,
                        "Toolbar — show kanji instrument labels (additive)",
                    )
                    .on_hover_text(
                        "Adds a small, dim kanji to each toolbar action whose canonical \
                         Japanese term is verified (e.g. New → 新, Save → 保, Find → 検). \
                         English-redundant — the kanji never replaces the label. \
                         Actions whose canonical kanji is uncertain stay English-only \
                         (Folklore-Consultant gate, DECISION-2026-005).",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.appearance.jp_glyph_labels,
                    &def.appearance.jp_glyph_labels,
                );
            });
        }
        space(ui);
    }

    // ---- Fonts ----  (no ligatures: egui has no OpenType shaping, so the
    // toggle is intentionally absent rather than shown as a dead control.)
    if section_visible(sel, q, "Fonts", &["size", "line height"]) {
        ui.heading("Fonts");
        if row_visible(q, "editor size") {
            ui.horizontal(|ui| {
                changed |= ui
                    .add(egui::Slider::new(&mut config.fonts.editor_size, 8.0..=32.0).text("size"))
                    .changed();
                changed |=
                    reset_to_default(ui, &mut config.fonts.editor_size, &def.fonts.editor_size);
            });
        }
        if row_visible(q, "line height") {
            ui.horizontal(|ui| {
                changed |= ui
                    .add(
                        egui::Slider::new(&mut config.fonts.line_height, 1.0..=2.5)
                            .text("line height"),
                    )
                    .changed();
                changed |=
                    reset_to_default(ui, &mut config.fonts.line_height, &def.fonts.line_height);
            });
        }
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
        ui.heading("Editor");
        if row_visible(q, "tab width") {
            ui.horizontal(|ui| {
                changed |= ui
                    .add(egui::Slider::new(&mut config.editor.tab_width, 1..=8).text("tab width"))
                    .changed();
                changed |=
                    reset_to_default(ui, &mut config.editor.tab_width, &def.editor.tab_width);
            });
        }
        if row_visible(q, "insert spaces") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(&mut config.editor.insert_spaces, "Insert spaces (Tab key)")
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.insert_spaces,
                    &def.editor.insert_spaces,
                );
            });
        }
        if row_visible(q, "line numbers") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(&mut config.editor.show_line_numbers, "Line numbers")
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.show_line_numbers,
                    &def.editor.show_line_numbers,
                );
            });
        }
        if row_visible(q, "word wrap") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(&mut config.editor.word_wrap, "Word wrap")
                    .changed();
                changed |=
                    reset_to_default(ui, &mut config.editor.word_wrap, &def.editor.word_wrap);
            });
        }
        if row_visible(q, "minimap") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(&mut config.editor.show_minimap, "Minimap")
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.show_minimap,
                    &def.editor.show_minimap,
                );
            });
        }
        if row_visible(q, "tab bar position top bottom left right") {
            // T18.4: position the open-tab strip relative to the editor.
            use scribe_core::config::TabBarPosition;
            let positions = [
                (TabBarPosition::Top, "top"),
                (TabBarPosition::Bottom, "bottom"),
                (TabBarPosition::Left, "left"),
                (TabBarPosition::Right, "right"),
            ];
            ui.horizontal(|ui| {
                ui.label("Tab bar position");
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
                    });
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.tab_bar_position,
                    &def.editor.tab_bar_position,
                );
            });
        }
        if row_visible(q, "restore session") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(&mut config.editor.restore_session, "Restore session")
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.restore_session,
                    &def.editor.restore_session,
                );
            });
        }
        if row_visible(q, "multi-note grid panes split editor central") {
            // Phase 18 T18.2 — toggle the multi-note grid. When ON, the
            // central editor surface renders every open tab as a movable,
            // resizable pane via egui_tiles. The single-pane code path
            // is unchanged for users who don't opt in.
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.editor.grid_enabled,
                        "Multi-note grid (experimental)",
                    )
                    .on_hover_text(
                        "Render every open tab as a movable / resizable pane in the central \
                         editor. Drag tabs between panes to rearrange; drag the splitter to resize. \
                         Cap of 6 panes lands in a follow-up; for now use the close ✕ on each pane \
                         to dismiss.",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.grid_enabled,
                    &def.editor.grid_enabled,
                );
            });
        }
        if row_visible(q, "experimental rope editor owned cursor undo keystone") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.editor.experimental_rope_editor,
                        "Experimental rope editor (own caret / undo)",
                    )
                    .on_hover_text(
                        "Use the in-house rope editor for normal files instead of the default \
                         egui text widget. Own caret, selection, and persistent-capable undo. \
                         Experimental: no IME / mouse-selection parity yet. Click the editor to \
                         focus it. Read-only huge files always use the rope browse path.",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.experimental_rope_editor,
                    &def.editor.experimental_rope_editor,
                );
            });
        }
        if row_visible(q, "session backup hot exit unsaved restore crash recovery") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.editor.session_backup,
                        "Restore unsaved notes after restart (session backup)",
                    )
                    .on_hover_text(
                        "Keeps a backup of unsaved buffers (including never-saved scratch \
                         notes) so they come back after a restart or crash — no save needed. \
                         Backups live in the config 'backup' folder and are deleted once you \
                         save. On by default.",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.session_backup,
                    &def.editor.session_backup,
                );
            });
        }
        if row_visible(q, "auto save autosave") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.editor.auto_save,
                        "Auto-save (after a short pause)",
                    )
                    .on_hover_text(
                        "Automatically save dirty file-backed buffers a few seconds after you \
                         stop typing. Untitled buffers are never auto-saved. Off by default.",
                    )
                    .changed();
                changed |=
                    reset_to_default(ui, &mut config.editor.auto_save, &def.editor.auto_save);
            });
        }
        if row_visible(q, "trim trailing whitespace on save") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.editor.trim_trailing_whitespace_on_save,
                        "Trim trailing whitespace on save",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.trim_trailing_whitespace_on_save,
                    &def.editor.trim_trailing_whitespace_on_save,
                );
            });
        }
        if row_visible(q, "final newline ensure on save") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.editor.final_newline_on_save,
                        "Ensure final newline on save",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.final_newline_on_save,
                    &def.editor.final_newline_on_save,
                );
            });
        }
        if row_visible(q, "restore cursor caret position per file") {
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.editor.restore_cursor_position,
                        "Restore caret position per file",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.restore_cursor_position,
                    &def.editor.restore_cursor_position,
                );
            });
        }
        space(ui);
    }

    // ---- Effects (CRT) ----
    if section_visible(
        sel,
        q,
        "Effects",
        &[
            "crt",
            "scanline",
            "glow",
            "bloom",
            "vignette",
            "curvature",
            "reduced motion",
        ],
    ) {
        ui.heading("CRT effect");
        ui.horizontal(|ui| {
            changed |= ui
                .checkbox(&mut config.effects.crt_enabled, "Enable CRT post-process")
                .changed();
            changed |= reset_to_default(
                ui,
                &mut config.effects.crt_enabled,
                &def.effects.crt_enabled,
            );
        });
        ui.add_enabled_ui(config.effects.crt_enabled, |ui| {
            for (label, cur, dflt) in [
                (
                    "scanline",
                    &mut config.effects.scanline,
                    def.effects.scanline,
                ),
                (
                    "glow",
                    &mut config.effects.phosphor_glow,
                    def.effects.phosphor_glow,
                ),
                ("bloom", &mut config.effects.bloom, def.effects.bloom),
                (
                    "vignette",
                    &mut config.effects.vignette,
                    def.effects.vignette,
                ),
                (
                    "curvature",
                    &mut config.effects.curvature,
                    def.effects.curvature,
                ),
            ] {
                ui.horizontal(|ui| {
                    changed |= ui
                        .add(egui::Slider::new(cur, 0.0..=1.0).text(label))
                        .changed();
                    changed |= reset_to_default(ui, cur, &dflt);
                });
            }
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.effects.respect_reduced_motion,
                        "Respect reduced motion",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.effects.respect_reduced_motion,
                    &def.effects.respect_reduced_motion,
                );
            });
        });
        space(ui);
    }

    // ---- Motion / animations (Phase 17 T17.3) ----
    if section_visible(
        sel,
        q,
        "Motion",
        &[
            "motion",
            "animation",
            "hover",
            "blink",
            "fade",
            "reduced motion",
            "battery",
        ],
    ) {
        ui.heading("Motion");
        // Master OFF by default — calm-surface principle (DECISION-2026-005);
        // animation is opt-in so idle frames cost the same as plain egui.
        ui.horizontal(|ui| {
            changed |= ui
                .checkbox(&mut config.motion.enabled, "Enable motion (master)")
                .on_hover_text(
                    "When off, every animation is suppressed regardless of the per-effect \
                     toggles below — idle frames cost the same as plain egui.",
                )
                .changed();
            changed |= reset_to_default(ui, &mut config.motion.enabled, &def.motion.enabled);
        });
        ui.add_enabled_ui(config.motion.enabled, |ui| {
            ui.horizontal(|ui| {
                changed |= ui
                    .add(
                        egui::Slider::new(&mut config.motion.intensity, 0.0..=1.0)
                            .text("Global intensity"),
                    )
                    .changed();
                changed |=
                    reset_to_default(ui, &mut config.motion.intensity, &def.motion.intensity);
            });
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.motion.respect_reduced_motion,
                        "Respect OS reduced-motion",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.respect_reduced_motion,
                    &def.motion.respect_reduced_motion,
                );
            });
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.motion.respect_battery,
                        "Disable on battery (laptop power-save)",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.respect_battery,
                    &def.motion.respect_battery,
                );
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Per-effect catalog").weak().small());
            for (label, field, dflt) in [
                ("Hover lift", &mut config.motion.hover, def.motion.hover),
                (
                    "Focus ring pulse",
                    &mut config.motion.focus_ring,
                    def.motion.focus_ring,
                ),
                (
                    "Panel slide",
                    &mut config.motion.panel_slide,
                    def.motion.panel_slide,
                ),
                (
                    "Tab underline",
                    &mut config.motion.tab_underline,
                    def.motion.tab_underline,
                ),
                (
                    "Palette lift-in",
                    &mut config.motion.palette_lift,
                    def.motion.palette_lift,
                ),
                (
                    "Cursor blink",
                    &mut config.motion.cursor_blink,
                    def.motion.cursor_blink,
                ),
                (
                    "Status-bar breathe",
                    &mut config.motion.status_breathe,
                    def.motion.status_breathe,
                ),
                ("Toast slide", &mut config.motion.toast, def.motion.toast),
                (
                    "Error glitch flash",
                    &mut config.motion.error_glitch,
                    def.motion.error_glitch,
                ),
                (
                    "ASCII boot splash",
                    &mut config.motion.ascii_boot_splash,
                    def.motion.ascii_boot_splash,
                ),
                (
                    "Idle pulse",
                    &mut config.motion.idle_pulse,
                    def.motion.idle_pulse,
                ),
                (
                    "Transition fade",
                    &mut config.motion.transition_fade,
                    def.motion.transition_fade,
                ),
            ] {
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(field, label).changed();
                    changed |= reset_to_default(ui, field, &dflt);
                });
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
        ui.heading("Window");
        // F-035 — always-on-top toggle. Takes effect immediately via the
        // ViewportCommand the app issues when this checkbox flips.
        ui.horizontal(|ui| {
            changed |= ui
                .checkbox(&mut config.window.always_on_top, "Always on top")
                .on_hover_text("Keep the SCR1B3 window above other windows.")
                .changed();
            changed |= reset_to_default(
                ui,
                &mut config.window.always_on_top,
                &def.window.always_on_top,
            );
        });
        ui.add_space(4.0);
        ui.heading("Window (transparency / glass)");
        // Master on/off switch for the whole transparency system. Off by default:
        // a normal opaque window is fast and never leaves a DWM ghost on close.
        ui.horizontal(|ui| {
            changed |= ui
                .checkbox(
                    &mut config.window.transparency_enabled,
                    "Enable window transparency (master)",
                )
                .on_hover_text(
                    "Master switch. When off, the window is fully opaque regardless of \
                     the mode below. Turn on to use transparent / glass / mica / vibrancy. \
                     Restart to apply the surface change.",
                )
                .changed();
            changed |= reset_to_default(
                ui,
                &mut config.window.transparency_enabled,
                &def.window.transparency_enabled,
            );
        });
        ui.add_enabled_ui(config.window.transparency_enabled, |ui| {
            let wmodes = [
                (WindowMode::Opaque, "opaque"),
                (WindowMode::Transparent, "transparent"),
                (WindowMode::Glass, "glass / acrylic"),
                (WindowMode::Mica, "mica (Win11)"),
                (WindowMode::Vibrancy, "vibrancy (macOS)"),
            ];
            ui.horizontal(|ui| {
                egui::ComboBox::from_label("mode (restart to apply blur)")
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
                    });
                changed |= reset_to_default(ui, &mut config.window.mode, &def.window.mode);
            });
            ui.add_enabled_ui(config.window.mode.is_translucent(), |ui| {
                ui.horizontal(|ui| {
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut config.window.opacity, 0.30..=1.0)
                                .text("opacity"),
                        )
                        .changed();
                    changed |=
                        reset_to_default(ui, &mut config.window.opacity, &def.window.opacity);
                });
            });
            ui.horizontal(|ui| {
                ui.label("tint");
                changed |= ui.text_edit_singleline(&mut config.window.tint).changed();
                changed |= reset_to_default(ui, &mut config.window.tint, &def.window.tint);
            });
            ui.horizontal(|ui| {
                changed |= ui
                    .add(
                        egui::Slider::new(&mut config.window.tint_strength, 0.0..=1.0)
                            .text("tint strength"),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.window.tint_strength,
                    &def.window.tint_strength,
                );
            });
        }); // end add_enabled_ui(transparency_enabled)
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
        ui.heading("Spellcheck (offline)");
        ui.horizontal(|ui| {
            changed |= ui
                .checkbox(&mut config.spellcheck.enabled, "Enable")
                .changed();
            changed |=
                reset_to_default(ui, &mut config.spellcheck.enabled, &def.spellcheck.enabled);
        });
        ui.add_enabled_ui(config.spellcheck.enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("Language");
                changed |= ui
                    .text_edit_singleline(&mut config.spellcheck.language)
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.language,
                    &def.spellcheck.language,
                );
            });
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(&mut config.spellcheck.check_comments, "Check comments")
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.check_comments,
                    &def.spellcheck.check_comments,
                );
            });
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(&mut config.spellcheck.check_strings, "Check strings")
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.check_strings,
                    &def.spellcheck.check_strings,
                );
            });
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(
                        &mut config.spellcheck.check_identifiers,
                        "Check identifiers",
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.check_identifiers,
                    &def.spellcheck.check_identifiers,
                );
            });
        });
        space(ui);
    }

    // ---- Updates ----
    if section_visible(sel, q, "Updates", &["update", "mode", "notify", "auto"]) {
        ui.heading("Updates (telemetry-free)");
        let modes = [
            (UpdateMode::Off, "off"),
            (UpdateMode::Notify, "notify"),
            (UpdateMode::Manual, "manual"),
            (UpdateMode::Auto, "auto"),
        ];
        ui.horizontal(|ui| {
            egui::ComboBox::from_label("mode")
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
                });
            changed |= reset_to_default(ui, &mut config.updates.mode, &def.updates.mode);
        });
        if row_visible(q, "check interval hours") {
            ui.horizontal(|ui| {
                ui.label("Check interval (hours)");
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
            });
        }
        space(ui);
    }

    // ---- Plugins ----
    if section_visible(sel, q, "Plugins", &["plugin", "mod"]) {
        ui.heading("Plugins");
        ui.horizontal(|ui| {
            changed |= ui
                .checkbox(&mut config.plugins.enabled, "Enable plugin/mod system")
                .changed();
            changed |= reset_to_default(ui, &mut config.plugins.enabled, &def.plugins.enabled);
        });
        ui.label(
            egui::RichText::new("Drop mods into the plugins dir — see PLUGINS.md")
                .weak()
                .small(),
        );
        // F-039 — open the plugin manager (Loaded / Registry / Install). The
        // request is stashed in egui temp data; the host reads + clears it
        // after `show` returns so it can open its own modal state.
        if ui.button("Manage plugins…").clicked() {
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
fn render_toolbar_editor(ui: &mut egui::Ui, config: &mut Config) -> bool {
    let mut changed = false;
    ui.heading("Quick-access toolbar");
    ui.label(
        egui::RichText::new(
            "Choose what shows in the top bar. Drag rows to reorder; drag from the \
             palette to add. Keyboard: ↑/↓ reorder, ✕ removes.",
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
    if ui.small_button("Reset sizing to defaults").clicked() {
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
                // A small grip glyph signals "this row is draggable".
                ui.add(egui::Label::new(egui::RichText::new("⠿").weak().small()).selectable(false))
                    .on_hover_text("Drag to reorder");
                if ui.add_enabled(i > 0, egui::Button::new("↑")).clicked() {
                    mv = Some((i, -1));
                }
                if ui.add_enabled(i + 1 < n, egui::Button::new("↓")).clicked() {
                    mv = Some((i, 1));
                }
                if ui.button("✕").clicked() {
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
                let chip = egui::Frame::default()
                    .inner_margin(egui::Margin::symmetric(6, 3))
                    .stroke(egui::Stroke::new(
                        1.0,
                        ui.visuals().widgets.inactive.bg_stroke.color,
                    ))
                    .corner_radius(egui::CornerRadius::same(3));
                chip.show(ui, |ui| {
                    ui.label(*label);
                });
            })
            .response
            .on_hover_text("Drag onto the list above to add");
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("add:");
        egui::ComboBox::from_id_salt("toolbar-add")
            .selected_text("choose…")
            .show_ui(ui, |ui| {
                for (id, label) in crate::app::TOOLBAR_ACTIONS {
                    if ui.selectable_label(false, *label).clicked() {
                        config.toolbar.items.push((*id).to_string());
                        changed = true;
                    }
                }
            });
        if ui.button("reset to defaults").clicked() {
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
        if ui.button("Export").clicked() {
            let safe = new_name.trim().to_lowercase().replace([' ', '_'], "-");
            if safe.is_empty() {
                status = "Export: name is empty".to_string();
            } else if let Some(dir) = Config::config_dir() {
                let theme_dir = dir.join("themes");
                let path = theme_dir.join(format!("{safe}.toml"));
                let theme =
                    Theme::builtin(&config.appearance.theme).unwrap_or_else(Theme::wired_noir);
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
        Err(_) => Theme::builtin(theme_name).unwrap_or_else(Theme::wired_noir),
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
