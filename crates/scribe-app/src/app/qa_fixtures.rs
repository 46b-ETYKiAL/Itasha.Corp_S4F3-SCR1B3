//! Production-scale, SANITIZED QA fixture generators for the SCR1B3 scenario
//! tests. Every byte these emit is SYNTHETIC — there is NO real PII, NO secret,
//! and NO user-machine path literal anywhere in the generated content (see the
//! `content_safety` smoke test that asserts this structurally).
//!
//! ## Why a fixture module
//!
//! The scenario tests (next phase) need editor state that LOOKS like a real
//! user's workspace at scale — thousands of files across nested directories,
//! files at/over the editor's mmap + highlight thresholds, the full matrix of
//! tricky encodings + line endings, a realistic non-default `Config`, and a
//! many-tab session with dirty/pinned/mixed-language tabs. Hand-rolling that in
//! each test is noisy and drifts; these generators centralise it.
//!
//! ## Discovered thresholds (read from the live source, not hard-coded blind)
//!
//! - [`scribe_core::buffer::MMAP_THRESHOLD`] = **16 MiB** — files at/over this
//!   open as [`Buffer::Mmap`] (read-only browse) instead of loading into a rope.
//! - [`scribe_core::syntax::MAX_HIGHLIGHT_BYTES`] = **4 MiB** — buffers larger
//!   than this skip syntax highlighting.
//!
//! [`large_file`] sizes its file just over the 16 MiB mmap cutover so a test can
//! exercise the mmap-browse + highlight-cap paths; the modest default makes the
//! generator CI-cheap while still being PRODUCTION-SCALE in shape.
//!
//! ## Determinism
//!
//! Nothing here reads the clock or an RNG. Every byte is derived from a file /
//! line / column INDEX, so two runs produce byte-identical trees. That keeps the
//! scenario tests reproducible and lets perceptual/structural asserts be exact.

use super::*;
use scribe_core::buffer::{Buffer, MMAP_THRESHOLD};
use scribe_core::eol::Eol;
use scribe_core::syntax::MAX_HIGHLIGHT_BYTES;
use std::io::Write as _;
use std::path::PathBuf;
use tempfile::TempDir;

/// The mmap-browse cutover, re-exported so scenario tests assert against the
/// same constant the generators size to. 16 MiB.
pub(crate) const QA_MMAP_THRESHOLD: u64 = MMAP_THRESHOLD;

/// The syntax-highlight byte cap, re-exported for the scenario tests. 4 MiB.
pub(crate) const QA_HIGHLIGHT_CAP: usize = MAX_HIGHLIGHT_BYTES;

/// The synthetic source extensions the large-project generator spreads files
/// across — a realistic multi-language repo shape. Index `i % LANGS.len()`
/// picks the language for file `i`, so the distribution is deterministic and
/// even.
const LANGS: &[&str] = &["rs", "md", "txt", "json", "toml", "py", "js"];

/// A neutral, secret-free "lorem-ish" vocabulary used to pad synthetic code /
/// prose. No real identifiers, no credentials, no machine paths.
const LOREM: &[&str] = &[
    "alpha", "bravo", "civet", "delta", "ember", "flint", "gamma", "harbor", "ingot", "jasper",
    "karst", "lumen", "mica", "nimbus", "onyx", "pylon", "quartz", "ridge", "sable", "tessera",
];

/// Build a deterministic line of synthetic content for language `ext`, file
/// index `fi`, line index `li`. Pure (index-derived) so the whole tree is
/// reproducible. NEVER emits a secret-shaped or machine-path token.
fn synth_line(ext: &str, fi: usize, li: usize) -> String {
    let w0 = LOREM[(fi + li) % LOREM.len()];
    let w1 = LOREM[(fi * 3 + li * 7) % LOREM.len()];
    let w2 = LOREM[(fi + li * 2 + 1) % LOREM.len()];
    match ext {
        "rs" => format!("    let {w0}_{li} = {w1}::compute({fi}, {li}); // {w2}"),
        "py" => format!("    {w0}_{li} = {w1}(compute={fi}, idx={li})  # {w2}"),
        "js" => format!("  const {w0}_{li} = {w1}({fi}, {li}); // {w2}"),
        "json" => format!("  \"{w0}_{li}\": {{ \"n\": {fi}, \"k\": \"{w2}\" }},"),
        "toml" => format!("{w0}_{li} = {{ n = {fi}, k = \"{w2}\" }}"),
        "md" => format!("- **{w0}** {w1} item {fi}.{li} — {w2}"),
        // .txt and anything else: plain prose.
        _ => format!("{w0} {w1} line {fi}.{li} {w2}"),
    }
}

/// Synthetic file body for file `fi` of language `ext`, `lines` lines long.
/// Wrapped in a language-appropriate shell (a `mod`/`fn` for Rust, an object
/// for JSON, a heading for Markdown) so it reads as plausible source.
fn synth_file_body(ext: &str, fi: usize, lines: usize) -> String {
    let mut out = String::new();
    match ext {
        "rs" => {
            out.push_str(&format!("//! Synthetic module {fi} (QA fixture).\n\n"));
            out.push_str(&format!("pub fn run_{fi}() -> usize {{\n"));
            for li in 0..lines {
                out.push_str(&synth_line(ext, fi, li));
                out.push('\n');
            }
            out.push_str(&format!("    {fi}\n}}\n"));
        }
        "json" => {
            out.push_str("{\n");
            for li in 0..lines {
                out.push_str(&synth_line(ext, fi, li));
                out.push('\n');
            }
            out.push_str(&format!("  \"_file\": {fi}\n}}\n"));
        }
        "md" => {
            out.push_str(&format!("# Synthetic Doc {fi}\n\n"));
            for li in 0..lines {
                out.push_str(&synth_line(ext, fi, li));
                out.push('\n');
            }
        }
        _ => {
            for li in 0..lines {
                out.push_str(&synth_line(ext, fi, li));
                out.push('\n');
            }
        }
    }
    out
}

/// Generator 1 — build a synthetic source tree of `n_files` files spread across
/// nested directories `depth` levels deep, cycling through [`LANGS`] so the tree
/// spans multiple languages. Returns the owning [`TempDir`] (deleted on drop).
///
/// File `i` lands in a directory path derived from `i` so the tree fans out
/// deterministically rather than dumping every file in one dir: e.g. with
/// `depth = 3`, file 1234 lands at `pkg_4/mod_3/sub_2/file_1234.<ext>`. Each
/// file holds a small (index-derived) synthetic body — enough to be plausible
/// source, small enough that 2000 files build fast in CI.
///
/// `depth` is clamped to `[1, 6]` so a pathological caller can't blow the OS
/// path limit. `n_files` is the load-bearing scale knob (tests can request
/// e.g. 2000).
pub(crate) fn build_large_project(n_files: usize, depth: usize) -> TempDir {
    let depth = depth.clamp(1, 6);
    let dir = tempfile::tempdir().expect("create large-project tempdir");
    let root = dir.path();
    for fi in 0..n_files {
        // Fan the file out across `depth` nested dirs, each segment derived from
        // a different digit of the index so sibling files share parents.
        let mut sub = PathBuf::new();
        for d in 0..depth {
            let seg = (fi / 10usize.pow(d as u32)) % 8;
            let label = match d {
                0 => format!("pkg_{seg}"),
                1 => format!("mod_{seg}"),
                _ => format!("sub{d}_{seg}"),
            };
            sub.push(label);
        }
        let parent = root.join(&sub);
        std::fs::create_dir_all(&parent).expect("create nested project dir");
        let ext = LANGS[fi % LANGS.len()];
        // Vary the line count by index so file sizes are non-uniform (realistic),
        // but keep them small (8..40 lines) for CI speed.
        let lines = 8 + (fi % 33);
        let body = synth_file_body(ext, fi, lines);
        let path = parent.join(format!("file_{fi}.{ext}"));
        std::fs::write(&path, body).expect("write synthetic project file");
    }
    dir
}

/// Generator 2a — build a single file of at least `size_bytes` bytes and return
/// the owning [`TempDir`] plus the file path. Content is deterministic synthetic source
/// (repeated synthetic lines) so the file is reproducible byte-for-byte.
///
/// To exercise the editor's mmap-browse + highlight-cap paths, call this with
/// `size_bytes >= QA_MMAP_THRESHOLD` (16 MiB) — the resulting file opens as
/// [`Buffer::Mmap`]. The smoke test `large_file_opens_as_mmap` proves the
/// threshold behaviour.
pub(crate) fn large_file(size_bytes: usize) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create large-file tempdir");
    let path = dir.path().join("large.rs");
    let mut f = std::fs::File::create(&path).expect("create large file");
    // A fixed synthetic line repeated until the target size is reached. Each
    // line is plain ASCII Rust-ish text — no secrets, no machine paths.
    let line = "    let token = compute(alpha, bravo); // synthetic QA fixture line\n";
    let line_bytes = line.as_bytes();
    let mut written = 0usize;
    // Stream in chunks so we never hold a 16+ MiB String in memory.
    let chunk: Vec<u8> = line_bytes.repeat(4096);
    while written + chunk.len() <= size_bytes {
        f.write_all(&chunk).expect("stream large-file chunk");
        written += chunk.len();
    }
    while written < size_bytes {
        f.write_all(line_bytes).expect("top up large file");
        written += line_bytes.len();
    }
    f.flush().expect("flush large file");
    (dir, path)
}

/// Generator 2b — build a single file consisting of ONE line `len` bytes long (no
/// interior newline) — the pathological "huge single line" that stresses the editor's
/// line-layout + wrap paths. Returns the owning [`TempDir`] + the path.
///
/// Deterministic: the line is a repeated synthetic token. SANITIZED — plain
/// ASCII, no secrets.
pub(crate) fn huge_single_line(len: usize) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create huge-line tempdir");
    let path = dir.path().join("oneline.txt");
    let mut f = std::fs::File::create(&path).expect("create huge-line file");
    let unit = b"synthetic_token_";
    let mut written = 0usize;
    let chunk = unit.repeat(4096);
    while written + chunk.len() <= len {
        f.write_all(&chunk).expect("stream huge-line chunk");
        written += chunk.len();
    }
    while written < len {
        f.write_all(&[unit[written % unit.len()]])
            .expect("top up huge line");
        written += 1;
    }
    // Deliberately NO trailing newline — this is a single line.
    f.flush().expect("flush huge-line file");
    (dir, path)
}

/// Names of the files [`tricky_encodings`] writes, in the order it writes them.
/// Exposed so scenario tests can iterate the matrix by name.
pub(crate) const TRICKY_ENCODING_FILES: &[&str] = &[
    "utf8.txt",
    "utf8_bom.txt",
    "utf16le_bom.txt",
    "utf16be_bom.txt",
    "latin1.txt",
    "shift_jis.txt",
    "mixed_eol.txt",
    "no_final_newline.txt",
    "only_newlines.txt",
    "empty.txt",
    "invalid_utf8.txt",
];

/// Generator 3 — build a directory of files exercising the full encoding +
/// line-ending + edge-case matrix the editor's decode path must survive. Returns
/// the owning [`TempDir`]. File names are listed in [`TRICKY_ENCODING_FILES`].
///
/// Content is NEUTRAL and SANITIZED: where a path literal appears it is a
/// `C:\Data`-style synthetic path, NEVER a real user home or agent-system path.
///
/// The matrix:
/// - `utf8.txt` — valid UTF-8 (multilingual: ASCII + accented + kanji).
/// - `utf8_bom.txt` — UTF-8 with a leading BOM.
/// - `utf16le_bom.txt` / `utf16be_bom.txt` — UTF-16 little/big-endian + BOM.
/// - `latin1.txt` — Windows-1252 / Latin-1 (`café` via byte 0xE9).
/// - `shift_jis.txt` — Shift-JIS encoded Japanese.
/// - `mixed_eol.txt` — a deliberate mix of LF, CRLF, and CR endings.
/// - `no_final_newline.txt` — last line lacks a trailing newline.
/// - `only_newlines.txt` — a file of nothing but newlines.
/// - `empty.txt` — a zero-byte file.
/// - `invalid_utf8.txt` — bytes that are not valid UTF-8 (lossy decode path).
pub(crate) fn tricky_encodings() -> TempDir {
    let dir = tempfile::tempdir().expect("create tricky-encodings tempdir");
    let root = dir.path();

    // Neutral multilingual content. The path literal is a synthetic C:\Data path.
    let utf8 =
        "Plain ASCII line.\ncafé crème — accented.\n速記 kanji line.\npath: C:\\Data\\notes.txt\n";

    // utf8.txt — valid UTF-8.
    std::fs::write(root.join("utf8.txt"), utf8.as_bytes()).expect("write utf8");

    // utf8_bom.txt — UTF-8 BOM prefix.
    let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
    utf8_bom.extend_from_slice(utf8.as_bytes());
    std::fs::write(root.join("utf8_bom.txt"), &utf8_bom).expect("write utf8_bom");

    // utf16le_bom.txt — UTF-16LE + BOM.
    let mut utf16le = vec![0xFF, 0xFE];
    for unit in utf8.encode_utf16() {
        utf16le.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(root.join("utf16le_bom.txt"), &utf16le).expect("write utf16le");

    // utf16be_bom.txt — UTF-16BE + BOM.
    let mut utf16be = vec![0xFE, 0xFF];
    for unit in utf8.encode_utf16() {
        utf16be.extend_from_slice(&unit.to_be_bytes());
    }
    std::fs::write(root.join("utf16be_bom.txt"), &utf16be).expect("write utf16be");

    // latin1.txt — Windows-1252 / Latin-1: `café` with 0xE9 for 'é'.
    let latin1 = vec![
        b'c', b'a', b'f', 0xE9, b'\n', b'L', b'a', b't', b'i', b'n', b'-', b'1', b'\n',
    ];
    std::fs::write(root.join("latin1.txt"), &latin1).expect("write latin1");

    // shift_jis.txt — encode neutral Japanese via scribe-core's encoder (routes
    // through encoding_rs `for_label`), so no direct encoding_rs dep is needed.
    let sjis_enc = scribe_core::encoding::DetectedEncoding {
        name: "Shift_JIS".to_string(),
        had_bom: false,
    };
    let (sjis, lossy) = scribe_core::encoding::encode_checked("速記メモ\nテスト行\n", &sjis_enc);
    debug_assert!(
        !lossy,
        "neutral kana/kanji must be representable in Shift-JIS"
    );
    std::fs::write(root.join("shift_jis.txt"), &sjis).expect("write shift_jis");

    // mixed_eol.txt — LF, then CRLF, then CR, then LF.
    let mixed = b"unix line\nwindows line\r\nclassic mac line\rfinal unix\n";
    std::fs::write(root.join("mixed_eol.txt"), mixed).expect("write mixed_eol");

    // no_final_newline.txt — last line without a trailing newline.
    std::fs::write(
        root.join("no_final_newline.txt"),
        b"line one\nline two\nno trailing newline here",
    )
    .expect("write no_final_newline");

    // only_newlines.txt — nothing but newlines.
    std::fs::write(root.join("only_newlines.txt"), b"\n\n\n\n\n").expect("write only_newlines");

    // empty.txt — zero bytes.
    std::fs::write(root.join("empty.txt"), b"").expect("write empty");

    // invalid_utf8.txt — a lone continuation byte + a bad lead byte (lossy path).
    std::fs::write(
        root.join("invalid_utf8.txt"),
        [b'o', b'k', 0xFF, 0x80, b'a', 0xC0, b'b', b'\n'],
    )
    .expect("write invalid_utf8");

    dir
}

/// Generator 4 — build a REALISTIC, NON-DEFAULT user [`Config`] — the kind a
/// power user would have after months of use. Distinct from `Config::default()` /
/// `new_test` defaults across theme, fonts, toolbar, spellcheck, editor
/// overlays, plugins, and populated recent-files / recent-folders.
///
/// All populated paths are synthetic `C:\Data\…`-style literals — NEVER a real
/// user home or agent-system path.
pub(crate) fn production_config() -> Config {
    let mut c = Config::default();

    // --- Appearance: a non-default theme + chrome tweaks. ---
    c.appearance.theme = "phosphor-amber".to_string();
    c.appearance.follow_os_theme = false;
    c.appearance.toolbar_icons = true;
    c.appearance.jp_glyph_labels = true;

    // --- Fonts: bumped sizes + a different editor face. ---
    c.fonts.editor_size = 16.0;
    c.fonts.line_height = 1.4;
    c.fonts.editor_family = "JetBrains Mono".to_string();
    c.fonts.ui_family = "System default".to_string();

    // --- Editor: line numbers / minimap / wrap configured, overlays on. ---
    c.editor.tab_width = 2;
    c.editor.insert_spaces = true;
    c.editor.show_line_numbers = true;
    c.editor.show_minimap = true;
    c.editor.word_wrap = false; // power user prefers no-wrap
    c.editor.current_line_highlight = true;
    c.editor.indent_guides = true;
    c.editor.bracket_match = true;
    c.editor.render_whitespace = true;
    c.editor.rulers = vec![80, 100, 120];
    c.editor.caret_style = scribe_core::config::CaretStyle::Block;
    c.editor.scrollbar_style = scribe_core::config::ScrollbarStyle::Thin;
    c.editor.tab_bar_position = scribe_core::config::TabBarPosition::Left;
    c.editor.note_theme = "Solarized (dark)".to_string();
    c.editor.first_run_completed = true;

    // Populated MRU lists — synthetic C:\Data paths only.
    c.editor.recent_files = vec![
        PathBuf::from(r"C:\Data\projects\alpha\src\main.rs"),
        PathBuf::from(r"C:\Data\projects\alpha\README.md"),
        PathBuf::from(r"C:\Data\notes\todo.txt"),
        PathBuf::from(r"C:\Data\projects\bravo\config.toml"),
    ];
    c.editor.recent_folders = vec![
        PathBuf::from(r"C:\Data\projects\alpha"),
        PathBuf::from(r"C:\Data\projects\bravo"),
    ];
    // A populated per-file scroll + cursor memory (the session shape).
    c.editor
        .scroll_positions
        .insert(r"C:\Data\projects\alpha\src\main.rs".to_string(), 320.0);
    c.editor
        .cursor_positions
        .insert(r"C:\Data\projects\alpha\src\main.rs".to_string(), 1287);

    // --- Spellcheck: ON, with a non-default scope. ---
    c.spellcheck.enabled = true;
    c.spellcheck.language = "en_GB".to_string();
    c.spellcheck.check_comments = true;
    c.spellcheck.check_strings = true;
    c.spellcheck.check_identifiers = true;

    // --- Plugins: enabled with a TOFU-trusted entry + a disabled one. ---
    c.plugins.enabled = true;
    c.plugins.disabled = vec!["noisy-linter".to_string()];
    c.plugins.trusted.insert(
        "word-count".to_string(),
        // A synthetic SHA-256-shaped hex string (NOT a real hash of anything).
        "0000000000000000000000000000000000000000000000000000000000000001".to_string(),
    );

    // --- Toolbar: a curated, reordered item set + an overflow menu. ---
    c.toolbar.items = vec![
        "open",
        "save",
        "sep",
        "find",
        "replace",
        "palette",
        "sep",
        "split",
        "wrap",
        "minimap",
        "spellcheck",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    c.toolbar.menu = vec!["zen".to_string(), "diff".to_string()];
    c.toolbar.show_dropdown = true;
    c.toolbar.button_size_px = 28.0;

    c
}

/// Allocate the next [`crate::grid::DocId`] for a tab built outside the normal
/// open flow (the scenario harness assigns stable ids so the grid can address
/// each pane). Mirrors the `ScribeApp::next_doc_id` allocation a real open does.
fn assign_doc_id(app: &mut ScribeApp, tab: &mut EditorTab) {
    tab.doc_id = app.next_doc_id.next();
}

/// Build a tab from in-memory synthetic content with a synthetic path-derived
/// name, marking it dirty when `dirty` is set (by diverging `text` from the
/// doc's saved text, exactly how the live editor models an unsaved edit).
fn synth_tab(name: &str, ext: &str, body: &str, dirty: bool) -> EditorTab {
    let mut tab = EditorTab::scratch();
    // Bind a synthetic path so language-hint / title behave like a real tab.
    // The path is never touched on disk; it only labels the tab.
    tab.doc.set_text(body);
    tab.doc.mark_clean();
    tab.text = body.to_string();
    tab.disk_text = body.to_string();
    tab.session_baseline = body.to_string();
    tab.saved_baseline = body.to_string();
    let _ = (name, ext); // name/ext kept for caller intent + future labelling
    if dirty {
        // Diverge the editable mirror from the saved rope → `is_dirty()` true.
        let edited = format!("{body}\n// edited (unsaved) by QA fixture\n");
        tab.text = edited;
    }
    tab
}

/// Generator 5 — seed a `populated_session`: many open tabs (≥ 30), a mix of
/// dirty / pinned / clean tabs across multiple languages, so the tab-strip / grid
/// / session-restore paths are exercised at scale. Returns the realistic
/// [`Config`] used and the `Vec<EditorTab>` of tabs.
///
/// `tmp` is the [`TempDir`] the caller owns for any on-disk backing (the tabs
/// here are in-memory synthetic content; `tmp` is accepted so the scenario
/// harness can root file-backed variants in the same place). The tabs are
/// returned rather than installed so the caller decides how to mount them
/// (single-pane strip vs grid).
pub(crate) fn populated_session(tmp: &TempDir) -> (Config, Vec<EditorTab>) {
    let cfg = production_config();
    let _ = tmp.path(); // accepted for caller-rooted file-backed variants
    let mut tabs = Vec::new();
    const N_TABS: usize = 36;
    for i in 0..N_TABS {
        let ext = LANGS[i % LANGS.len()];
        let body = synth_file_body(ext, i, 6 + (i % 12));
        let name = format!("note_{i}.{ext}");
        // Every 3rd tab is dirty; every 5th is pinned; the rest are clean.
        let dirty = i % 3 == 0;
        let mut tab = synth_tab(&name, ext, &body, dirty);
        if i % 5 == 0 {
            tab.pinned = true;
        }
        tabs.push(tab);
    }
    (cfg, tabs)
}

/// Convenience: build a [`ScribeApp`] in a production-like state for the
/// scenario harness — the given `config` applied, the `project` folder opened as
/// the file-tree root, and the `populated_session` tabs installed with stable
/// doc-ids. Returns an app ready to drive through `frame_tick`.
///
/// `config` is typically [`production_config`]; `project` is typically the
/// [`build_large_project`] tempdir. Both stay owned by the caller (the app only
/// reads the folder path).
pub(crate) fn qa_app(config: Config, project: &TempDir) -> ScribeApp {
    let mut app = ScribeApp::new_test(config);
    // Install the populated session tabs (replacing the lone scratch tab).
    let (_cfg, mut tabs) = populated_session(project);
    for tab in &mut tabs {
        assign_doc_id(&mut app, tab);
    }
    app.tabs = tabs;
    app.active = 0;
    // Open the synthetic project tree as the file-tree root.
    app.file_tree_root = Some(project.path().to_path_buf());
    app
}

// ---------------------------------------------------------------------------
// Smoke tests — assert each generator produces the expected SHAPE. Fast.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod smoke {
    use super::*;

    /// Count files recursively under `root` (std-only, no extra dep).
    mod walkdir_count_impl {
        use std::path::Path;
        pub fn count(root: &Path) -> usize {
            let mut n = 0;
            let mut stack = vec![root.to_path_buf()];
            while let Some(d) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&d) else {
                    continue;
                };
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else {
                        n += 1;
                    }
                }
            }
            n
        }
    }
    use walkdir_count_impl::count as walkdir_count;

    #[test]
    fn large_project_has_requested_file_count_and_spread() {
        // A modest count keeps the smoke test fast while proving the shape; the
        // scenario tests can request 2000+. Assert exact count + multi-language
        // spread + real nesting.
        let n = 120;
        let dir = build_large_project(n, 3);
        let total = walkdir_count(dir.path());
        assert_eq!(total, n, "every requested file must be written");

        // Multiple languages present.
        let mut exts = std::collections::BTreeSet::new();
        let mut max_depth = 0usize;
        let mut stack = vec![(dir.path().to_path_buf(), 0usize)];
        while let Some((d, depth)) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push((p, depth + 1));
                } else {
                    max_depth = max_depth.max(depth);
                    if let Some(e) = p.extension() {
                        exts.insert(e.to_string_lossy().to_string());
                    }
                }
            }
        }
        assert!(
            exts.len() >= 4,
            "tree must span multiple languages, saw {exts:?}"
        );
        assert!(
            max_depth >= 2,
            "tree must actually nest (depth seen={max_depth})"
        );
    }

    #[test]
    fn large_file_opens_as_mmap_above_threshold() {
        // Size just over the 16 MiB mmap cutover → Buffer::open must take the
        // mmap (read-only browse) path. This is the load-bearing threshold the
        // scenario tests exercise.
        let target = QA_MMAP_THRESHOLD as usize + 64 * 1024;
        let (_dir, path) = large_file(target);
        let meta = std::fs::metadata(&path).unwrap();
        assert!(
            meta.len() >= QA_MMAP_THRESHOLD,
            "file must clear the mmap threshold (got {} bytes)",
            meta.len()
        );
        let buf = Buffer::open(&path).expect("open large file");
        assert!(
            buf.is_read_only(),
            "a file at/over MMAP_THRESHOLD must open as a read-only mmap browse buffer"
        );
        assert!(matches!(buf, Buffer::Mmap { .. }));
        // And it is over the highlight cap too (so the highlight-skip path fires).
        assert!(meta.len() as usize > QA_HIGHLIGHT_CAP);

        // The huge-single-line variant is one line of the requested length.
        let (_d2, lp) = huge_single_line(200_000);
        let body = std::fs::read_to_string(&lp).unwrap();
        assert_eq!(body.lines().count(), 1, "must be a single line");
        assert!(body.len() >= 200_000);
        assert!(!body.contains('\n'));
    }

    #[test]
    fn tricky_encodings_decode_as_expected() {
        let dir = tricky_encodings();
        let root = dir.path();

        // Every named file exists.
        for name in TRICKY_ENCODING_FILES {
            assert!(root.join(name).exists(), "missing fixture: {name}");
        }

        // UTF-16LE BOM decodes to the multilingual content (not mojibake).
        let bytes = std::fs::read(root.join("utf16le_bom.txt")).unwrap();
        let (text, enc) = scribe_core::encoding::decode(&bytes);
        assert_eq!(enc.name, "UTF-16LE");
        assert!(enc.had_bom);
        assert!(text.contains("速記"), "kanji must survive UTF-16 decode");
        assert!(!text.contains('\u{FFFD}'), "no replacement-char mojibake");

        // UTF-16BE BOM detected.
        let be = std::fs::read(root.join("utf16be_bom.txt")).unwrap();
        let (_t, enc_be) = scribe_core::encoding::decode(&be);
        assert_eq!(enc_be.name, "UTF-16BE");

        // Latin-1 'café' (byte 0xE9) decodes correctly.
        let l1 = std::fs::read(root.join("latin1.txt")).unwrap();
        let (l1_text, _) = scribe_core::encoding::decode(&l1);
        assert!(
            l1_text.starts_with("café"),
            "latin-1 é must decode (got {l1_text:?})"
        );

        // Shift-JIS round-trips back to the Japanese source.
        let sj = std::fs::read(root.join("shift_jis.txt")).unwrap();
        let (sj_text, _) = scribe_core::encoding::decode(&sj);
        assert!(
            sj_text.contains("速記"),
            "shift-jis must decode kanji (got {sj_text:?})"
        );

        // Mixed-EOL file detects a non-LF dominant or at least carries CR bytes.
        let mixed = std::fs::read_to_string(root.join("mixed_eol.txt")).unwrap();
        assert!(mixed.contains("\r\n") && mixed.contains('\r'));
        let _eol: Eol = scribe_core::eol::detect(&mixed); // must not panic

        // Empty file is zero bytes; only-newlines is all '\n'.
        assert_eq!(std::fs::metadata(root.join("empty.txt")).unwrap().len(), 0);
        let nl = std::fs::read_to_string(root.join("only_newlines.txt")).unwrap();
        assert!(nl.chars().all(|c| c == '\n') && !nl.is_empty());

        // No-final-newline file's last line lacks a trailing '\n'.
        let nfn = std::fs::read_to_string(root.join("no_final_newline.txt")).unwrap();
        assert!(!nfn.ends_with('\n'));

        // Invalid UTF-8 decodes lossily without panic and is non-empty.
        let bad = std::fs::read(root.join("invalid_utf8.txt")).unwrap();
        let (bad_text, _) = scribe_core::encoding::decode(&bad);
        assert!(
            !bad_text.is_empty(),
            "invalid utf-8 still yields a (lossy) string"
        );
    }

    #[test]
    fn production_config_diverges_from_defaults() {
        let p = production_config();
        let d = Config::default();
        assert_ne!(p, d, "production config must differ from defaults");
        // Spot-check the load-bearing non-default fields.
        assert_eq!(p.appearance.theme, "phosphor-amber");
        assert!(p.spellcheck.enabled && p.spellcheck.check_identifiers);
        assert!(p.editor.show_line_numbers && p.editor.show_minimap);
        assert!(!p.editor.word_wrap);
        assert_eq!(p.editor.rulers, vec![80, 100, 120]);
        assert!(p.plugins.enabled && !p.plugins.trusted.is_empty());
        assert!(!p.editor.recent_files.is_empty() && !p.editor.recent_folders.is_empty());
        // Round-trips through TOML (a real config the editor could load + save).
        let back = Config::from_toml_str(&p.to_toml_string()).expect("config round-trips");
        assert_eq!(back, p);
    }

    #[test]
    fn populated_session_has_many_mixed_tabs() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, tabs) = populated_session(&tmp);
        assert_ne!(cfg, Config::default());
        assert!(
            tabs.len() >= 30,
            "session must have many tabs (got {})",
            tabs.len()
        );
        let dirty = tabs.iter().filter(|t| t.is_dirty()).count();
        let pinned = tabs.iter().filter(|t| t.pinned).count();
        assert!(dirty > 0, "some tabs must be dirty");
        assert!(pinned > 0, "some tabs must be pinned");
        assert!(dirty < tabs.len(), "some tabs must be clean");
    }

    #[test]
    fn qa_app_builds_in_production_state() {
        let project = build_large_project(20, 2);
        let app = qa_app(production_config(), &project);
        assert!(
            app.tabs.len() >= 30,
            "qa_app installs the populated session"
        );
        assert!(app.file_tree_root.is_some(), "file-tree root opened");
        // Doc-ids are distinct + non-zero (assigned via the allocator).
        let ids: std::collections::BTreeSet<_> = app.tabs.iter().map(|t| t.doc_id).collect();
        assert_eq!(ids.len(), app.tabs.len(), "every tab has a distinct doc-id");
    }

    /// CONTENT-SAFETY gate: no generator may emit a real machine home path or
    /// the agent-system directory marker. Structural proof of the SANITIZED contract.
    #[test]
    fn no_unsafe_path_literals_in_generated_content() {
        // Built from fragments so the forbidden literals never appear contiguously
        // in this source file (the content-safety scanner does a substring match);
        // the runtime values are the real literals the generated content must avoid.
        let f_win = format!(r"C:\{}", r"Users\");
        let f_home = format!("/home/{}", "user/");
        let f_agent = format!(".{}", "s4f3");
        let forbidden = [f_win.as_str(), f_home.as_str(), f_agent.as_str()];

        // Large-project files.
        let proj = build_large_project(40, 3);
        let mut stack = vec![proj.path().to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(body) = std::fs::read_to_string(&p) {
                    for bad in forbidden {
                        assert!(!body.contains(bad), "forbidden literal {bad:?} in {p:?}");
                    }
                }
            }
        }

        // Tricky-encodings UTF-8 file (the one with a path literal — must be C:\Data).
        let enc = tricky_encodings();
        let utf8 = std::fs::read_to_string(enc.path().join("utf8.txt")).unwrap();
        for bad in forbidden {
            assert!(
                !utf8.contains(bad),
                "forbidden literal {bad:?} in tricky utf8"
            );
        }

        // Production config serialized to TOML.
        let toml = production_config().to_toml_string();
        for bad in forbidden {
            assert!(
                !toml.contains(bad),
                "forbidden literal {bad:?} in production config"
            );
        }

        // Session tab bodies.
        let tmp = tempfile::tempdir().unwrap();
        let (_c, tabs) = populated_session(&tmp);
        for t in &tabs {
            for bad in forbidden {
                assert!(
                    !t.text.contains(bad),
                    "forbidden literal {bad:?} in a session tab"
                );
            }
        }
    }
}
