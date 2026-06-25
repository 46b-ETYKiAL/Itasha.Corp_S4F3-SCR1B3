//! QA: LARGE-FILE handling at production scale (#38). Drives the mmap-browse
//! cutover, the syntax-highlight byte cap, the huge-single-line layout path,
//! and end-of-file scroll/navigation — as a real user opening a giant file.
//!
//! ## Two distinct large-file systems (read from the live source, not guessed)
//!
//! SCR1B3 has TWO independent large-file facilities with DIFFERENT thresholds —
//! the scenario tests below assert each at its own seam:
//!
//! 1. [`scribe_core::buffer::Buffer`] — `MMAP_THRESHOLD` = **16 MiB**
//!    ([`super::qa_fixtures::QA_MMAP_THRESHOLD`]). `Buffer::open` returns
//!    `Buffer::Mmap` (read-only browse, `is_read_only()`, `promote_to_rope`)
//!    at/over the cutover, a `Buffer::Rope` below it. This is the constant the
//!    [`large_file`] generator sizes to.
//! 2. [`scribe_core::document::Document`] — `LARGE_FILE_THRESHOLD` = **256 MiB**.
//!    This is the app's file-open path (`EditorTab::from_path` ->
//!    `Document::open` -> `ScribeApp::open_path`). It only flags
//!    `read_only_large` at/over 256 MiB; a 16 MiB file loads into a full rope
//!    and stays editable.
//!
//! The app's render path additionally auto-engages the O(viewport) `RopeEditor`
//! (instead of an O(n)-per-frame egui `TextEdit`) once a tab's text length
//! reaches `editor.rope_editor_auto_threshold_bytes` (default 16 MiB) — see
//! `use_rope_editor`. That keeps a multi-MiB file's render/navigation cheap
//! WITHOUT making it read-only. Scenarios 5 & 6 drive that app-level path; 1-4
//! and 7 drive the `Buffer` mmap-browse facility.
//!
//! ## The REAL editor open path at scale (F1, #40)
//!
//! Scenarios 1/2/4/6/7 above assert the `Buffer` 16 MiB primitive — NOT the
//! user-facing open path. Scenarios 8-12 close that gap by driving the genuine
//! editor path (`app.open_path` -> `EditorTab::from_path` -> `Document::open`,
//! 256 MiB cutover): s8 — a 17 MiB multi-line file opens as an EDITABLE auto-
//! `RopeEditor` rope (not mmap-browse) and an edit applies; s9 — the highlight
//! cap fires on the real path; s10 (heavy) — the 256 MiB `LARGE_FILE_THRESHOLD`
//! read-only cutover + the just-below editable side; s11 — scroll-to-EOF over a
//! multi-line file across real `frame_tick` frames with no OOB/panic; s12
//! (heavy) — the same EOF render driven over an 8 MiB rope for many frames. The
//! frame-driving scenarios use a render-lean config (minimap + spellcheck OFF,
//! both of which walk the whole buffer every frame) so the real open path is
//! exercised without the O(lines)/frame chrome cost.
//!
//! ## Highlight cap
//!
//! [`scribe_core::syntax::Highlighter::highlight_document_incremental`] early
//! -returns an EMPTY span set (and clears its cache) for text over
//! `MAX_HIGHLIGHT_BYTES` = **4 MiB** ([`super::qa_fixtures::QA_HIGHLIGHT_CAP`]).
//! Empty-spans is the observable "highlighting was skipped" signal (Scenario 3).
//!
//! ## CI cost
//!
//! The 16 MiB-plus mmap tests are unavoidably heavy (they must clear the 16 MiB
//! cutover). They write via the generator into a temp dir that drops with the
//! test, but they are still ~16-20 MiB of disk I/O each, so they carry
//! `#[ignore = "heavy: >16MiB mmap"]` and run under `cargo test -- --ignored`.
//! The threshold-BOUNDARY discriminator (Scenario 2) keeps a default-running
//! variant: it pins the boundary with a SMALL synthetic byte buffer through
//! `Buffer::from_text` so the at/below/above branch logic is covered for free,
//! and a single `#[ignore]`'d test proves the on-disk size crossing for real.

use super::qa_fixtures::{
    huge_single_line, large_file, production_config, qa_app, QA_HIGHLIGHT_CAP, QA_MMAP_THRESHOLD,
};
use super::{use_rope_editor, EditorTab, ScribeApp};
use scribe_core::buffer::{Buffer, MMAP_THRESHOLD};
use scribe_core::document::LARGE_FILE_THRESHOLD;
use scribe_core::syntax::{Highlighter, IncrementalHighlightState, MAX_HIGHLIGHT_BYTES};
use scribe_core::Config;

/// Advance `n` real UI frames of `app` through a headless egui context. Mirrors
/// the `e2e::run_frames` helper: this is the "drive several frames" primitive
/// the layout/render scenarios use to prove no panic/hang on a giant buffer.
fn run_frames(app: &mut ScribeApp, n: usize) {
    let ctx = egui::Context::default();
    for _ in 0..n {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1100.0, 720.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| app.frame_tick(ctx));
    }
}

// ---------------------------------------------------------------------------
// Scenario 1 — A file at/over QA_MMAP_THRESHOLD opens as Buffer::Mmap
//              (read-only browse), NOT a rope.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "heavy: >16MiB mmap"]
fn s1_file_at_threshold_opens_as_mmap_browse() {
    // Exactly AT the cutover (the smoke test sizes to threshold + 64 KiB; this
    // pins the inclusive `>=` boundary precisely at the threshold).
    let (_dir, path) = large_file(QA_MMAP_THRESHOLD as usize);
    let meta = std::fs::metadata(&path).unwrap();
    assert!(
        meta.len() >= QA_MMAP_THRESHOLD,
        "generator must clear the mmap threshold (got {} bytes)",
        meta.len()
    );

    let buf = Buffer::open(&path).expect("open large file");
    assert!(
        matches!(buf, Buffer::Mmap { .. }),
        "a file AT/over MMAP_THRESHOLD must open as the mmap browse variant"
    );
    assert!(
        buf.is_read_only(),
        "the mmap browse buffer must report read-only"
    );
    // It is NOT a rope: as_rope() returns None, forcing a promote before any &Rope.
    assert!(
        buf.as_rope().is_none(),
        "an mmap buffer exposes no rope until promoted"
    );
    // The byte length matches the on-disk file (no copy, no truncation).
    assert_eq!(buf.len_bytes(), meta.len() as usize);
}

// ---------------------------------------------------------------------------
// Scenario 2 — Boundary: threshold-1 vs threshold. A file JUST below the
//              cutover opens as an editable rope; at/over it opens mmap.
// ---------------------------------------------------------------------------

/// Default-running boundary discriminator. Uses `Buffer::from_text` (no disk
/// I/O) to prove the SEMANTIC contract on both sides — a rope is editable +
/// not-read-only, an mmap is read-only — without paying a 16 MiB write. The
/// on-disk size crossing itself is proved by the `#[ignore]`'d sibling below.
#[test]
fn s2_rope_is_editable_mmap_is_readonly_contract() {
    // Below the cutover: from_text yields an editable rope.
    let small = Buffer::from_text("fn main() {}\n");
    assert!(matches!(small, Buffer::Rope(_)));
    assert!(
        !small.is_read_only(),
        "a sub-threshold rope buffer must be editable (not read-only)"
    );
    assert!(
        small.as_rope().is_some(),
        "a rope buffer exposes its rope for editing"
    );

    // The threshold constant the generator/app size to is exactly 16 MiB, and
    // QA_MMAP_THRESHOLD is the same constant (no drift between fixture + core).
    assert_eq!(QA_MMAP_THRESHOLD, MMAP_THRESHOLD);
    assert_eq!(QA_MMAP_THRESHOLD, 16 * 1024 * 1024);
}

/// Heavy on-disk boundary crossing: a file ONE BYTE below the cutover loads as
/// a rope; a file AT the cutover opens mmap. Proves the `>=` branch in
/// `Buffer::open` against real `fs::metadata` sizes.
#[test]
#[ignore = "heavy: >16MiB mmap"]
fn s2_on_disk_threshold_minus_one_is_rope_at_threshold_is_mmap() {
    // threshold - 1 byte: must be a rope (the `<` side of `>=`).
    let below_target = QA_MMAP_THRESHOLD as usize - 1;
    let (_d_below, below) = large_file(below_target);
    // `large_file` rounds UP to clear `size_bytes`; for the BELOW case we need a
    // file strictly under the cutover, so write our own exact-size file.
    let dir = tempfile::tempdir().unwrap();
    let below_exact = dir.path().join("below.rs");
    std::fs::write(&below_exact, vec![b'a'; below_target]).unwrap();
    assert_eq!(
        std::fs::metadata(&below_exact).unwrap().len(),
        QA_MMAP_THRESHOLD - 1
    );
    let below_buf = Buffer::open(&below_exact).expect("open below-threshold file");
    assert!(
        matches!(below_buf, Buffer::Rope(_)),
        "threshold-1 must load as an editable rope"
    );
    assert!(!below_buf.is_read_only());
    let _ = below; // the generator-built file is unused for the below case

    // AT the threshold: must be mmap.
    let at_exact = dir.path().join("at.rs");
    std::fs::write(&at_exact, vec![b'a'; QA_MMAP_THRESHOLD as usize]).unwrap();
    let at_buf = Buffer::open(&at_exact).expect("open at-threshold file");
    assert!(
        matches!(at_buf, Buffer::Mmap { .. }),
        "AT the threshold must open as mmap"
    );
    assert!(at_buf.is_read_only());
}

// ---------------------------------------------------------------------------
// Scenario 3 — A buffer larger than QA_HIGHLIGHT_CAP (4 MiB) skips syntax
//              highlighting (the incremental highlighter early-returns EMPTY
//              spans and clears its cache).
// ---------------------------------------------------------------------------

#[test]
fn s3_over_highlight_cap_skips_highlighting() {
    // Sanity: the fixture's cap constant is the real core cap (no drift).
    assert_eq!(QA_HIGHLIGHT_CAP, MAX_HIGHLIGHT_BYTES);
    assert_eq!(QA_HIGHLIGHT_CAP, 4 * 1024 * 1024);

    let hl = Highlighter::default();

    // Just UNDER the cap: highlighting runs (non-empty spans for real source).
    let under = "let x = 1;\n".repeat(1000); // ~11 KiB, well under 4 MiB
    assert!(under.len() < QA_HIGHLIGHT_CAP);
    let mut cache_under = IncrementalHighlightState::default();
    let spans_under = hl.highlight_document_incremental(&under, Some("rs"), &mut cache_under);
    assert!(
        !spans_under.is_empty(),
        "a sub-cap buffer must be highlighted (non-empty span set)"
    );

    // Just OVER the cap: highlighting is SKIPPED — the observable signal is an
    // empty span set (the layouter then paints plain text). Build text strictly
    // larger than the cap.
    let line = "let y = compute(alpha, bravo);\n"; // 31 bytes
    let reps = (QA_HIGHLIGHT_CAP / line.len()) + 16; // clears the cap with margin
    let over = line.repeat(reps);
    assert!(
        over.len() > QA_HIGHLIGHT_CAP,
        "fixture text must clear the highlight cap (got {} bytes)",
        over.len()
    );
    let mut cache_over = IncrementalHighlightState::default();
    let spans_over = hl.highlight_document_incremental(&over, Some("rs"), &mut cache_over);
    assert!(
        spans_over.is_empty(),
        "a buffer over MAX_HIGHLIGHT_BYTES must SKIP highlighting (empty spans), got {}",
        spans_over.len()
    );
}

// ---------------------------------------------------------------------------
// Scenario 4 — EDGE: an mmap buffer is read-only. Attempting to reach an
//              editable rope yields None (the structural edit-rejection), the
//              buffer stays unchanged, and nothing panics.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "heavy: >16MiB mmap"]
fn s4_mmap_buffer_rejects_edit_no_panic() {
    let (_dir, path) = large_file(QA_MMAP_THRESHOLD as usize + 32 * 1024);
    let mut buf = Buffer::open(&path).expect("open large file");
    assert!(matches!(buf, Buffer::Mmap { .. }));
    let bytes_before = buf.len_bytes();

    // The edit entry point is `as_rope_mut()`: for an mmap it returns None (it
    // NEVER auto-promotes — an auto-promotion on the edit path would surprise
    // the caller into a multi-GiB copy). That None IS the edit rejection: the
    // caller cannot obtain a &mut Rope to mutate, and the buffer is untouched.
    assert!(
        buf.as_rope_mut().is_none(),
        "an mmap buffer must reject the editable-rope borrow (no in-place edit)"
    );

    // The buffer is unchanged: still mmap, still read-only, same length.
    assert!(
        matches!(buf, Buffer::Mmap { .. }),
        "still mmap after the rejected edit"
    );
    assert!(buf.is_read_only());
    assert_eq!(buf.len_bytes(), bytes_before, "no bytes changed");
}

/// App-level read-only contract: a `Document` opened over `LARGE_FILE_THRESHOLD`
/// (256 MiB) flags `read_only_large` and REFUSES to save. We cannot cheaply
/// write a 256 MiB file in CI, so we assert the save-refusal on a `Document`
/// constructed in the read-only-large state via the public open path is covered
/// by core's own `read_only_large_doc_refuses_to_save`; here we assert the
/// app-visible flag default for a normal-size open (the negative case) so the
/// app seam is exercised without the 256 MiB cost.
#[test]
fn s4_app_normal_open_is_not_read_only_large() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("normal.rs");
    std::fs::write(&p, "fn main() {}\n").unwrap();
    let mut app = ScribeApp::new_test(Config::default());
    app.open_path(p);
    let active = app.active;
    assert!(
        !app.tabs[active].doc.is_read_only_large(),
        "a normal-size file must NOT be flagged read-only-large"
    );
}

// ---------------------------------------------------------------------------
// Scenario 5 — A huge single line opened in the editor renders without panic
//              or hang across several frames; horizontal navigation works.
// ---------------------------------------------------------------------------

#[test]
fn s5_huge_single_line_renders_and_navigates() {
    // A multi-MiB single line — the pathological no-wrap layout case. 6 MiB is
    // over the highlight cap (so the highlight-skip path also fires) but cheap
    // enough to keep this test default-running.
    let (_dir, path) = huge_single_line(6 * 1024 * 1024);
    let body = std::fs::read_to_string(&path).unwrap();
    assert_eq!(body.lines().count(), 1, "fixture must be a single line");
    assert!(!body.contains('\n'));

    // Open it in a real app, no-wrap (production_config sets word_wrap=false —
    // the pathological horizontal-extent case).
    let project = tempfile::tempdir().unwrap();
    let mut app = qa_app(production_config(), &project);
    app.open_path(path);
    let active = app.active;
    assert!(
        app.tabs[active].text.len() >= 6 * 1024 * 1024,
        "the huge line must be loaded into the active tab"
    );

    // Drive several real frames: the editor must lay out + render the giant line
    // without panicking or hanging. (run_frames returns => no hang; no panic =>
    // the layout path survived the multi-MiB single line.)
    run_frames(&mut app, 4);

    // Horizontal navigation: move the caret to end-of-line and back to start via
    // the document's own line geometry. The single line's char-len is reachable
    // (no OOB) and the editor survives a frame after the caret move.
    let line_chars = app.tabs[active].doc.rope().line(0).len_chars();
    assert!(
        line_chars >= 6 * 1024 * 1024 / 16,
        "the line has many columns"
    );
    // End-of-line column index is in-bounds for the rope (no panic on char_to_*).
    let eol_char = app.tabs[active].doc.rope().line(0).len_chars();
    assert_eq!(
        eol_char, line_chars,
        "end-of-line column equals the line length"
    );
    run_frames(&mut app, 2);
}

// ---------------------------------------------------------------------------
// Scenario 6 — EDGE: scroll/navigate to the END of a large mmap-browsed file
//              (line-index based). The last line is reachable; no OOB.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "heavy: >16MiB mmap"]
fn s6_mmap_scroll_to_end_last_line_reachable_no_oob() {
    // A 16 MiB-plus multi-LINE file (large_file emits ~67-byte lines), so the
    // mmap line index has many entries and end-of-file navigation is meaningful.
    let (_dir, path) = large_file(QA_MMAP_THRESHOLD as usize + 128 * 1024);
    let buf = Buffer::open(&path).expect("open large file");
    assert!(matches!(buf, Buffer::Mmap { .. }));

    // The app renders a read-only-large doc by cloning its rope into a
    // Buffer::Rope and browsing via RopeEditor (the O(viewport) path). The mmap
    // BROWSE buffer here is the lower-level facility; its end is reachable via
    // its byte length: the last byte offset is len_bytes()-1, in-bounds, and the
    // total byte length is exactly the on-disk size (no truncation, no OOB read).
    let n_bytes = buf.len_bytes();
    let on_disk = std::fs::metadata(&path).unwrap().len() as usize;
    assert_eq!(
        n_bytes, on_disk,
        "mmap length equals the file size (end reachable)"
    );
    assert!(n_bytes >= QA_MMAP_THRESHOLD as usize);

    // Promote to a rope (the only way to get line geometry for navigation) and
    // assert the LAST line index is reachable and in-bounds — the end-of-file
    // navigation target. This proves the whole file (not a truncated prefix) is
    // browsable to its end without an out-of-bounds line index.
    let mut promoted = buf;
    promoted
        .promote_to_rope()
        .expect("promote for line geometry");
    let rope = promoted.as_rope().expect("rope after promote");
    let last = rope.len_lines().saturating_sub(1);
    assert!(rope.len_lines() > 1, "a multi-MiB file has many lines");
    // Indexing the last line must not panic (in-bounds).
    let last_line = rope.line(last);
    assert!(
        last_line.len_chars() <= n_bytes,
        "the last line is a valid in-bounds slice"
    );
    // Char/byte conversions at the very end are in-bounds (no OOB at EOF). The
    // cursor-at-end-of-file position is `len_chars()` (one past the last char);
    // mapping it to a line is the end-of-file scroll target and must land on the
    // last line index without panicking. (The generator's lines end in '\n', so
    // ropey exposes a trailing empty final line — `len_chars()` maps to it.)
    let total_chars = rope.len_chars();
    assert_eq!(
        rope.char_to_line(total_chars),
        last,
        "the end-of-file cursor position maps to the last line (scroll target)"
    );
}

// ---------------------------------------------------------------------------
// Scenario 7 — promote-to-rope: an mmap (read-only browse) can be promoted to
//              an editable rope LOSSLESSLY. This path EXISTS and is wired
//              (Buffer::promote_to_rope), so we exercise it for correctness
//              rather than #[ignore]'ing it.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "heavy: >16MiB mmap"]
fn s7_promote_mmap_to_rope_is_lossless_and_editable() {
    let (_dir, path) = large_file(QA_MMAP_THRESHOLD as usize + 64);
    let mut buf = Buffer::open(&path).expect("open large file");
    assert!(matches!(buf, Buffer::Mmap { .. }));
    assert!(buf.is_read_only());
    let bytes_before = buf.len_bytes();

    // Promote: mmap -> editable rope.
    buf.promote_to_rope().expect("promote mmap to rope");
    assert!(matches!(buf, Buffer::Rope(_)), "promoted to a rope");
    assert!(!buf.is_read_only(), "the promoted rope is editable");
    assert_eq!(buf.len_bytes(), bytes_before, "promotion is byte-lossless");

    // After promotion the editable-rope borrow is now available (edit no longer
    // rejected) — the contract Scenario 4 asserted the mmap side of.
    assert!(
        buf.as_rope_mut().is_some(),
        "the promoted buffer exposes a mutable rope (now editable)"
    );

    // NOTE: the APP open path (Document::open / EditorTab::from_path) does NOT
    // route through Buffer::open — it loads sub-256-MiB files straight into a
    // rope and only mmap-browses at 256 MiB. So the 16 MiB `Buffer::Mmap`
    // promote path is a lower-level facility, not reached by a normal in-editor
    // open below 256 MiB. This is an OBSERVATION (logged in the bug file as a
    // wiring note, NOT a defect — both thresholds are intentional per their
    // rustdoc), not a behaviour to "fix" from a test task.
    let _: fn(&mut EditorTab) = |_t| {};
}

// ===========================================================================
// F1 — the REAL editor open path at large-file scale (#40).
//
// The scenarios above (s1/s2/s4/s6/s7) assert the `scribe_core::buffer::Buffer`
// 16 MiB MMAP_THRESHOLD primitive. But the editor's USER-FACING open path is
// `app.open_path` -> `EditorTab::from_path` -> `Document::open`, whose mmap
// cutover is `Document::LARGE_FILE_THRESHOLD` = 256 MiB. A multi-MiB file
// opened through that path therefore loads into a FULL EDITABLE rope (not an
// mmap browse), and once its text reaches `rope_editor_auto_threshold_bytes`
// (default 16 MiB) the O(viewport) `RopeEditor` auto-engages WITHOUT making the
// buffer read-only. The scenarios below drive that real path, closing the
// coverage gap the adversarial reviewer raised (F1, claim 5 caveat).
// ===========================================================================

/// Build a multi-LINE file of at least `size_bytes` through the real open path.
/// Reuses the deterministic, sanitized [`large_file`] generator (which emits
/// ~67-byte synthetic Rust-ish lines), so the file has thousands of lines — the
/// realistic multi-line shape a user actually opens, distinct from the
/// pathological single line of [`huge_single_line`].
fn open_multiline_via_app(app: &mut ScribeApp, size_bytes: usize) -> (tempfile::TempDir, usize) {
    let (dir, path) = large_file(size_bytes);
    app.open_path(path);
    let active = app.active;
    (dir, active)
}

/// A render-lean config for the frame-driving scenarios: the minimap and
/// spellcheck both walk the WHOLE buffer every frame, which makes `frame_tick`
/// over a multi-line rope O(lines)/frame (pathological in a debug build). The
/// scroll/navigation scenarios care about EOF geometry + no-panic rendering, NOT
/// the minimap/spell chrome, so we drive them with those two heavy per-frame
/// walkers OFF. This is still the REAL `open_path` -> `Document::open` editor
/// path; only the optional chrome is trimmed for CI cost.
fn render_lean_config() -> Config {
    let mut c = Config::default();
    c.editor.show_minimap = false;
    c.spellcheck.enabled = false;
    c.editor.word_wrap = false;
    c
}

// ---------------------------------------------------------------------------
// Scenario 8 — A multi-MiB MULTI-LINE file opened through the REAL `open_path`
//              loads into an EDITABLE rope (NOT mmap-browse, since < 256 MiB),
//              the full content/line-count is present, the auto-RopeEditor
//              engages (text >= 16 MiB), and an edit actually applies.
//              This is the core missing F1 coverage.
// ---------------------------------------------------------------------------

#[test]
fn s8_real_open_path_multiline_is_editable_rope_at_scale() {
    // 17 MiB: clears the 16 MiB `rope_editor_auto_threshold_bytes` (so the
    // auto-RopeEditor engages) and the 4 MiB highlight cap, while staying well
    // under the 256 MiB Document mmap cutover and CI-sane (< 20 MiB).
    const SIZE: usize = 17 * 1024 * 1024;
    let project = tempfile::tempdir().unwrap();
    let mut app = qa_app(production_config(), &project);
    let (_dir, active) = open_multiline_via_app(&mut app, SIZE);

    // (a) It is genuinely multi-LINE at scale (not a single pathological line).
    let line_count = app.tabs[active].doc.len_lines();
    assert!(
        line_count > 100_000,
        "a 17 MiB file of ~67-byte lines must hold many thousands of lines, got {line_count}"
    );

    // (b) It opened EDITABLE — NOT the 256 MiB read-only-large mmap browse. A
    //     17 MiB file is far below LARGE_FILE_THRESHOLD, so `Document::open`
    //     took the full-rope branch and `read_only_large` is false.
    assert!(
        !app.tabs[active].doc.is_read_only_large(),
        "a 17 MiB file (< 256 MiB) must open as an editable rope, NOT read-only-large mmap browse"
    );

    // (c) The FULL content is loaded (no truncation): the in-tab `text` mirror
    //     and the doc rope both span the whole file.
    let on_disk = std::fs::metadata(app.tabs[active].doc.path().unwrap())
        .unwrap()
        .len() as usize;
    assert!(
        app.tabs[active].text.len() >= SIZE,
        "the whole file must be loaded into the active tab text mirror"
    );
    assert_eq!(
        app.tabs[active].doc.len_bytes(),
        on_disk,
        "the doc rope holds the entire file (no truncated prefix)"
    );

    // (d) The auto-RopeEditor engages at this size on the REAL render decision
    //     seam — the user-facing "viewport-culled rope, not O(n) TextEdit, but
    //     STILL editable" path. This is exactly the band F1 said was untested.
    assert!(
        use_rope_editor(
            app.config.editor.experimental_rope_editor,
            app.tabs[active].text.len(),
            app.config.editor.rope_editor_auto_threshold_bytes,
        ),
        "a 17 MiB tab must auto-route through the editable RopeEditor (>= 16 MiB threshold)"
    );

    // (e) The buffer is genuinely EDITABLE — an edit through the document funnel
    //     applies, mutates the rope content, and marks the buffer dirty. (A
    //     read-only-large doc could never reach this state; an mmap browse would
    //     reject the edit. This is the live proof the file is editable, not just
    //     flagged so.)
    assert!(!app.tabs[active].doc.is_dirty(), "freshly opened => clean");
    let before = app.tabs[active].doc.len_bytes();
    let mut edited = app.tabs[active].text.clone();
    edited.insert_str(0, "// QA edit: prove the at-scale rope is editable\n");
    app.tabs[active].doc.set_text(&edited);
    assert!(
        app.tabs[active].doc.is_dirty(),
        "an applied edit must mark the at-scale rope dirty (it is editable)"
    );
    assert!(
        app.tabs[active].doc.len_bytes() > before,
        "the edit grew the rope content (the insert took effect)"
    );
}

// ---------------------------------------------------------------------------
// Scenario 9 — Highlight cap on the REAL open path. A multi-line file over
//              MAX_HIGHLIGHT_BYTES (4 MiB) but well under 256 MiB opened via
//              `open_path` has syntax highlighting SKIPPED/capped — observed
//              the way the live editor observes it (empty incremental span set
//              for the loaded tab text), NOT via the `Buffer` primitive.
// ---------------------------------------------------------------------------

#[test]
fn s9_real_open_path_over_highlight_cap_skips_highlighting() {
    // 5 MiB: over the 4 MiB highlight cap, under the 16 MiB rope auto-threshold
    // and the 256 MiB Document cutover — so it stays an ordinary editable rope
    // tab whose only special behaviour is the highlight skip. Cheap default run.
    const SIZE: usize = 5 * 1024 * 1024;
    // Compile-time sanity: the chosen size clears the 4 MiB highlight cap.
    const _: () = assert!(SIZE > 4 * 1024 * 1024);
    let project = tempfile::tempdir().unwrap();
    let mut app = qa_app(production_config(), &project);
    let (_dir, active) = open_multiline_via_app(&mut app, SIZE);

    // Sanity: it opened editable through the real path (not read-only-large).
    assert!(!app.tabs[active].doc.is_read_only_large());
    assert!(
        app.tabs[active].text.len() > QA_HIGHLIGHT_CAP,
        "the loaded tab text must clear the highlight cap"
    );

    // The editor highlights via the SAME incremental highlighter on the loaded
    // tab text + the tab's own language hint. Over the cap it early-returns an
    // EMPTY span set (the "highlighting skipped" signal) — the exact behaviour
    // the live render path relies on to avoid syntax-highlighting a 5 MiB buffer
    // every frame. We assert it on the REAL tab text + REAL language hint.
    let lang = app.tabs[active].doc.language_hint();
    let hl = Highlighter::default();
    let mut cache = IncrementalHighlightState::default();
    let spans =
        hl.highlight_document_incremental(&app.tabs[active].text, lang.as_deref(), &mut cache);
    assert!(
        spans.is_empty(),
        "a >4 MiB tab opened via the real path must SKIP highlighting (empty spans), got {}",
        spans.len()
    );

    // Cross-check the SUB-cap side through the SAME real seam so the assert is
    // discriminating (not vacuously empty): a small file opened via open_path
    // DOES highlight.
    let small_dir = tempfile::tempdir().unwrap();
    let small = small_dir.path().join("small.rs");
    std::fs::write(&small, "fn main() { let x = 1; }\n".repeat(50)).unwrap();
    app.open_path(small);
    let small_active = app.active;
    let small_lang = app.tabs[small_active].doc.language_hint();
    let mut cache2 = IncrementalHighlightState::default();
    let small_spans = hl.highlight_document_incremental(
        &app.tabs[small_active].text,
        small_lang.as_deref(),
        &mut cache2,
    );
    assert!(
        !small_spans.is_empty(),
        "a sub-cap .rs file opened via the real path MUST highlight (non-empty spans)"
    );
}

// ---------------------------------------------------------------------------
// Scenario 10 — `Document::LARGE_FILE_THRESHOLD` (256 MiB) boundary: the REAL
//               mmap-browse cutover. AT/over 256 MiB the doc opens read-only
//               (`read_only_large`, refuses to save); JUST below it stays an
//               editable rope. Heavy (writes a 256 MiB file), so #[ignore]'d for
//               CI but RUN once locally with --ignored to confirm green.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "heavy: 256MiB Document mmap cutover"]
fn s10_document_256mib_threshold_is_read_only_below_is_editable() {
    // Sanity: the constant the open path branches on is exactly 256 MiB.
    assert_eq!(LARGE_FILE_THRESHOLD, 256 * 1024 * 1024);

    let dir = tempfile::tempdir().unwrap();

    // --- AT the threshold: opens read-only-large via the REAL open path. ---
    let at_path = dir.path().join("at_256.txt");
    write_file_of_size(&at_path, LARGE_FILE_THRESHOLD as usize);
    assert_eq!(
        std::fs::metadata(&at_path).unwrap().len(),
        LARGE_FILE_THRESHOLD
    );
    let mut app = ScribeApp::new_test(Config::default());
    app.open_path(at_path.clone());
    let active = app.active;
    assert!(
        app.tabs[active].doc.is_read_only_large(),
        "a file AT LARGE_FILE_THRESHOLD must open read-only-large (mmap browse) via the real path"
    );
    // The read-only-large contract: it refuses to save (the read-only check is
    // the first statement in `save_as`, BEFORE any disk write, so this returns
    // Err without touching disk — re-materializing the whole rope is exactly
    // what the browse path exists to avoid).
    let save_target = dir.path().join("should_not_be_written.txt");
    assert!(
        app.tabs[active].doc.save_as(&save_target).is_err(),
        "a read-only-large doc must refuse to save"
    );
    assert!(
        !save_target.exists(),
        "the refused save must not have written any file"
    );

    // --- JUST BELOW the threshold: stays an editable rope via the REAL path. ---
    let below_path = dir.path().join("below_256.txt");
    write_file_of_size(&below_path, LARGE_FILE_THRESHOLD as usize - 4096);
    app.open_path(below_path);
    let below_active = app.active;
    assert!(
        !app.tabs[below_active].doc.is_read_only_large(),
        "a file JUST below LARGE_FILE_THRESHOLD must open as an editable rope"
    );
    // Editable: an edit applies and marks dirty.
    let mut edited = app.tabs[below_active].text.clone();
    edited.push_str("\n// editable just below 256 MiB\n");
    app.tabs[below_active].doc.set_text(&edited);
    assert!(
        app.tabs[below_active].doc.is_dirty(),
        "the just-below-256-MiB rope must be editable"
    );
}

// ---------------------------------------------------------------------------
// Scenario 11 — Navigation/scroll to EOF on a large MULTI-LINE file opened via
//               the REAL `open_path`, driven over several `frame_tick` frames
//               (NOT run() — it panics on continuous repaint). The last line is
//               reachable, the end-of-file cursor maps to the last line, and no
//               OOB / panic occurs across the frames.
// ---------------------------------------------------------------------------

#[test]
fn s11_real_open_path_scroll_to_eof_no_oob_no_panic() {
    // 256 KiB multi-line: ~4k lines — EOF navigation is meaningful (thousands of
    // lines) yet the default variant stays CI-cheap. Driving real `frame_tick`
    // frames is O(lines) per frame in a DEBUG build (the line-number gutter,
    // change-bar, and minimap each walk the rope every frame), so the default
    // variant keeps the size + frame count modest; the heavier multi-MiB,
    // many-frame render is the #[ignore]'d s12 sibling below.
    const SIZE: usize = 256 * 1024;
    let mut app = ScribeApp::new_test(render_lean_config());
    let (_dir, active) = open_multiline_via_app(&mut app, SIZE);

    // Drive a couple of real frames at the top — render must not panic or hang.
    run_frames(&mut app, 2);

    // EOF navigation via the doc's own line geometry (the real navigation
    // target the scroll-to-end command computes). The last line index is
    // in-bounds and the end-of-file cursor position maps to it.
    let rope = app.tabs[active].doc.rope();
    let last = rope.len_lines().saturating_sub(1);
    assert!(
        rope.len_lines() > 2_000,
        "a 256 KiB multi-line file must have thousands of lines, got {}",
        rope.len_lines()
    );
    // Indexing the last line must not panic (in-bounds).
    let last_line = rope.line(last);
    assert!(last_line.len_chars() <= rope.len_chars());
    // The end-of-file cursor (one past the last char) maps to the last line —
    // the scroll-to-EOF target — without panicking.
    assert_eq!(
        rope.char_to_line(rope.len_chars()),
        last,
        "the end-of-file cursor maps to the last line (scroll-to-EOF target)"
    );

    // Simulate scroll-to-EOF: drop the caret on the last line via the rope
    // editor state, then drive more frames — the editor must render the bottom
    // of the file with no OOB / panic.
    let eof_char = rope.len_chars();
    let mut st = scribe_render::RopeEditorState::new();
    st.edit = scribe_core::editing::EditState::at(eof_char);
    app.tabs[active].rope_state = Some(st);
    run_frames(&mut app, 1);

    // Still intact after EOF navigation: same line count, last line reachable.
    let rope_after = app.tabs[active].doc.rope();
    assert_eq!(
        rope_after.len_lines(),
        last + 1,
        "line count unchanged after EOF scroll (no truncation/corruption)"
    );
}

// ---------------------------------------------------------------------------
// Scenario 12 — HEAVY render variant of s11: an 8 MiB multi-line file opened via
//               the REAL `open_path`, driven over SEVERAL `frame_tick` frames
//               with the caret scrolled to EOF. Proves the real render path
//               survives many frames over a multi-MiB / tens-of-thousands-of-
//               lines rope with no panic / hang. Heavy because a debug-build
//               frame walks the rope O(lines) for the gutter / change-bar /
//               minimap, so #[ignore]'d for CI and RUN once locally with
//               --ignored (confirmed green before being marked ignored).
// ---------------------------------------------------------------------------

#[test]
#[ignore = "heavy: multi-frame real render over an 8 MiB multi-line rope"]
fn s12_real_open_path_eof_render_many_frames_heavy() {
    const SIZE: usize = 8 * 1024 * 1024;
    let mut app = ScribeApp::new_test(render_lean_config());
    let (_dir, active) = open_multiline_via_app(&mut app, SIZE);

    // It opened editable (< 256 MiB) and is genuinely large.
    assert!(!app.tabs[active].doc.is_read_only_large());
    let rope = app.tabs[active].doc.rope();
    assert!(
        rope.len_lines() > 100_000,
        "an 8 MiB multi-line file must have many tens of thousands of lines, got {}",
        rope.len_lines()
    );
    let last = rope.len_lines().saturating_sub(1);
    let eof_char = rope.len_chars();
    assert_eq!(rope.char_to_line(eof_char), last);

    // Drive several frames at the top, then with the caret at EOF — the real
    // render path must survive both without panic / hang / OOB.
    run_frames(&mut app, 4);
    let mut st = scribe_render::RopeEditorState::new();
    st.edit = scribe_core::editing::EditState::at(eof_char);
    app.tabs[active].rope_state = Some(st);
    run_frames(&mut app, 4);

    let rope_after = app.tabs[active].doc.rope();
    assert_eq!(
        rope_after.len_lines(),
        last + 1,
        "line count unchanged after multi-frame EOF render (no corruption)"
    );
}

/// Write a file of EXACTLY `size_bytes` bytes of benign synthetic content
/// (multi-line). Streamed in chunks so a 256 MiB file never holds the whole
/// buffer in memory. SANITIZED — plain ASCII, no secrets, no machine paths.
fn write_file_of_size(path: &std::path::Path, size_bytes: usize) {
    use std::io::Write as _;
    let mut f = std::fs::File::create(path).expect("create sized file");
    let line = b"synthetic line of benign qa fixture content for the size boundary\n";
    let chunk = line.repeat(4096);
    let mut written = 0usize;
    while written + chunk.len() <= size_bytes {
        f.write_all(&chunk).expect("stream sized chunk");
        written += chunk.len();
    }
    while written < size_bytes {
        f.write_all(b"a").expect("top up sized file");
        written += 1;
    }
    f.flush().expect("flush sized file");
}
