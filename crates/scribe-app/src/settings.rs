//! In-app settings window. Edits the live `Config` (deep customization without
//! hand-editing TOML). Returns `true` when something changed so the caller can
//! persist + re-apply the theme. Kept as a free function so it never fights the
//! `ScribeApp` borrow.

use eframe::egui;
use scribe_core::config::{UpdateMode, WindowMode};
use scribe_core::Config;

/// Render the settings window. `open` is toggled false when the user closes it.
/// Returns `true` if any field changed this frame.
pub fn show(ctx: &egui::Context, config: &mut Config, open: &mut bool) -> bool {
    let mut changed = false;
    let mut keep_open = *open;
    egui::Window::new("settings")
        .open(&mut keep_open)
        .collapsible(true)
        .resizable(true)
        .default_width(420.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Appearance");
                changed |= ui
                    .horizontal(|ui| {
                        ui.label("Theme");
                        ui.text_edit_singleline(&mut config.appearance.theme)
                            .changed()
                    })
                    .inner;
                changed |= ui
                    .checkbox(
                        &mut config.appearance.follow_os_theme,
                        "Follow OS dark/light",
                    )
                    .changed();
                changed |= ui
                    .checkbox(
                        &mut config.appearance.frameless,
                        "Frameless window (restart to apply)",
                    )
                    .changed();

                ui.separator();
                ui.heading("Fonts");
                changed |= ui
                    .add(egui::Slider::new(&mut config.fonts.editor_size, 8.0..=32.0).text("size"))
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut config.fonts.line_height, 1.0..=2.5)
                            .text("line height"),
                    )
                    .changed();
                changed |= ui
                    .checkbox(&mut config.fonts.ligatures, "Ligatures")
                    .changed();

                ui.separator();
                ui.heading("Editor");
                changed |= ui
                    .add(egui::Slider::new(&mut config.editor.tab_width, 1..=8).text("tab width"))
                    .changed();
                changed |= ui
                    .checkbox(&mut config.editor.insert_spaces, "Insert spaces")
                    .changed();
                changed |= ui
                    .checkbox(&mut config.editor.show_line_numbers, "Line numbers")
                    .changed();
                changed |= ui
                    .checkbox(&mut config.editor.word_wrap, "Word wrap")
                    .changed();
                changed |= ui
                    .checkbox(&mut config.editor.show_minimap, "Minimap")
                    .changed();
                changed |= ui
                    .checkbox(&mut config.editor.restore_session, "Restore session")
                    .changed();

                ui.separator();
                ui.heading("CRT effect");
                changed |= ui
                    .checkbox(&mut config.effects.crt_enabled, "Enable CRT post-process")
                    .changed();
                ui.add_enabled_ui(config.effects.crt_enabled, |ui| {
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut config.effects.scanline, 0.0..=1.0)
                                .text("scanline"),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut config.effects.phosphor_glow, 0.0..=1.0)
                                .text("glow"),
                        )
                        .changed();
                    changed |= ui
                        .add(egui::Slider::new(&mut config.effects.bloom, 0.0..=1.0).text("bloom"))
                        .changed();
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut config.effects.vignette, 0.0..=1.0)
                                .text("vignette"),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut config.effects.curvature, 0.0..=1.0)
                                .text("curvature"),
                        )
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut config.effects.respect_reduced_motion,
                            "Respect reduced motion",
                        )
                        .changed();
                });

                ui.separator();
                ui.heading("Window (transparency / glass)");
                let wmodes = [
                    (WindowMode::Opaque, "opaque"),
                    (WindowMode::Transparent, "transparent"),
                    (WindowMode::Glass, "glass / acrylic"),
                    (WindowMode::Mica, "mica (Win11)"),
                    (WindowMode::Vibrancy, "vibrancy (macOS)"),
                ];
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
                ui.add_enabled_ui(config.window.mode.is_translucent(), |ui| {
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut config.window.opacity, 0.30..=1.0)
                                .text("opacity"),
                        )
                        .changed();
                });
                changed |= ui
                    .horizontal(|ui| {
                        ui.label("tint");
                        ui.text_edit_singleline(&mut config.window.tint).changed()
                    })
                    .inner;
                changed |= ui
                    .add(
                        egui::Slider::new(&mut config.window.tint_strength, 0.0..=1.0)
                            .text("tint strength"),
                    )
                    .changed();

                ui.separator();
                ui.heading("Spellcheck (offline)");
                changed |= ui
                    .checkbox(&mut config.spellcheck.enabled, "Enable")
                    .changed();
                ui.add_enabled_ui(config.spellcheck.enabled, |ui| {
                    changed |= ui
                        .horizontal(|ui| {
                            ui.label("Language");
                            ui.text_edit_singleline(&mut config.spellcheck.language)
                                .changed()
                        })
                        .inner;
                    changed |= ui
                        .checkbox(&mut config.spellcheck.check_comments, "Check comments")
                        .changed();
                    changed |= ui
                        .checkbox(&mut config.spellcheck.check_strings, "Check strings")
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut config.spellcheck.check_identifiers,
                            "Check identifiers",
                        )
                        .changed();
                });

                ui.separator();
                ui.heading("Updates (telemetry-free)");
                let modes = [
                    (UpdateMode::Off, "off"),
                    (UpdateMode::Notify, "notify"),
                    (UpdateMode::Manual, "manual"),
                    (UpdateMode::Auto, "auto"),
                ];
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

                ui.separator();
                ui.heading("Plugins");
                changed |= ui
                    .checkbox(&mut config.plugins.enabled, "Enable plugin/mod system")
                    .changed();
                ui.label(
                    egui::RichText::new("Drop mods into the plugins dir — see PLUGINS.md")
                        .weak()
                        .small(),
                );
            });
        });
    *open = keep_open;
    changed
}
