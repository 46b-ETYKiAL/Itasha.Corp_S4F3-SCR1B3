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
use scribe_core::config::{ClaimType, ToolbarConfig, UpdateMode};
use scribe_core::{Config, ReportingMode};

/// Left-nav categories, in display order. Look-and-feel groups first
/// (Appearance, Fonts, Window, Toolbar, Motion), then editing behaviour
/// (Editor, Spellcheck), then system (Plugins, Default app, Updates, Privacy) —
/// with "Default app" sitting directly below Plugins, then Updates and Privacy.
const CATEGORIES: &[&str] = &[
    "Appearance",
    "Fonts",
    "Window",
    "Toolbar",
    "Motion",
    "Editor",
    "Spellcheck",
    "Plugins",
    "Default app",
    "Updates",
    "Privacy",
];

/// egui temp-data key the Plugins section sets when "Manage plugins…" is
/// clicked. The host reads + clears it after [`show`] returns and opens its
/// own plugin-manager modal — settings owns no modal state of its own.
fn open_plugin_manager_id() -> egui::Id {
    egui::Id::new("scr1b3_open_plugin_manager")
}

/// egui temp-data key holding the last default-app registration status message,
/// so the "Default app" section can show the result of the most recent attempt
/// across frames (settings owns no persistent state of its own).
fn default_app_status_id() -> egui::Id {
    egui::Id::new("scr1b3_default_app_status")
}

/// Shared slot a background default-app registration thread writes its result
/// into. `Arc<Mutex<..>>` is `Clone + Send + Sync`, so it can live in egui
/// ctx-data (the settings pane owns no state of its own) while the worker runs.
type RegShared = std::sync::Arc<std::sync::Mutex<Option<crate::integration::RegisterReport>>>;

/// ctx-data key for the in-flight registration handle (present ⇒ a registration
/// is running; the section shows a spinner instead of freezing the UI).
fn register_pending_id() -> egui::Id {
    egui::Id::new("scr1b3_register_pending")
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

/// Index of the theme reached by stepping `delta` from `current` in `names`.
///
/// `rem_euclid` wraps the list in both directions, so the prev/next arrows never
/// dead-end. A `current` that is NOT a built-in (a user theme from
/// `<config_dir>/themes/`) has no position to step from, so it lands on the
/// first entry going forward and the last going backward — the arrows always
/// have a defined landing spot.
///
/// Pure, and a free function rather than a closure inside the render body, so it
/// can be tested without driving the UI. It was a closure, and every one of its
/// mutants survived (`>` → `==`/`<`/`>=`, `+` → `-`/`*`, `n - 1` → `n + 1`, the
/// `delta > 0` guard → both `true` AND `false`) because nothing clicks the
/// arrows in a test. An input that is not a parameter is not testable.
///
/// # Panics
/// Never for a non-empty `names`; the caller indexes with the returned value,
/// which `rem_euclid` keeps in `0..names.len()`.
fn step_theme_index(names: &[&str], current: &str, delta: isize) -> usize {
    let n = names.len() as isize;
    debug_assert!(n > 0, "there is always at least one built-in theme");
    let next = match names.iter().position(|t| *t == current) {
        Some(i) => (i as isize + delta).rem_euclid(n),
        None if delta > 0 => 0,
        None => n - 1,
    };
    next as usize
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

/// The human label for a W1TN3SS reporting mode, in CONSENT language (never
/// surveillance/telemetry/always-on copy). Shared by the settings selector and
/// the consent dialog so the wording stays one source of truth.
pub fn reporting_mode_label(mode: ReportingMode) -> &'static str {
    match mode {
        ReportingMode::Off => "Never (off)",
        ReportingMode::AskEachTime => "Ask each time",
        ReportingMode::Always => "Always send",
    }
}

/// A per-stream reporting-mode selector (Off / Ask each time / Always),
/// rendered as three equal-weight radio choices in consent language. Returns
/// `true` if the mode changed. The three options carry IDENTICAL affordance —
/// no pre-emphasis on "Always" (GDPR "freely given" + no dark pattern).
fn reporting_mode_selector(ui: &mut egui::Ui, id: &str, mode: &mut ReportingMode) -> bool {
    let before = *mode;
    ui.horizontal(|ui| {
        // The three choices are equal-weight radios laid out in the same row;
        // none is visually privileged. `Off` is first because it is the default
        // and the most privacy-conservative reading.
        for choice in [
            ReportingMode::Off,
            ReportingMode::AskEachTime,
            ReportingMode::Always,
        ] {
            ui.radio_value(mode, choice, reporting_mode_label(choice))
                .on_hover_text(match choice {
                    ReportingMode::Off => {
                        "Never capture or send anything for this stream (the default)."
                    }
                    ReportingMode::AskEachTime => {
                        "Capture locally, then ask you each time before anything is sent — \
                         with an editable preview of the exact report."
                    }
                    ReportingMode::Always => {
                        "Send automatically after a crash. Even then the report is captured \
                         and spooled locally first, and you can review what is collected here."
                    }
                });
        }
    });
    let _ = id; // id reserved for a future grid layout; radios key off the value.
    *mode != before
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
    // The theme's UN-tinted text-field background. The app colour-tint shifts the
    // global `extreme_bg_color` (the editor well), which would otherwise also
    // tint the Settings text inputs; the Settings window is exempt from the tint,
    // so its fields are reset to this untinted colour.
    field_bg: egui::Color32,
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

    // A FIXED-default-size, RESIZABLE window. The old per-page width jump (an
    // auto-sized window whose width was driven by a width-greedy page) is gone
    // now that the Toolbar palette uses a bounded Grid — so the window no longer
    // auto-sizes to content; it opens at `default_size` and the user can resize.
    // Because the size is explicit (not content-driven), the content can fill the
    // available width responsively without re-introducing the jump. egui
    // constrains the window to the app window (screen_rect), so it always fits.
    // A tall default + bigger inner editors mean the UI/Syntax colour lists are
    // visible without forced scrolling. `_v4` discards the old fixed-size rect.
    let screen = ctx.content_rect();
    // Width = only as wide as the content needs: the 170px category nav + the
    // content pane, whose widest page (Toolbar) is capped at TB_W (560) + the
    // search row, so ~760 fits everything without the old slack. (Was 920, which
    // opened noticeably wider than necessary.) Still user-resizable.
    let def_w = 760.0_f32.min(screen.width() - 24.0);
    let def_h = 760.0_f32.min(screen.height() - 24.0);
    egui::Window::new("settings")
        .id(egui::Id::new("scr1b3_settings_v4"))
        .open(&mut keep_open)
        .collapsible(false)
        .resizable(true)
        .default_size([def_w, def_h])
        .min_size([500.0, 380.0])
        .max_height(screen.height() - 16.0)
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
            // Exempt the Settings window from the app colour-tint: the frame fill
            // above is the un-tinted window colour, and resetting the field
            // background here un-tints every text input in the window (search box,
            // theme-name fields) so the tint stays a MAIN-APP-only effect.
            ui.visuals_mut().extreme_bg_color = field_bg;
            // The window size is explicit (default_size, user-resizable), so the
            // content fills the available width responsively — no fixed pin is
            // needed and none of the old per-page width jump can occur.
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
                        // Fill the content pane — safe now that the window is a
                        // fixed/user-controlled size (the field can't drive the
                        // window width any more). Leave room for the clear (✕).
                        ui.add(
                            egui::TextEdit::singleline(&mut query)
                                .hint_text("search settings")
                                .desired_width(ui.available_width() - 28.0),
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
                    // Fill the remaining content area (the window size is explicit,
                    // so this can't drive the window). Vertical scroll handles any
                    // overflow taller than the (now tall) window.
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

/// Step an index in a list of `len` options by `delta`, wrapping both ways
/// (`rem_euclid`). A `current` of `None` (the selected value is not in the list,
/// e.g. a user-typed theme name) lands on the first option stepping forward and
/// the last stepping back, so the arrows always have a defined destination.
/// Generalizes `step_theme_index` to any indexed option list.
fn step_index(len: usize, current: Option<usize>, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let n = len as isize;
    match current {
        Some(i) => (i as isize + delta).rem_euclid(n) as usize,
        None if delta > 0 => 0,
        None => (n - 1) as usize,
    }
}

/// A dropdown flanked by prev/next stepper arrows — the generalization of the
/// theme step-arrows applied to EVERY settings dropdown so an option can be
/// cycled in place without opening the menu (mirrors C0PL4ND). The `ComboBox` is
/// pinned to `width` so the flanking arrows stay stationary as the selected label
/// changes length; the Phosphor carets carry accessible hover names
/// (`Previous {what}` / `Next {what}`) because a bare caret glyph reads only as a
/// codepoint to AccessKit. `current_idx` is the selected option's index (`None`
/// when the current value is not in the list → arrows land on an end).
/// `on_pick(i)` applies option `i` (and performs any side-effects). Renders the
/// control column only (the caller owns the label + ↺ reset columns). Returns
/// whether the value changed.
fn stepper_combo(
    ui: &mut egui::Ui,
    id_salt: &str,
    width: f32,
    what: &str,
    len: usize,
    current_idx: Option<usize>,
    selected: &str,
    label_at: impl Fn(usize) -> String,
    mut on_pick: impl FnMut(usize),
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new(egui_phosphor::thin::CARET_LEFT))
            .on_hover_text(format!("Previous {what}"))
            .clicked()
        {
            on_pick(step_index(len, current_idx, -1));
            changed = true;
        }
        // Track the pick locally so `selectable_value` closes the menu on select;
        // apply it via `on_pick` after the menu closure.
        let mut picked = current_idx;
        egui::ComboBox::from_id_salt(id_salt)
            .width(width)
            .selected_text(selected.to_owned())
            .show_ui(ui, |ui| {
                for i in 0..len {
                    ui.selectable_value(&mut picked, Some(i), label_at(i));
                }
            })
            .response
            .on_hover_text(format!("Choose a {what}, or use the arrows to cycle."));
        if picked != current_idx {
            if let Some(i) = picked {
                on_pick(i);
                changed = true;
            }
        }
        if ui
            .add(egui::Button::new(egui_phosphor::thin::CARET_RIGHT))
            .on_hover_text(format!("Next {what}"))
            .clicked()
        {
            on_pick(step_index(len, current_idx, 1));
            changed = true;
        }
    });
    changed
}

/// A slider flanked by −/+ step buttons (minus LEFT, plus RIGHT) — added to
/// every settings slider so a value can be nudged one `step` without dragging.
/// The buttons CLAMP at the range bounds (no wrap) and are disabled at the bound
/// (W3C ARIA APG slider guidance). `enabled` gates the whole control in lock-step
/// with a parent toggle (the `add_enabled` gating several Motion/Window sliders
/// already use). Generic over any `egui` numeric so integer sliders (tab width,
/// scroll-off, check interval) step by 1 the same way. Renders the control column
/// only (the caller owns the label + ↺ reset). Returns whether the value changed.
fn stepped_slider<N: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    enabled: bool,
    val: &mut N,
    range: std::ops::RangeInclusive<N>,
    step: f64,
) -> bool {
    let lo = range.start().to_f64();
    let hi = range.end().to_f64();
    let mut changed = false;
    ui.horizontal(|ui| {
        let cur = val.to_f64();
        if ui
            .add_enabled(
                enabled && cur > lo,
                egui::Button::new(egui_phosphor::thin::MINUS),
            )
            .on_hover_text("Decrease")
            .clicked()
        {
            *val = N::from_f64((cur - step).max(lo));
            changed = true;
        }
        changed |= ui
            .add_enabled(enabled, egui::Slider::new(val, range.clone()))
            .changed();
        let cur = val.to_f64();
        if ui
            .add_enabled(
                enabled && cur < hi,
                egui::Button::new(egui_phosphor::thin::PLUS),
            )
            .on_hover_text("Increase")
            .clicked()
        {
            *val = N::from_f64((cur + step).min(hi));
            changed = true;
        }
    });
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
                ui.horizontal(|ui| {
                    let names = scribe_core::theme::Theme::builtin_names();
                    // Prev/next arrows that cycle the built-in themes IN PLACE
                    // (no dropdown open). They are kept STATIONARY by pinning the
                    // ComboBox between them to a FIXED width, so the arrows never
                    // shift as the selected theme name changes length. Phosphor
                    // carets render from the icon atlas (a raw "<"/">" glyph can
                    // tofu in some UI fonts). `rem_euclid` wraps around the list;
                    // a custom (non-built-in) theme name steps onto the first/last
                    // built-in so the arrows always have a defined landing spot
                    // (mirrors C0PL4ND's theme step arrows).
                    let step = |config: &mut Config, delta: isize| {
                        let next = step_theme_index(names, &config.appearance.theme, delta);
                        config.appearance.theme = names[next].to_string();
                        // #88/#106 — switching theme resets BOTH the app and note
                        // background overrides to the new theme (parity with the
                        // dropdown path below).
                        config.appearance.background_override = None;
                        config.appearance.note_background_override = None;
                    };
                    if ui
                        .add(egui::Button::new(egui_phosphor::thin::CARET_LEFT))
                        .on_hover_text("Previous theme")
                        .clicked()
                    {
                        step(config, -1);
                        changed = true;
                    }
                    egui::ComboBox::from_id_salt("theme-picker")
                        // Fixed width => the flanking arrows are stationary and a
                        // long theme name is clipped inside the box rather than
                        // pushing the "next" arrow to the right.
                        .width(168.0)
                        .selected_text(config.appearance.theme.clone())
                        .show_ui(ui, |ui| {
                            for name in names {
                                if ui
                                    .selectable_value(
                                        &mut config.appearance.theme,
                                        (*name).to_string(),
                                        *name,
                                    )
                                    .changed()
                                {
                                    config.appearance.background_override = None;
                                    config.appearance.note_background_override = None;
                                    changed = true;
                                }
                            }
                        })
                        .response
                        .on_hover_text(
                            "Choose a built-in colour theme, or use the arrows to cycle.",
                        );
                    if ui
                        .add(egui::Button::new(egui_phosphor::thin::CARET_RIGHT))
                        .on_hover_text("Next theme")
                        .clicked()
                    {
                        step(config, 1);
                        changed = true;
                    }
                });
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
            if row_visible(q, "ui scale zoom accessibility a11y") {
                // M6 — whole-app accessibility zoom, applied via ctx.set_zoom_factor
                // once per frame. Clamped to 0.5..=3.0 with a NaN/inf guard by
                // Config::effective_ui_scale so a wild value can't blank the window.
                ui.label("UI scale").on_hover_text(
                    "Zoom the entire interface — text, chrome, and controls — for readability. \
                     1.0 is the standard size; the range is 0.5× to 3×.",
                );
                changed |= ui
                    .add(egui::Slider::new(&mut config.ui_scale, 0.5..=3.0))
                    .changed();
                changed |= reset_to_default(ui, &mut config.ui_scale, &def.ui_scale);
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
                "Draw the window without the OS title bar (a custom in-app title bar is used). \
                 Known Windows limitation: with a glass/mica/vibrancy backdrop the DWM can re-add \
                 the native min/max/close buttons over the custom title bar (a doubled caption). \
                 If you see that, turn frameless OFF — the native frame composes cleanly with the \
                 backdrop.",
                &mut config.appearance.frameless,
                &def.appearance.frameless,
            );
            changed |= grid_bool(
                ui,
                q,
                "toolbar in titlebar compact chrome",
                "Toolbar in the title bar",
                "Move the quick-access toolbar into the custom title bar (between the app name and \
                 the window buttons) and hide the separate toolbar row — a compact single-row \
                 chrome. Requires the frameless window.",
                &mut config.appearance.toolbar_in_titlebar,
                &def.appearance.toolbar_in_titlebar,
            );
            changed |= grid_bool(
                ui,
                q,
                "status bar bottom show hide",
                "Show the bottom status bar",
                "Show the status bar along the bottom of the window (cursor position, encoding, \
                 line endings, spellcheck and diagnostics counts). Turn it off for a more \
                 distraction-free editing surface without entering full zen mode.",
                &mut config.appearance.show_status_bar,
                &def.appearance.show_status_bar,
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
        // Full in-app theme creator/editor: seeds from the active theme, live
        // colour pickers grouped by UI/Syntax with a live preview, then Save
        // writes an editable user theme TOML and switches to it. Supersedes the
        // old export-button + hidden colour-list flow.
        if row_visible(
            q,
            "theme create edit customize export palette colour color user editor",
        ) {
            // #10 — a separator + heading delimits the toolbar toggles above from
            // the theme-creator section below, so the colour editor reads as its
            // own distinct block rather than running on from the toggle list.
            ui.separator();
            ui.label(egui::RichText::new("Create / edit a theme").strong());
            ui.add_space(4.0);
            changed |= crate::theme_editor::show(ui, config);
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
            changed |= grid_bool(
                ui,
                q,
                "rich markdown note colouring tokens master switch",
                "Rich markdown colouring",
                "Colour extra note tokens the syntax grammar leaves plain — divider \
                 lines, #tags, ~~strikethrough~~, task boxes and table pipes. The \
                 per-token switches below tune individual passes; turn this off to \
                 disable them all at once.",
                &mut config.editor.md_rich_coloring,
                &def.editor.md_rich_coloring,
            );
            changed |= grid_bool(
                ui,
                q,
                "markdown colour divider lines separators rules setext",
                "  • Divider lines (----  ====//)",
                "Colour decorative divider lines: ----, ====//====//, * * *, setext \
                 underlines, and box-drawing rules. Active when Rich markdown \
                 colouring is on.",
                &mut config.editor.md_color_dividers,
                &def.editor.md_color_dividers,
            );
            changed |= grid_bool(
                ui,
                q,
                "markdown colour hashtags tags",
                "  • #tags",
                "Colour #tag tokens in the editor. Active when Rich markdown \
                 colouring is on.",
                &mut config.editor.md_color_tags,
                &def.editor.md_color_tags,
            );
            changed |= grid_bool(
                ui,
                q,
                "markdown colour strikethrough",
                "  • ~~strikethrough~~",
                "Colour ~~strikethrough~~ spans. Active when Rich markdown \
                 colouring is on.",
                &mut config.editor.md_color_strikethrough,
                &def.editor.md_color_strikethrough,
            );
            changed |= grid_bool(
                ui,
                q,
                "markdown colour task boxes checkboxes",
                "  • Task boxes [ ] [x]",
                "Colour GFM task checkboxes at the start of a list item. Active when \
                 Rich markdown colouring is on.",
                &mut config.editor.md_color_task_boxes,
                &def.editor.md_color_task_boxes,
            );
            changed |= grid_bool(
                ui,
                q,
                "markdown colour table pipes cell separators",
                "  • Table pipes |",
                "Colour the | cell separators in table rows. Active when Rich \
                 markdown colouring is on.",
                &mut config.editor.md_color_table_pipes,
                &def.editor.md_color_table_pipes,
            );
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
            "scroll speed",
            "animate jump scrolls",
            "middle click autoscroll",
            "autoscroll sensitivity",
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
                "change bar",
                "Change bar",
                "Mark edited lines in the gutter: amber for unsaved edits, green \
                 once saved, none for untouched lines (Notepad++ style).",
                &mut config.editor.show_change_bar,
                &def.editor.show_change_bar,
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
            changed |= grid_bool(
                ui,
                q,
                "snippets tab trigger expand prefix",
                "Tab-trigger snippets",
                "Expand a snippet when Tab is pressed right after a known prefix \
                 from snippets.toml in the config folder. Applies to the in-house \
                 editor.",
                &mut config.editor.snippets_enabled,
                &def.editor.snippets_enabled,
            );
            changed |= grid_bool(
                ui,
                q,
                "current line highlight caret row band",
                "Highlight current line",
                "Draw a faint band across the line the caret is on.",
                &mut config.editor.current_line_highlight,
                &def.editor.current_line_highlight,
            );
            changed |= grid_bool(
                ui,
                q,
                "indent guides vertical lines",
                "Indent guides",
                "Draw faint vertical guide lines at each indent level.",
                &mut config.editor.indent_guides,
                &def.editor.indent_guides,
            );
            changed |= grid_bool(
                ui,
                q,
                "bracket match highlight pair",
                "Bracket-match highlight",
                "Box the bracket next to the caret and its matching partner.",
                &mut config.editor.bracket_match,
                &def.editor.bracket_match,
            );
            changed |= grid_bool(
                ui,
                q,
                "highlight selection occurrences",
                "Highlight occurrences",
                "When text is selected, box every other matching run in view.",
                &mut config.editor.highlight_selection_occurrences,
                &def.editor.highlight_selection_occurrences,
            );
            changed |= grid_bool(
                ui,
                q,
                "trailing whitespace highlight",
                "Trailing whitespace",
                "Tint trailing spaces/tabs on each line (distinct from \
                 \"render whitespace\", which shows all whitespace).",
                &mut config.editor.highlight_trailing_whitespace,
                &def.editor.highlight_trailing_whitespace,
            );
            changed |= grid_bool(
                ui,
                q,
                "smooth scroll wheel easing",
                "Smooth scrolling",
                "Ease wheel scrolling. Turn off for snappier, discrete-notch scrolling.",
                &mut config.editor.smooth_scroll,
                &def.editor.smooth_scroll,
            );
            if row_visible(q, "caret style cursor shape bar block underline") {
                use scribe_core::config::CaretStyle;
                let styles = [
                    (CaretStyle::Bar, "bar"),
                    (CaretStyle::Block, "block"),
                    (CaretStyle::Underline, "underline"),
                ];
                ui.label("Caret style")
                    .on_hover_text("Shape of the text caret: thin bar, full block, or underline.");
                egui::ComboBox::from_id_salt("caret-style")
                    .selected_text(
                        styles
                            .iter()
                            .find(|(s, _)| *s == config.editor.caret_style)
                            .map(|(_, l)| *l)
                            .unwrap_or("bar"),
                    )
                    .show_ui(ui, |ui| {
                        for (style, label) in styles {
                            changed |= ui
                                .selectable_value(&mut config.editor.caret_style, style, label)
                                .changed();
                        }
                    });
                changed |=
                    reset_to_default(ui, &mut config.editor.caret_style, &def.editor.caret_style);
                ui.end_row();
            }
            if row_visible(q, "caret width cursor thickness") {
                ui.label("Caret width")
                    .on_hover_text("Caret thickness for the bar/underline styles (points).");
                changed |= ui
                    .add(egui::Slider::new(&mut config.editor.caret_width, 1.0..=4.0))
                    .changed();
                changed |=
                    reset_to_default(ui, &mut config.editor.caret_width, &def.editor.caret_width);
                ui.end_row();
            }
            if row_visible(q, "scrollbar style chrome auto thin hidden") {
                use scribe_core::config::ScrollbarStyle;
                let styles = [
                    (ScrollbarStyle::Auto, "auto"),
                    (ScrollbarStyle::Thin, "thin"),
                    (ScrollbarStyle::Hidden, "hidden"),
                ];
                ui.label("Scrollbar style")
                    .on_hover_text("Editor scrollbar chrome: default, a slim bar, or hidden.");
                egui::ComboBox::from_id_salt("scrollbar-style")
                    .selected_text(
                        styles
                            .iter()
                            .find(|(s, _)| *s == config.editor.scrollbar_style)
                            .map(|(_, l)| *l)
                            .unwrap_or("auto"),
                    )
                    .show_ui(ui, |ui| {
                        for (style, label) in styles {
                            changed |= ui
                                .selectable_value(&mut config.editor.scrollbar_style, style, label)
                                .changed();
                        }
                    });
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.scrollbar_style,
                    &def.editor.scrollbar_style,
                );
                ui.end_row();
            }
        });
        ui.add_space(6.0);

        // -- Scroll --
        group(
            ui,
            "Scroll",
            "Mouse-wheel speed, jump-scroll animation, and middle-click autoscroll.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-editor-scroll", |ui| {
            if row_visible(q, "scroll speed") {
                ui.label("Scroll speed").on_hover_text(
                    "Mouse-wheel scrolling speed. egui's default (40) feels slow next to \
                     Windows; 75 is the SCR1B3 default.",
                );
                changed |= ui
                    .add(egui::Slider::new(&mut config.scroll.speed, 10.0..=200.0))
                    .changed();
                changed |= reset_to_default(ui, &mut config.scroll.speed, &def.scroll.speed);
                ui.end_row();
            }
            changed |= grid_bool(
                ui,
                q,
                "animate jump scrolls",
                "Animate jump scrolls",
                "Ease programmatic jumps (go-to-line, find-next) instead of snapping instantly.",
                &mut config.scroll.animate_jumps,
                &def.scroll.animate_jumps,
            );
            changed |= grid_bool(
                ui,
                q,
                "middle click autoscroll",
                "Middle-click autoscroll",
                "Click the mouse wheel, then move the pointer away from the click point to \
                 scroll continuously (Windows-style). Any click exits.",
                &mut config.scroll.autoscroll,
                &def.scroll.autoscroll,
            );
            if row_visible(q, "autoscroll sensitivity") {
                ui.label("Autoscroll sensitivity").on_hover_text(
                    "How fast middle-click autoscroll drifts per pixel of pointer offset \
                     from the click point.",
                );
                changed |= ui
                    .add(egui::Slider::new(
                        &mut config.scroll.autoscroll_sensitivity,
                        2.0..=15.0,
                    ))
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.scroll.autoscroll_sensitivity,
                    &def.scroll.autoscroll_sensitivity,
                );
                ui.end_row();
            }
            if row_visible(q, "autoscroll dead zone deadzone") {
                ui.label("Autoscroll dead zone").on_hover_text(
                    "Radius (px) around the middle-click origin where the pointer produces NO \
                     scrolling — a still zone so small jitters don't drift the page.",
                );
                changed |= ui
                    .add(egui::Slider::new(
                        &mut config.scroll.autoscroll_dead_zone,
                        4.0..=40.0,
                    ))
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.scroll.autoscroll_dead_zone,
                    &def.scroll.autoscroll_dead_zone,
                );
                ui.end_row();
            }
            changed |= grid_bool(
                ui,
                q,
                "drag select autoscroll wheel edge",
                "Drag-select autoscroll",
                "While selecting with the left button held, roll the wheel — or hold the \
                 pointer near the top/bottom edge — to scroll and keep extending the \
                 selection past the visible area.",
                &mut config.scroll.drag_autoscroll,
                &def.scroll.drag_autoscroll,
            );
            changed |= grid_bool(
                ui,
                q,
                "scroll past end beyond last line",
                "Scroll past end",
                "Allow scrolling a little beyond the last line so it can sit at a \
                 comfortable height instead of pinned to the bottom.",
                &mut config.scroll.scroll_past_end,
                &def.scroll.scroll_past_end,
            );
            if row_visible(q, "caret scroll off surrounding lines scrolloff") {
                ui.label("Caret scroll-off").on_hover_text(
                    "Keep the caret at least this many lines from the top/bottom edge when \
                     navigating by keyboard (Vim scrolloff). 0 disables.",
                );
                changed |= ui
                    .add(egui::Slider::new(
                        &mut config.scroll.caret_scroll_off,
                        0..=12,
                    ))
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.scroll.caret_scroll_off,
                    &def.scroll.caret_scroll_off,
                );
                ui.end_row();
            }
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
                            let prev = config.editor.tab_bar_position;
                            if ui
                                .selectable_value(&mut config.editor.tab_bar_position, pos, label)
                                .changed()
                            {
                                changed = true;
                                // Switching TO a Left/Right bar from a non-side one
                                // defaults "rotate side tabs" ON (the requested
                                // default for vertical bars); the user can turn it
                                // back off. No effect Top/Bottom.
                                if pos.is_vertical() && !prev.is_vertical() {
                                    config.editor.side_tabs_rotated = true;
                                }
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
            if row_visible(
                q,
                "wrap note title two lines side bar left right multi-line",
            ) {
                // Effective ONLY when the tab bar is on the Left/Right AND in
                // HORIZONTAL orientation (rotated OFF): a title too long for one
                // line then wraps to a 2nd line (max two; the 2nd truncates with
                // an ellipsis if even two don't fit). Greyed otherwise so the
                // double dependency (side bar + not rotated) is obvious.
                let is_horizontal_side = config.editor.tab_bar_position.is_vertical()
                    && !config.editor.side_tabs_rotated;
                ui.add_enabled_ui(is_horizontal_side, |ui| {
                    changed |= ui
                        .checkbox(
                            &mut config.editor.side_tabs_wrap_two_lines,
                            "Wrap note titles to 2 lines (side bar)",
                        )
                        .on_hover_text(
                            "When the tab bar is on the Left or Right with horizontal labels: \
                             ON lets a title that doesn't fit on one line wrap onto a second \
                             line (max two lines; the second is truncated with … if the title \
                             is longer still). OFF keeps each side-bar title on a single line, \
                             truncated with … when the bar is narrower than the title. No \
                             effect for Top/Bottom or the rotated (vertical-text) side variant.",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.side_tabs_wrap_two_lines,
                    &def.editor.side_tabs_wrap_two_lines,
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
                "restore session reopen saved files tabs",
                "Reopen saved files from last session",
                "Reopens the SAVED files/tabs you had open when you last closed SCR1B3. \
                 (Distinct from 'Restore unsaved notes' below, which recovers never-saved \
                 buffers — the two are independent and you can use either, both, or neither.)",
                &mut config.editor.restore_session,
                &def.editor.restore_session,
            );
            changed |= grid_bool(
                ui,
                q,
                "session backup hot exit unsaved restore crash recovery",
                "Restore unsaved notes after restart",
                "Keeps a backup of UNSAVED buffers (including never-saved scratch notes) so \
                 they come back after a restart or crash — no save needed. Backups live in \
                 the config 'backup' folder and are deleted once you save. (Distinct from \
                 'Reopen saved files' above.) On by default.",
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
            if row_visible(q, "default save format markdown plain text extension") {
                use scribe_core::config::DefaultSaveFormat;
                ui.label("Default save format").on_hover_text(
                    "The file type a brand-new note suggests in the Save dialog. Markdown \
                     (.md) by default — you can still pick any name or extension when saving.",
                );
                egui::ComboBox::from_id_salt("default-save-format")
                    .selected_text(config.integration.default_save_format.ui_label())
                    .show_ui(ui, |ui| {
                        for fmt in DefaultSaveFormat::ALL {
                            changed |= ui
                                .selectable_value(
                                    &mut config.integration.default_save_format,
                                    fmt,
                                    fmt.ui_label(),
                                )
                                .changed();
                        }
                    });
                changed |= reset_to_default(
                    ui,
                    &mut config.integration.default_save_format,
                    &def.integration.default_save_format,
                );
                ui.end_row();
            }
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
            if row_visible(q, "motion animation speed intensity ui transition") {
                ui.label("UI transition speed").on_hover_text(
                    "Scale how long the editor's CHROME transitions take — hover fades, panel \
                     and collapsible expand/collapse, combobox/menu fades, and value-change \
                     lerps. 0 makes every transition instant; 1 is egui's full transition time \
                     and 2 is double that. This does NOT control the retro visual effects \
                     (flicker / VHS / mesh) — those have their own per-effect speed sliders below.",
                );
                changed |= ui
                    .add_enabled(
                        on,
                        egui::Slider::new(&mut config.motion.intensity, 0.0..=2.0),
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
            if row_visible(q, "crt scanlines retro motion effect") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.motion.crt_scanlines, "CRT scanlines")
                        .on_hover_text(
                            "Draw subtle drifting horizontal scanlines over the editor for a \
                             retro CRT look (a calm animated post-effect).",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.crt_scanlines,
                    &def.motion.crt_scanlines,
                );
                ui.end_row();
            }
            if row_visible(q, "scanline darkness strength") {
                ui.label("Scanline darkness").on_hover_text(
                    "How dark the CRT scanlines are — 0 is invisible, 1 is strong dark bands.",
                );
                changed |= ui
                    .add_enabled(
                        on && config.motion.crt_scanlines,
                        egui::Slider::new(&mut config.motion.scanline_darkness, 0.0..=1.0),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.scanline_darkness,
                    &def.motion.scanline_darkness,
                );
                ui.end_row();
            }
            if row_visible(q, "wired node mesh ambient background motion") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(
                            &mut config.motion.wired_ambient,
                            "Wired node-mesh background",
                        )
                        .on_hover_text(
                            "Draw an animated node-mesh ambient background behind the editor.",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.wired_ambient,
                    &def.motion.wired_ambient,
                );
                ui.end_row();
            }
            if row_visible(q, "node mesh density motion") {
                ui.label("Mesh density").on_hover_text(
                    "How many nodes the wired-mesh background draws (sparse to dense). \
                     Higher values scale the node count with the window size.",
                );
                changed |= ui
                    .add_enabled(
                        on && config.motion.wired_ambient,
                        egui::Slider::new(&mut config.motion.mesh_density, 0.0..=2.0),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.mesh_density,
                    &def.motion.mesh_density,
                );
                ui.end_row();
            }
            if row_visible(q, "node mesh brightness motion") {
                ui.label("Mesh brightness").on_hover_text(
                    "How bright the wired-mesh lines and nodes are. 1 is the standard look; \
                     lower dims the lattice toward invisible, higher makes it pop.",
                );
                changed |= ui
                    .add_enabled(
                        on && config.motion.wired_ambient,
                        egui::Slider::new(&mut config.motion.mesh_brightness, 0.0..=3.0),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.mesh_brightness,
                    &def.motion.mesh_brightness,
                );
                ui.end_row();
            }
            if row_visible(q, "mesh drift speed motion") {
                ui.label("Mesh drift speed").on_hover_text(
                    "How fast the wired-mesh nodes drift. 1 is the standard rate; lower is a \
                     slower, calmer breathe and higher makes the lattice shift faster.",
                );
                changed |= ui
                    .add_enabled(
                        on && config.motion.wired_ambient,
                        egui::Slider::new(&mut config.motion.mesh_drift_speed, 0.25..=3.0),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.mesh_drift_speed,
                    &def.motion.mesh_drift_speed,
                );
                ui.end_row();
            }
            if row_visible(q, "node mesh colour color theme motion") {
                ui.label("Mesh colour").on_hover_text(
                    "Colour of the wired-mesh nodes and links. By default it follows the \
                     active theme's accent; pick a colour to pin it, then use 'Reset to \
                     theme' to go back to following the theme.",
                );
                ui.add_enabled_ui(on && config.motion.wired_ambient, |ui| {
                    ui.horizontal(|ui| {
                        // Seed the picker with the active theme's accent so the
                        // "follow theme" state shows the colour it will actually
                        // paint. Resolving the built-in by name is best-effort — a
                        // user TOML theme (not a built-in) falls back to the
                        // shipped default accent seed.
                        let theme_accent =
                            scribe_core::theme::Theme::builtin(&config.appearance.theme)
                                .map(|t| {
                                    let [r, g, b, _] = t
                                        .ui(
                                            "accent",
                                            scribe_core::theme::Rgba::new(0x00, 0xe5, 0xff, 255),
                                        )
                                        .to_array();
                                    egui::Color32::from_rgb(r, g, b)
                                })
                                .unwrap_or(egui::Color32::from_rgb(0x00, 0xe5, 0xff));
                        let mut col = config
                            .motion
                            .mesh_color
                            .map(|[r, g, b]| egui::Color32::from_rgb(r, g, b))
                            .unwrap_or(theme_accent);
                        if ui.color_edit_button_srgba(&mut col).changed() {
                            config.motion.mesh_color = Some([col.r(), col.g(), col.b()]);
                            changed = true;
                        }
                        // The reset appears only once the mesh colour has been
                        // pinned away from the theme (mirrors the App-background
                        // "Follow theme" affordance above).
                        if config.motion.mesh_color.is_some()
                            && ui
                                .small_button("Reset to theme")
                                .on_hover_text(
                                    "Clear the custom colour; follow the theme's accent.",
                                )
                                .clicked()
                        {
                            config.motion.mesh_color = None;
                            changed = true;
                        }
                    });
                });
                ui.end_row();
            }
            if row_visible(q, "vhs tracking lines motion effect") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.motion.vhs_tracking, "VHS tracking lines")
                        .on_hover_text(
                            "Faint bright bands sweep down the window like analogue tape tracking.",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.vhs_tracking,
                    &def.motion.vhs_tracking,
                );
                ui.end_row();
            }
            if row_visible(q, "vhs drift speed tracking motion") {
                ui.label("VHS drift speed").on_hover_text(
                    "How fast the VHS tracking bands sweep down the window. 1 is the standard \
                     rate; lower drifts more slowly and higher sweeps faster.",
                );
                changed |= ui
                    .add_enabled(
                        on && config.motion.vhs_tracking,
                        egui::Slider::new(&mut config.motion.vhs_speed, 0.25..=3.0),
                    )
                    .changed();
                changed |=
                    reset_to_default(ui, &mut config.motion.vhs_speed, &def.motion.vhs_speed);
                ui.end_row();
            }
            if row_visible(q, "screen flicker motion effect") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.motion.flicker, "Screen flicker")
                        .on_hover_text("Subtle CRT-style brightness flicker over the whole window.")
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(ui, &mut config.motion.flicker, &def.motion.flicker);
                ui.end_row();
            }
            if row_visible(q, "flicker strength motion") {
                ui.label("Flicker strength")
                    .on_hover_text("How strong the screen flicker is (capped low for comfort).");
                changed |= ui
                    .add_enabled(
                        on && config.motion.flicker,
                        egui::Slider::new(&mut config.motion.flicker_strength, 0.0..=0.20),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.flicker_strength,
                    &def.motion.flicker_strength,
                );
                ui.end_row();
            }
            if row_visible(q, "flicker speed cadence motion") {
                ui.label("Flicker speed").on_hover_text(
                    "How fast the screen flicker pulses. 1 is the standard cadence; lower is a \
                     slower shimmer and higher flickers faster. Independent of the strength.",
                );
                changed |= ui
                    .add_enabled(
                        on && config.motion.flicker,
                        egui::Slider::new(&mut config.motion.flicker_speed, 0.25..=3.0),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.flicker_speed,
                    &def.motion.flicker_speed,
                );
                ui.end_row();
            }
            if row_visible(q, "caret cursor trail motion effect") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.motion.caret_trail, "Caret ghost-trail")
                        .on_hover_text("A fading echo follows the caret as it moves.")
                        .changed();
                });
                ui.label("");
                changed |=
                    reset_to_default(ui, &mut config.motion.caret_trail, &def.motion.caret_trail);
                ui.end_row();
            }
            if row_visible(q, "caret cursor trail intensity motion effect") {
                ui.label("Caret-trail intensity").on_hover_text(
                    "How far the caret ghost-trail reaches — from a faint short flick to a \
                     bold, long comet tail.",
                );
                changed |= ui
                    .add_enabled(
                        on && config.motion.caret_trail,
                        egui::Slider::new(&mut config.motion.caret_trail_intensity, 0.0..=2.0),
                    )
                    .changed();
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.caret_trail_intensity,
                    &def.motion.caret_trail_intensity,
                );
                ui.end_row();
            }
            if row_visible(q, "boot glitch startup motion effect") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.motion.boot_glitch, "Boot glitch")
                        .on_hover_text(
                            "A one-shot glitch sweep plays for a moment when the app launches.",
                        )
                        .changed();
                });
                ui.label("");
                changed |=
                    reset_to_default(ui, &mut config.motion.boot_glitch, &def.motion.boot_glitch);
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
            "Transparency",
            "Make the window see-through (the desktop shows behind it).",
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Reveal the desktop through the window. Use the opacity slider for how \
                 see-through it is and the tint to colour it — changes apply immediately.",
            )
            .weak()
            .small(),
        );
        ui.add_space(2.0);
        settings_grid(ui, "settings-window-glass", |ui| {
            // Single on/off switch — off by default (opaque is fast).
            changed |= grid_bool(
                ui,
                q,
                "transparency enable window see-through desktop",
                "Enable window transparency",
                "Make the window see-through so the desktop shows behind it. Use the opacity \
                 slider for how see-through, and the tint below to colour it. Applies immediately.",
                &mut config.window.transparency_enabled,
                &def.window.transparency_enabled,
            );
            let tos = config.window.transparency_enabled;
            if row_visible(q, "window opacity transparent") {
                let translucent = tos;
                ui.label("Opacity").on_hover_text(
                    "How see-through the window is — 1.0 is fully opaque, lower is more \
                     transparent. Only active for translucent modes.",
                );
                changed |= ui
                    .add_enabled(
                        translucent,
                        // Floor at 0.0 for MAXIMUM transparency — the chrome/panel
                        // fills fully vanish; the editor text is painted opaque on
                        // top so it stays legible even at zero.
                        egui::Slider::new(&mut config.window.opacity, 0.0..=1.0),
                    )
                    .changed();
                changed |= reset_to_default(ui, &mut config.window.opacity, &def.window.opacity);
                ui.end_row();
            }
            changed |= grid_bool(
                ui,
                q,
                "window tint enable colour on off toggle",
                "Enable window tint",
                "Blend the tint colour into the app window background. Turn off to \
                 remove the tint without losing your colour + strength. The tint \
                 only shows once Tint strength is above 0.",
                &mut config.window.tint_enabled,
                &def.window.tint_enabled,
            );
            // The tint sub-controls are gated by the tint MASTER toggle
            // (`tint_enabled`), NOT by transparency: the tint is colour-math on
            // the panel/editor background fills and applies in BOTH opaque and
            // translucent window modes (see `render_support::panel_fill`, which
            // blends the tint BEFORE the translucency alpha). Gating them on
            // `transparency_enabled` was the regression that left the Tint
            // colour + strength greyed-out and "not functioning" whenever
            // transparency was off — even though the tint would have worked.
            let tint_on = config.window.tint_enabled;
            if row_visible(q, "window tint colour hex") {
                ui.label("Tint").on_hover_text(
                    "Colour tint blended into the app window background, as a hex \
                     code (e.g. #1a1a2e). Works in both opaque and transparent \
                     window modes.",
                );
                ui.add_enabled_ui(tint_on, |ui| {
                    ui.horizontal(|ui| {
                        // Click the swatch → egui colour picker pop-out; the hex
                        // field stays for exact/paste entry. The two are kept in
                        // sync (picker writes the hex back).
                        let mut col = parse_hex_color(&config.window.tint)
                            .unwrap_or(egui::Color32::from_rgb(0x08, 0x06, 0x0d));
                        if ui
                            .color_edit_button_srgba(&mut col)
                            .on_hover_text("Pick the tint colour.")
                            .changed()
                        {
                            config.window.tint =
                                format!("#{:02x}{:02x}{:02x}", col.r(), col.g(), col.b());
                            changed = true;
                        }
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut config.window.tint)
                                    .desired_width(96.0),
                            )
                            .on_hover_text(
                                "Hex colour (e.g. #1a1a2e), or click the swatch to pick.",
                            )
                            .changed();
                    });
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
                        tint_on,
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
                    // NOTE: a manual check deliberately does NOT stamp
                    // `last_check_unix`. That field is the AUTO-mode interval
                    // throttle; letting a manual press write it used to suppress
                    // the on-launch Notify check for 24h, so the user could
                    // relaunch and never be told a release was out. Notify now
                    // checks every launch regardless of this field, and Auto's
                    // throttle should not be reset by a manual press.
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

    // ---- Privacy ----
    if section_visible(
        sel,
        q,
        "Privacy",
        &["privacy", "clear", "data", "recent", "session", "forget"],
    ) {
        head(
            ui,
            "Privacy",
            "SCR1B3 is telemetry-free — everything stays on your device and nothing about you \
             is sent. The only local state that records what you've worked on is the \
             recent-files list and the session-restore snapshot (which keeps unsaved buffers on \
             disk so they survive a restart). You can erase both here.",
        );
        if row_visible(
            q,
            "clear local data recent files session restore forget unsaved",
        ) {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "Recent files remembered: {}. Session restore keeps on-disk copies of \
                     unsaved buffers.",
                    config.editor.recent_files.len()
                ))
                .weak()
                .small(),
            );
            ui.add_space(4.0);
            let cleared_id = egui::Id::new("scr1b3_privacy_cleared");
            if ui
                .button("Clear local data")
                .on_hover_text(
                    "Erase the recent-files (MRU) list AND the session-restore snapshot, \
                     including the on-disk copies of any unsaved buffers. Open documents and \
                     SAVED files are NOT touched; your settings and themes are kept.",
                )
                .clicked()
            {
                config.editor.recent_files.clear();
                let removed = scribe_core::Config::config_dir()
                    .map(|dir| scribe_core::session::clear_session_state(&dir))
                    .unwrap_or(0);
                changed = true; // persist the emptied recent-files list
                ui.ctx().data_mut(|d| d.insert_temp(cleared_id, removed));
            }
            if let Some(removed) = ui.ctx().data(|d| d.get_temp::<usize>(cleared_id)) {
                ui.label(
                    egui::RichText::new(format!(
                        "Cleared — removed {removed} session file(s) and emptied the recent-files \
                         list."
                    ))
                    .small()
                    .weak(),
                );
            }
        }

        // ---- Opt-in crash & issue reporting (W1TN3SS) ----
        // Two INDEPENDENT streams, each default OFF, each with its own consent
        // posture — never bundled under one toggle. The copy is deliberately
        // consent-framed: "a report you choose to send", never beacon/telemetry/
        // always-on/tracking language.
        group(
            ui,
            "Crash & issue reporting (opt-in)",
            "Off by default. Nothing is ever sent without your say-so. Each report is captured \
             on your device first; you see and can edit the exact text before it leaves, and \
             you choose whether to send it.",
        );
        if row_visible(
            q,
            "crash report reporting opt-in send error panic diagnostics",
        ) {
            ui.add_space(2.0);
            ui.label(egui::RichText::new("Crash reports").strong());
            ui.label(
                egui::RichText::new(
                    "When SCR1B3 closes unexpectedly, capture a short technical report (the \
                     error message and where in our code it happened) so it can be fixed.",
                )
                .weak()
                .small(),
            );
            changed |=
                reporting_mode_selector(ui, "reporting-crash", &mut config.reporting.crash_reports);
            ui.add_space(8.0);
        }
        if row_visible(
            q,
            "manual issue feedback reporting opt-in send report problem",
        ) {
            ui.label(egui::RichText::new("Manual issue reports").strong());
            ui.label(
                egui::RichText::new(
                    "When you choose to report a problem yourself, this controls whether that \
                     report may be sent. You always write and review it first.",
                )
                .weak()
                .small(),
            );
            changed |= reporting_mode_selector(
                ui,
                "reporting-manual",
                &mut config.reporting.manual_issues,
            );
            ui.add_space(8.0);
        }
        if row_visible(q, "what we never collect privacy explainer reporting") {
            // The "what we never collect" panel — the single highest-trust
            // artifact a privacy-first app ships (privacy-consent.md §3).
            egui::CollapsingHeader::new("What a report never contains")
                .id_salt("scr1b3_reporting_never_collect")
                .default_open(true)
                .show(ui, |ui| {
                    for line in [
                        "Your documents, notes, or any file contents — never included.",
                        "Your file paths or folder names — stripped before you ever see the report.",
                        "Your username, computer name, or home-directory path — removed.",
                        "Any device, install, or tracking ID — there is none; reports are not \
                         linkable to you or to each other.",
                        "Your IP address is never stored by us beyond the moment of upload.",
                    ] {
                        ui.label(egui::RichText::new(format!("• {line}")).small());
                    }
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(
                            "A report carries only a short, sanitized error message, where in \
                             OUR code it happened, and your OS + app version — and only if you \
                             send it.",
                        )
                        .weak()
                        .small(),
                    );
                });
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

    // ---- Default app (file-type associations) ----
    if section_visible(
        sel,
        q,
        "Default app",
        &[
            "default",
            "file type",
            "association",
            "open with",
            "handler",
        ],
    ) {
        head(
            ui,
            "Default app",
            "Open text & code files in SCR1B3 by default.",
        );
        group(
            ui,
            "File types",
            "Choose which kinds of file SCR1B3 should handle.",
        );

        // Checklist bound to the persisted claim set (empty ⇒ all selected).
        let mut selected = config.integration.claimed_types();
        for ct in ClaimType::ALL {
            let mut on = selected.contains(&ct);
            if ui.checkbox(&mut on, ct.label()).changed() {
                if on {
                    if !selected.contains(&ct) {
                        selected.push(ct);
                    }
                } else {
                    selected.retain(|c| *c != ct);
                }
                // Persist the EXPLICIT selection (resolved order) so a later load
                // reflects exactly what the user picked.
                config.integration.claimed_types = ClaimType::ALL
                    .into_iter()
                    .filter(|c| selected.contains(c))
                    .map(|c| c.key().to_string())
                    .collect();
                changed = true;
            }
        }
        space(ui);

        // Honest per-OS copy: on Windows the app can register + deep-link, but
        // only the user can confirm the default in the system UI.
        let os_note = if cfg!(windows) {
            "Windows requires you to confirm the choice in its Settings window — \
             SCR1B3 will open it for you. (No app can change the default for you.)"
        } else if cfg!(target_os = "macos") {
            "SCR1B3 will be registered for these types; macOS asks you to confirm \
             the default once in Finder (Get Info ▸ Open With ▸ Change All)."
        } else {
            "SCR1B3 will be set as the default for these file types."
        };
        ui.label(egui::RichText::new(os_note).weak().small());

        // A registration already running? (present handle ⇒ show a spinner, keep
        // the button disabled so a second click can't spawn a duplicate worker.)
        let pending: Option<RegShared> = ui
            .ctx()
            .data(|d| d.get_temp::<RegShared>(register_pending_id()));
        let enabled = !selected.is_empty() && pending.is_none();
        let btn = ui.add_enabled(
            enabled,
            egui::Button::new(if cfg!(windows) {
                "Register SCR1B3 & open Default Apps…"
            } else {
                "Set SCR1B3 as the default"
            }),
        );
        if btn.clicked() {
            config.integration.register_file_types = true;
            config.integration.last_registration_unix = Some(crate::app::now_unix());
            changed = true;
            // Run registration OFF the UI thread — it spawns several `reg.exe`
            // processes and would otherwise FREEZE the window for a few seconds
            // with no feedback ("nothing seems to be happening"). The worker
            // writes its result into a shared slot and wakes the UI.
            let shared: RegShared = std::sync::Arc::default();
            let sink = shared.clone();
            let types = selected.clone();
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let report = crate::integration::register(&types);
                if let Ok(mut slot) = sink.lock() {
                    *slot = Some(report);
                }
                ctx.request_repaint(); // wake the UI to pick up the result
            });
            ui.ctx().data_mut(|d| {
                d.insert_temp(register_pending_id(), shared);
                d.remove::<String>(default_app_status_id()); // clear a stale status
            });
        }

        // Poll the in-flight registration: show a spinner while it runs, then
        // stash its result message and drop the handle when it finishes.
        if let Some(shared) = pending {
            let done = shared.lock().ok().and_then(|mut slot| slot.take());
            if let Some(report) = done {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(default_app_status_id(), report.message.clone());
                    d.remove::<RegShared>(register_pending_id());
                });
            } else {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new("Registering… opening the Windows Default Apps page")
                            .small(),
                    );
                });
                ui.ctx().request_repaint(); // keep polling until the worker finishes
            }
        }

        // Surface the most-recent completed attempt's status.
        if let Some(msg) = ui
            .ctx()
            .data(|d| d.get_temp::<String>(default_app_status_id()))
        {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(msg).small());
        }
        space(ui);
    }

    changed
}

/// Drag-and-drop payload for the toolbar editor (Phase 18 T18.5b).
/// `Reorder(i)` means "move existing item at index i". `AddAction(id)`
/// means "append a new action from the palette".
#[derive(Clone, Debug)]
enum ToolbarDrag {
    /// Reorder within ONE list. `salt` scopes the drag to its editor so a drag
    /// started in the quick-access list can't be dropped into the dropdown-menu
    /// list (and vice versa) — both editors share the `ToolbarDrag` payload type,
    /// so the salt is what keeps their drops from cross-contaminating.
    Reorder { salt: &'static str, from: usize },
    /// Add a palette action into the `salt` list at the drop position.
    AddAction { salt: &'static str, id: String },
}

impl ToolbarDrag {
    fn salt(&self) -> &'static str {
        match self {
            ToolbarDrag::Reorder { salt, .. } | ToolbarDrag::AddAction { salt, .. } => salt,
        }
    }
}

/// Apply a queued keyboard ↑/↓ move (`dir` = -1 up, +1 down) to `list`. Clamps
/// the destination into range and swaps the two rows. Returns `true` if the list
/// actually changed (a no-op move at a clamped edge returns `false`). Pure +
/// unit-tested so the keyboard-reorder path of [`toolbar_list_editor`] is covered
/// without driving the full egui frame.
fn apply_keyboard_move(list: &mut [String], from: usize, dir: isize) -> bool {
    let n = list.len();
    if n == 0 || from >= n {
        return false;
    }
    let to = (from as isize + dir).clamp(0, n as isize - 1) as usize;
    if from != to {
        list.swap(from, to);
        return true;
    }
    false
}

/// Apply a queued toolbar drop to `list`. `target` is the insertion slot. A
/// reorder removes the source and re-inserts it (adjusting the target if the
/// removal shifted it); an add inserts the new id. Pure + unit-tested so the
/// shared drag behaviour of BOTH toolbar editors can't silently regress.
fn apply_toolbar_drop(list: &mut Vec<String>, target: usize, drag: ToolbarDrag) -> bool {
    match drag {
        ToolbarDrag::Reorder { from, .. } => {
            if from >= list.len() {
                return false;
            }
            let item = list.remove(from);
            let t = if from < target { target - 1 } else { target };
            list.insert(t.min(list.len()), item);
            true
        }
        ToolbarDrag::AddAction { id, .. } => {
            list.insert(target.min(list.len()), id);
            true
        }
    }
}

/// One toolbar-list editor used by BOTH the quick-access list and the
/// dropdown-menu list, so they look and behave identically (the prior divergence
/// — one drag-based, one click-based — was the "needs uniformity" report). Each
/// row is a grip + drag-to-reorder source with keyboard ↑/↓/✕; the palette below
/// is a 3-column grid of chips that DRAG onto the list (insert at position) or
/// CLICK to append. `salt` scopes the drag-and-drop to this list; `allow_sep`
/// includes the separator action (meaningful only for the quick-access bar).
/// Returns whether the list changed.
fn toolbar_list_editor(
    ui: &mut egui::Ui,
    list: &mut Vec<String>,
    salt: &'static str,
    allow_sep: bool,
) -> bool {
    use egui_phosphor::thin as ph;
    let mut changed = false;
    let mut mv: Option<(usize, isize)> = None;
    let mut rm: Option<usize> = None;
    let mut drop_actions: Vec<(usize, ToolbarDrag)> = Vec::new();
    let n = list.len();
    // The in-flight drag, but ONLY if it belongs to this list (so insertion
    // guides + drop zones stay scoped and don't react to the other editor).
    let dragged: Option<ToolbarDrag> = egui::DragAndDrop::payload::<ToolbarDrag>(ui.ctx())
        .map(|arc| (*arc).clone())
        .filter(|d| d.salt() == salt);

    if n == 0 {
        ui.label(
            egui::RichText::new("Empty — drag a chip from the palette, or click it to add.")
                .weak()
                .small(),
        );
    }
    for (i, item) in list.iter().enumerate() {
        let label = action_label(item);
        let drag_id = egui::Id::new((salt, "row-drag", i));
        ui.dnd_drag_source(drag_id, ToolbarDrag::Reorder { salt, from: i }, |ui| {
            ui.horizontal(|ui| {
                let grip_c = ui.visuals().weak_text_color();
                crate::app::grip_handle(ui, false, grip_c, false)
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
                if ui.button(ph::X).on_hover_text("Remove").clicked() {
                    rm = Some(i);
                }
                ui.label(label);
            });
        });
        let (_resp, dropped) = ui.dnd_drop_zone::<ToolbarDrag, _>(
            egui::Frame::default()
                .inner_margin(egui::Margin::symmetric(2, 1))
                .stroke(egui::Stroke::NONE),
            |ui| {
                if dragged.is_some() {
                    ui.add(egui::Separator::default().horizontal().spacing(1.0));
                } else {
                    ui.add_space(2.0);
                }
            },
        );
        if let Some(payload) = dropped {
            if payload.salt() == salt {
                drop_actions.push((i + 1, (*payload).clone()));
            }
        }
    }
    // Leading drop zone so a drag can land at index 0.
    let (_lead, lead_dropped) = ui.dnd_drop_zone::<ToolbarDrag, _>(
        egui::Frame::default()
            .inner_margin(egui::Margin::symmetric(2, 1))
            .stroke(egui::Stroke::NONE),
        |ui| {
            if dragged.is_some() {
                ui.label(egui::RichText::new("drop here for the top").weak().small());
            } else {
                ui.add_space(2.0);
            }
        },
    );
    if let Some(payload) = lead_dropped {
        if payload.salt() == salt {
            drop_actions.push((0, (*payload).clone()));
        }
    }

    if let Some((i, d)) = mv {
        changed |= apply_keyboard_move(list, i, d);
    }
    if let Some(i) = rm {
        list.remove(i);
        changed = true;
    }
    // Apply drops in reverse so earlier insertions don't shift later targets.
    for (target, drag) in drop_actions.into_iter().rev() {
        if apply_toolbar_drop(list, target, drag) {
            changed = true;
        }
    }

    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Palette (drag onto the list, or click to add)")
            .strong()
            .small(),
    );
    // 3-column GRID (bounded width, never `available_width` — see TB_W note) of
    // chips that are BOTH drag sources AND clickable. Click = append; drag =
    // insert at the drop position.
    egui::Grid::new((salt, "palette"))
        .num_columns(3)
        .spacing([6.0, 6.0])
        .show(ui, |ui| {
            let mut col = 0;
            for (id, plabel) in crate::app::TOOLBAR_ACTIONS {
                if !allow_sep && *id == "sep" {
                    continue;
                }
                let drag_id = egui::Id::new((salt, "palette-drag", *id));
                let resp = ui
                    .dnd_drag_source(
                        drag_id,
                        ToolbarDrag::AddAction {
                            salt,
                            id: (*id).to_string(),
                        },
                        |ui| {
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
                                let grip_c = ui.visuals().weak_text_color();
                                crate::app::grip_handle(ui, false, grip_c, false);
                                ui.label(*plabel);
                            });
                        },
                    )
                    .response
                    .on_hover_text("Drag onto the list above, or click, to add")
                    .on_hover_cursor(egui::CursorIcon::Grab);
                // Chips are draggable (Sense::drag) AND clickable: a press that
                // doesn't turn into a drag appends the action — the keyboard- and
                // click-friendly add path that replaces the old combobox.
                if resp.clicked() {
                    list.push((*id).to_string());
                    changed = true;
                }
                col += 1;
                if col % 3 == 0 {
                    ui.end_row();
                }
            }
        });
    changed
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
/// Fixed wrap width for the multi-line update-status messages (no-asset /
/// error). A cap well under the page width forces them to wrap to ~2-3 lines and
/// keeps the settings window from GROWING (egui auto-sizes window width to its
/// UN-wrapped content, so a long one-line message + an inline link would widen
/// the whole window — the exact bug this caps). The action link/button sits on
/// its own line beneath the text. Module-level so the layout invariant is
/// unit-testable.
const UPDATE_STATUS_MSG_WIDTH: f32 = 340.0;

fn render_update_status(ui: &mut egui::Ui, updater: &mut crate::updater::Updater) {
    use crate::updater::UpdateState;
    enum Act {
        // Boxed: `ReleaseInfo` now carries the signed-manifest pin + ordinal, so
        // it is the largest variant; box it to keep the enum small.
        Download(Box<scribe_core::update::ReleaseInfo>),
        Apply,
        RunInstaller,
        Recheck,
    }
    let mut act: Option<Act> = None;
    match &updater.state {
        UpdateState::Idle => {}
        UpdateState::Checking => {
            ui.spinner();
            ui.label("Checking…");
        }
        UpdateState::UpToDate { latest } => {
            // Show BOTH the running version AND the newest release found, so
            // "up to date" is never ambiguous (the user can see the check
            // actually reached GitHub and what the latest release is).
            ui.label(
                egui::RichText::new(format!(
                    "Up to date — you're on v{} (latest release: v{latest}).",
                    crate::updater::current_version()
                ))
                .weak(),
            );
        }
        UpdateState::NoAssetForPlatform {
            latest,
            target,
            html_url,
        } => {
            // A newer release exists but has no build for this platform — NEVER
            // silently report "up to date". Render in a FIXED-width column so the
            // message WRAPS to a few lines and "Open the releases page" sits on its
            // OWN line BELOW it. egui's Window auto-size measures content
            // UN-wrapped, so a one-line message + an inline link would force the
            // whole settings window wider (the trap render_toolbar_editor
            // documents). Capping the width + wrapping keeps the window stable.
            ui.vertical(|ui| {
                ui.set_max_width(UPDATE_STATUS_MSG_WIDTH);
                let warn = ui.visuals().warn_fg_color;
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "v{latest} is available, but there's no build for your platform \
                             ({target})."
                        ))
                        .color(warn),
                    )
                    .wrap(),
                );
                if ui
                    .link("Open the releases page")
                    .on_hover_text("Download the latest release manually from your browser.")
                    .clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(html_url.clone()));
                }
            });
        }
        UpdateState::Available(info) => {
            ui.label(format!("v{} is available.", info.version));
            if ui.button("Update now").clicked() {
                act = Some(Act::Download(Box::new(info.clone())));
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
        UpdateState::ReadyToRunInstaller { version, .. } => {
            ui.label(format!("v{version} downloaded + verified."));
            if ui
                .button("Install update (asks for admin)")
                .on_hover_text(
                    "SCR1B3 is installed in a protected location, so the verified \
                     installer updates it in place silently — no installer window, \
                     no extra clicks. Windows shows a single administrator prompt; \
                     then SCR1B3 closes and relaunches on the new version.",
                )
                .clicked()
            {
                act = Some(Act::RunInstaller);
            }
        }
        UpdateState::Applied { version } => {
            ui.label(format!("Updated to v{version} — restarting…"));
        }
        UpdateState::Failed(e) => {
            // Same fixed-width-wrap treatment as NoAssetForPlatform: a long error
            // string must wrap to a few lines (with Retry below) rather than widen
            // the settings window.
            ui.vertical(|ui| {
                ui.set_max_width(UPDATE_STATUS_MSG_WIDTH);
                let err = ui.visuals().error_fg_color;
                ui.add(
                    egui::Label::new(egui::RichText::new(format!("Update failed: {e}")).color(err))
                        .wrap(),
                );
                if ui.button("Retry").clicked() {
                    act = Some(Act::Recheck);
                }
            });
        }
    }
    match act {
        Some(Act::Download(info)) => updater.start_download(ui.ctx(), *info),
        Some(Act::Apply) => updater.apply_and_restart(ui.ctx()),
        Some(Act::RunInstaller) => updater.run_installer(ui.ctx()),
        Some(Act::Recheck) => updater.start_check(ui.ctx(), crate::updater::LaunchKind::Manual),
        None => {}
    }
}

fn render_toolbar_editor(ui: &mut egui::Ui, config: &mut Config) -> bool {
    let mut changed = false;
    // Cap the editor content to a FIXED width (NOT available_width). This is the
    // load-bearing line for "the Settings window doesn't widen on the Toolbar
    // page": binding to available_width creates a feedback loop (wide window →
    // wide available_width → long description labels + the search row fill it →
    // egui grows the window past its max_width because content min-size wins) —
    // that is exactly what made this page render ~240px wider than the others.
    // A fixed cap < the content-pane width makes the long labels wrap and lets
    // nothing demand more than the window already allows, so the window stays the
    // same width as every other page.
    // Bound the editor AND wrap the long description labels at a fixed width.
    // Each description is ~840px on one line; egui's Window auto-size measures
    // them UN-wrapped, and that is the page-specific width-greedy element that
    // made the Toolbar window wider than every other settings page (other pages'
    // descriptions are short). A fixed-width allocation forces them to wrap, so
    // the page's intrinsic width matches the rest.
    const TB_W: f32 = 560.0;
    ui.set_max_width(TB_W);
    ui.heading("Quick-access toolbar");
    ui.allocate_ui_with_layout(
        egui::vec2(TB_W, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
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
        },
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
    changed |= toolbar_list_editor(ui, &mut config.toolbar.items, "items", true);
    if ui
        .small_button("Reset toolbar to defaults")
        .on_hover_text("Restore the quick-access items, sizing, and menu to their defaults.")
        .clicked()
    {
        config.toolbar = ToolbarConfig::default();
        changed = true;
    }

    // ---- User-curated "more-actions" dropdown menu (same editor as Items) ----
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new("Dropdown (more-actions menu)")
            .strong()
            .small(),
    );
    ui.label(
        egui::RichText::new(
            "Actions parked in the toolbar's more-actions menu — reachable without taking a \
             toolbar slot, so the bar stays clean. Curated with the SAME controls as the items \
             above.",
        )
        .weak()
        .small(),
    );
    if ui
        .checkbox(
            &mut config.toolbar.show_dropdown,
            "Show the more-actions dropdown on the toolbar",
        )
        .on_hover_text(
            "When off, the overflow button is hidden even if actions are parked in it \
             (they stay reachable via the command palette).",
        )
        .changed()
    {
        changed = true;
    }
    changed |= toolbar_list_editor(ui, &mut config.toolbar.menu, "menu", false);
    if ui
        .small_button("Clear menu")
        .on_hover_text("Remove every action from the more-actions menu.")
        .clicked()
        && !config.toolbar.menu.is_empty()
    {
        config.toolbar.menu.clear();
        changed = true;
    }
    changed
}

#[cfg(test)]
mod toolbar_drop {
    use super::{apply_toolbar_drop, ToolbarDrag};

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn reorder_interior_and_in_place_kill_offset_and_boundary_mutants() {
        // The existing reorder tests all drop at a target that, AFTER the remove,
        // clamps to the list tail via `t.min(list.len())` — so every mutant on
        // `let t = if from < target { target - 1 } else { target };` produces the
        // same clamped output and survives. These two cases keep `t` INTERIOR
        // (unclamped) so the offset (`- -> +`/`/`) and boundary (`< -> <=`/`==`)
        // mutants each diverge.
        // (A) from<target, interior insert: 0<2 -> t=1 -> [b,a,c,d].
        //   `< -> ==`: t=target=2 -> [b,c,a,d]; `- -> +`: t=3 -> [b,c,d,a];
        //   `- -> /`: t=2 -> [b,c,a,d]. All differ from [b,a,c,d].
        let mut list = v(&["a", "b", "c", "d"]);
        assert!(apply_toolbar_drop(
            &mut list,
            2,
            ToolbarDrag::Reorder {
                salt: "items",
                from: 0
            }
        ));
        assert_eq!(list, v(&["b", "a", "c", "d"]));

        // (B) from == target: orig is a genuine no-op (t=target). The `< -> <=`
        //   mutant makes 1<=1 true -> t=target-1=0 -> [b,a,c].
        let mut list = v(&["a", "b", "c"]);
        assert!(apply_toolbar_drop(
            &mut list,
            1,
            ToolbarDrag::Reorder {
                salt: "items",
                from: 1
            }
        ));
        assert_eq!(list, v(&["a", "b", "c"]));
    }

    #[test]
    fn reorder_moves_item_to_target_slot() {
        // Move index 0 to slot 3 (drop AFTER the 2nd row): a,b,c -> b,c,a.
        let mut list = v(&["a", "b", "c"]);
        assert!(apply_toolbar_drop(
            &mut list,
            3,
            ToolbarDrag::Reorder {
                salt: "items",
                from: 0
            }
        ));
        assert_eq!(list, v(&["b", "c", "a"]));
    }

    #[test]
    fn reorder_to_top_and_no_op() {
        let mut list = v(&["a", "b", "c"]);
        // Drop the last at slot 0 → moves to top.
        apply_toolbar_drop(
            &mut list,
            0,
            ToolbarDrag::Reorder {
                salt: "items",
                from: 2,
            },
        );
        assert_eq!(list, v(&["c", "a", "b"]));
        // Out-of-range source is a safe no-op (never panics / never mutates).
        let mut short = v(&["a"]);
        assert!(!apply_toolbar_drop(
            &mut short,
            0,
            ToolbarDrag::Reorder {
                salt: "items",
                from: 9
            }
        ));
        assert_eq!(short, v(&["a"]));
    }

    #[test]
    fn add_inserts_at_target() {
        let mut list = v(&["a", "b"]);
        apply_toolbar_drop(
            &mut list,
            1,
            ToolbarDrag::AddAction {
                salt: "items",
                id: "x".to_string(),
            },
        );
        assert_eq!(list, v(&["a", "x", "b"]));
        // Target past the end clamps to append.
        apply_toolbar_drop(
            &mut list,
            999,
            ToolbarDrag::AddAction {
                salt: "menu",
                id: "z".to_string(),
            },
        );
        assert_eq!(list, v(&["a", "x", "b", "z"]));
    }

    #[test]
    fn salt_identifies_the_owning_list() {
        assert_eq!(
            ToolbarDrag::Reorder {
                salt: "menu",
                from: 0
            }
            .salt(),
            "menu"
        );
        assert_eq!(
            ToolbarDrag::AddAction {
                salt: "items",
                id: "a".into()
            }
            .salt(),
            "items"
        );
    }
}

/// The theme prev/next arrows.
///
/// Every mutant of this logic survived the first cargo-mutants sweep of
/// scribe-app, because it lived as a closure inside `render_sections` and
/// nothing in the suite clicks the arrows. Extracting it to a pure function is
/// what makes the assertions below possible at all; each test names the
/// surviving mutant it kills.
#[cfg(test)]
mod theme_step {
    use super::step_theme_index;

    const NAMES: [&str; 4] = ["alpha", "beta", "gamma", "delta"];

    /// Kills `+` → `-` and `+` → `*` at the `i as isize + delta` site.
    #[test]
    fn stepping_forward_moves_to_the_next_theme() {
        assert_eq!(step_theme_index(&NAMES, "alpha", 1), 1, "alpha -> beta");
        assert_eq!(step_theme_index(&NAMES, "beta", 1), 2, "beta -> gamma");
    }

    #[test]
    fn stepping_backward_moves_to_the_previous_theme() {
        assert_eq!(step_theme_index(&NAMES, "gamma", -1), 1, "gamma -> beta");
    }

    /// `rem_euclid` is the whole point: the arrows must never dead-end. A plain
    /// `%` would give -1 here, which would panic the caller's index.
    #[test]
    fn the_ends_wrap_in_both_directions() {
        assert_eq!(
            step_theme_index(&NAMES, "alpha", -1),
            3,
            "backward off the front wraps to the last theme, never underflows"
        );
        assert_eq!(
            step_theme_index(&NAMES, "delta", 1),
            0,
            "forward off the end wraps to the first theme"
        );
    }

    /// Kills the `delta > 0` guard mutants — BOTH `true` and `false` survived,
    /// which is what a wholly-undriven branch looks like. A user theme is not in
    /// the built-in list, so it has no position to step from; the direction of
    /// travel decides where it lands.
    #[test]
    fn a_user_theme_lands_on_the_end_it_is_travelling_toward() {
        assert_eq!(
            step_theme_index(&NAMES, "my-custom-theme", 1),
            0,
            "forward from a non-built-in lands on the FIRST built-in"
        );
        assert_eq!(
            step_theme_index(&NAMES, "my-custom-theme", -1),
            3,
            "backward from a non-built-in lands on the LAST built-in — this is \
             the arm the `delta > 0` guard selects, and replacing that guard \
             with either true or false must not survive"
        );
    }

    /// Kills `delta > 0` → `delta >= 0`: the guard is STRICTLY forward. Only a
    /// positive step lands a non-built-in on the FIRST entry; a zero step (no
    /// movement — outside the ±1 the arrows ever pass, but a valid input to this
    /// pure function) is not "forward", so it falls to the same last-entry arm as
    /// a backward step. Weakening `>` to `>=` would divert the zero case to the
    /// first entry, which this pins against.
    #[test]
    fn a_zero_step_from_a_user_theme_is_not_forward() {
        assert_eq!(
            step_theme_index(&NAMES, "my-custom-theme", 0),
            3,
            "a zero step is not strictly forward, so a non-built-in lands on the              LAST built-in — `delta > 0` must not weaken to `delta >= 0`"
        );
    }

    /// Kills `n - 1` → `n + 1` / `n / 1`: a wrong landing index here would be
    /// out of bounds and panic the caller.
    #[test]
    fn the_backward_landing_spot_is_in_bounds() {
        let i = step_theme_index(&NAMES, "not-a-builtin", -1);
        assert!(
            i < NAMES.len(),
            "the returned index is used to index `names` directly, so it must be \
             in bounds; got {i} for a list of {}",
            NAMES.len()
        );
    }

    /// Kills `==` → `!=` in the `position` predicate: matching the WRONG theme
    /// would step from an unrelated entry.
    #[test]
    fn the_current_theme_is_matched_exactly() {
        // With `!=`, `position` returns the first name that ISN'T "alpha" —
        // index 0 would become index 1 and every step would be off by one.
        assert_eq!(
            step_theme_index(&NAMES, "alpha", 1),
            1,
            "stepping from alpha must start at alpha's own index"
        );
        assert_eq!(
            step_theme_index(&NAMES, "delta", -1),
            2,
            "stepping from the LAST theme must start at its index, not the first \
             non-match"
        );
    }

    #[test]
    fn a_single_theme_list_stays_put_in_both_directions() {
        let one = ["only"];
        assert_eq!(step_theme_index(&one, "only", 1), 0);
        assert_eq!(step_theme_index(&one, "only", -1), 0);
    }

    /// The real built-in list must work, not just the synthetic one above.
    #[test]
    fn the_real_builtin_list_round_trips() {
        let names = scribe_core::theme::Theme::builtin_names();
        assert!(!names.is_empty(), "there must be built-in themes to step");
        let first = names[0];
        let back = step_theme_index(names, first, -1);
        assert_eq!(back, names.len() - 1, "first steps back to last");
        assert_eq!(
            step_theme_index(names, names[back], 1),
            0,
            "and forward again returns to the first"
        );
    }
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

    /// A TOO-LONG all-hex string must be rejected, not silently truncated.
    ///
    /// Every case above is too SHORT or not hex at all, and that asymmetry hid a
    /// real hole: the `|| h.len() != 6` guard could be broken to `&&` and the
    /// whole suite stayed green. With `&&`, `"aabbccdd"` stops being rejected
    /// and parses as `aabbcc` — an 8-digit `#rrggbbaa` silently loses its alpha
    /// and yields a colour the user never chose, rather than falling back to the
    /// default. The short cases cannot catch that: they fail later anyway when
    /// `comp()` runs off the end of the string, so they pass either way.
    ///
    /// Found by cargo-mutants (`settings.rs:69 replace || with &&`, MISSED).
    #[test]
    fn rejects_too_long_even_when_every_digit_is_valid_hex() {
        assert_eq!(
            parse_hex_color("aabbccdd"),
            None,
            "an 8-digit hex must be rejected, not truncated to its first 6 digits"
        );
        assert_eq!(
            parse_hex_color("#aabbccdd"),
            None,
            "the `#` prefix must not change the length verdict"
        );
        assert_eq!(
            parse_hex_color("1122334"),
            None,
            "one digit too many is still too many"
        );
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
    use super::{
        open_plugin_manager_id, request_category, settings_cat_id, take_open_plugin_manager_request,
    };

    #[test]
    fn take_open_plugin_manager_request_consumes_a_pending_flag_once() {
        // The Plugins section sets a temp bool to ask the host to open the plugin
        // manager. The host accessor must return true ONCE (and clear the flag),
        // then false on the next call — a latch, not a level (so the manager
        // doesn't re-open every frame).
        let ctx = egui::Context::default();
        // Absent flag → false (the empty/no-request path).
        assert!(
            !take_open_plugin_manager_request(&ctx),
            "no pending request must read as false"
        );
        // Simulate the Plugins section raising the request.
        ctx.data_mut(|d| d.insert_temp(open_plugin_manager_id(), true));
        assert!(
            take_open_plugin_manager_request(&ctx),
            "a pending request must read as true once"
        );
        assert!(
            !take_open_plugin_manager_request(&ctx),
            "the flag must be cleared after one read (one-shot latch)"
        );
    }

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
            "motion.flicker_strength" => src.contains("clamped_flicker_strength"),
            "motion.mesh_density" => src.contains("clamped_mesh_density"),
            "motion.mesh_brightness" => {
                src.contains("mesh_link_alpha") || src.contains("mesh_dot_alpha")
            }
            "motion.flicker_speed" => src.contains("clamped_flicker_speed"),
            "motion.vhs_speed" => src.contains("clamped_vhs_speed"),
            "motion.mesh_drift_speed" => src.contains("clamped_mesh_drift_speed"),
            "motion.mesh_color" => src.contains("resolved_mesh_color"),
            "ui_scale" => src.contains("effective_ui_scale"),
            "editor.caret_width" => src.contains("clamped_caret_width"),
            _ => false,
        }
    }

    /// Every Settings-exposed config field that MUST have a runtime consumer.
    const WIRED: &[&str] = &[
        "appearance.theme",
        "appearance.frameless",
        "appearance.toolbar_in_titlebar",
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
        "editor.show_change_bar",
        "editor.word_wrap",
        "editor.show_minimap",
        "editor.render_whitespace",
        "editor.snippets_enabled",
        "editor.current_line_highlight",
        "editor.indent_guides",
        "editor.bracket_match",
        "editor.highlight_selection_occurrences",
        "editor.highlight_trailing_whitespace",
        "editor.smooth_scroll",
        "editor.caret_style",
        "editor.caret_width",
        "editor.scrollbar_style",
        "editor.tab_bar_position",
        "editor.side_tabs_rotated",
        "editor.side_tabs_wrap_two_lines",
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
        "motion.crt_scanlines",
        "motion.scanline_darkness",
        "motion.wired_ambient",
        "motion.mesh_density",
        "motion.mesh_brightness",
        "motion.vhs_tracking",
        "motion.vhs_speed",
        "motion.flicker",
        "motion.flicker_strength",
        "motion.flicker_speed",
        "motion.mesh_drift_speed",
        "motion.mesh_color",
        "motion.caret_trail",
        "motion.caret_trail_intensity",
        "motion.boot_glitch",
        "ui_scale",
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

    /// Runtime source EXCLUDING the config module (and the settings UI), for the
    /// section-level guard below.
    ///
    /// `runtime_source` deliberately keeps `scribe-core/src/config/**` in scope —
    /// the dotted `section.field` probes it uses never appear in the struct
    /// definitions, so their presence still proves a real consumer. A SECTION
    /// probe (`.keybindings`) is not so lucky: it matches the field declaration
    /// in `config/mod.rs`, which would make an entirely unread section look
    /// consumed. Dropping the config module is what makes the guard honest.
    fn runtime_source_outside_config() -> String {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let mut out = String::new();
        for dir in [
            format!("{manifest}/src"),
            format!("{manifest}/../scribe-core/src"),
        ] {
            collect_outside_config(Path::new(&dir), &mut out);
        }
        out
    }

    fn collect_outside_config(dir: &Path, out: &mut String) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                // The config module DEFINES the sections; it cannot be evidence
                // that anything CONSUMES them.
                if p.file_name().is_some_and(|n| n == "config") {
                    continue;
                }
                collect_outside_config(&p, out);
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

    /// Every top-level key a user can write in `scr1b3.toml`, each of which MUST
    /// be read by runtime code outside the config module.
    ///
    /// `schema_version` is deliberately absent: it is migration metadata, and its
    /// only legitimate consumer (`Config::migrate`) lives inside the config module
    /// by design.
    const WIRED_SECTIONS: &[&str] = &[
        "editor",
        "appearance",
        "fonts",
        "window",
        "updates",
        "spellcheck",
        "plugins",
        "toolbar",
        "motion",
        "scroll",
        "reporting",
        "integration",
        "keybindings",
        "ui_scale",
    ];

    /// The section-level companion to `every_wired_setting_has_a_runtime_consumer`.
    ///
    /// That guard only audits controls exposed in the Settings WINDOW, so a config
    /// surface with no Settings UI is invisible to it. `[keybindings]` shipped that
    /// way: 35 rebindable actions that parsed, validated, and were read by nothing,
    /// while every Settings-exposed control was correctly wired and the guard stayed
    /// green. This checks the other axis — a whole section that nothing consumes.
    #[test]
    fn every_config_section_has_a_runtime_consumer() {
        let src = runtime_source_outside_config();
        for &section in WIRED_SECTIONS {
            // A section is consumed if its field access appears, or — for one read
            // only through a config METHOD — if that method does. `ui_scale` is
            // reached via `Config::effective_ui_scale` (the clamp/NaN guard), so
            // the literal `.ui_scale` never appears at the call site.
            let consumed = src.contains(&format!(".{section}"))
                || match section {
                    "ui_scale" => src.contains("effective_ui_scale"),
                    _ => false,
                };
            assert!(
                consumed,
                "DEAD CONFIG SECTION: `[{section}]` can be written in scr1b3.toml but no \
                 runtime code outside the config module reads it — it is a false promise",
            );
        }
    }
}

/// Visual-QA for the update-status pane. We cannot "see" the rendered UI, so we
/// drive it through the `egui_kittest` harness and assert the layout invariants
/// the user reported against — via AccessKit node *rects* (geometry), never
/// pixel sampling (unreliable on wgpu). The regression: a long status message
/// rendered on one line with the action inline to its right, widening the whole
/// settings window. The fix wraps the message in a width-capped column with the
/// action on its own line below.
#[cfg(test)]
mod update_status_layout {
    use super::{render_update_status, UPDATE_STATUS_MSG_WIDTH};
    use crate::updater::{UpdateState, Updater};
    use egui_kittest::kittest::Queryable as _;

    // A generous margin over the cap for widget padding/frame insets.
    const MARGIN: f32 = 16.0;

    #[test]
    fn no_asset_status_wraps_and_keeps_link_below_within_width() {
        let latest = "0.9.9";
        let target = "x86_64-pc-windows-msvc";
        let msg =
            format!("v{latest} is available, but there's no build for your platform ({target}).");
        let mut h = egui_kittest::Harness::builder()
            // Wide canvas on purpose: if the layout did NOT cap its width, the
            // message would render on one long line and the link would sit far to
            // the right (the bug). The cap must hold regardless of canvas width.
            .with_size(egui::Vec2::new(1280.0, 400.0))
            .build_ui(move |ui| {
                let mut updater = Updater::default();
                updater.state = UpdateState::NoAssetForPlatform {
                    latest: latest.to_string(),
                    target: target.to_string(),
                    html_url: "https://github.com/o/r/releases".to_string(),
                };
                render_update_status(ui, &mut updater);
            });
        h.run();

        let message = h.get_by_label(msg.as_str()).rect();
        let link = h.get_by_label("Open the releases page").rect();

        // 1. The message wrapped: its right edge stays within the cap (+ margin),
        //    so this pane cannot force the settings window wider.
        assert!(
            message.right() <= UPDATE_STATUS_MSG_WIDTH + MARGIN,
            "no-asset message not width-capped: right={} cap={UPDATE_STATUS_MSG_WIDTH}",
            message.right(),
        );
        // 2. The link sits on its OWN line BELOW the message (not inline to its
        //    right — the layout that extended the window before the fix).
        assert!(
            link.top() >= message.bottom() - 2.0,
            "'Open the releases page' is not below the message: link.top={} message.bottom={}",
            link.top(),
            message.bottom(),
        );
        // 3. The link itself stays within the capped column too.
        assert!(
            link.right() <= UPDATE_STATUS_MSG_WIDTH + MARGIN,
            "link extends past the width cap: right={} cap={UPDATE_STATUS_MSG_WIDTH}",
            link.right(),
        );
    }

    #[test]
    fn failed_status_wraps_long_error_and_keeps_retry_within_width() {
        let err = "update check failed: a very long error message that would, without wrapping, \
                   stretch the settings window far beyond its intended width and push everything \
                   sideways — which is exactly the regression this guards against";
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(1280.0, 400.0))
            .build_ui(move |ui| {
                let mut updater = Updater::default();
                updater.state = UpdateState::Failed(err.to_string());
                render_update_status(ui, &mut updater);
            });
        h.run();

        let message = h
            .get_by_label(format!("Update failed: {err}").as_str())
            .rect();
        let retry = h.get_by_label("Retry").rect();
        // The wrapped error stays within the cap, and Retry sits below it inside
        // the same width-bounded column — an error string can't widen the window.
        assert!(
            message.right() <= UPDATE_STATUS_MSG_WIDTH + MARGIN,
            "failed message not width-capped: right={} cap={UPDATE_STATUS_MSG_WIDTH}",
            message.right(),
        );
        assert!(
            retry.top() >= message.bottom() - 2.0,
            "Retry is not below the error message: retry.top={} message.bottom={}",
            retry.top(),
            message.bottom(),
        );
        assert!(
            retry.right() <= UPDATE_STATUS_MSG_WIDTH + MARGIN,
            "Retry extends past the width cap: right={} cap={UPDATE_STATUS_MSG_WIDTH}",
            retry.right(),
        );
    }
}

#[cfg(test)]
mod pure_helpers {
    //! Pure (no-egui-frame) logic lifted out of the render glue so each branch
    //! is asserted directly, not merely executed. Keyboard reorder, label
    //! mapping, search visibility, consent-mode labels, and the hex colour
    //! round-trip the colour pickers depend on.
    use super::{
        action_label, apply_keyboard_move, parse_hex_color, reporting_mode_label, row_visible,
        section_visible,
    };
    use scribe_core::ReportingMode;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn keyboard_move_down_swaps_with_next() {
        let mut list = v(&["a", "b", "c"]);
        assert!(apply_keyboard_move(&mut list, 0, 1));
        assert_eq!(list, v(&["b", "a", "c"]));
    }

    #[test]
    fn keyboard_move_up_swaps_with_previous() {
        let mut list = v(&["a", "b", "c"]);
        assert!(apply_keyboard_move(&mut list, 2, -1));
        assert_eq!(list, v(&["a", "c", "b"]));
    }

    #[test]
    fn keyboard_move_clamps_at_edges_as_noop() {
        // Up from the top and down from the bottom clamp to the same slot → no
        // change, no panic, returns false.
        let mut list = v(&["a", "b", "c"]);
        assert!(!apply_keyboard_move(&mut list, 0, -1));
        assert_eq!(list, v(&["a", "b", "c"]));
        assert!(!apply_keyboard_move(&mut list, 2, 1));
        assert_eq!(list, v(&["a", "b", "c"]));
    }

    #[test]
    fn keyboard_move_out_of_range_or_empty_is_safe_noop() {
        let mut empty: Vec<String> = Vec::new();
        assert!(!apply_keyboard_move(&mut empty, 0, 1));
        let mut one = v(&["only"]);
        assert!(!apply_keyboard_move(&mut one, 9, -1)); // from past the end
        assert_eq!(one, v(&["only"]));
    }

    #[test]
    fn keyboard_move_large_dir_clamps_to_far_edge() {
        // A big positive delta clamps to the last slot (swaps from→last).
        let mut list = v(&["a", "b", "c", "d"]);
        assert!(apply_keyboard_move(&mut list, 0, 99));
        assert_eq!(list, v(&["d", "b", "c", "a"]));
    }

    #[test]
    fn action_label_maps_separator_and_known_action() {
        assert_eq!(action_label("sep"), "— separator —");
        // A real toolbar action id resolves to its human label (not the raw id).
        let (id, label) = crate::app::TOOLBAR_ACTIONS[0];
        assert_eq!(action_label(id), label);
    }

    #[test]
    fn action_label_falls_back_to_id_for_unknown() {
        assert_eq!(action_label("not-a-real-action-id"), "not-a-real-action-id");
    }

    #[test]
    fn section_visible_uses_selection_when_no_query() {
        assert!(section_visible("Editor", "", "Editor", &["tab width"]));
        assert!(!section_visible("Editor", "", "Fonts", &["editor size"]));
    }

    #[test]
    fn section_visible_matches_category_or_label_when_searching() {
        // Query matches the category name itself.
        assert!(section_visible("Fonts", "edit", "Editor", &["tab width"]));
        // Query matches a child label even though the category differs.
        assert!(section_visible(
            "Fonts",
            "tab width",
            "Editor",
            &["tab width"]
        ));
        // Query that matches neither category nor any label → hidden.
        assert!(!section_visible("Fonts", "zzz", "Editor", &["tab width"]));
    }

    #[test]
    fn row_visible_matches_lowercased_query_against_lowercased_label() {
        // The caller (`show`) lowercases the query once; `row_visible` then
        // lowercases the LABEL and does a substring test. An empty query shows
        // every row.
        assert!(row_visible("", "Anything")); // empty query shows all
        assert!(row_visible("theme", "Active THEME picker")); // label case-folded
        assert!(row_visible("theme", "Active theme picker"));
        assert!(!row_visible("missing", "Active theme picker"));
        // The query is assumed pre-lowercased (matches `show()`'s contract): a
        // non-lowercased query will NOT match a lowercase label substring.
        assert!(!row_visible("THEME", "theme"));
    }

    #[test]
    fn reporting_mode_labels_are_consent_language() {
        assert_eq!(reporting_mode_label(ReportingMode::Off), "Never (off)");
        assert_eq!(
            reporting_mode_label(ReportingMode::AskEachTime),
            "Ask each time"
        );
        assert_eq!(reporting_mode_label(ReportingMode::Always), "Always send");
        // No surveillance/telemetry copy leaks into the user-facing labels.
        for m in [
            ReportingMode::Off,
            ReportingMode::AskEachTime,
            ReportingMode::Always,
        ] {
            let l = reporting_mode_label(m).to_lowercase();
            assert!(!l.contains("telemetry") && !l.contains("surveillance"));
        }
    }

    #[test]
    fn hex_color_round_trips_through_the_picker_format() {
        // The colour pickers store `#rrggbb` via `format!("#{:02x}{:02x}{:02x}")`
        // then re-parse via `parse_hex_color`. Pin that round-trip so a future
        // format/parse drift can't silently corrupt a saved colour.
        for (r, g, b) in [(0u8, 0u8, 0u8), (0x0d, 0x0b, 0x14), (255, 16, 64)] {
            let s = format!("#{r:02x}{g:02x}{b:02x}");
            let parsed = parse_hex_color(&s).expect("round-trip parse");
            assert_eq!((parsed.r(), parsed.g(), parsed.b()), (r, g, b), "for {s}");
        }
    }
}

#[cfg(test)]
mod pane_render {
    //! Drive `render_sections` for EVERY settings category through the real
    //! egui frame (via `egui_kittest`), plus the search-filter and a couple of
    //! interactive flows (toggle a checkbox, click a per-setting ↺ reset). This
    //! exercises the ~1.7k-line per-pane render glue that pure-fn tests can't
    //! reach, asserting against AccessKit-visible labels — never pixels.
    use super::{render_sections, CATEGORIES};
    use crate::updater::Updater;
    use egui_kittest::kittest::Queryable as _;
    use scribe_core::Config;

    /// Render one category pane and return the harness so the caller can query
    /// AccessKit nodes. `query` is the search filter (empty = the whole pane).
    fn render_category(category: &str, query: &str) -> egui_kittest::Harness<'static> {
        let category = category.to_string();
        let query = query.to_string();
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(900.0, 1400.0))
            .build_ui(move |ui| {
                let mut cfg = Config::default();
                let mut updater = Updater::default();
                render_sections(ui, &mut cfg, &mut updater, &category, &query);
            });
        h.run();
        h
    }

    #[test]
    fn every_category_renders_its_heading_without_panicking() {
        // Map each nav category to the heading `render_sections` emits for it.
        let expected: &[(&str, &str)] = &[
            ("Appearance", "Appearance"),
            ("Fonts", "Fonts"),
            ("Window", "Window"),
            ("Toolbar", "Quick-access toolbar"),
            ("Motion", "Motion"),
            ("Editor", "Editor"),
            ("Spellcheck", "Spellcheck (offline)"),
            ("Plugins", "Plugins"),
            ("Updates", "Updates"),
            ("Privacy", "Privacy"),
            ("Default app", "Default app"),
        ];
        // Guard: the table must stay in lockstep with the real nav list, so a
        // newly-added category can't silently skip its render-coverage here.
        assert_eq!(
            expected.len(),
            CATEGORIES.len(),
            "pane_render category table drifted from CATEGORIES"
        );
        for (nav, heading) in expected {
            let h = render_category(nav, "");
            // The heading is rendered as a top-of-pane label; finding it proves
            // the pane's render path executed end-to-end for this category.
            assert!(
                h.query_by_label(heading).is_some(),
                "category `{nav}` did not render its `{heading}` heading",
            );
        }
    }

    #[test]
    fn search_filter_shows_cross_category_matches_and_hides_others() {
        // Searching "theme" from the (irrelevant) Updates tab still surfaces the
        // Appearance "Theme" control — the cross-category search behaviour.
        let h = render_category("Updates", "theme");
        assert!(
            h.query_by_label("Theme").is_some(),
            "search for 'theme' should surface the Appearance Theme control across categories",
        );
    }

    #[test]
    fn empty_search_on_a_category_shows_that_categorys_rows() {
        // The Editor pane has a "Tab width" control; with no query and Editor
        // selected it must be present.
        let h = render_category("Editor", "");
        assert!(
            h.query_by_label("Tab width").is_some(),
            "Editor pane should render its Tab width control",
        );
    }

    #[test]
    fn nonmatching_search_renders_no_rows_but_does_not_panic() {
        // A query that matches no category and no label renders an (empty) pane;
        // the render path must still execute cleanly.
        let h = render_category("Appearance", "zzz-nothing-matches-this");
        assert!(
            h.query_by_label("Theme").is_none(),
            "a non-matching search must hide the Theme row",
        );
    }

    #[test]
    fn toggling_a_checkbox_reports_changed_and_flips_config() {
        // Drive the Editor pane, click the "Line numbers" checkbox, and assert
        // both the returned `changed` flag (in the frame the click lands) and the
        // mutated config field — proof the grid_bool→config wiring is live.
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(900.0, 1400.0))
            .build_ui_state(
                |ui, state: &mut (Config, Updater, bool)| {
                    let (cfg, updater, changed) = state;
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        // OR-accumulate so the changed=true frame isn't lost if a
                        // later no-op frame runs before we read the flag.
                        *changed |= render_sections(ui, cfg, updater, "Editor", "");
                    });
                },
                (Config::default(), Updater::default(), false),
            );
        h.run();
        let before = h.state().0.editor.show_line_numbers;
        // Reset the accumulator so we only observe the change caused by the click.
        h.state_mut().2 = false;
        h.get_by_label("Line numbers").click();
        h.run();
        let (cfg, _u, changed) = h.state();
        assert_ne!(
            cfg.editor.show_line_numbers, before,
            "clicking the checkbox must flip the config field",
        );
        assert!(
            *changed,
            "a toggled checkbox must make render_sections report changed=true",
        );
    }
}

#[cfg(test)]
mod update_status_states {
    //! Render `render_update_status` for EACH non-trivial `UpdateState` and
    //! assert the user-facing status label for that state. We only RENDER each
    //! state (no button clicks) — the action arms (`Update now` / `Restart` /
    //! `Install` / `Retry`) spawn network/thread work, so those `clicked()`
    //! bodies stay intentionally uncovered; the label-emission lines for every
    //! state get covered. The wrapped no-asset / failed layouts are covered by
    //! the sibling `update_status_layout` module.
    use super::render_update_status;
    use crate::updater::{current_version, UpdateState, Updater};
    use egui_kittest::kittest::Queryable as _;
    use scribe_core::update::ReleaseInfo;
    use std::path::PathBuf;

    fn release(version: &str) -> ReleaseInfo {
        ReleaseInfo {
            version: semver::Version::parse(version).unwrap(),
            tag: format!("v{version}"),
            asset_url: "https://example.invalid/a.tar.gz".to_string(),
            sig_url: "https://example.invalid/a.tar.gz.minisig".to_string(),
            sha_url: "https://example.invalid/a.tar.gz.sha256".to_string(),
            html_url: "https://example.invalid/releases/tag".to_string(),
            pinned_sha256: "deadbeef".to_string(),
            release_index: Some(4_044),
            installer: None,
        }
    }

    /// Render one update state and return the harness for label queries.
    fn render_state(state: UpdateState) -> egui_kittest::Harness<'static> {
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(600.0, 300.0))
            .build_ui(move |ui| {
                let mut updater = Updater::default();
                updater.state = state.clone();
                render_update_status(ui, &mut updater);
            });
        h.run();
        h
    }

    #[test]
    fn checking_shows_a_spinner_label() {
        // The spinner requests a repaint every frame, so `run()` (which loops to
        // a steady state) would exceed max_steps. A single step is enough to
        // render — and assert — the "Checking…" label beside the spinner.
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(600.0, 300.0))
            .build_ui(|ui| {
                let mut updater = Updater::default();
                updater.state = UpdateState::Checking;
                render_update_status(ui, &mut updater);
            });
        h.run_steps(1);
        assert!(h.query_by_label("Checking…").is_some());
    }

    #[test]
    fn up_to_date_shows_current_and_latest_versions() {
        let h = render_state(UpdateState::UpToDate {
            latest: "9.9.9".to_string(),
        });
        let expected = format!(
            "Up to date — you're on v{} (latest release: v9.9.9).",
            current_version()
        );
        assert!(
            h.query_by_label(expected.as_str()).is_some(),
            "up-to-date label must name both the running version and the latest release",
        );
    }

    #[test]
    fn available_shows_version_with_update_and_changelog_affordances() {
        let h = render_state(UpdateState::Available(release("1.2.3")));
        assert!(h.query_by_label("v1.2.3 is available.").is_some());
        // The action affordances render (we do NOT click them — that spawns a
        // download); their presence proves the Available arm rendered fully.
        assert!(h.query_by_label("Update now").is_some());
        assert!(h.query_by_label("changelog").is_some());
    }

    #[test]
    fn downloading_renders_a_progress_bar_without_panicking() {
        // The progress bar has no text label; rendering it (incl. the >0 total
        // fraction branch) exercises the Downloading arm. A zero total takes the
        // 0.0 fallback branch — render that too.
        let _h = render_state(UpdateState::Downloading {
            received: 50,
            total: 100,
        });
        let _h0 = render_state(UpdateState::Downloading {
            received: 0,
            total: 0,
        });
    }

    #[test]
    fn ready_to_apply_shows_restart_affordance() {
        let h = render_state(UpdateState::ReadyToApply {
            staged: PathBuf::from("/srv/x/staged-binary"),
            version: "2.0.0".to_string(),
        });
        assert!(h.query_by_label("v2.0.0 downloaded + verified.").is_some());
        assert!(h.query_by_label("Restart to finish update").is_some());
    }

    #[test]
    fn ready_to_run_installer_shows_install_affordance() {
        let h = render_state(UpdateState::ReadyToRunInstaller {
            installer: PathBuf::from("/srv/x/setup.exe"),
            version: "2.0.0".to_string(),
        });
        assert!(h.query_by_label("v2.0.0 downloaded + verified.").is_some());
        assert!(h
            .query_by_label("Install update (asks for admin)")
            .is_some());
    }

    #[test]
    fn applied_shows_restarting_message() {
        let h = render_state(UpdateState::Applied {
            version: "3.1.4".to_string(),
        });
        assert!(h
            .query_by_label("Updated to v3.1.4 — restarting…")
            .is_some());
    }

    #[test]
    fn idle_renders_nothing_but_does_not_panic() {
        // The Idle arm is an empty match body; rendering it must be a clean no-op.
        let h = render_state(UpdateState::Idle);
        assert!(h.query_by_label("Checking…").is_none());
    }
}
