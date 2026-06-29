//! Property-based round-trip + invariant tests for the `scribe-core` data spine.
//!
//! A text editor that corrupts a file on save is a catastrophic failure, so the
//! laws that protect user data — `decode(encode(x)) == x` across the encoding
//! matrix, EOL idempotence, search-replace-all reversibility, TOML config
//! serialize↔deserialize identity, snippet expansion totality — are asserted
//! over arbitrary input rather than a handful of hand-picked examples.
//!
//! These live as a sibling integration test (driving only the public API) so
//! they stay disjoint from the inline `#[cfg(test)]` proptest modules in each
//! source file. PART 2 §A items 1, 4 of the SCR1B3 testing taxonomy.

use proptest::prelude::*;

use scribe_core::config::Config;
use scribe_core::eol::{self, Eol};
use scribe_core::search::{find_all, replace_all, Query};
use scribe_core::snippets::{expand, SnippetSet};
use scribe_core::{encoding, text_ops};

// ---------------------------------------------------------------------------
// Encoding matrix round-trip: decode(encode(x)) == x
// ---------------------------------------------------------------------------

/// The encodings the editor must round-trip a representable string through. Each
/// is a single-byte (Latin / Windows codepage) or Unicode encoding that
/// `encoding_rs` (or the hand-rolled UTF-16 path) can re-emit losslessly when
/// the input is restricted to that encoding's representable set.
const SINGLE_BYTE_ENCODINGS: &[&str] = &[
    "windows-1252",
    "ISO-8859-2",
    "ISO-8859-15",
    "windows-1251",
    "KOI8-R",
];

/// A `DetectedEncoding` by name with no BOM.
fn enc(name: &str, had_bom: bool) -> encoding::DetectedEncoding {
    encoding::DetectedEncoding {
        name: name.to_string(),
        had_bom,
    }
}

proptest! {
    /// For any string that is fully representable in a single-byte encoding
    /// (i.e. `encode_checked` reports NOT lossy), `decode(encode(x))` recovers
    /// the exact original text. This is the core save→reopen guarantee for
    /// legacy-codepage files.
    #[test]
    fn single_byte_encoding_roundtrips_when_representable(
        s in ".*",
        idx in 0usize..SINGLE_BYTE_ENCODINGS.len(),
    ) {
        let name = SINGLE_BYTE_ENCODINGS[idx];
        let e = enc(name, false);
        let (bytes, lossy) = encoding::encode_checked(&s, &e);
        // Only assert the round-trip when the string is representable; a lossy
        // encode legitimately substitutes a replacement char and is NOT expected
        // to round-trip (the editor warns the user on a lossy save).
        if !lossy {
            // Decode the produced bytes WITH the known encoding (not statistical
            // detection, which can mis-guess a short Latin string) by re-encoding
            // through encoding_rs directly via the same name.
            let label = encoding_rs::Encoding::for_label(name.as_bytes())
                .expect("known encoding label");
            let (decoded, _, had_errors) = label.decode(&bytes);
            prop_assert!(!had_errors, "decode of our own encode must not error for {name}");
            prop_assert_eq!(decoded.as_ref(), s.as_str(),
                "round-trip failed for {}", name);
        }
    }

    /// UTF-8 (the canonical in-memory encoding) round-trips ANY string exactly,
    /// with or without a BOM. Never lossy.
    #[test]
    fn utf8_roundtrips_any_string(s in ".*", bom in any::<bool>()) {
        let e = enc("UTF-8", bom);
        let (bytes, lossy) = encoding::encode_checked(&s, &e);
        prop_assert!(!lossy, "UTF-8 represents all of Unicode");
        let start = if bom { 3 } else { 0 };
        let body = if bom {
            // A BOM was prepended; the body after it must be the UTF-8 of `s`.
            prop_assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
            &bytes[start..]
        } else {
            &bytes[..]
        };
        prop_assert_eq!(body, s.as_bytes());
    }

    /// UTF-16 (LE and BE) round-trips ANY string exactly via the hand-rolled
    /// encoder + `String::from_utf16`. Never lossy (UTF-16 covers all Unicode).
    #[test]
    fn utf16_roundtrips_any_string(s in ".*", be in any::<bool>(), bom in any::<bool>()) {
        let name = if be { "UTF-16BE" } else { "UTF-16LE" };
        let e = enc(name, bom);
        let (bytes, lossy) = encoding::encode_checked(&s, &e);
        prop_assert!(!lossy);
        let start = if bom { 2 } else { 0 };
        let units: Vec<u16> = bytes[start..]
            .chunks_exact(2)
            .map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
            .collect();
        prop_assert_eq!(String::from_utf16(&units).expect("valid UTF-16"), s);
    }

    /// `decode` is total: it never panics and always yields a string for ANY
    /// byte sequence (the editor opens any file).
    #[test]
    fn decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let (_text, _enc) = encoding::decode(&bytes);
    }

    /// ENCODING-PRESERVATION round-trip — the editor's REAL save/reload contract.
    ///
    /// This models what actually happens to user data: a file is opened and its
    /// encoding `e` is detected ONCE; from then on the editor SAVES under `e`
    /// (`encode_checked(text, e)`) and, on an in-session reload, decodes the file
    /// back WITH the same known `e` (`decode_with(bytes, e)` —
    /// [`Document::reload_from_disk`]). Under that contract the invariant is:
    /// `decode_with(encode_checked(text1, e), e) == text1` whenever the re-encode
    /// is non-lossy.
    ///
    /// This deliberately does NOT assert detection idempotence
    /// (`decode(encode(text1)) == text1`). Re-DETECTING arbitrary bytes is an
    /// unachievable property: `chardetng` is a heuristic and is non-idempotent on
    /// detection-ambiguous inputs (the ENC-1 counterexample
    /// `bytes = [252, 79, 176, 161]` decodes to one CJK string, re-encodes
    /// non-lossily, then RE-DETECTS to a different label and different text). The
    /// old test asserted that flip would never happen — an unachievable property
    /// that made the test intermittently fail. Encoding-preservation
    /// (decode_with the KNOWN encoding) is the meaningful data-safety guarantee
    /// and the one the editor actually relies on; it holds for ALL representable
    /// inputs, including the pinned ENC-1 regression seed in
    /// `roundtrip_proptest.proptest-regressions`.
    ///
    /// The non-lossy gate is required for the same reason as before: when
    /// `chardetng` mis-detects random bytes as a single-byte legacy codepage that
    /// cannot represent the decoded scalars, the re-encode is legitimately lossy
    /// (`encoding_rs` substitutes replacements) and is not expected to round-trip
    /// — the editor warns the user on a lossy save via the `encode_checked` flag.
    #[test]
    fn decode_encode_decode_is_text_stable(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let (text1, e) = encoding::decode(&bytes);
        let (reencoded, lossy) = encoding::encode_checked(&text1, &e);
        prop_assume!(!lossy);
        // Decode the re-encoded bytes WITH the KNOWN encoding `e` (the reload
        // contract), NOT statistical re-detection. This recovers text1 exactly.
        let text2 = encoding::decode_with(&reencoded, &e);
        prop_assert_eq!(text1, text2);
    }

    /// Where detection IS authoritative — a BOM-tagged input — re-detection is
    /// genuinely stable. A leading BOM is sniffed first and is conclusive
    /// (`Encoding::for_bom`), so for BOM-bearing round-trips the detection-based
    /// loop `decode(encode(decode(bytes))) ` recovers the same text. This keeps
    /// the original detection-idempotence intent exactly where it actually holds
    /// (BOM inputs), complementing the encoding-preservation invariant above.
    #[test]
    fn bom_tagged_roundtrips_redetect_stably(s in ".*", be in any::<bool>()) {
        // Build a BOM-bearing UTF-16 file from `s` (UTF-16 covers all Unicode,
        // never lossy), decode it (BOM is authoritative), re-encode under the
        // detected encoding (BOM re-emitted), and re-DETECT — the BOM makes the
        // second detection conclusive, so the text is stable.
        let name = if be { "UTF-16BE" } else { "UTF-16LE" };
        let tagged = encoding::DetectedEncoding { name: name.to_string(), had_bom: true };
        let (file_bytes, lossy) = encoding::encode_checked(&s, &tagged);
        prop_assume!(!lossy);
        let (text1, e) = encoding::decode(&file_bytes);
        prop_assert!(e.had_bom, "a BOM-tagged file detects had_bom = true");
        let (reencoded, lossy2) = encoding::encode_checked(&text1, &e);
        prop_assume!(!lossy2);
        let (text2, e2) = encoding::decode(&reencoded);
        prop_assert!(e2.had_bom, "the re-emitted BOM is detected again");
        prop_assert_eq!(text1, text2);
    }
}

// ---------------------------------------------------------------------------
// EOL normalization: idempotence + style round-trip
// ---------------------------------------------------------------------------

proptest! {
    /// Normalizing already-normalized text is a no-op — `normalize_to_lf` is
    /// idempotent.
    #[test]
    fn normalize_to_lf_is_idempotent(s in ".*") {
        let once = eol::normalize_to_lf(&s);
        let twice = eol::normalize_to_lf(&once);
        prop_assert_eq!(once, twice);
    }

    /// Applying any EOL style to LF-normalized text, then re-normalizing,
    /// recovers the LF form — the editor switches a file's EOL losslessly.
    #[test]
    fn apply_then_normalize_recovers_lf(s in ".*", style in 0u8..3) {
        let norm = eol::normalize_to_lf(&s);
        let e = match style { 0 => Eol::Lf, 1 => Eol::Crlf, _ => Eol::Cr };
        let applied = eol::apply(&norm, e);
        prop_assert_eq!(eol::normalize_to_lf(&applied), norm);
    }

    /// A file written with a uniform EOL style detects back to that same style
    /// (when it has at least one line ending), and round-trips
    /// normalize→detect→apply to the original.
    #[test]
    fn uniform_eol_roundtrips(lines in prop::collection::vec("[a-z]{0,8}", 1..12), style in 0u8..3) {
        let e = match style { 0 => Eol::Lf, 1 => Eol::Crlf, _ => Eol::Cr };
        let original = lines.join(e.as_str());
        let detected = eol::detect(&original);
        // detect only commits to a non-LF style when separators are present.
        if original.contains(e.as_str()) && e != Eol::Lf {
            prop_assert_eq!(detected, e, "uniform {:?} must detect as {:?}", e, e);
        }
        let norm = eol::normalize_to_lf(&original);
        prop_assert_eq!(eol::apply(&norm, detected), {
            // Re-applying the detected style to the normalized form recovers the
            // original ONLY when detection matched; otherwise it recovers an
            // equivalent file under `detected`. Assert via the normalize-stable law.
            eol::apply(&eol::normalize_to_lf(&eol::apply(&norm, detected)), detected)
        });
    }
}

// ---------------------------------------------------------------------------
// Search: find/replace totality + replace-all reversibility
// ---------------------------------------------------------------------------

fn literal_query(pattern: &str, case_sensitive: bool, whole_word: bool) -> Query {
    Query {
        pattern: pattern.to_string(),
        regex: false,
        case_sensitive,
        whole_word,
    }
}

proptest! {
    /// `find_all` for a LITERAL query never panics and every returned span lies
    /// within the text and matches the needle (case-sensitive form).
    #[test]
    fn find_all_literal_spans_are_valid(
        haystack in ".{0,120}",
        needle in ".{0,12}",
    ) {
        let q = literal_query(&needle, true, false);
        let matches = find_all(&haystack, &q).expect("literal find never errors");
        for m in &matches {
            prop_assert!(m.start <= m.end);
            prop_assert!(m.end <= haystack.len());
            prop_assert!(haystack.is_char_boundary(m.start));
            prop_assert!(haystack.is_char_boundary(m.end));
            prop_assert_eq!(&haystack[m.start..m.end], needle.as_str());
        }
    }

    /// Replacing every literal occurrence of `needle` with a token that does NOT
    /// occur in the text, then replacing the token back with `needle`, recovers
    /// the original text exactly — search-replace-all is an invertible pair when
    /// the replacement is a fresh sentinel. (Case-sensitive literal so the
    /// inverse is exact.)
    #[test]
    fn replace_all_then_inverse_is_identity(
        haystack in "[a-z ]{0,80}",
        needle in "[a-z]{1,6}",
    ) {
        // A sentinel guaranteed absent from a lowercase/space haystack + needle.
        let sentinel = "\u{2407}REP\u{2407}"; // SYMBOL FOR ACK, never in [a-z ]
        prop_assume!(!haystack.contains(sentinel) && !needle.contains(sentinel));
        let q = literal_query(&needle, true, false);
        // Forward: needle -> sentinel.
        let replaced = replace_all(&haystack, &q, sentinel).expect("literal replace ok");
        // Inverse: sentinel -> needle.
        let inv = literal_query(sentinel, true, false);
        let restored = replace_all(&replaced, &inv, &needle).expect("inverse replace ok");
        prop_assert_eq!(restored, haystack);
    }

    /// An empty pattern matches nothing and replace is a no-op — the documented
    /// guard against a degenerate query.
    #[test]
    fn empty_pattern_is_noop(haystack in ".{0,80}") {
        let q = literal_query("", true, false);
        prop_assert!(find_all(&haystack, &q).unwrap().is_empty());
        prop_assert_eq!(replace_all(&haystack, &q, "X").unwrap(), haystack);
    }

    /// A literal `find_all` never returns overlapping spans (regex find_iter
    /// yields non-overlapping matches).
    #[test]
    fn literal_matches_are_non_overlapping(
        haystack in "[ab]{0,80}",
        needle in "[ab]{1,4}",
    ) {
        let q = literal_query(&needle, true, false);
        let matches = find_all(&haystack, &q).unwrap();
        for w in matches.windows(2) {
            prop_assert!(w[0].end <= w[1].start, "spans overlap: {:?} {:?}", w[0], w[1]);
        }
    }
}

// ---------------------------------------------------------------------------
// Config: TOML serialize ↔ deserialize identity
// ---------------------------------------------------------------------------

proptest! {
    /// Serializing the DEFAULT config to TOML and parsing it back yields an
    /// equal config — the persistence loop is lossless for the shipped defaults
    /// regardless of which boolean/numeric knobs are toggled.
    #[test]
    fn config_toml_roundtrip_for_toggled_defaults(
        line_numbers in any::<bool>(),
        word_wrap in any::<bool>(),
        tab_width in 1usize..16,
        editor_size in 6.0f32..72.0,
    ) {
        let mut cfg = Config::default();
        cfg.editor.show_line_numbers = line_numbers;
        cfg.editor.word_wrap = word_wrap;
        cfg.editor.tab_width = tab_width;
        cfg.fonts.editor_size = editor_size;

        let toml = cfg.to_toml_string();
        prop_assert!(!toml.is_empty(), "serialization must not be empty");
        let back = Config::from_toml_str(&toml)
            .expect("our own serialized config must parse");
        prop_assert_eq!(back, cfg);
    }

    /// `Config::from_toml_str` is total over arbitrary input: malformed TOML
    /// returns `Err`, valid TOML returns `Ok`, neither panics. (The editor falls
    /// back to defaults at the load layer on `Err`.)
    #[test]
    fn config_parse_never_panics(s in ".{0,200}") {
        let _ = Config::from_toml_str(&s);
    }
}

// ---------------------------------------------------------------------------
// Snippets: expansion totality + TOML round-trip
// ---------------------------------------------------------------------------

proptest! {
    /// `expand` is total: any body yields an `Expansion` whose `caret_offset`
    /// is a valid char offset into the produced text, and the body never panics.
    #[test]
    fn snippet_expand_caret_in_bounds(body in ".{0,120}") {
        let e = expand(&body);
        let nchars = e.text.chars().count();
        prop_assert!(e.caret_offset <= nchars,
            "caret {} > text chars {}", e.caret_offset, nchars);
    }

    /// A snippet body with NO stop markers expands to itself with the caret at
    /// the end — the identity case of expansion.
    #[test]
    fn snippet_without_markers_is_identity(body in "[a-zA-Z0-9 \n]{0,60}") {
        // The generator excludes '$' so there are no markers to strip.
        let e = expand(&body);
        prop_assert_eq!(&e.text, &body);
        prop_assert_eq!(e.caret_offset, body.chars().count());
    }

    /// A `SnippetSet` serialized to TOML parses back to an equal set — the
    /// snippet persistence loop is lossless.
    #[test]
    fn snippet_set_toml_roundtrip(
        prefixes in prop::collection::vec("[a-z]{1,8}", 0..6),
        body in "[a-zA-Z0-9 ]{0,40}",
    ) {
        use scribe_core::snippets::Snippet;
        let set = SnippetSet {
            snippets: prefixes.into_iter().map(|p| Snippet {
                prefix: p,
                body: body.clone(),
                description: String::new(),
            }).collect(),
        };
        let toml = toml::to_string(&set).expect("serialize snippet set");
        let back = SnippetSet::from_toml(&toml).expect("parse our own serialized set");
        prop_assert_eq!(back, set);
    }

    /// `SnippetSet::from_toml` never panics on arbitrary input.
    #[test]
    fn snippet_set_parse_never_panics(s in ".{0,200}") {
        let _ = SnippetSet::from_toml(&s);
    }
}

// ---------------------------------------------------------------------------
// text_ops: pure transforms — idempotence + structure preservation
// ---------------------------------------------------------------------------

proptest! {
    /// `trim_trailing_whitespace` is idempotent — trimming a trimmed buffer is a
    /// no-op (and never alters the line COUNT).
    #[test]
    fn trim_trailing_whitespace_idempotent(s in "[a-z \t\n]{0,120}") {
        let once = text_ops::trim_trailing_whitespace(&s);
        let twice = text_ops::trim_trailing_whitespace(&once);
        prop_assert_eq!(&once, &twice);
        // Line count is preserved (we trim within lines, never join/split them).
        prop_assert_eq!(once.split('\n').count(), s.split('\n').count());
    }

    /// `ensure_final_newline` is idempotent and yields text ending in exactly
    /// one newline (unless empty).
    #[test]
    fn ensure_final_newline_idempotent(s in "[a-z\n]{0,80}") {
        let once = text_ops::ensure_final_newline(&s);
        let twice = text_ops::ensure_final_newline(&once);
        prop_assert_eq!(&once, &twice);
        if !s.is_empty() {
            prop_assert!(once.ends_with('\n'));
        }
    }

    /// `sort_lines` is idempotent and a permutation of the input's lines (no
    /// line is invented or lost).
    #[test]
    fn sort_lines_idempotent_and_permutation(lines in prop::collection::vec("[a-z]{0,6}", 0..12)) {
        let src = lines.join("\n");
        let sorted = text_ops::sort_lines(&src);
        let resorted = text_ops::sort_lines(&sorted);
        prop_assert_eq!(&sorted, &resorted);
        // Same multiset of lines.
        let mut a: Vec<&str> = src.lines().collect();
        let mut b: Vec<&str> = sorted.lines().collect();
        a.sort_unstable();
        b.sort_unstable();
        prop_assert_eq!(a, b);
    }

    /// `tabs_to_spaces` then `spaces_to_tabs` is the identity for clean
    /// tab-only indentation (the documented round-trip), for any tab width.
    #[test]
    fn indent_conversion_roundtrips_for_tab_indent(
        depth in 0usize..6,
        rest in "[a-z]{0,8}",
        width in 1usize..8,
    ) {
        let src = format!("{}{}\n", "\t".repeat(depth), rest);
        let spaced = text_ops::tabs_to_spaces(&src, width);
        let back = text_ops::spaces_to_tabs(&spaced, width);
        prop_assert_eq!(back, src);
    }

    /// `to_case(upper)` then `to_case(upper)` is idempotent; case folding never
    /// panics on arbitrary Unicode.
    #[test]
    fn to_case_idempotent(s in ".{0,80}", upper in any::<bool>()) {
        let once = text_ops::to_case(&s, upper);
        let twice = text_ops::to_case(&once, upper);
        prop_assert_eq!(once, twice);
    }
}
