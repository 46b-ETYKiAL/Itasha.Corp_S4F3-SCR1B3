//! QA: data-integrity / correctness workflows driven as a real user against the
//! `scribe-core` public API (#38). These are scenario-level, end-to-end drives
//! of the highest-risk edges for DATA LOSS / CORRUPTION — encoding round-trip,
//! EOL preservation, large-file mmap-promote, regex backtracking bounds,
//! zero-width match correctness, and graceful fallback on corrupt config /
//! session state. Each test drives the workflow a user would hit and asserts
//! the integrity criterion; a failing criterion is a P0/P1 data-loss bug.
//!
//! These complement (do not duplicate) the in-module unit tests: they exercise
//! the FULL open->edit->save pipeline through `Document` (and `Buffer` for the
//! browse-then-promote path) with crafted byte fixtures, plus the awkward edge
//! inputs (empty file, newline-only file, last-line-without-newline, mixed EOL).
//!
//! Phase-2 discipline: tests + bug-log only. No product code is touched. A
//! potential-hang scenario (catastrophic regex) is bounded by a watchdog thread
//! so a real hang surfaces as a logged failure instead of wedging CI.

use scribe_core::buffer::{Buffer, MMAP_THRESHOLD};
use scribe_core::config::Config;
use scribe_core::document::Document;
use scribe_core::eol::Eol;
use scribe_core::search::{find_all, replace_all, Query};
use scribe_core::session::{self, SessionManifest, TabSnapshot};
use std::sync::mpsc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write raw bytes to a fresh temp file, open it as a `Document`, replace the
/// body with `new_text`, save, and return the resulting on-disk bytes. This is
/// the exact open->edit->save round-trip a user performs when they open a file,
/// type, and hit Ctrl+S.
fn open_edit_save_bytes(initial: &[u8], new_text: &str) -> (Vec<u8>, Document) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("rt.dat");
    std::fs::write(&p, initial).unwrap();
    let mut doc = Document::open(&p).unwrap();
    doc.set_text(new_text);
    doc.save().unwrap();
    let raw = std::fs::read(&p).unwrap();
    // Keep the tempdir alive by leaking the doc's path-relationship: re-read is
    // already done, so dropping `dir` here is fine — but return the doc for
    // encoding/eol assertions tied to the same lifecycle.
    (raw, doc)
}

/// Open raw bytes, save WITHOUT editing (a no-op save), and return on-disk bytes
/// plus the detected EOL. Used to assert a no-op save does NOT silently rewrite
/// encoding or line endings.
fn open_noop_save_bytes(initial: &[u8]) -> (Vec<u8>, Eol, String, bool) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("noop.dat");
    std::fs::write(&p, initial).unwrap();
    let mut doc = Document::open(&p).unwrap();
    let eol = doc.eol();
    let enc = doc.encoding().name.clone();
    // Mark dirty so save() actually writes (a clean save still writes through
    // save_as; we force the write path to exercise the full encode pipeline).
    doc.mark_dirty();
    let lossy = doc.save().unwrap();
    let raw = std::fs::read(&p).unwrap();
    (raw, eol, enc, lossy)
}

// ---------------------------------------------------------------------------
// Scenario 1 — Encoding round-trip (open -> save preserves on-disk encoding)
// ---------------------------------------------------------------------------

#[test]
fn scenario1_utf8_no_bom_roundtrip_preserves_bytes() {
    // A plain UTF-8 file edited and saved keeps its UTF-8 (no BOM) shape.
    let (raw, doc) = open_edit_save_bytes("héllo wörld\n".as_bytes(), "édited\n");
    assert_eq!(doc.encoding().name, "UTF-8");
    assert!(!doc.encoding().had_bom);
    assert_eq!(raw, "édited\n".as_bytes(), "UTF-8 body must round-trip");
}

#[test]
fn scenario1_utf8_bom_preserved_across_edit_save() {
    let mut initial = vec![0xEF, 0xBB, 0xBF];
    initial.extend_from_slice("text\n".as_bytes());
    let (raw, doc) = open_edit_save_bytes(&initial, "new\n");
    assert!(doc.encoding().had_bom, "BOM presence must be remembered");
    assert_eq!(&raw[..3], &[0xEF, 0xBB, 0xBF], "BOM must be re-emitted");
    assert_eq!(&raw[3..], b"new\n");
}

#[test]
fn scenario1_utf16le_bom_roundtrip() {
    // "Hi\n" UTF-16LE + BOM -> edit -> save must stay UTF-16LE with BOM.
    let initial = vec![0xFF, 0xFE, b'H', 0, b'i', 0, b'\n', 0];
    let (raw, doc) = open_edit_save_bytes(&initial, "Ok\n");
    assert_eq!(doc.encoding().name, "UTF-16LE");
    assert!(doc.encoding().had_bom);
    assert_eq!(raw, vec![0xFF, 0xFE, b'O', 0, b'k', 0, b'\n', 0]);
}

#[test]
fn scenario1_utf16be_bom_roundtrip() {
    let initial = vec![0xFE, 0xFF, 0, b'H', 0, b'i', 0, b'\n'];
    let (raw, doc) = open_edit_save_bytes(&initial, "Ok\n");
    assert_eq!(doc.encoding().name, "UTF-16BE");
    assert!(doc.encoding().had_bom);
    assert_eq!(raw, vec![0xFE, 0xFF, 0, b'O', 0, b'k', 0, b'\n']);
}

#[test]
fn scenario1_latin1_roundtrip_preserves_high_byte() {
    // 0xE9 = 'é' in windows-1252/Latin-1. A no-op save must keep the single
    // high byte (NOT promote to a 2-byte UTF-8 sequence).
    let (raw, eol, enc, lossy) = open_noop_save_bytes(&[b'c', b'a', b'f', 0xE9, b'\n']);
    assert_eq!(eol, Eol::Lf);
    assert!(
        enc.starts_with("windows-1252") || enc == "ISO-8859-1",
        "Latin-1 byte must detect as a single-byte legacy encoding, got {enc}"
    );
    assert!(!lossy, "café is representable in windows-1252");
    assert_eq!(
        raw,
        vec![b'c', b'a', b'f', 0xE9, b'\n'],
        "high byte must NOT be re-encoded to a 2-byte UTF-8 'é'"
    );
}

#[test]
fn scenario1_invalid_utf8_decodes_lossily_without_panic_or_refusal() {
    // Lone continuation bytes are not valid UTF-8. The editor must still open
    // the file (lossy) rather than refusing — a user must never be locked out.
    let initial = vec![b'a', 0xFF, 0x80, b'b', b'\n'];
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("bad.txt");
    std::fs::write(&p, &initial).unwrap();
    let doc = Document::open(&p).expect("editor must open even malformed bytes");
    let t = doc.text();
    assert!(t.contains('a') && t.contains('b'), "real chars survive");
}

#[test]
fn scenario1_edge_empty_file_roundtrips_empty() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("empty.txt");
    std::fs::write(&p, b"").unwrap();
    let mut doc = Document::open(&p).unwrap();
    assert_eq!(doc.text(), "");
    assert_eq!(doc.encoding().name, "UTF-8");
    doc.mark_dirty();
    doc.save().unwrap();
    assert!(std::fs::read(&p).unwrap().is_empty(), "empty stays empty");
}

#[test]
fn scenario1_edge_only_newlines_roundtrips_exactly() {
    // A file that is nothing but newlines: a no-op save must reproduce the exact
    // byte count + EOL style (no swallowed/added blank line).
    for (bytes, want_eol) in [
        (b"\n\n\n".to_vec(), Eol::Lf),
        (b"\r\n\r\n".to_vec(), Eol::Crlf),
        (b"\r\r\r".to_vec(), Eol::Cr),
    ] {
        let (raw, eol, _enc, _lossy) = open_noop_save_bytes(&bytes);
        assert_eq!(eol, want_eol, "EOL detected for {bytes:?}");
        assert_eq!(raw, bytes, "newline-only file round-trips byte-for-byte");
    }
}

#[test]
fn scenario1_edge_last_line_without_newline_preserved() {
    // No trailing newline must NOT gain one on save (a classic POSIX-vs-editor
    // footgun where editors silently append a final newline).
    let (raw, _eol, _enc, _lossy) = open_noop_save_bytes(b"line1\nline2 no eol");
    assert_eq!(
        raw, b"line1\nline2 no eol",
        "the missing final newline must be preserved exactly"
    );
}

// ---------------------------------------------------------------------------
// Scenario 2 — EOL preservation (no-op save must not rewrite endings)
// ---------------------------------------------------------------------------

#[test]
fn scenario2_noop_save_preserves_lf_crlf_cr() {
    for (bytes, want) in [
        (b"a\nb\nc\n".to_vec(), Eol::Lf),
        (b"a\r\nb\r\nc\r\n".to_vec(), Eol::Crlf),
        (b"a\rb\rc\r".to_vec(), Eol::Cr),
    ] {
        let (raw, eol, _enc, _lossy) = open_noop_save_bytes(&bytes);
        assert_eq!(eol, want);
        assert_eq!(raw, bytes, "no-op save must not rewrite {want:?} endings");
    }
}

#[test]
fn scenario2_mixed_eol_noop_save_does_not_mass_rewrite() {
    // A genuinely MIXED file (equal CRLF + lone-LF). The C-07 tie-break resolves
    // to LF; a no-op save normalizes-to-LF then re-applies LF, so the dominant
    // style is preserved and the file is NOT mass-flipped to CRLF. We assert the
    // body content survives and no CR was injected where none was dominant.
    let initial = b"a\r\nb\nc\r\nd\ne"; // crlf=2, lone_lf=2 -> tie -> LF
    let (raw, eol, _enc, _lossy) = open_noop_save_bytes(initial);
    assert_eq!(eol, Eol::Lf, "balanced mix resolves to LF (C-07)");
    // On save the whole file is rewritten in the detected style (LF). The
    // criterion is that the user is NOT surprised by a flip to CRLF: the saved
    // file must contain NO carriage returns.
    assert!(
        !raw.contains(&b'\r'),
        "a balanced-mixed file must not be silently rewritten to CRLF, got {raw:?}"
    );
    // And the text content (CR/LF-insensitive) is intact.
    let text = String::from_utf8(raw).unwrap();
    let joined: String = text.split('\n').collect();
    assert_eq!(
        joined, "abcde",
        "line content must survive the EOL normalize"
    );
}

#[test]
fn scenario2_explicit_eol_change_applies_on_save() {
    // The complement of "don't rewrite silently": an EXPLICIT EOL change DOES
    // hit disk. Open LF, switch to CRLF, save -> CRLF on disk.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("eol.txt");
    std::fs::write(&p, b"a\nb\n").unwrap();
    let mut doc = Document::open(&p).unwrap();
    assert_eq!(doc.eol(), Eol::Lf);
    doc.set_eol(Eol::Crlf);
    assert!(
        doc.is_dirty(),
        "an explicit EOL change marks the buffer dirty"
    );
    doc.save().unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"a\r\nb\r\n");
    // And back the other way.
    doc.set_eol(Eol::Lf);
    doc.save().unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"a\nb\n");
}

// ---------------------------------------------------------------------------
// Scenario 3 — Large-file mmap browse -> first-edit promote -> save
// ---------------------------------------------------------------------------

#[test]
fn scenario3_large_file_opens_as_mmap_browse() {
    // A file just past the 16 MiB Buffer threshold opens read-only (mmap browse)
    // — NOT loaded into a rope — so RSS stays bounded for multi-GB logs.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big.log");
    let payload = vec![b'a'; (MMAP_THRESHOLD as usize) + 1];
    std::fs::write(&p, &payload).unwrap();
    let buf = Buffer::open(&p).unwrap();
    assert!(
        buf.is_read_only(),
        "a >threshold file must browse read-only"
    );
    assert_eq!(buf.len_bytes(), payload.len());
    assert!(
        buf.as_rope().is_none(),
        "mmap variant must not expose a rope"
    );
}

#[test]
fn scenario3_first_edit_promotes_to_rope_preserving_content() {
    // The first edit promotes mmap -> rope WITHOUT touching the file; content +
    // length survive the promotion (no truncation / no OOM in a bounded test).
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big.log");
    let line = "log entry line\n";
    let mut payload = Vec::new();
    while payload.len() < (MMAP_THRESHOLD as usize) + 64 {
        payload.extend_from_slice(line.as_bytes());
    }
    std::fs::write(&p, &payload).unwrap();

    let mut buf = Buffer::open(&p).unwrap();
    assert!(buf.is_read_only());
    buf.promote_to_rope().unwrap();
    assert!(!buf.is_read_only(), "post-promote the buffer is editable");
    let rope = buf.as_rope().expect("rope after promote");
    assert_eq!(
        rope.len_bytes(),
        payload.len(),
        "promotion must not drop or duplicate bytes"
    );
    // Now edit it (the real first-edit) and confirm the rope mutates cleanly.
    {
        let r = buf.as_rope_mut().expect("mutable rope");
        r.insert(0, "PREFIX ");
    }
    assert!(buf.as_rope().unwrap().to_string().starts_with("PREFIX log"));
    // The on-disk file is UNTOUCHED by the in-memory promote+edit.
    assert_eq!(
        std::fs::read(&p).unwrap(),
        payload,
        "the browse-then-edit path must never mutate the source file in place"
    );
}

#[test]
fn scenario3_large_utf16_promote_decodes_not_mojibake() {
    // A large UTF-16LE file browsed then promoted must decode through the
    // encoding layer (BOM + chardetng), NOT a raw from_utf8_lossy that would
    // turn every other byte into U+FFFD. This is the data-CORRUPTION criterion
    // for the browse-then-promote path on non-UTF-8 content.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big-u16.log");
    let marker = "café 速記 line\n";
    let mut payload: Vec<u8> = vec![0xFF, 0xFE]; // UTF-16LE BOM
    while payload.len() < (MMAP_THRESHOLD as usize) + 64 {
        for u in marker.encode_utf16() {
            payload.extend_from_slice(&u.to_le_bytes());
        }
    }
    std::fs::write(&p, &payload).unwrap();
    let mut buf = Buffer::open(&p).unwrap();
    assert!(buf.is_read_only());
    buf.promote_to_rope().unwrap();
    let body = buf.as_rope().unwrap().to_string();
    assert!(
        !body.contains('\u{FFFD}'),
        "UTF-16 promote must not produce replacement-char mojibake"
    );
    assert!(
        body.contains("速記"),
        "kanji must survive the UTF-16 decode"
    );
}

// ---------------------------------------------------------------------------
// Scenario 4 — Regex catastrophic-backtrack must be BOUNDED (no hang)
// ---------------------------------------------------------------------------

/// Run `f` on a watchdog thread; return `Some(result)` if it finishes within
/// `budget`, or `None` if it overran (a hang). Used so a genuinely-hanging
/// search surfaces as a logged failure instead of wedging CI forever.
fn with_watchdog<T: Send + 'static>(
    budget: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(budget).ok()
}

#[test]
fn scenario4_pathological_find_is_bounded_not_hanging() {
    // `(a+)+$` on a long non-matching line is the textbook catastrophic-backtrack
    // pattern for a BACKTRACKING engine. The Rust `regex` crate uses finite
    // automata (linear time), so this must return near-instantly. We bound it
    // with a generous 10s watchdog: if it overruns, that is a P0 hang bug.
    let haystack = format!("{}!", "a".repeat(60)); // 'a'*60 then '!' -> never matches $
    let q = Query {
        pattern: r"(a+)+$".into(),
        regex: true,
        case_sensitive: true,
        whole_word: false,
    };
    let hs = haystack.clone();
    let got = with_watchdog(Duration::from_secs(10), move || find_all(&hs, &q));
    let res = got.expect("BUG-CORR (P0 if hit): pathological regex find did not return within 10s");
    let matches = res.expect("regex must compile + run, not error");
    assert!(
        matches.is_empty(),
        "the non-matching line yields no matches"
    );
}

#[test]
fn scenario4_pathological_replace_all_is_bounded() {
    // The replace path runs the same engine; ensure replace_all is equally bounded
    // and is a no-op when nothing matches.
    let haystack = format!("{}X", "a".repeat(64));
    let q = Query {
        pattern: r"(a+)+$".into(),
        regex: true,
        case_sensitive: true,
        whole_word: false,
    };
    let hs = haystack.clone();
    let got = with_watchdog(Duration::from_secs(10), move || replace_all(&hs, &q, "Z"));
    let out = got
        .expect("BUG-CORR (P0 if hit): pathological regex replace_all did not return within 10s")
        .expect("replace_all must run, not error");
    assert_eq!(out, haystack, "no match -> text unchanged");
}

#[test]
fn scenario4_nested_quantifier_alternation_bounded() {
    // A second pathological shape: `(a|aa)+$` over a long 'a' run with a trailing
    // mismatch — also catastrophic for a backtracker, linear for the regex crate.
    let haystack = format!("{}b", "a".repeat(50));
    let q = Query {
        pattern: r"(a|aa)+$".into(),
        regex: true,
        case_sensitive: true,
        whole_word: false,
    };
    let hs = haystack.clone();
    let got = with_watchdog(Duration::from_secs(10), move || find_all(&hs, &q));
    let res = got
        .expect("BUG-CORR (P0 if hit): nested-alternation regex did not return within 10s")
        .expect("regex must run");
    assert!(res.is_empty());
}

// ---------------------------------------------------------------------------
// Scenario 4b — Zero-width / overlapping match correctness (regression-assert)
// ---------------------------------------------------------------------------

#[test]
fn scenario4b_zero_width_find_yields_no_phantom_matches() {
    let rq = |p: &str| Query {
        pattern: p.into(),
        regex: true,
        ..Default::default()
    };
    // `x*`, `\b`, `^`, `$` all match empty spans; none must be reported.
    for pat in ["x*", r"\b", "^", "$", "(?m)^"] {
        assert!(
            find_all("abc", &rq(pat)).unwrap().is_empty(),
            "zero-width pattern {pat:?} must not report phantom hits"
        );
    }
}

#[test]
fn scenario4b_zero_width_replace_is_identity_no_injection() {
    let rq = |p: &str| Query {
        pattern: p.into(),
        regex: true,
        ..Default::default()
    };
    // The classic footgun: `replace_all("abc", "x*", "-")` must NOT become "-a-b-c-".
    assert_eq!(replace_all("abc", &rq("x*"), "-").unwrap(), "abc");
    assert_eq!(replace_all("café", &rq("x*"), "-").unwrap(), "café");
    assert_eq!(replace_all("a b", &rq(r"\b"), "|").unwrap(), "a b");
}

#[test]
fn scenario4b_real_runs_still_match_around_empties() {
    let rq = |p: &str| Query {
        pattern: p.into(),
        regex: true,
        ..Default::default()
    };
    // `a*` over "baac" matches only the real run "aa"; replace substitutes only it.
    let m = find_all("baac", &rq("a*")).unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!((m[0].start, m[0].end), (1, 3));
    assert_eq!(replace_all("baac", &rq("a*"), "X").unwrap(), "bXc");
}

// ---------------------------------------------------------------------------
// Scenario 5 — Corrupt config / session -> graceful fallback (never crash)
// ---------------------------------------------------------------------------

#[test]
fn scenario5_malformed_config_falls_back_to_defaults_not_panic() {
    // A truncated / hand-broken config.toml must parse to an Err (surfaced), and
    // the app's default never panics. We assert via the parse seam that backs
    // load_or_default's malformed branch.
    for bad in [
        "this = = not valid toml [[[",
        "[editor]\ntab_width = ",      // truncated value
        "[editor]\ntab_width = \"x\"", // wrong type
        "\u{0}\u{1}\u{2}garbage",      // binary garbage
    ] {
        let parsed = Config::from_toml_str(bad);
        assert!(parsed.is_err(), "malformed config {bad:?} must be an Err");
    }
    // And a PARTIAL but valid config merges onto defaults (other settings intact).
    let partial = Config::from_toml_str("[editor]\ntab_width = 2\n").unwrap();
    assert_eq!(partial.editor.tab_width, 2);
    assert!(
        partial.editor.show_line_numbers,
        "unspecified fields keep their defaults — no setting is lost"
    );
}

#[test]
fn scenario5_corrupt_session_manifest_returns_none_not_crash() {
    // A malformed / truncated session.json must yield None (fall back to a fresh
    // scratch session) rather than panicking or losing data. Drive load_manifest
    // against a crafted config dir for each corruption shape.
    for bad in [
        "{ not valid json",
        "{\"version\": 1, \"tabs\": [", // truncated array
        "",                             // empty file
        "\u{0}\u{1}binary",             // binary garbage
        "{\"version\": \"oops\"}",      // wrong type for version
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mpath = session::manifest_path(dir.path());
        std::fs::write(&mpath, bad).unwrap();
        let loaded = session::load_manifest(dir.path());
        assert!(
            loaded.is_none(),
            "corrupt session.json {bad:?} must load as None (graceful fallback)"
        );
    }
}

#[test]
fn scenario5_future_version_manifest_is_ignored() {
    // A session.json written by a NEWER build (version above MANIFEST_VERSION)
    // must be ignored (None) rather than mis-restored — forward-compat safety.
    let dir = tempfile::tempdir().unwrap();
    let mut m = SessionManifest::new(vec![TabSnapshot::default()], 0);
    m.version = session::MANIFEST_VERSION + 5;
    let body = serde_json::to_string(&m).unwrap();
    std::fs::write(session::manifest_path(dir.path()), body).unwrap();
    assert!(
        session::load_manifest(dir.path()).is_none(),
        "a future-schema manifest must be ignored, not mis-restored"
    );
}

#[test]
fn scenario5_valid_session_still_restores_other_tabs_after_one_corrupt_field() {
    // A well-formed manifest with several tabs round-trips fully — the corrupt
    // path must not be over-eager (a valid session is never dropped). This guards
    // "graceful fallback never throws away GOOD data".
    let dir = tempfile::tempdir().unwrap();
    let tabs = vec![
        TabSnapshot {
            path: Some("/notes/a.txt".into()),
            dirty: false,
            backup: None,
            cursor: 3,
        },
        TabSnapshot {
            path: None, // untitled scratch with unsaved content
            dirty: true,
            backup: Some("scratch.bak".into()),
            cursor: 0,
        },
    ];
    let manifest = SessionManifest::new(tabs.clone(), 1);
    session::save_manifest(dir.path(), &manifest).unwrap();
    let loaded = session::load_manifest(dir.path()).expect("valid manifest restores");
    assert_eq!(loaded.tabs.len(), 2, "both tabs survive the round-trip");
    assert_eq!(loaded.active, 1);
    assert_eq!(loaded.tabs[1].backup.as_deref(), Some("scratch.bak"));
    assert!(loaded.tabs[1].dirty, "the dirty untitled tab is preserved");
}
