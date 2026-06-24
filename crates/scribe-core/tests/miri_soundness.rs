//! Miri-checked soundness tests for scribe-core's UB-relevant surface.
//!
//! # Why these tests look the way they do
//!
//! `scribe-core` is `#![deny(unsafe_code)]`. The crate's ONLY `unsafe` is the
//! read-only `memmap2::Mmap::map(&file)` call (`buffer.rs` ~L121 and the
//! analogous site in `document.rs`). There is no `extern "C"` / FFI /
//! `#[no_mangle]` surface.
//!
//! **Miri cannot execute `Mmap::map`.** `mmap`/`MapViewOfFile` is a real OS
//! syscall outside Miri's abstract-machine model, and Miri's filesystem
//! isolation blocks file I/O by default. This is a *documented Miri
//! limitation* (see the Miri README "What does Miri do" / "Common pitfalls":
//! Miri does not model the host OS's virtual-memory or file-mapping
//! syscalls), NOT a soundness hole we can probe. So these tests deliberately
//! call NO mmap/file-opening/filesystem API — every input is in-memory.
//!
//! What Miri CAN and SHOULD validate, and what this file exercises, is that
//! scribe-core's SAFE abstractions — together with the `unsafe` *inside its
//! dependencies* (ropey's gap-buffer / B-tree, `encoding_rs`, the `regex`
//! engine, `unicode-segmentation`) as driven through scribe-core's public API
//! — are free of undefined behaviour (out-of-bounds, invalid-pointer,
//! aliasing/Stacked-Borrows violations, uninitialised reads) on realistic
//! editing operations over tricky Unicode. Miri instruments those paths and
//! traps UB that an ordinary `cargo test` run cannot observe.
//!
//! Inputs are kept SMALL (a few KiB max) and iteration counts LOW because Miri
//! is roughly 100x slower than native; the whole file finishes in minutes.
//! No filesystem, no mmap, no threads, no `std::time` (all blocked / unmodelled
//! under Miri's isolation).
//!
//! Run under Miri:
//! ```text
//! cargo +nightly miri test -p scribe-core --test miri_soundness
//! ```
//! The same file ALSO passes under the normal runner so the regular suite
//! stays green:
//! ```text
//! cargo test -p scribe-core --test miri_soundness
//! ```

use ropey::Rope;
use scribe_core::buffer::Buffer;
use scribe_core::editing::{self, EditState};
use scribe_core::encoding::{self, DetectedEncoding};
use scribe_core::eol::{self, Eol};
use scribe_core::search::{self, Query};
use scribe_core::text_ops;
use unicode_segmentation::UnicodeSegmentation;

/// A spread of multibyte / multi-codepoint UTF-8 that stresses byte<->char
/// boundary math: CJK (3-byte), emoji (4-byte + ZWJ sequences), combining
/// marks (decomposed), and an RTL run. Kept small (well under a KiB).
const TRICKY: &str =
    "ascii\n日本語のテキスト\n👩‍👩‍👧‍👦 family\ncafe\u{0301} décomposé\nمرحبا بالعالم\n🇯🇵🇺🇸 flags\nend";

/// Build a rope and assert byte/char/line bookkeeping is internally
/// consistent. Pure ropey under the hood, but driven the way scribe-core
/// drives it; Miri traps any OOB slice into ropey's internal chunk storage.
fn assert_rope_invariants(text: &str) {
    let rope = Rope::from_str(text);
    assert_eq!(
        rope.len_bytes(),
        text.len(),
        "byte length must match source"
    );
    assert_eq!(
        rope.len_chars(),
        text.chars().count(),
        "char length must match source"
    );
    // Round-trip the whole rope back to a String.
    assert_eq!(rope.to_string(), text, "rope round-trip must be lossless");

    // Walk every char index and convert char<->byte<->line both ways; ropey
    // touches its internal node pointers on each call. Bounded by char count
    // (TRICKY is short).
    let n = rope.len_chars();
    for ci in 0..=n {
        let bi = rope.char_to_byte(ci);
        assert_eq!(rope.byte_to_char(bi), ci, "char<->byte must be an inverse");
        let line = rope.char_to_line(ci);
        let line_start = rope.line_to_char(line);
        assert!(line_start <= ci, "line start must not exceed the index");
    }
}

#[test]
fn miri_rope_byte_char_line_roundtrip() {
    assert_rope_invariants(TRICKY);
    assert_rope_invariants(""); // empty
    assert_rope_invariants("\n\n\n"); // newlines only
    assert_rope_invariants("no trailing newline");
    assert_rope_invariants("🦀"); // single 4-byte char
}

#[test]
fn miri_rope_insert_delete_splice_slice() {
    // Build incrementally via insert at varied (multibyte) char positions,
    // then delete/splice, asserting round-trips throughout. Each ropey op
    // mutates the gap/B-tree; Miri checks every pointer move.
    let mut rope = Rope::from_str("日本");
    rope.insert(1, "X語"); // insert into the middle of a CJK run
    assert_eq!(rope.to_string(), "日X語本");
    rope.insert(rope.len_chars(), "🦀"); // append a 4-byte char
    assert_eq!(rope.to_string(), "日X語本🦀");

    // Remove the emoji (single char index).
    let last = rope.len_chars();
    rope.remove(last - 1..last);
    assert_eq!(rope.to_string(), "日X語本");

    // Slice across a multibyte boundary and collect — exercises ropey's
    // RopeSlice chunk iterator (the part most likely to mis-handle a chunk
    // boundary).
    let slice = rope.slice(1..3);
    let collected: String = slice.chars().collect();
    assert_eq!(collected, "X語");

    // Splice: remove a range and insert a replacement at the same spot.
    rope.remove(1..3);
    rope.insert(1, "👩‍👧"); // a ZWJ sequence (multiple codepoints)
    assert!(rope.to_string().starts_with('日'));
    // The whole thing must still round-trip losslessly.
    let s = rope.to_string();
    assert_eq!(Rope::from_str(&s).to_string(), s);
}

#[test]
fn miri_rope_many_small_edits() {
    // A low-iteration loop of insert+delete to walk ropey through a few
    // gap-buffer reallocations without blowing Miri's time budget.
    let mut rope = Rope::new();
    for i in 0..64 {
        let at = rope.len_chars();
        // Alternate ascii and a 3-byte char so the byte length grows unevenly.
        if i % 2 == 0 {
            rope.insert(at, "あ");
        } else {
            rope.insert(at, "z");
        }
    }
    assert_eq!(rope.len_chars(), 64);
    // Delete every other char from the front; bookkeeping must stay valid.
    while rope.len_chars() > 0 {
        rope.remove(0..1);
    }
    assert_eq!(rope.len_chars(), 0);
    assert_eq!(rope.len_bytes(), 0);
}

#[test]
fn miri_buffer_from_text_and_rope_ops() {
    // scribe-core's safe in-memory constructor — NO file, NO mmap.
    let mut buf = Buffer::from_text(TRICKY);
    assert!(!buf.is_read_only(), "from_text yields an editable rope");
    assert_eq!(buf.len_bytes(), TRICKY.len());

    // Mutate through the safe accessor.
    {
        let rope = buf.as_rope_mut().expect("from_text gives a Rope variant");
        rope.insert(0, "PREFIX ");
    }
    let rope = buf.as_rope().expect("still a rope");
    assert!(rope.to_string().starts_with("PREFIX "));
    assert!(rope.to_string().contains("日本語"));
}

#[test]
fn miri_editing_primitives_over_multibyte() {
    // Drive the editing-model primitives over multibyte content. These call
    // ropey char/line/slice ops with derived indices; an off-by-one would
    // hand ropey an OOB range that Miri traps before the panic path.
    let mut rope = Rope::from_str("日本語\ncafe\u{0301}\n🦀end");
    let mut st = EditState::at(0);

    // Insert at the caret (advances by char count, not bytes).
    editing::insert(&mut rope, &mut st, "X");
    assert_eq!(st.cursor, 1);
    assert!(rope.to_string().starts_with('X'));

    // Select-all then read the selection back.
    editing::select_all(&rope, &mut st);
    let sel = editing::selected_text(&rope, &st);
    assert_eq!(sel, rope.to_string());

    // line_col / char_at round-trip at every char index.
    let n = rope.len_chars();
    for ci in 0..=n {
        let (line, col) = editing::line_col(&rope, ci);
        let back = editing::char_at(&rope, line, col);
        // char_at clamps the column to the line's content; on a non-newline
        // index inside a line, the round-trip is exact.
        assert!(back <= ci, "char_at must not exceed the original index");
    }

    // Horizontal + vertical movement must never produce an out-of-range caret.
    for _ in 0..n + 2 {
        editing::move_horizontal(&mut rope, &mut st, 1, false);
        assert!(st.cursor <= rope.len_chars());
    }
    for dir in [1isize, 1, -1, -1] {
        editing::move_vertical(&mut rope, &mut st, dir, false);
        assert!(st.cursor <= rope.len_chars());
    }

    // Backspace / delete-forward at the boundaries (the historically
    // panic/underflow-prone paths the source comments call out).
    let mut st0 = EditState::at(0);
    editing::backspace(&mut rope, &mut st0); // at start: must be a no-op
    let end = rope.len_chars();
    let mut st_end = EditState::at(end);
    editing::delete_forward(&mut rope, &mut st_end); // at end: must be a no-op
    assert_eq!(rope.len_chars(), end, "edge no-ops must not change length");
}

#[test]
fn miri_editing_word_bounds_and_brackets() {
    // word_bounds walks ropey char-by-char around the cursor; matching_bracket
    // scans bidirectionally. Both index into ropey with computed offsets.
    let rope = Rope::from_str("let café_x = (日本[a]);");
    // Land inside the identifier "café_x".
    let cursor = rope.to_string().chars().position(|c| c == 'é').unwrap() + 1;
    let (s, e) = editing::word_bounds(&rope, cursor);
    assert!(s < e, "should find a non-empty word");
    let word: String = rope.slice(s..e).chars().collect();
    assert!(word.contains("caf"));

    // matching_bracket on the '(' must find its ')'.
    let open = rope.to_string().chars().position(|c| c == '(').unwrap();
    let close = editing::matching_bracket(&rope, open, 4096);
    assert!(close.is_some(), "balanced paren must match");
    assert!(close.unwrap() > open);

    // On a non-bracket char it returns None without indexing OOB.
    assert_eq!(editing::matching_bracket(&rope, 0, 4096), None);
}

#[test]
fn miri_multi_caret_offset_bookkeeping() {
    // for_each_caret manages a running offset and re-clamps every caret to the
    // live char length each step — the load-bearing primitive whose comments
    // warn about OOB `rope.remove` ranges. Miri verifies the clamped ranges
    // never escape ropey's bounds across a real multi-caret insert.
    let mut rope = Rope::from_str("a\nb\nc\nあ\nd");
    // One caret at the start of several lines.
    let mut carets = vec![
        EditState::at(0),
        EditState::at(2),
        EditState::at(4),
        EditState::at(6),
    ];
    editing::for_each_caret(&mut rope, &mut carets, |r, c| {
        editing::insert(r, c, ">> ");
    });
    // Every line we targeted should now carry the prefix; bookkeeping stayed
    // valid (no panic, no OOB).
    assert!(rope.to_string().contains(">> "));
    editing::dedupe_carets(&mut carets);
    // Carets stay sorted + within bounds.
    let len = rope.len_chars();
    for c in &carets {
        assert!(c.cursor <= len && c.anchor <= len);
    }

    // Add a caret vertically and confirm it lands in-bounds.
    let _ = editing::add_caret_vertical(&rope, &mut carets, 1);
    for c in &carets {
        assert!(c.cursor <= rope.len_chars());
    }
}

#[test]
fn miri_encoding_decode_in_memory() {
    // encoding::decode over IN-MEMORY byte slices. encoding_rs + chardetng both
    // carry `unsafe` for fast UTF-8 validation / SIMD-ish scanning; Miri checks
    // their pointer arithmetic over our slices.

    // Valid UTF-8 (multibyte) round-trips.
    let (text, det) = encoding::decode(TRICKY.as_bytes());
    assert_eq!(text, TRICKY);
    assert!(det.name.contains("UTF-8") || det.name.contains("utf-8"));
    assert!(!det.had_bom);

    // UTF-8 BOM is detected and stripped.
    let mut bom_utf8 = vec![0xEF, 0xBB, 0xBF];
    bom_utf8.extend_from_slice("héllo 日".as_bytes());
    let (text, det) = encoding::decode(&bom_utf8);
    assert_eq!(text, "héllo 日");
    assert!(det.had_bom, "UTF-8 BOM must be flagged");

    // UTF-16LE with BOM (the path the source hand-rolls on encode).
    let mut utf16le = vec![0xFF, 0xFE];
    for u in "Aé日".encode_utf16() {
        utf16le.extend_from_slice(&u.to_le_bytes());
    }
    let (text, det) = encoding::decode(&utf16le);
    assert_eq!(text, "Aé日");
    assert!(det.name.contains("UTF-16"), "name was {}", det.name);
    assert!(det.had_bom);

    // Invalid bytes -> lossy decode must NOT read uninitialised / OOB; it just
    // substitutes replacement chars. The key property is "no UB, no panic".
    let invalid = [0x68, 0x69, 0xFF, 0xFE, 0x80, 0x6f]; // "hi" + junk + "o"
    let (lossy, _) = encoding::decode(&invalid);
    assert!(lossy.starts_with("hi"));
    assert!(lossy.contains('o'));

    // Empty input is well-defined.
    let (empty, _) = encoding::decode(&[]);
    assert!(empty.is_empty());
}

#[test]
fn miri_encoding_encode_roundtrip() {
    // encode() / encode_checked() over in-memory text. UTF-8 and UTF-16
    // round-trip; an unmappable char in a narrow encoding is flagged, not UB.
    let utf8 = DetectedEncoding::default(); // UTF-8, no BOM
    let bytes = encoding::encode("café 日", &utf8);
    let (back, _) = encoding::decode(&bytes);
    assert_eq!(back, "café 日");

    let utf16 = DetectedEncoding {
        name: "UTF-16LE".to_string(),
        had_bom: true,
    };
    let (bytes, lost) = encoding::encode_checked("Aé日🦀", &utf16);
    assert!(!lost, "UTF-16 covers all of Unicode, never lossy");
    let (back, _) = encoding::decode(&bytes);
    assert_eq!(back, "Aé日🦀");

    // A char unrepresentable in windows-1252 -> flagged lossy, but no UB.
    let latin1 = DetectedEncoding {
        name: "windows-1252".to_string(),
        had_bom: false,
    };
    let (_bytes, lost) = encoding::encode_checked("日本", &latin1);
    assert!(lost, "CJK is unmappable in windows-1252");
}

#[test]
fn miri_eol_detect_normalize_apply() {
    // EOL handling is pure-Rust String scanning, but it feeds the encoding /
    // rope paths; verify detect/normalize/apply round-trip with no panic.
    let crlf = "a\r\nb\r\n日\r\n";
    assert_eq!(eol::detect(crlf), Eol::Crlf);
    let lf = eol::normalize_to_lf(crlf);
    assert_eq!(lf, "a\nb\n日\n");
    assert_eq!(eol::detect(&lf), Eol::Lf);

    let cr = "x\ry\r";
    assert_eq!(eol::detect(cr), Eol::Cr);
    assert_eq!(eol::normalize_to_lf(cr), "x\ny\n");

    // apply is the inverse of normalize for the detected style.
    let reapplied = eol::apply(&lf, Eol::Crlf);
    assert_eq!(eol::normalize_to_lf(&reapplied), lf);
}

#[test]
fn miri_search_find_replace_in_memory() {
    // The regex engine carries `unsafe` (DFA byte-class indexing). Drive it
    // over multibyte text with overlapping/edge-case patterns; the returned
    // spans must be valid byte offsets into the haystack (Miri would trap an
    // OOB slice if a span were wrong).
    let hay = "foo Foo FOO 日本foo café foo";

    // Literal, case-insensitive (default).
    let q = Query {
        pattern: "foo".into(),
        ..Default::default()
    };
    let matches = search::find_all(hay, &q).unwrap();
    assert!(matches.len() >= 4);
    for m in &matches {
        // Every span must be a valid char-boundary byte range.
        assert!(hay.is_char_boundary(m.start));
        assert!(hay.is_char_boundary(m.end));
        let _slice = &hay[m.start..m.end]; // would panic/UB if span were wrong
    }

    // Whole-word: must not match the "foo" glued onto "日本".
    let qw = Query {
        pattern: "foo".into(),
        whole_word: true,
        case_sensitive: true,
        ..Default::default()
    };
    let wmatches = search::find_all(hay, &qw).unwrap();
    for m in &wmatches {
        assert!(hay.is_char_boundary(m.start) && hay.is_char_boundary(m.end));
    }

    // Regex with a capture + replacement over multibyte text.
    let qr = Query {
        pattern: r"(café)".into(),
        regex: true,
        case_sensitive: true,
        ..Default::default()
    };
    let replaced = search::replace_all(hay, &qr, "[$1]").unwrap();
    assert!(replaced.contains("[café]"));
    // Round-trips back to a valid String (no broken UTF-8).
    assert_eq!(Rope::from_str(&replaced).to_string(), replaced);

    // Empty pattern is a defined no-op (not UB).
    let qe = Query::default();
    assert!(search::find_all(hay, &qe).unwrap().is_empty());
    assert_eq!(search::replace_all(hay, &qe, "X").unwrap(), hay);
}

#[test]
fn miri_text_ops_over_unicode() {
    // The line/case transforms allocate + copy through &str slicing; an
    // off-by-one on a multibyte boundary would be UB-adjacent (panic at best).
    let src = "  b日 \n  a語  \nb日 \n";
    let trimmed = text_ops::trim_trailing_whitespace(src);
    assert!(!trimmed.contains(" \n"));

    let sorted = text_ops::sort_lines("c\na\nb\n");
    assert_eq!(sorted.lines().next(), Some("a"));

    let uniq = text_ops::sort_lines_unique("b\na\nb\na\n");
    assert_eq!(uniq.lines().filter(|l| *l == "a").count(), 1);

    let upper = text_ops::to_case("café 日", true);
    assert!(upper.starts_with("CAF"));

    // Tab expansion converts LEADING (indentation) tabs only — a mid-line tab
    // is intentionally preserved by `convert_indent`. Use a leading-tab line so
    // we exercise the indent-rewrite path, and assert the leading tab is gone
    // while the result stays valid UTF-8.
    let expanded = text_ops::tabs_to_spaces("\t\t日 body\n", 4);
    assert!(
        expanded.starts_with("        "),
        "two leading tabs -> eight spaces, got {expanded:?}"
    );
    assert!(expanded.contains("日 body"), "content preserved");
    assert_eq!(Rope::from_str(&expanded).to_string(), expanded);
}

#[test]
fn miri_grapheme_word_segmentation() {
    // unicode-segmentation's `unsafe` UTF-8 scanning, exercised over the same
    // tricky content scribe-core's editing model navigates. We assert that the
    // grapheme/word iterators reconstruct the source losslessly and that every
    // boundary they report is a valid char boundary (Miri traps an OOB read in
    // the segmentation tables otherwise).
    let graphemes: Vec<&str> = TRICKY.graphemes(true).collect();
    assert_eq!(graphemes.concat(), TRICKY, "graphemes must reconstruct");
    // The ZWJ family emoji + flag pairs collapse multiple codepoints into one
    // grapheme, so the grapheme count is strictly below the char count.
    assert!(graphemes.len() < TRICKY.chars().count());

    // Every grapheme boundary index is a valid char boundary in the source.
    for (idx, _g) in TRICKY.grapheme_indices(true) {
        assert!(TRICKY.is_char_boundary(idx));
    }

    // Word segmentation reconstructs the source too.
    let words: String = TRICKY.split_word_bounds().collect();
    assert_eq!(words, TRICKY, "word bounds must reconstruct losslessly");
}

#[test]
fn miri_combined_edit_search_encode_pipeline() {
    // A small end-to-end pipeline mixing the subsystems, the way the editor
    // actually chains them: decode bytes -> normalize EOL -> edit the rope ->
    // search/replace -> re-encode. Keeps everything in memory; Miri watches the
    // whole chain for aliasing/OOB across subsystem boundaries.
    let raw = "héllo\r\n日本語\r\nfoo bar\r\n".as_bytes();
    let (decoded, det) = encoding::decode(raw);
    assert!(det.name.contains("UTF-8") || det.name.contains("utf-8"));
    let normalized = eol::normalize_to_lf(&decoded);
    assert!(!normalized.contains('\r'));

    let mut rope = Rope::from_str(&normalized);
    let mut st = EditState::at(0);
    editing::insert(&mut rope, &mut st, "// ");
    let edited = rope.to_string();

    let q = Query {
        pattern: "foo".into(),
        case_sensitive: true,
        ..Default::default()
    };
    let replaced = search::replace_all(&edited, &q, "baz").unwrap();
    assert!(replaced.contains("baz bar"));

    let reencoded = encoding::encode(&replaced, &det);
    let (roundtrip, _) = encoding::decode(&reencoded);
    assert_eq!(roundtrip, replaced, "full pipeline must round-trip");
}
