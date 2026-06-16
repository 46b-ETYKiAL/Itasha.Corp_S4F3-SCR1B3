//! Phase 17 T17.5 — verify the JP-glyph instrument-label discipline.
use super::{jp_glyph, toolbar_widget};

#[test]
fn verified_canonical_kanji_present_for_high_confidence_ids() {
    // The Folklore-Consultant gate requires "verified-accurate kanji ONLY".
    // These 11 are the verified-canonical IT-Japanese forms; this test
    // pins them so an accidental edit (typo, replacement with an
    // unverified glyph) regresses loudly.
    assert_eq!(jp_glyph("new"), Some("新"));
    assert_eq!(jp_glyph("open"), Some("開"));
    assert_eq!(jp_glyph("save"), Some("保"));
    assert_eq!(jp_glyph("saveas"), Some("別"));
    assert_eq!(jp_glyph("find"), Some("検"));
    assert_eq!(jp_glyph("split"), Some("分"));
    assert_eq!(jp_glyph("minimap"), Some("図"));
    assert_eq!(jp_glyph("wrap"), Some("折"));
    assert_eq!(jp_glyph("fold"), Some("畳"));
    assert_eq!(jp_glyph("linenumbers"), Some("番"));
    assert_eq!(jp_glyph("spellcheck"), Some("綴"));
}

#[test]
fn bundled_jp_subset_covers_every_toolbar_kanji() {
    // #56 — the toolbar kanji rendered as tofu because no font in the stack
    // covered CJK. We bundle a Noto Sans JP subset; this asserts that subset
    // actually contains a glyph for every kanji `jp_glyph` can emit, read
    // through skrifa (the same font crate epaint/egui 0.34 rasterizes with).
    // A botched regeneration that drops a glyph fails here, loudly.
    use skrifa::{raw::FontRef, MetadataProvider as _};
    const SUBSET: &[u8] =
        include_bytes!("../../../../assets/fonts/NotoSansJP/NotoSansJP-Subset.ttf");
    let face = FontRef::new(SUBSET).expect("bundled JP subset must parse");
    let charmap = face.charmap();
    let ids = [
        "new",
        "open",
        "save",
        "saveas",
        "find",
        "split",
        "minimap",
        "wrap",
        "fold",
        "linenumbers",
        "spellcheck",
    ];
    for id in ids {
        let kanji = jp_glyph(id).expect("id has a verified kanji");
        let ch = kanji.chars().next().unwrap();
        let gid = charmap.map(ch);
        assert!(
            gid.is_some_and(|g| g.to_u32() != 0),
            "bundled JP subset is missing a glyph for {id} = {kanji:?} \
                 (regenerate via scripts/generate-jp-kanji-subset.py)"
        );
    }
}

#[test]
fn uncertain_ids_omit_kanji() {
    // Western-metaphor or acronym/loanword actions stay English-only —
    // the canonical kanji is uncertain or contested. They MUST return
    // None so a future "ship a guess" doesn't slip through.
    assert_eq!(jp_glyph("openfolder"), None);
    assert_eq!(jp_glyph("palette"), None);
    assert_eq!(jp_glyph("lsp"), None);
    // Unknown ids also return None — the helper never invents.
    assert_eq!(jp_glyph("not-a-toolbar-action"), None);
}

#[test]
fn widget_falls_back_to_label_when_disabled_or_unknown() {
    // jp_glyph_labels=false → primary label only, regardless of action.
    let off = toolbar_widget("new", false, false, 14.0, egui::Color32::PLACEHOLDER);
    assert_eq!(off.text(), "new");
    // Even with the flag on, an action without verified kanji returns
    // only the primary label — no kanji is invented.
    let on_unknown = toolbar_widget("openfolder", false, true, 14.0, egui::Color32::PLACEHOLDER);
    assert_eq!(on_unknown.text(), "folder");
}

#[test]
fn widget_appends_kanji_when_enabled_for_verified_action() {
    // jp_glyph_labels=true + verified action → primary then kanji.
    // The LayoutJob's flattened text contains both pieces.
    let on = toolbar_widget("save", false, true, 14.0, egui::Color32::PLACEHOLDER);
    let text = on.text();
    assert!(text.starts_with("save"), "got {text:?}");
    assert!(text.contains("保"), "got {text:?}");
}

#[test]
fn kanji_label_keeps_english_text_colour_constant() {
    // #105 — with kanji ON, the ENGLISH (primary) section must use
    // PLACEHOLDER so the widget substitutes its normal text colour, i.e.
    // identical to kanji-OFF. Only the kanji section is explicitly tinted.
    let on = toolbar_widget("save", false, true, 14.0, egui::Color32::PLACEHOLDER);
    match on {
        egui::WidgetText::LayoutJob(job) => {
            assert_eq!(
                job.sections[0].format.color,
                egui::Color32::PLACEHOLDER,
                "english label must inherit the widget colour (constant on/off)"
            );
            assert_ne!(
                job.sections[1].format.color,
                egui::Color32::PLACEHOLDER,
                "the kanji section is the one that is tinted"
            );
        }
        other => panic!("expected a LayoutJob with a kanji section, got {other:?}"),
    }
}

#[test]
fn selected_toggle_pins_primary_to_accent_with_kanji_on() {
    // #22 — a SELECTED toolbar toggle passes a CONCRETE accent as the primary
    // colour, so with kanji ON the LayoutJob's primary (english) section must
    // carry that exact accent — NOT PLACEHOLDER, which `selectable_label`
    // would recolour to its strong-contrast (white) selected colour. The
    // kanji section keeps its own dim tint. This is the regression guard for
    // the "kanji-on selected toggle renders white instead of accent" bug.
    let accent = egui::Color32::from_rgb(0, 255, 254);
    let on = toolbar_widget("save", false, true, 14.0, accent);
    match on {
        egui::WidgetText::LayoutJob(job) => {
            assert_eq!(
                job.sections[0].format.color, accent,
                "selected english label must be accent even with kanji on"
            );
            assert_ne!(
                job.sections[0].format.color,
                egui::Color32::PLACEHOLDER,
                "a selected toggle must NOT leave the primary as PLACEHOLDER"
            );
            assert_ne!(
                job.sections[1].format.color, accent,
                "the kanji section keeps its own dim tint"
            );
        }
        other => panic!("expected a LayoutJob with a kanji section, got {other:?}"),
    }
}
