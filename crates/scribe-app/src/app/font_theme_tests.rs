//! #87 — bundled font themes. The selected family must lead the Monospace
//! family list (so it actually renders), JetBrains Mono must stay right
//! behind it as a fallback, every bundled face + the JP subset must be
//! registered, and an unknown name must fall back gracefully.
use super::render_support::font_family_key;
use super::{build_fonts, FONT_FAMILIES};

#[test]
fn selected_family_leads_with_jetbrains_fallback() {
    let f = build_fonts("IBM Plex Mono", "System default");
    let mono = &f.families[&egui::FontFamily::Monospace];
    assert_eq!(mono[0], "IBMPlexMono", "selected face renders first");
    assert_eq!(mono[1], "JetBrainsMono", "JetBrains kept as fallback");
    for (_, key) in FONT_FAMILIES {
        assert!(f.font_data.contains_key(*key), "{key} registered");
    }
    assert!(f.font_data.contains_key("NotoSansJP-Subset"));
}

#[test]
fn unknown_family_falls_back_to_jetbrains() {
    assert_eq!(font_family_key("No Such Font"), "JetBrainsMono");
    let f = build_fonts("No Such Font", "System default");
    assert_eq!(f.families[&egui::FontFamily::Monospace][0], "JetBrainsMono");
}

#[test]
fn ui_family_overrides_only_the_proportional_family() {
    // #103 — the UI family leads Proportional; the note family leads
    // Monospace; the two are independent.
    let f = build_fonts("JetBrains Mono", "Fira Mono");
    assert_eq!(f.families[&egui::FontFamily::Proportional][0], "FiraMono");
    assert_eq!(f.families[&egui::FontFamily::Monospace][0], "JetBrainsMono");
    // "System default" leaves egui's built-in UI font at the head.
    let f2 = build_fonts("JetBrains Mono", "System default");
    assert_ne!(f2.families[&egui::FontFamily::Proportional][0], "FiraMono");
}

#[test]
fn default_family_is_jetbrains_and_does_not_double_insert() {
    let f = build_fonts("JetBrains Mono", "System default");
    let mono = &f.families[&egui::FontFamily::Monospace];
    assert_eq!(mono[0], "JetBrainsMono");
    // No redundant second JetBrains entry when it's already the selection.
    assert_ne!(mono.get(1).map(String::as_str), Some("JetBrainsMono"));
}
