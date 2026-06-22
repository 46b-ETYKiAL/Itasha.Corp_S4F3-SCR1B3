//! WU-1 coverage: pin the pure string/index/session helpers that the egui glue
//! in `app/mod.rs` calls into. These functions carry editor logic (UTF-8 index
//! conversion, bracket matching, session signatures, font-key resolution) that
//! is independent of any egui frame, so they are tested directly here rather
//! than through the e2e render loop. Companion to the inline tests that already
//! cover `byte_to_char_index`, `effective_wrap_width`, `use_rope_editor`,
//! `apply_indent`, `line_col_from_char_index`, and `pick_bookmark`.

use super::{
    char_to_byte, font_family_key, font_state_key, matching_bracket_char_indices, now_unix,
    path_to_uri, session_signature, EditorTab,
};

// ---- char_to_byte (the inverse of byte_to_char_index) ----

#[test]
fn char_to_byte_ascii_is_identity() {
    let s = "hello";
    for ci in 0..=s.chars().count() {
        assert_eq!(char_to_byte(s, ci), ci, "ascii char {ci}");
    }
}

#[test]
fn char_to_byte_multibyte() {
    // "é" is 2 bytes (U+00E9), "中" is 3 bytes.
    let s = "é中z";
    assert_eq!(char_to_byte(s, 0), 0);
    assert_eq!(char_to_byte(s, 1), 2, "byte offset of '中'");
    assert_eq!(char_to_byte(s, 2), 5, "byte offset of 'z'");
    assert_eq!(char_to_byte(s, 3), 6, "one past the end → len");
}

#[test]
fn char_to_byte_past_end_clamps_to_len() {
    let s = "ab";
    assert_eq!(char_to_byte(s, 99), s.len());
    assert_eq!(char_to_byte("", 0), 0);
    assert_eq!(char_to_byte("", 5), 0);
}

// ---- matching_bracket_char_indices ----

#[test]
fn bracket_match_simple_pair_caret_after_opener() {
    // "(x)" — caret char-index 1 sits just after '(' at index 0.
    let r = matching_bracket_char_indices("(x)", 1);
    assert_eq!(r, Some((0, 2)));
}

#[test]
fn bracket_match_prefers_char_left_of_caret() {
    // "()" — caret at index 1 is between ')' (left) and end. Left char is ')',
    // which scans backward to its opener at 0.
    assert_eq!(matching_bracket_char_indices("()", 1), Some((0, 1)));
}

#[test]
fn bracket_match_caret_on_closer_scans_backward() {
    // Caret at index 0 has no char to its left, so it inspects the char AT the
    // caret: '(' → scans forward to ')'.
    assert_eq!(matching_bracket_char_indices("()", 0), Some((0, 1)));
}

#[test]
fn bracket_match_respects_nesting() {
    // "((a))" indices: ( ( a ) )  = 0 1 2 3 4
    // caret 1 → left char '(' at 0 → its partner is the OUTER ')' at 4.
    assert_eq!(matching_bracket_char_indices("((a))", 1), Some((0, 4)));
    // caret 2 → left char '(' at 1 → inner ')' at 3.
    assert_eq!(matching_bracket_char_indices("((a))", 2), Some((1, 3)));
}

#[test]
fn bracket_match_all_three_kinds() {
    assert_eq!(matching_bracket_char_indices("[x]", 1), Some((0, 2)));
    assert_eq!(matching_bracket_char_indices("{x}", 1), Some((0, 2)));
}

#[test]
fn bracket_match_unbalanced_returns_none() {
    assert_eq!(matching_bracket_char_indices("(((", 1), None);
    assert_eq!(matching_bracket_char_indices(")))", 1), None);
}

#[test]
fn bracket_match_no_bracket_near_caret_returns_none() {
    assert_eq!(matching_bracket_char_indices("abc", 1), None);
    assert_eq!(matching_bracket_char_indices("", 0), None);
}

#[test]
fn bracket_match_handles_multibyte_before_bracket() {
    // The function works on char indices, so a multibyte prefix must not shift
    // the reported indices. "中(x)" — caret char-index 2 → left char '(' at 1.
    assert_eq!(matching_bracket_char_indices("中(x)", 2), Some((1, 3)));
}

// ---- session_signature ----

#[test]
fn session_signature_empty_is_empty_string() {
    assert_eq!(session_signature(&[]), "");
}

#[test]
fn session_signature_ignores_pathless_scratch_tabs() {
    // A scratch tab has no path, so it contributes nothing to the signature.
    let tabs = vec![EditorTab::scratch(), EditorTab::scratch()];
    assert_eq!(session_signature(&tabs), "");
}

#[test]
fn session_signature_is_order_independent() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    std::fs::write(&a, "").unwrap();
    std::fs::write(&b, "").unwrap();

    let tabs_ab = vec![
        EditorTab::from_path(a.clone()).unwrap(),
        EditorTab::from_path(b.clone()).unwrap(),
    ];
    let tabs_ba = vec![
        EditorTab::from_path(b).unwrap(),
        EditorTab::from_path(a).unwrap(),
    ];
    // Paths are sorted before joining, so tab order does not change the sig.
    assert_eq!(session_signature(&tabs_ab), session_signature(&tabs_ba));
    assert!(session_signature(&tabs_ab).contains("a.txt"));
    assert!(session_signature(&tabs_ab).contains("b.txt"));
}

// ---- path_to_uri ----

#[test]
fn path_to_uri_unix_absolute_gets_double_slash() {
    let uri = path_to_uri(std::path::Path::new("/home/u/x.rs"));
    assert_eq!(uri, "file:///home/u/x.rs");
}

#[test]
fn path_to_uri_backslashes_become_forward_slashes() {
    // A Windows-style path is normalised to forward slashes; a drive-letter
    // path is relative-style (no leading slash) so it gets the triple-slash.
    let uri = path_to_uri(std::path::Path::new(r"C:\code\x.rs"));
    assert_eq!(uri, "file:///C:/code/x.rs");
}

#[test]
fn path_to_uri_relative_gets_triple_slash() {
    let uri = path_to_uri(std::path::Path::new("rel/x.rs"));
    assert_eq!(uri, "file:///rel/x.rs");
}

// ---- font_state_key / font_family_key ----

#[test]
fn font_state_key_combines_both_families_with_nul_separator() {
    let fonts = scribe_core::config::FontConfig {
        editor_family: "JetBrains Mono".into(),
        ui_family: "IBM Plex Mono".into(),
        ..Default::default()
    };
    let key = font_state_key(&fonts);
    assert_eq!(key, "JetBrains Mono\u{0}IBM Plex Mono");
}

#[test]
fn font_state_key_distinguishes_swapped_families() {
    let a = scribe_core::config::FontConfig {
        editor_family: "Fira Mono".into(),
        ui_family: "Cousine".into(),
        ..Default::default()
    };
    let b = scribe_core::config::FontConfig {
        editor_family: "Cousine".into(),
        ui_family: "Fira Mono".into(),
        ..Default::default()
    };
    assert_ne!(
        font_state_key(&a),
        font_state_key(&b),
        "swapping editor/ui must change the cache key"
    );
}

#[test]
fn font_family_key_resolves_known_display_names() {
    assert_eq!(font_family_key("JetBrains Mono"), "JetBrainsMono");
    assert_eq!(font_family_key("IBM Plex Mono"), "IBMPlexMono");
    assert_eq!(font_family_key("Fira Mono"), "FiraMono");
}

#[test]
fn font_family_key_unknown_falls_back_to_jetbrains() {
    assert_eq!(font_family_key("Comic Sans"), "JetBrainsMono");
    assert_eq!(font_family_key(""), "JetBrainsMono");
}

// ---- now_unix ----

#[test]
fn now_unix_is_after_2020() {
    // 2020-01-01T00:00:00Z = 1_577_836_800. A correct clock yields a value well
    // past this; a pre-epoch clock saturates to 0 (never panics).
    let t = now_unix();
    assert!(t > 1_577_836_800, "now_unix() = {t} should be after 2020");
}
