//! Integration test: the full `Document` lifecycle through the PUBLIC API
//! (open → edit → save → reopen) over REAL files on disk.
//!
//! The inline `#[cfg(test)]` module in `document.rs` and the sibling suites
//! (`crash_safety_atomic_save.rs`, `i18n_encoding_matrix.rs`) cover the
//! atomic-write invariant, the encoding-unit primitives, and the i18n grapheme
//! matrix in isolation. This suite is the COMPLEMENTARY end-to-end surface: it
//! drives `Document::open` / `save` / `save_as` against actual `tempfile`-backed
//! files and asserts the round-trip contract a user depends on:
//!
//!   * content survives open → set_text → save → reopen byte-for-byte;
//!   * the ORIGINAL encoding (UTF-8 ±BOM / UTF-16LE / UTF-16BE / Latin-1) is
//!     preserved across a save/reload cycle (the bytes on disk stay in that
//!     encoding, not silently rewritten to UTF-8);
//!   * the ORIGINAL EOL style (LF / CRLF / CR) is preserved on save even though
//!     the in-memory rope is always LF-normalized;
//!   * `save_as` retargets the document and the new path round-trips;
//!   * dirty-state transitions track open(clean) → edit(dirty) → save(clean);
//!   * the small (rope) open path is exercised end-to-end (the multi-GB mmap
//!     threshold is asserted as a constant contract, not by writing 256 MiB).
//!
//! Public-API only (`scribe_core::{Document, ...}`), disjoint from the crate's
//! internal unit tests.

use scribe_core::document::{Document, LARGE_FILE_THRESHOLD};
use scribe_core::eol::Eol;
use std::path::Path;
use tempfile::tempdir;

/// Write raw bytes to a fresh file under `dir` and return the path.
fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).expect("write fixture");
    p
}

/// UTF-16LE encode `s` with a BOM (mirrors what an editor writes on save).
fn utf16le_with_bom(s: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// UTF-16BE encode `s` with a BOM.
fn utf16be_with_bom(s: &str) -> Vec<u8> {
    let mut out = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_be_bytes());
    }
    out
}

#[test]
fn open_edit_save_reopen_roundtrips_content() {
    let dir = tempdir().unwrap();
    let path = write_file(dir.path(), "doc.txt", b"first line\nsecond line\n");

    let mut doc = Document::open(&path).unwrap();
    assert!(!doc.is_dirty(), "a freshly opened file is clean");
    assert_eq!(doc.text(), "first line\nsecond line\n");

    doc.set_text("first line\nsecond line\nthird line\n");
    assert!(doc.is_dirty(), "set_text marks the document dirty");

    let lossy = doc.save().unwrap();
    assert!(!lossy, "ASCII into UTF-8 is never lossy");
    assert!(!doc.is_dirty(), "a successful save clears the dirty flag");

    // Reopen a brand-new Document from the same path — the edit is durable.
    let reopened = Document::open(&path).unwrap();
    assert_eq!(reopened.text(), "first line\nsecond line\nthird line\n");
    assert!(!reopened.is_dirty());
}

#[test]
fn utf8_no_bom_encoding_preserved_across_save_reload() {
    let dir = tempdir().unwrap();
    // Non-ASCII UTF-8 (no BOM): the on-disk bytes must stay UTF-8.
    let path = write_file(dir.path(), "u8.txt", "héllo wörld 速記\n".as_bytes());

    let mut doc = Document::open(&path).unwrap();
    assert_eq!(doc.encoding().name, "UTF-8");
    assert!(!doc.encoding().had_bom);

    doc.set_text("héllo wörld 速記 + edit\n");
    assert!(!doc.save().unwrap());

    // On-disk bytes are still valid UTF-8 and contain the multibyte glyphs.
    let raw = std::fs::read(&path).unwrap();
    assert_eq!(String::from_utf8(raw).unwrap(), "héllo wörld 速記 + edit\n");

    let reopened = Document::open(&path).unwrap();
    assert_eq!(reopened.encoding().name, "UTF-8");
    assert!(!reopened.encoding().had_bom);
    assert_eq!(reopened.text(), "héllo wörld 速記 + edit\n");
}

#[test]
fn utf8_bom_is_preserved_on_save() {
    let dir = tempdir().unwrap();
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice("with bom\n".as_bytes());
    let path = write_file(dir.path(), "bom.txt", &bytes);

    let mut doc = Document::open(&path).unwrap();
    assert!(doc.encoding().had_bom, "BOM detected on open");
    assert_eq!(doc.text(), "with bom\n");

    doc.set_text("with bom edited\n");
    assert!(!doc.save().unwrap());

    let raw = std::fs::read(&path).unwrap();
    assert_eq!(&raw[..3], &[0xEF, 0xBB, 0xBF], "BOM re-emitted on save");
    let reopened = Document::open(&path).unwrap();
    assert!(reopened.encoding().had_bom);
    assert_eq!(reopened.text(), "with bom edited\n");
}

#[test]
fn utf16le_encoding_roundtrips_through_document_save() {
    let dir = tempdir().unwrap();
    let path = write_file(dir.path(), "u16le.txt", &utf16le_with_bom("café 写本\n"));

    let mut doc = Document::open(&path).unwrap();
    assert_eq!(doc.encoding().name, "UTF-16LE");
    assert!(doc.encoding().had_bom);
    assert_eq!(doc.text(), "café 写本\n");

    doc.set_text("café 写本 edited\n");
    assert!(
        !doc.save().unwrap(),
        "UTF-16 covers all of Unicode (never lossy)"
    );

    // The on-disk file is still UTF-16LE with a BOM (not silently UTF-8).
    let raw = std::fs::read(&path).unwrap();
    assert_eq!(&raw[..2], &[0xFF, 0xFE], "UTF-16LE BOM preserved");
    let reopened = Document::open(&path).unwrap();
    assert_eq!(reopened.encoding().name, "UTF-16LE");
    assert_eq!(reopened.text(), "café 写本 edited\n");
}

#[test]
fn utf16be_encoding_roundtrips_through_document_save() {
    let dir = tempdir().unwrap();
    let path = write_file(dir.path(), "u16be.txt", &utf16be_with_bom("héllo\n"));

    let mut doc = Document::open(&path).unwrap();
    assert_eq!(doc.encoding().name, "UTF-16BE");
    assert_eq!(doc.text(), "héllo\n");

    doc.set_text("héllo BE\n");
    assert!(!doc.save().unwrap());

    let raw = std::fs::read(&path).unwrap();
    assert_eq!(&raw[..2], &[0xFE, 0xFF], "UTF-16BE BOM preserved");
    let reopened = Document::open(&path).unwrap();
    assert_eq!(reopened.encoding().name, "UTF-16BE");
    assert_eq!(reopened.text(), "héllo BE\n");
}

#[test]
fn latin1_encoding_roundtrips_and_lossy_save_is_flagged() {
    let dir = tempdir().unwrap();
    // 0xE9 = 'é' in windows-1252 / Latin-1.
    let path = write_file(dir.path(), "latin1.txt", &[b'c', b'a', b'f', 0xE9, b'\n']);

    let mut doc = Document::open(&path).unwrap();
    // chardetng classifies a lone high byte as a Latin family encoding.
    assert!(
        doc.encoding().name.contains("1252")
            || doc.encoding().name.contains("8859")
            || doc.encoding().name == "UTF-8",
        "detected: {}",
        doc.encoding().name
    );
    assert_eq!(doc.text(), "café\n");

    // Saving a character the encoding CAN represent is not lossy.
    doc.set_text("cafés\n");
    let lossy_ok = doc.save().unwrap();
    let reopened = Document::open(&path).unwrap();
    assert_eq!(reopened.text(), "cafés\n");
    let _ = lossy_ok; // detection-dependent; the reload is the load-bearing check

    // Saving a kanji into a Latin encoding loses data — Document MUST report it.
    let mut doc2 = Document::open(&path).unwrap();
    if !doc2.encoding().name.eq_ignore_ascii_case("UTF-8") {
        doc2.set_text("speak 速記\n");
        let lossy = doc2.save().unwrap();
        assert!(
            lossy,
            "kanji is unmappable in a Latin encoding — must be flagged"
        );
    }
}

#[test]
fn crlf_eol_preserved_across_save_reload() {
    let dir = tempdir().unwrap();
    let path = write_file(dir.path(), "crlf.txt", b"line1\r\nline2\r\nline3\r\n");

    let doc = Document::open(&path).unwrap();
    assert_eq!(doc.eol(), Eol::Crlf, "CRLF detected on open");
    // In-memory text is LF-normalized regardless of on-disk EOL.
    assert_eq!(doc.text(), "line1\nline2\nline3\n");

    let mut doc = doc;
    doc.set_text("line1\nline2\nline3\nline4\n");
    assert!(!doc.save().unwrap());

    // On-disk bytes carry CRLF again — never silently converted to LF.
    let raw = std::fs::read(&path).unwrap();
    assert_eq!(raw, b"line1\r\nline2\r\nline3\r\nline4\r\n");
    let reopened = Document::open(&path).unwrap();
    assert_eq!(reopened.eol(), Eol::Crlf);
}

#[test]
fn cr_eol_preserved_across_save_reload() {
    let dir = tempdir().unwrap();
    let path = write_file(dir.path(), "cr.txt", b"a\rb\rc\r");

    let doc = Document::open(&path).unwrap();
    assert_eq!(doc.eol(), Eol::Cr, "classic-Mac CR detected");
    assert_eq!(doc.text(), "a\nb\nc\n");

    let mut doc = doc;
    doc.set_text("a\nb\nc\nd\n");
    assert!(!doc.save().unwrap());
    let raw = std::fs::read(&path).unwrap();
    assert_eq!(raw, b"a\rb\rc\rd\r");
}

#[test]
fn explicit_eol_change_is_honored_on_save() {
    let dir = tempdir().unwrap();
    let path = write_file(dir.path(), "switch.txt", b"x\ny\n");

    let mut doc = Document::open(&path).unwrap();
    assert_eq!(doc.eol(), Eol::Lf);
    assert!(!doc.is_dirty());

    // The user switches the line-ending style; that is itself an edit.
    doc.set_eol(Eol::Crlf);
    assert!(doc.is_dirty(), "changing EOL marks the doc dirty");
    assert!(!doc.save().unwrap());

    assert_eq!(std::fs::read(&path).unwrap(), b"x\r\ny\r\n");
    let reopened = Document::open(&path).unwrap();
    assert_eq!(reopened.eol(), Eol::Crlf);
}

#[test]
fn save_as_retargets_and_new_path_roundtrips() {
    let dir = tempdir().unwrap();
    let src = write_file(dir.path(), "src.txt", b"original\n");
    let dst = dir.path().join("dst.txt");

    let mut doc = Document::open(&src).unwrap();
    doc.set_text("original\nappended\n");
    assert!(!doc.save_as(&dst).unwrap());

    // Document now points at the new path; the new file round-trips.
    assert_eq!(doc.path(), Some(dst.as_path()));
    assert_eq!(doc.file_name(), "dst.txt");
    assert!(!doc.is_dirty());

    let from_dst = Document::open(&dst).unwrap();
    assert_eq!(from_dst.text(), "original\nappended\n");
    // The original source file is untouched by save_as.
    assert_eq!(std::fs::read(&src).unwrap(), b"original\n");
}

#[test]
fn scratch_buffer_save_without_path_errors_then_save_as_succeeds() {
    let dir = tempdir().unwrap();
    let mut doc = Document::scratch();
    assert!(doc.path().is_none());
    assert_eq!(doc.file_name(), "untitled");

    doc.set_text("scratch content\n");
    // save() with no path is a clean Err, never a panic.
    assert!(doc.save().is_err(), "scratch save with no path must Err");

    let dst = dir.path().join("from-scratch.txt");
    assert!(!doc.save_as(&dst).unwrap());
    assert_eq!(Document::open(&dst).unwrap().text(), "scratch content\n");
}

#[test]
fn dirty_state_transitions_track_the_full_cycle() {
    let dir = tempdir().unwrap();
    let path = write_file(dir.path(), "state.txt", b"v0\n");

    let mut doc = Document::open(&path).unwrap();
    assert!(!doc.is_dirty(), "open → clean");

    doc.set_text("v1\n");
    assert!(doc.is_dirty(), "edit → dirty");

    doc.save().unwrap();
    assert!(!doc.is_dirty(), "save → clean");

    doc.mark_dirty();
    assert!(doc.is_dirty(), "explicit mark_dirty");
    doc.mark_clean();
    assert!(!doc.is_dirty(), "explicit mark_clean");
}

#[test]
fn small_file_opens_via_the_rope_path_not_readonly_large() {
    let dir = tempdir().unwrap();
    // A few KiB is far below LARGE_FILE_THRESHOLD: rope path, fully editable.
    let body = "x".repeat(8 * 1024);
    let path = write_file(dir.path(), "small.txt", body.as_bytes());

    let mut doc = Document::open(&path).unwrap();
    assert!(
        !doc.is_read_only_large(),
        "an 8 KiB file is well below the {LARGE_FILE_THRESHOLD}-byte mmap threshold"
    );
    assert_eq!(doc.len_bytes(), 8 * 1024);
    doc.set_text("edited small\n");
    assert!(!doc.save().unwrap());
    assert_eq!(Document::open(&path).unwrap().text(), "edited small\n");
}

#[test]
fn large_file_threshold_is_the_documented_256_mib_constant() {
    // The mmap/read-only-large boundary is a load-bearing contract: the rope
    // path covers everything below it. We assert the constant rather than write
    // a 256 MiB fixture (which would make the suite I/O-bound for no extra
    // coverage of the round-trip logic above).
    assert_eq!(LARGE_FILE_THRESHOLD, 256 * 1024 * 1024);
}

#[test]
fn language_hint_derives_from_extension() {
    let dir = tempdir().unwrap();
    let rs = write_file(dir.path(), "main.rs", b"fn main() {}\n");
    let doc = Document::open(&rs).unwrap();
    assert_eq!(doc.language_hint().as_deref(), Some("rs"));

    let noext = write_file(dir.path(), "README", b"hi\n");
    assert!(Document::open(&noext).unwrap().language_hint().is_none());
}
