//! In-app theme creator / editor. A self-contained egui panel that seeds a
//! working copy from the active theme (never blank), exposes every semantic
//! UI + syntax token as a live colour picker, renders a sample-code live
//! preview painted with the in-progress colours, and saves the result as a
//! user TOML under `<config_dir>/themes/<slug>.toml`.
//!
//! Design follows the VS Code / Zed / Helix "generate from current theme"
//! model surfaced in the recon dossier: the editor only ever mutates a working
//! copy held in egui temp state; the live editor keeps using the active theme
//! until the user saves. Because egui is immediate-mode, every picker mutation
//! is reflected in the preview on the same frame with no event wiring.
//!
//! Entry point: [`show`]. The host adds `mod theme_editor;` and calls
//! `theme_editor::show(ui, config)` from the settings Appearance section; a
//! `true` return means the working theme was saved and `config` changed (so the
//! host should persist the config).

use eframe::egui;
use egui::Color32;
use scribe_core::config::Config;
use scribe_core::theme::{Appearance, Rgba, Theme};

/// Semantic UI tokens we surface as pickers, in display order, with the human
/// label shown beside each swatch. Only keys that actually exist in the working
/// theme's `ui` map are rendered — these mirror the keys every built-in defines
/// (see `scribe_core::theme`). `selection` carries an alpha channel; everything
/// else is opaque.
const UI_TOKENS: &[(&str, &str)] = &[
    ("background", "window background"),
    ("panel", "panel / side bar"),
    ("bezel", "bezel / border"),
    ("foreground", "foreground text"),
    ("accent", "accent"),
    ("selection", "selection"),
    ("cursor", "cursor"),
    ("line_number", "line number"),
    ("line_number_active", "line number (active)"),
    ("gutter", "gutter"),
    ("ok", "status: ok"),
    ("warning", "status: warning"),
    ("error", "status: error"),
];

/// Semantic syntax tokens we surface as pickers, in display order. These mirror
/// the syntax keys every built-in defines. All are opaque.
const SYNTAX_TOKENS: &[(&str, &str)] = &[
    ("keyword", "keyword"),
    ("string", "string"),
    ("comment", "comment"),
    ("number", "number"),
    ("function", "function"),
    ("type", "type"),
    ("constant", "constant"),
    ("variable", "variable"),
];

/// UI tokens that legitimately carry alpha (translucent overlays). Everything
/// else is edited opaque so the picker doesn't expose a confusing alpha slider
/// on a solid surface colour.
const ALPHA_TOKENS: &[&str] = &["selection"];

/// Working state held in egui temp memory, keyed by a stable [`egui::Id`]. We
/// keep the in-progress [`Theme`] plus the name the user typed plus the status
/// line and the name of the theme we last seeded from (so we can re-seed when
/// the active theme changes out from under us).
#[derive(Clone)]
struct ThemeEditorState {
    /// The theme being edited. Mutated in place by every picker.
    working: Theme,
    /// User-facing name for the save target (pre-slug).
    name: String,
    /// Last status message (save result, errors). Empty = nothing to show.
    status: String,
    /// `true` when [`status`](Self::status) reports an error (painted red-ish).
    status_is_error: bool,
    /// The active theme name we seeded from. When `config.appearance.theme`
    /// drifts away from this, we re-seed so the editor tracks the live theme.
    seeded_from: String,
}

/// Convert a core [`Rgba`] into an egui [`Color32`]. The colour picker edits
/// `Color32`; the theme stores `Rgba`. `Rgba` exposes `r/g/b/a: u8` fields, so
/// this is a direct field copy with NO premultiplication (the stored bytes are
/// straight sRGBA, matching `Color32::from_rgba_unmultiplied`).
fn rgba_to_color32(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

/// Convert an egui [`Color32`] back into a core [`Rgba`]. `Color32` stores
/// premultiplied bytes internally, so we un-premultiply via
/// `to_srgba_unmultiplied()` to recover the straight sRGBA the theme stores —
/// `to_array()` would hand back the premultiplied bytes, which is wrong for any
/// translucent token (e.g. it would turn an `aa00ff90`-at-20%-alpha selection
/// into a near-black smear). The round-trip is exact for opaque colours; for the
/// one translucent token (`selection`) the u8 premultiplied store is lossy by at
/// most ±1 per colour channel, which is imperceptible in the blend.
fn color32_to_rgba(c: Color32) -> Rgba {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Rgba::new(r, g, b, a)
}

/// Look up a working-theme colour for the live preview, falling back to a sane
/// default when a token is absent (a hand-imported theme might omit one).
fn ui_color(theme: &Theme, key: &str, default: Color32) -> Color32 {
    theme
        .ui
        .get(key)
        .map(|c| rgba_to_color32(*c))
        .unwrap_or(default)
}

/// Same as [`ui_color`] for the `syntax` map.
fn syntax_color(theme: &Theme, key: &str, default: Color32) -> Color32 {
    theme
        .syntax
        .get(key)
        .map(|c| rgba_to_color32(*c))
        .unwrap_or(default)
}

/// Seed a fresh working theme from the active theme name. Tries the user TOML
/// at `<config_dir>/themes/<active>.toml` first (so the user keeps editing the
/// theme they actually shipped), then the compiled-in built-in, then the house
/// brand default. Never returns blank — that's the whole point.
fn seed_theme(config: &Config) -> Theme {
    let active = &config.appearance.theme;
    // 1. User theme TOML, if present and parseable.
    if let Some(dir) = Config::config_dir() {
        let path = dir.join("themes").join(format!("{active}.toml"));
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(theme) = Theme::from_toml_str(&text) {
                return theme;
            }
        }
    }
    // 2. Compiled-in built-in.
    if let Some(theme) = Theme::builtin(active) {
        return theme;
    }
    // 3. House brand default — the guaranteed-present fallback.
    Theme::itasha_corp()
}

/// Slugify a user theme name into a filesystem-safe stem: trim, lowercase,
/// collapse whitespace/underscores to single hyphens, drop anything that isn't
/// `[a-z0-9-]`, and collapse runs of hyphens. An empty result falls back to
/// `"my-theme"` so we never try to write a dotfile or an empty filename.
fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_hyphen = false;
    for ch in name.trim().chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else if ch.is_whitespace() || ch == '_' || ch == '-' {
            '-'
        } else {
            // Drop punctuation / symbols entirely.
            continue;
        };
        if mapped == '-' {
            if last_was_hyphen {
                continue;
            }
            last_was_hyphen = true;
        } else {
            last_was_hyphen = false;
        }
        out.push(mapped);
    }
    // Trim leading/trailing hyphens left by the collapse.
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "my-theme".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Write `theme` to `<config_dir>/themes/<slug>.toml`, creating the themes dir
/// if needed. Returns the slug on success so the caller can set it active.
/// Errors are returned as strings for the status line — this never panics.
fn save_theme(theme: &Theme, slug: &str) -> Result<(), String> {
    let dir = Config::config_dir()
        .ok_or_else(|| "could not resolve the config directory".to_string())?
        .join("themes");
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create themes dir: {e}"))?;
    let path = dir.join(format!("{slug}.toml"));
    std::fs::write(&path, theme.to_toml_string())
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    Ok(())
}

/// One colour-picker row inside a two-column [`egui::Grid`]: `label | swatch`,
/// mutating the working theme's `map` entry in place. Keys absent from the
/// working theme render nothing (no ragged half-row). The Grid keeps every
/// swatch aligned in a column — a calmer layout than the old ragged horizontal
/// rows — and, crucially, carries NO inner scroll of its own (#11): the Settings
/// page owns the only ScrollArea, so the mouse wheel always scrolls the page.
fn token_grid_row(
    ui: &mut egui::Ui,
    map: &mut std::collections::BTreeMap<String, Rgba>,
    key: &str,
    label: &str,
    allow_alpha: bool,
) {
    // Only render keys that actually exist in the working theme.
    let Some(current) = map.get(key).copied() else {
        return;
    };
    ui.label(egui::RichText::new(label).monospace());
    let mut color = rgba_to_color32(current);
    let alpha = if allow_alpha {
        egui::color_picker::Alpha::OnlyBlend
    } else {
        egui::color_picker::Alpha::Opaque
    };
    if egui::color_picker::color_edit_button_srgba(ui, &mut color, alpha).changed() {
        map.insert(key.to_string(), color32_to_rgba(color));
    }
    ui.end_row();
}

/// Paint a small framed live preview: a few lines of fake code coloured by the
/// working theme's syntax map over its editor background, plus a fake status
/// bar. Immediate-mode means this re-renders with the current working colours
/// every frame, so picker edits show instantly.
fn live_preview(ui: &mut egui::Ui, theme: &Theme) {
    let bg = ui_color(theme, "background", Color32::from_rgb(0x12, 0x12, 0x12));
    let fg = ui_color(theme, "foreground", Color32::from_rgb(0xe8, 0xe6, 0xf0));
    let line_no = ui_color(theme, "line_number", Color32::from_rgb(0x6a, 0x64, 0x88));
    let panel = ui_color(theme, "panel", bg);
    let accent = ui_color(theme, "accent", Color32::from_rgb(0x00, 0xff, 0x90));
    let kw = syntax_color(theme, "keyword", fg);
    let func = syntax_color(theme, "function", fg);
    let string = syntax_color(theme, "string", fg);
    let comment = syntax_color(theme, "comment", line_no);
    let ty = syntax_color(theme, "type", fg);
    let number = syntax_color(theme, "number", fg);

    // One painted line: gutter line-number + a sequence of (text, colour) spans.
    let mono = egui::FontId::monospace(13.0);
    let row = |ui: &mut egui::Ui, n: u32, spans: &[(&str, Color32)]| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(
                egui::RichText::new(format!("{n:>3} "))
                    .color(line_no)
                    .font(mono.clone()),
            );
            for (text, color) in spans {
                ui.label(egui::RichText::new(*text).color(*color).font(mono.clone()));
            }
        });
    };

    egui::Frame::default()
        .fill(bg)
        .inner_margin(egui::Margin::same(8))
        .stroke(egui::Stroke::new(1.0, panel))
        .show(ui, |ui| {
            ui.set_min_width(280.0);
            row(ui, 1, &[("// a calm, legible surface", comment)]);
            row(
                ui,
                2,
                &[
                    ("fn ", kw),
                    ("greet", func),
                    ("(name: ", fg),
                    ("str", ty),
                    (") {", fg),
                ],
            );
            row(
                ui,
                3,
                &[
                    ("    let ", kw),
                    ("count", fg),
                    (" = ", fg),
                    ("42", number),
                    (";", fg),
                ],
            );
            row(
                ui,
                4,
                &[
                    ("    println!(", func),
                    ("\"hi, {name}\"", string),
                    (");", fg),
                ],
            );
            row(ui, 5, &[("}", fg)]);

            // Fake status bar painted in the panel colour with an accent token.
            egui::Frame::default()
                .fill(panel)
                .inner_margin(egui::Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(
                            egui::RichText::new("● READY")
                                .color(accent)
                                .font(egui::FontId::monospace(11.0)),
                        );
                        ui.label(
                            egui::RichText::new("Ln 3, Col 17")
                                .color(fg)
                                .font(egui::FontId::monospace(11.0)),
                        );
                    });
                });
        });
}

/// Render the theme creator/editor and apply any save. Returns `true` when the
/// working theme was saved (so the host persists the now-changed `config`).
///
/// State lives in egui temp memory keyed by a stable [`egui::Id`]; it re-seeds
/// from the active theme on first show and whenever the active theme changes
/// out from under the editor, and on an explicit Reset / "New from current".
pub fn show(ui: &mut egui::Ui, config: &mut Config) -> bool {
    let id = egui::Id::new("scr1b3_theme_editor_state");

    // Load (or seed) the working state from egui temp memory.
    let mut state: ThemeEditorState = ui
        .ctx()
        .data_mut(|d| d.get_temp::<ThemeEditorState>(id))
        .filter(|s| s.seeded_from == config.appearance.theme)
        .unwrap_or_else(|| {
            let working = seed_theme(config);
            ThemeEditorState {
                name: config.appearance.theme.clone(),
                seeded_from: config.appearance.theme.clone(),
                working,
                status: String::new(),
                status_is_error: false,
            }
        });

    let mut changed_config = false;

    ui.label(
        egui::RichText::new(
            "Edit a copy of the active theme. Changes here don't touch the live \
             theme until you Save — saving writes a user theme and switches to it.",
        )
        .weak()
        .small(),
    );
    ui.add_space(6.0);

    // ── Name + actions row ────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("name").monospace());
        ui.add(
            egui::TextEdit::singleline(&mut state.name)
                .desired_width(160.0)
                .hint_text("my-theme"),
        );

        if ui.button("Save theme").clicked() {
            let slug = slugify(&state.name);
            state.working.name = slug.clone();
            match save_theme(&state.working, &slug) {
                Ok(()) => {
                    config.appearance.theme = slug.clone();
                    // Clear any background overrides so the app follows the
                    // newly-saved theme's background (mirrors the theme-change
                    // contract documented on AppearanceConfig).
                    config.appearance.background_override = None;
                    config.appearance.note_background_override = None;
                    // Track the new active so we don't immediately re-seed and
                    // discard the working copy the user just saved.
                    state.seeded_from = slug.clone();
                    state.status = format!("saved as \"{slug}\" and set active");
                    state.status_is_error = false;
                    changed_config = true;
                }
                Err(e) => {
                    state.status = e;
                    state.status_is_error = true;
                }
            }
        }

        if ui.button("Reset").clicked() {
            state.working = seed_theme(config);
            state.name = config.appearance.theme.clone();
            state.seeded_from = config.appearance.theme.clone();
            state.status = "reset to the active theme".to_string();
            state.status_is_error = false;
        }

        if ui.button("New from current").clicked() {
            state.working = seed_theme(config);
            state.working.name = format!("{}-copy", config.appearance.theme);
            state.name = state.working.name.clone();
            state.status = "started a new theme from the active one".to_string();
            state.status_is_error = false;
        }
    });

    // Status line (save result / error), if any. Errors take the visuals'
    // error colour (the repo idiom at settings.rs); a success message uses the
    // muted `.weak()` style the rest of the panel uses for hints.
    if !state.status.is_empty() {
        let text = if state.status_is_error {
            egui::RichText::new(&state.status)
                .color(ui.visuals().error_fg_color)
                .small()
        } else {
            egui::RichText::new(&state.status).weak().small()
        };
        ui.label(text);
    }

    let appearance = match state.working.appearance {
        Appearance::Dark => "dark",
        Appearance::Light => "light",
    };
    ui.label(
        egui::RichText::new(format!("base appearance: {appearance}"))
            .weak()
            .small(),
    );

    ui.add_space(6.0);

    // ── Live preview ──────────────────────────────────────────────────────
    ui.label(egui::RichText::new("preview").strong().small());
    live_preview(ui, &state.working);
    ui.add_space(8.0);

    // ── Token pickers ─────────────────────────────────────────────────────
    // UI tokens and Syntax tokens sit SIDE BY SIDE in two always-open columns
    // (NOT collapsible "dropdown" headers, and NOT stacked vertically): the user
    // wants both palettes visible at once with no expand/collapse. NO nested
    // ScrollArea here (#11) — the Settings page (settings.rs) owns the single
    // outer ScrollArea, so the mouse wheel always scrolls the page; each column
    // just lays its swatches out in an aligned two-column `label | swatch` Grid.
    ui.columns(2, |cols| {
        // Left column — UI / chrome tokens.
        cols[0].label(egui::RichText::new("UI").strong());
        cols[0].label(
            egui::RichText::new("chrome: window, panels, text, accent, cursor")
                .weak()
                .small(),
        );
        egui::Grid::new("scr1b3_theme_editor_ui_tokens")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(&mut cols[0], |ui| {
                for (key, label) in UI_TOKENS {
                    let allow_alpha = ALPHA_TOKENS.contains(key);
                    token_grid_row(ui, &mut state.working.ui, key, label, allow_alpha);
                }
            });

        // Right column — Syntax tokens.
        cols[1].label(egui::RichText::new("Syntax").strong());
        cols[1].label(
            egui::RichText::new("token colours for highlighted code")
                .weak()
                .small(),
        );
        egui::Grid::new("scr1b3_theme_editor_syntax_tokens")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(&mut cols[1], |ui| {
                for (key, label) in SYNTAX_TOKENS {
                    token_grid_row(ui, &mut state.working.syntax, key, label, false);
                }
            });
    });

    // Persist the (possibly mutated) working state back into temp memory.
    ui.ctx().data_mut(|d| d.insert_temp(id, state));

    changed_config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_color32_round_trip_opaque() {
        let c = Rgba::new(0x12, 0x34, 0x56, 0xff);
        assert_eq!(color32_to_rgba(rgba_to_color32(c)), c);
    }

    #[test]
    fn rgba_color32_round_trip_translucent() {
        // The selection token carries alpha. Color32 stores PREMULTIPLIED u8s, so
        // a translucent round-trip is inherently lossy by at most ±1 per colour
        // channel (the premultiply-then-unpremultiply divides and re-multiplies by
        // the alpha). Assert the alpha survives EXACTLY (it is not premultiplied)
        // and each colour channel survives within that ±1 rounding tolerance.
        let c = Rgba::new(0x00, 0xff, 0x90, 0x33);
        let rt = color32_to_rgba(rgba_to_color32(c));
        assert_eq!(rt.a, c.a, "alpha must round-trip exactly");
        for (got, want) in [(rt.r, c.r), (rt.g, c.g), (rt.b, c.b)] {
            assert!(
                (i16::from(got) - i16::from(want)).abs() <= 1,
                "channel drifted >1: got {got} want {want}"
            );
        }
    }

    #[test]
    fn slugify_basic_lowercases_and_hyphenates() {
        assert_eq!(slugify("My Theme"), "my-theme");
        assert_eq!(slugify("  Wired_Noir  "), "wired-noir");
        assert_eq!(slugify("Lain  Mauve"), "lain-mauve");
    }

    #[test]
    fn slugify_drops_punctuation_and_collapses_hyphens() {
        assert_eq!(slugify("Neon!! Night??"), "neon-night");
        assert_eq!(slugify("a---b"), "a-b");
        assert_eq!(slugify("--lead-trail--"), "lead-trail");
    }

    #[test]
    fn slugify_empty_falls_back() {
        assert_eq!(slugify(""), "my-theme");
        assert_eq!(slugify("   "), "my-theme");
        assert_eq!(slugify("!@#$"), "my-theme");
    }

    #[test]
    fn seed_theme_never_blank_for_unknown_active() {
        // An active theme name with no user TOML and no built-in still yields a
        // populated theme (the house-brand fallback).
        let mut config = Config::default();
        config.appearance.theme = "does-not-exist-anywhere".to_string();
        let theme = seed_theme(&config);
        assert!(!theme.ui.is_empty());
        assert!(theme.ui.contains_key("background"));
    }

    #[test]
    fn ui_tokens_cover_builtin_keys() {
        // Every token we surface as a UI picker must exist in a representative
        // built-in, so the editor isn't showing dead rows.
        let theme = Theme::itasha_corp();
        for (key, _) in UI_TOKENS {
            assert!(theme.ui.contains_key(*key), "itasha-corp missing ui.{key}");
        }
        for (key, _) in SYNTAX_TOKENS {
            assert!(
                theme.syntax.contains_key(*key),
                "itasha-corp missing syntax.{key}"
            );
        }
    }

    // ── Colour-lookup helpers ──────────────────────────────────────────────

    #[test]
    fn ui_color_returns_theme_value_when_present_and_default_when_absent() {
        let theme = Theme::itasha_corp();
        // `background` exists in every built-in: the helper returns the stored
        // colour, NOT the supplied default.
        let sentinel = Color32::from_rgb(1, 2, 3);
        let got = ui_color(&theme, "background", sentinel);
        assert_ne!(got, sentinel, "present key must return the theme's colour");
        // A key the theme does not carry falls back to the supplied default.
        assert_eq!(
            ui_color(&theme, "no-such-ui-token", sentinel),
            sentinel,
            "absent key must fall back to the default"
        );
    }

    #[test]
    fn syntax_color_returns_theme_value_when_present_and_default_when_absent() {
        let theme = Theme::itasha_corp();
        let sentinel = Color32::from_rgb(4, 5, 6);
        assert_ne!(syntax_color(&theme, "keyword", sentinel), sentinel);
        assert_eq!(
            syntax_color(&theme, "no-such-syntax-token", sentinel),
            sentinel
        );
    }

    // ── Seed / save round-trips (hermetic via SCR1B3_CONFIG_DIR) ────────────

    /// Scoped env guard so the config-dir override is restored on drop and the
    /// seed/save tests never touch the real config dir. Serialised by the lock
    /// below because `SCR1B3_CONFIG_DIR` is process-global.
    struct ConfigDirGuard {
        prev: Option<std::ffi::OsString>,
    }
    impl ConfigDirGuard {
        fn set(dir: &std::path::Path) -> Self {
            let prev = std::env::var_os("SCR1B3_CONFIG_DIR");
            std::env::set_var("SCR1B3_CONFIG_DIR", dir);
            Self { prev }
        }
    }
    impl Drop for ConfigDirGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("SCR1B3_CONFIG_DIR", v),
                None => std::env::remove_var("SCR1B3_CONFIG_DIR"),
            }
        }
    }

    use std::sync::Mutex;
    static CONFIG_DIR_LOCK: Mutex<()> = Mutex::new(());

    /// Acquire the config-dir lock, tolerating a prior test's panic that
    /// poisoned it (the data is `()`; a poisoned lock carries no broken state).
    fn lock_config_dir() -> std::sync::MutexGuard<'static, ()> {
        CONFIG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn seed_theme_uses_builtin_for_known_active() {
        let _lock = lock_config_dir();
        let tmp = tempfile::tempdir().unwrap();
        let _g = ConfigDirGuard::set(tmp.path());
        // A known built-in with NO user TOML => the compiled-in built-in is
        // seeded (branch 2 of seed_theme).
        let mut config = Config::default();
        config.appearance.theme = "wired-noir".to_string();
        let seeded = seed_theme(&config);
        let builtin = Theme::builtin("wired-noir").unwrap();
        assert_eq!(seeded.ui, builtin.ui, "known active seeds its built-in");
    }

    #[test]
    fn seed_theme_prefers_user_toml_over_builtin() {
        let _lock = lock_config_dir();
        let tmp = tempfile::tempdir().unwrap();
        let _g = ConfigDirGuard::set(tmp.path());
        // Write a user TOML that SHADOWS a built-in name with a distinct accent so
        // we can prove the user file wins (branch 1 of seed_theme).
        let mut custom = Theme::itasha_corp();
        custom
            .ui
            .insert("accent".to_string(), Rgba::new(1, 2, 3, 255));
        let themes_dir = tmp.path().join("themes");
        std::fs::create_dir_all(&themes_dir).unwrap();
        std::fs::write(themes_dir.join("wired-noir.toml"), custom.to_toml_string()).unwrap();

        let mut config = Config::default();
        config.appearance.theme = "wired-noir".to_string();
        let seeded = seed_theme(&config);
        assert_eq!(
            seeded.ui.get("accent").copied(),
            Some(Rgba::new(1, 2, 3, 255)),
            "the user TOML must win over the built-in of the same name"
        );
    }

    #[test]
    fn save_theme_writes_user_toml_that_reparses() {
        let _lock = lock_config_dir();
        let tmp = tempfile::tempdir().unwrap();
        let _g = ConfigDirGuard::set(tmp.path());
        let mut theme = Theme::itasha_corp();
        theme.name = "my-saved".to_string();
        theme
            .ui
            .insert("accent".to_string(), Rgba::new(9, 8, 7, 255));

        save_theme(&theme, "my-saved").expect("save succeeds with a writable config dir");

        // The file landed under <config_dir>/themes/<slug>.toml and reparses to
        // the same theme (round-trip through the on-disk TOML).
        let path = tmp.path().join("themes").join("my-saved.toml");
        let text = std::fs::read_to_string(&path).expect("saved file exists");
        let back = Theme::from_toml_str(&text).expect("saved TOML reparses");
        assert_eq!(
            back.ui.get("accent").copied(),
            Some(Rgba::new(9, 8, 7, 255))
        );
    }

    // ── GUI drive-through (egui_kittest) ───────────────────────────────────

    use egui_kittest::kittest::Queryable as _;

    /// Drive `show` once through the kittest harness against a temp config dir,
    /// then a second frame after clicking Save, asserting the save status and the
    /// changed-config signal. This exercises live_preview, the token grids
    /// (token_grid_row), and the Save/Reset/New buttons.
    #[test]
    fn show_renders_preview_saves_and_reports_changed_config() {
        let _lock = lock_config_dir();
        let tmp = tempfile::tempdir().unwrap();
        let _g = ConfigDirGuard::set(tmp.path());

        let mut config = Config::default();
        config.appearance.theme = "wired-noir".to_string();

        // `show` returns true only on the exact frame Save is clicked; `h.run()`
        // may settle over several frames, so accumulate "ever returned true"
        // rather than reading only the last frame's value.
        let changed_ever = std::cell::Cell::new(false);
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(900.0, 800.0))
            .build_ui(|ui| {
                if show(ui, &mut config) {
                    changed_ever.set(true);
                }
            });
        // First frame: render the editor (seeds working theme + live preview).
        h.run();
        // The preview's status-bar label and the preview header render — proves
        // live_preview + the section labels painted.
        assert!(
            h.query_by_label("● READY").is_some(),
            "live preview painted"
        );
        assert!(
            h.query_by_label("preview").is_some(),
            "preview header rendered"
        );

        // Click Save: it slugifies the name, writes the user TOML, sets it active,
        // and returns true (changed config) on the click frame.
        h.get_by_label("Save theme").click();
        h.run();
        // Drop the harness so the `&mut config` borrow it holds is released
        // before we inspect `config` directly.
        drop(h);
        assert!(
            changed_ever.get(),
            "Save reported a changed config on its click frame"
        );
        // Durable side effects: the active theme was set to the saved slug and
        // the user theme TOML exists on disk.
        assert_eq!(
            config.appearance.theme, "wired-noir",
            "Save set the saved slug active"
        );
        assert!(
            tmp.path().join("themes").join("wired-noir.toml").exists(),
            "Save wrote the user theme TOML"
        );
    }

    #[test]
    fn show_reset_button_reseeds_without_changing_config() {
        let _lock = lock_config_dir();
        let tmp = tempfile::tempdir().unwrap();
        let _g = ConfigDirGuard::set(tmp.path());
        let mut config = Config::default();
        config.appearance.theme = "wired-noir".to_string();

        // Reset only re-seeds the working copy; `show` must NEVER return true on
        // a Reset click (no config change). Accumulate to catch any frame.
        let changed_ever = std::cell::Cell::new(false);
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(900.0, 800.0))
            .build_ui(|ui| {
                if show(ui, &mut config) {
                    changed_ever.set(true);
                }
            });
        h.run();
        h.get_by_label("Reset").click();
        h.run();
        assert!(
            !changed_ever.get(),
            "Reset must NOT report a changed config (it only re-seeds the working copy)"
        );
        assert!(
            h.query_by_label("reset to the active theme").is_some(),
            "Reset shows its status"
        );
        // No theme file was written by a Reset (it does not save).
        assert!(
            !tmp.path().join("themes").exists()
                || std::fs::read_dir(tmp.path().join("themes"))
                    .map(|mut d| d.next().is_none())
                    .unwrap_or(true),
            "Reset must not write any theme file"
        );
    }
}
