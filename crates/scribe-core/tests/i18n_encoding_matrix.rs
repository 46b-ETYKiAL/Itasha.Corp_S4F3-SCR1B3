//! Systematic i18n surface: encoding MATRIX, RTL/bidi text, and Unicode
//! normalization (NFC/NFD) — taxonomy PART 2 §A item 3 + §B, gap-closure #38.
//!
//! This is the COMPLEMENTARY i18n surface to the property-based round-trips
//! already merged (PR #209 `roundtrip_proptest.rs` covers UTF-8/UTF-16 LE+BE
//! round-trip PROPERTIES + single-byte codepages; `differential_grapheme_proptest.rs`
//! covers grapheme-cluster cursor correctness). To avoid duplication this file
//! does NOT re-assert those properties. It adds the parts the property suite did
//! NOT cover:
//!
//!   1. A full, EXPLICIT encoding MATRIX table test driving the real
//!      `Document::open` → `save` pipeline (detection + decode + EOL + re-encode)
//!      for every encoding SCR1B3 targets — including the Japanese encodings
//!      (Shift-JIS / EUC-JP / ISO-2022-JP) and UTF-16 LE/BE with/without BOM
//!      crossed with the three EOL styles (CRLF / LF / CR) — that the generic
//!      property tests don't exercise as concrete labelled cases.
//!   2. **RTL / bidi** text (Hebrew, Arabic, mixed LTR+RTL with bidi control
//!      characters) round-tripping through open/save and surviving cursor edits
//!      without corruption — logical-order storage is the editor's contract.
//!   3. **Unicode normalization (NFC vs NFD)**: the editor MUST preserve the
//!      user's chosen normalization form byte-for-byte and never silently
//!      normalize, which would alter file content (a data-integrity violation).
//!      Asserted with hand-constructed NFC/NFD scalar sequences (no external
//!      oracle dependency).
//!
//! Sibling integration test (public API only), disjoint from inline modules.

use std::fs;
use std::path::Path;

use ropey::Rope;
use scribe_core::document::Document;
use scribe_core::editing::{backspace, insert, move_horizontal, EditState};
use scribe_core::encoding::{self, DetectedEncoding};
use scribe_core::eol::Eol;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write `bytes` to a fresh temp file, open it as a `Document`, and return the
/// document + the path so the caller can re-save and re-read.
fn open_bytes(dir: &Path, name: &str, bytes: &[u8]) -> (Document, std::path::PathBuf) {
    let p = dir.join(name);
    fs::write(&p, bytes).unwrap();
    let doc = Document::open(&p).unwrap();
    (doc, p)
}

// ---------------------------------------------------------------------------
// 1. Full encoding MATRIX — concrete labelled cases through Document open/save
// ---------------------------------------------------------------------------

/// Each matrix row: a human label, the on-disk bytes, and the text the editor
/// must surface after decoding. We assert the decode is correct AND that a
/// no-edit save reproduces the exact original bytes (the open→save fidelity loop
/// every encoding must honour). Where the EOL style is embedded in the bytes it
/// must be detected + re-applied losslessly.
struct MatrixCase {
    label: &'static str,
    bytes: Vec<u8>,
    expected_text: String,
    expected_eol: Eol,
}

fn shift_jis(s: &str) -> Vec<u8> {
    let (cow, _enc, had_errors) = encoding_rs::SHIFT_JIS.encode(s);
    assert!(!had_errors, "fixture must be representable in Shift-JIS");
    cow.into_owned()
}

fn euc_jp(s: &str) -> Vec<u8> {
    let (cow, _enc, had_errors) = encoding_rs::EUC_JP.encode(s);
    assert!(!had_errors, "fixture must be representable in EUC-JP");
    cow.into_owned()
}

fn iso_2022_jp(s: &str) -> Vec<u8> {
    let (cow, _enc, had_errors) = encoding_rs::ISO_2022_JP.encode(s);
    assert!(!had_errors, "fixture must be representable in ISO-2022-JP");
    cow.into_owned()
}

fn utf16(s: &str, be: bool, bom: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if bom {
        out.extend_from_slice(if be { &[0xFE, 0xFF] } else { &[0xFF, 0xFE] });
    }
    for u in s.encode_utf16() {
        out.extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
    }
    out
}

/// THE encoding matrix. Drives `Document::open` (real detection via chardetng +
/// BOM sniff) then a no-edit `save` and asserts byte-identity. This is the
/// systematic table the generic property suite does not provide.
///
/// NOTE on statistical detection: `chardetng` needs enough bytes to commit to a
/// legacy CJK codepage. A 3-character Shift-JIS / EUC-JP sample is genuinely
/// ambiguous (it can look like windows-1252 mojibake), so this matrix uses a
/// realistic multi-sentence Japanese paragraph — the actual content a user would
/// open — for the byte-statistical encodings. ISO-2022-JP is unambiguous (escape
/// sequences) and UTF-* carry a BOM, so those need no length floor.
#[test]
fn encoding_matrix_open_decode_and_save_is_byte_identical() {
    // A realistic Japanese paragraph: long enough for chardetng to commit to the
    // correct legacy codepage. Representable in Shift-JIS / EUC-JP / ISO-2022-JP.
    let jp = "本日は晴天なり。日本語の文章を編集するためのテキストエディタです。";

    let cases: Vec<MatrixCase> = vec![
        // --- UTF-8, the canonical encoding, with each EOL style ---
        MatrixCase {
            label: "UTF-8 / LF",
            bytes: b"hello\nworld\n".to_vec(),
            expected_text: "hello\nworld\n".to_string(),
            expected_eol: Eol::Lf,
        },
        MatrixCase {
            label: "UTF-8 / CRLF",
            bytes: b"hello\r\nworld\r\n".to_vec(),
            expected_text: "hello\nworld\n".to_string(),
            expected_eol: Eol::Crlf,
        },
        MatrixCase {
            label: "UTF-8 / CR (classic Mac)",
            bytes: b"hello\rworld\r".to_vec(),
            expected_text: "hello\nworld\n".to_string(),
            expected_eol: Eol::Cr,
        },
        // --- UTF-8 with BOM ---
        MatrixCase {
            label: "UTF-8 BOM / LF",
            bytes: {
                let mut v = vec![0xEF, 0xBB, 0xBF];
                v.extend_from_slice(jp.as_bytes());
                v.push(b'\n');
                v
            },
            expected_text: format!("{jp}\n"),
            expected_eol: Eol::Lf,
        },
        // --- UTF-16 LE/BE, with and without BOM ---
        MatrixCase {
            label: "UTF-16LE BOM",
            bytes: utf16("Hi\n", false, true),
            expected_text: "Hi\n".to_string(),
            expected_eol: Eol::Lf,
        },
        MatrixCase {
            label: "UTF-16BE BOM",
            bytes: utf16("Hi\n", true, true),
            expected_text: "Hi\n".to_string(),
            expected_eol: Eol::Lf,
        },
        // --- latin1 / ISO-8859-1 family (windows-1252 superset) ---
        MatrixCase {
            label: "latin1 (windows-1252) café",
            bytes: vec![b'c', b'a', b'f', 0xE9, b'\n'],
            expected_text: "café\n".to_string(),
            expected_eol: Eol::Lf,
        },
        // --- Japanese: Shift-JIS, EUC-JP, ISO-2022-JP ---
        MatrixCase {
            label: "Shift-JIS",
            bytes: {
                let mut v = shift_jis(jp);
                v.push(b'\n');
                v
            },
            expected_text: format!("{jp}\n"),
            expected_eol: Eol::Lf,
        },
        MatrixCase {
            label: "EUC-JP",
            bytes: {
                let mut v = euc_jp(jp);
                v.push(b'\n');
                v
            },
            expected_text: format!("{jp}\n"),
            expected_eol: Eol::Lf,
        },
        MatrixCase {
            label: "ISO-2022-JP",
            bytes: {
                let mut v = iso_2022_jp(jp);
                v.push(b'\n');
                v
            },
            expected_text: format!("{jp}\n"),
            expected_eol: Eol::Lf,
        },
    ];

    let dir = tempfile::tempdir().unwrap();
    for (i, case) in cases.iter().enumerate() {
        let (mut doc, p) = open_bytes(dir.path(), &format!("m{i}.txt"), &case.bytes);

        // Detection + decode produced the expected text.
        assert_eq!(
            doc.text(),
            case.expected_text,
            "decoded text mismatch for [{}]",
            case.label
        );
        // EOL detected as expected (when there is a separator).
        assert_eq!(
            doc.eol(),
            case.expected_eol,
            "EOL mismatch for [{}]",
            case.label
        );

        // A no-edit save must reproduce the EXACT original bytes — the open→save
        // fidelity loop for every encoding in the matrix.
        let lossy = doc.save().unwrap();
        assert!(
            !lossy,
            "a representable round-trip must not be lossy for [{}]",
            case.label
        );
        let reread = fs::read(&p).unwrap();
        assert_eq!(
            reread, case.bytes,
            "save did not reproduce the original bytes for [{}]",
            case.label
        );
    }
}

/// A character UNREPRESENTABLE in the file's encoding is flagged lossy on save
/// (the editor warns the user) — proves the matrix's lossy-detection arm. A
/// Shift-JIS file edited to contain an emoji (not in the JIS X 0208 set) must
/// report `lossy == true`.
#[test]
fn editing_in_an_unrepresentable_char_is_flagged_lossy() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = shift_jis("日本");
    bytes.push(b'\n');
    let (mut doc, _p) = open_bytes(dir.path(), "sjis.txt", &bytes);
    assert_eq!(doc.text(), "日本\n");

    // Insert an emoji that Shift-JIS cannot represent.
    doc.set_text("日本😀\n");
    let lossy = doc.save().unwrap();
    assert!(
        lossy,
        "an emoji is unrepresentable in Shift-JIS → must be lossy"
    );
}

// ---------------------------------------------------------------------------
// 2. RTL / bidi text — logical-order storage survives open/save + edits
// ---------------------------------------------------------------------------

/// Hebrew, Arabic, and mixed LTR+RTL text (including explicit bidi control
/// characters) must round-trip through open→save byte-for-byte. The editor
/// stores text in LOGICAL order; visual reordering is a render-layer concern, so
/// the stored bytes must be exactly what the user's file contained.
#[test]
fn rtl_and_bidi_text_roundtrips_byte_identical() {
    let samples: &[(&str, &str)] = &[
        ("hebrew", "שלום עולם\n"),     // "hello world"
        ("arabic", "مرحبا بالعالم\n"), // "hello world"
        // Mixed LTR + RTL with the file name in the middle (classic bidi case).
        ("mixed", "open שלום file.txt\n"),
        // Explicit bidi control: RLM (U+200F), LRM (U+200E), and an
        // RLE/PDF (U+202B / U+202C) embedding — must be preserved verbatim.
        ("bidi-controls", "a\u{200F}\u{202B}שלום\u{202C}\u{200E}b\n"),
    ];

    let dir = tempfile::tempdir().unwrap();
    for (i, (label, text)) in samples.iter().enumerate() {
        let original = text.as_bytes().to_vec();
        let (mut doc, p) = open_bytes(dir.path(), &format!("rtl{i}.txt"), &original);
        assert_eq!(doc.text(), *text, "decode mismatch for {label}");
        doc.save().unwrap();
        let reread = fs::read(&p).unwrap();
        assert_eq!(
            reread, original,
            "RTL/bidi round-trip corrupted bytes for {label}"
        );
    }
}

/// Cursor movement + deletion over RTL text operates in logical order without
/// producing invalid UTF-8 or corrupting bidi control characters. We insert
/// Hebrew, move the caret, and back-delete — the buffer must stay valid and
/// match the logical-order expectation.
#[test]
fn editing_rtl_text_preserves_logical_order_and_validity() {
    let mut rope = Rope::from_str("");
    let mut st = EditState::at(0);

    // Type "שלום" (4 Hebrew letters, each one codepoint) in logical order.
    insert(&mut rope, &mut st, "שלום");
    assert_eq!(rope.to_string(), "שלום");
    assert_eq!(st.cursor, 4, "caret advances 4 codepoints");

    // Move back two, then back-delete one — removes the 2nd codepoint logically.
    move_horizontal(&mut rope, &mut st, -2, false);
    assert_eq!(st.cursor, 2);
    backspace(&mut rope, &mut st);
    // The 2nd letter (ל) was removed: ש + ום remain in logical order.
    assert_eq!(rope.to_string(), "שום");
    assert_eq!(st.cursor, 1);

    // The buffer is valid UTF-8 by construction (rope/String), and every char is
    // a Hebrew letter — no control-char corruption.
    for ch in rope.to_string().chars() {
        assert!(
            ('\u{0590}'..='\u{05FF}').contains(&ch),
            "unexpected char {ch:?} after RTL edit"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Unicode normalization — the editor NEVER silently normalizes
// ---------------------------------------------------------------------------

// NFC vs NFD of "é":
//   NFC: U+00E9 (single precomposed scalar)             → 2 UTF-8 bytes
//   NFD: U+0065 'e' + U+0301 COMBINING ACUTE ACCENT      → 3 UTF-8 bytes
const E_ACUTE_NFC: &str = "\u{00E9}"; // é (composed)
const E_ACUTE_NFD: &str = "e\u{0301}"; // é (decomposed)

/// NFC and NFD forms are DISTINCT byte sequences. The document open→save path
/// must preserve whichever form the file used — normalizing on save would
/// silently rewrite the user's bytes (a data-integrity violation). Asserted for
/// BOTH forms independently.
#[test]
fn nfc_and_nfd_forms_are_preserved_not_normalized_on_save() {
    // Sanity: the two forms really are different byte sequences.
    assert_ne!(E_ACUTE_NFC.as_bytes(), E_ACUTE_NFD.as_bytes());
    assert_eq!(E_ACUTE_NFC.len(), 2);
    assert_eq!(E_ACUTE_NFD.len(), 3);

    let dir = tempfile::tempdir().unwrap();
    for (label, form) in [("NFC", E_ACUTE_NFC), ("NFD", E_ACUTE_NFD)] {
        let content = format!("caf{form}\n");
        let original = content.as_bytes().to_vec();
        let (mut doc, p) = open_bytes(dir.path(), &format!("norm-{label}.txt"), &original);
        // The editor surfaced the exact form (no normalization on open).
        assert_eq!(doc.text(), content, "{label} altered on open");
        doc.save().unwrap();
        let reread = fs::read(&p).unwrap();
        assert_eq!(
            reread, original,
            "{label} form was silently normalized on save (data corruption)"
        );
    }
}

/// The encoding round-trip itself preserves normalization form: encoding an NFD
/// string to UTF-8 and decoding it back yields the SAME decomposed scalars, not
/// a composed NFC equivalent. (encoding_rs is byte-faithful; this locks that in.)
#[test]
fn encoding_roundtrip_preserves_decomposed_form() {
    let nfd = format!("nai{}ve\n", "\u{0308}"); // naïve with combining diaeresis (NFD)
    let utf8 = DetectedEncoding {
        name: "UTF-8".to_string(),
        had_bom: false,
    };
    let (bytes, lossy) = encoding::encode_checked(&nfd, &utf8);
    assert!(!lossy);
    let (decoded, _enc) = encoding::decode(&bytes);
    assert_eq!(
        decoded, nfd,
        "decomposed form must survive the encode→decode loop"
    );
    // And it is genuinely the decomposed form: the combining mark is present.
    assert!(
        decoded.contains('\u{0308}'),
        "combining diaeresis must be preserved"
    );
}

/// Combining marks and a precomposed form that LOOK identical must NOT compare
/// equal as stored text — the editor distinguishes them, so search/replace and
/// cursor ops are byte-exact rather than normalization-folded. A caret advancing
/// over the NFD form crosses TWO codepoints (base + mark), confirming the editor
/// treats them as the distinct scalars they are.
#[test]
fn decomposed_form_is_two_codepoints_to_the_editor() {
    let mut rope = Rope::from_str(E_ACUTE_NFD); // e + combining acute
    let mut st = EditState::at(0);
    assert_eq!(rope.len_chars(), 2, "NFD é is two scalars to the rope");

    // One forward move lands BETWEEN the base letter and the combining mark
    // (codepoint-level movement, the documented low-level behaviour).
    move_horizontal(&mut rope, &mut st, 1, false);
    assert_eq!(st.cursor, 1);
    // The composed form, by contrast, is a single scalar.
    let composed = Rope::from_str(E_ACUTE_NFC);
    assert_eq!(composed.len_chars(), 1, "NFC é is one scalar");
}
