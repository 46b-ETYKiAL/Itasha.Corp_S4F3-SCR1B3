//! Offscreen render-to-image VISUAL QA harness.
//!
//! These tests render the REAL `ScribeApp` frame to a PNG via egui_kittest's
//! wgpu backend, so the app's actual pixels can be inspected (the change bar,
//! occurrence boxes, gutter, status bar, modals, …) — the one thing the
//! AccessKit-based e2e tests cannot do.
//!
//! They are `#[ignore]` by default and gated behind a real-adapter probe, so a
//! GPU-less CI runner never runs (or fails) them. Run on a GPU host with:
//!   cargo test -p scribe-app --features … visual_qa -- --ignored --nocapture
//! Each scene prints the PNG path it wrote.

use super::*;
use egui_kittest::Harness;

/// True if a usable wgpu adapter resolves on this host. Avoids the panic
/// `Harness::wgpu()` raises when no adapter exists, so CI skips cleanly.
fn gpu_available() -> bool {
    let instance = wgpu::Instance::default();
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .is_ok()
}

fn out_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join("scr1b3-visual-qa");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Render `app`'s frame to `<temp>/scr1b3-visual-qa/<name>.png`. Returns the
/// path, or `None` when no GPU adapter is available (clean skip).
fn render_scene(name: &str, w: f32, h: f32, app: ScribeApp) -> Option<std::path::PathBuf> {
    if !gpu_available() {
        eprintln!("[visual-qa] no GPU adapter; skipping `{name}`");
        return None;
    }
    let mut harness: Harness<'static, ScribeApp> = Harness::builder()
        .with_size(egui::vec2(w, h))
        .wgpu()
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    // A few frames so the one-frame-lagged gutter Ys + galley layout settle.
    for _ in 0..5 {
        harness.step();
    }
    let img = harness
        .render()
        .expect("kittest wgpu render of the real ScribeApp frame must succeed");
    let path = out_dir().join(format!("{name}.png"));
    img.save(&path).expect("save visual-qa png");
    eprintln!(
        "[visual-qa] wrote {} ({}x{})",
        path.display(),
        img.width(),
        img.height()
    );
    Some(path)
}

/// Base config for QA scenes: first-run done (no welcome modal) and motion
/// disabled so the frame is static (no perpetual repaint → render is stable).
fn qa_config() -> Config {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.motion.enabled = false;
    cfg
}

const SAMPLE: &str = "fn main() {\n    let x = 1;\n    let y = 2;\n    println!(\"{x} {y}\");\n}\n";

#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_default() {
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    render_scene("default", 1100.0, 720.0, app);
}

/// A markdown-rich note sample used by the note-colouring QA scenes — exercises
/// headings, setext underlines, `----` thematic breaks, decorative `====//`
/// dividers, bold/italic/code/strike, quotes, lists, task boxes, links, `#tags`,
/// and tables.
const MD_COLOR_SAMPLE: &str = r#"# ATX Heading 1
## ATX Heading 2
### ATX Heading 3

Setext Heading 1
================

Setext Heading 2
----------------

Thematic break below:

----

Decorative divider:

====//====//====//

Some **bold**, *italic*, `inline code` and ~~strikethrough~~ text.

> A blockquote line.

- bullet item
- [ ] unchecked task
- [x] checked task
1. numbered item

```rust
fn code() { let x = 1; }
```

A [link](https://example.com), a #tag, and a bare https://bare.example.com URL.

| col a | col b |
|:-----:|-------|
| 1     | 2     |
"#;

/// Render the markdown sample under a given note theme. Writes a temp `.md` so
/// `language_hint` routes to the markdown grammar; sets `note_theme` so the
/// frame applies the chosen palette.
fn render_markdown_scene(scene: &str, note_theme: &str, rich: bool) {
    let path = out_dir().join(format!("{scene}.md"));
    std::fs::write(&path, MD_COLOR_SAMPLE).expect("write md sample");
    let mut cfg = qa_config();
    cfg.editor.note_theme = note_theme.to_string();
    cfg.editor.md_rich_coloring = rich;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let t = EditorTab::from_path(path).expect("open md sample");
    app.tabs.push(t);
    app.active = 0;
    render_scene(scene, 1100.0, 900.0, app);
}

/// The default note theme with ALL markdown-colouring passes on — confirms
/// `----` / `====//` dividers, `#tags`, `~~strikethrough~~`, task boxes, and
/// table pipes are all coloured.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_markdown_coloring() {
    render_markdown_scene("markdown_coloring", "base16-eighties.dark", true);
}

/// Master switch OFF — proves every extra pass can be disabled: the same note
/// falls back to plain syntect grammar highlighting (dividers/tags/strike/tasks/
/// tables uncoloured).
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_markdown_no_coloring() {
    render_markdown_scene("markdown_no_coloring", "base16-eighties.dark", false);
}

/// A newly-added popular palette (Dracula) — confirms the new note themes load
/// and colour markdown (headings/bold/italic/link/code/divider).
#[test]
#[ignore = "GPU render; run with --ignore on a host with a wgpu adapter"]
fn scene_markdown_dracula() {
    render_markdown_scene("markdown_dracula", "Dracula", true);
}

/// NARROW window + a LONG status string — proves the bottom status-bar filename
/// (right-aligned `self.status`, e.g. "opened /…/file.rs") TRUNCATES with an
/// ellipsis instead of overflowing leftward and overlapping the left-side
/// indicators (EOL / encoding / language / counts / caret). Read the PNG: the
/// left segments must stay legible and the status text must end in "…" at the
/// boundary, never paint over the left text.
#[test]
#[ignore = "GPU render"]
fn scene_status_bar_narrow() {
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    // A long status path that, on a narrow window, would previously overflow the
    // right_to_left status segment leftward across the left indicators.
    app.status =
        "opened /workspace/projects/very/deep/nested/path/to/a/long_file_name.rs".to_string();
    render_scene("status_bar_narrow", 480.0, 360.0, app);
}

/// Change bar: line 2 edited+saved (green), line 3 edited+unsaved (amber),
/// the rest untouched (no stripe).
#[test]
#[ignore = "GPU render"]
fn scene_change_bar() {
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    // current text
    t.text = SAMPLE.to_string();
    // session baseline differs on lines 2 AND 3 (both edited this session)
    t.session_baseline =
        "fn main() {\n    let A = 1;\n    let B = 2;\n    println!(\"{x} {y}\");\n}\n".to_string();
    // saved baseline matches line 2 (so it's Saved/green) but still differs on
    // line 3 (so it stays Unsaved/amber).
    t.saved_baseline =
        "fn main() {\n    let x = 1;\n    let B = 2;\n    println!(\"{x} {y}\");\n}\n".to_string();
    t.change_gen = None;
    app.tabs.push(t);
    app.active = 0;
    render_scene("change_bar", 1100.0, 720.0, app);
}

/// Find bar open with a query → the highlight-all match washes (same paint
/// path the selection-occurrence boxes reuse).
#[test]
#[ignore = "GPU render"]
fn scene_find_bar() {
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    app.find_open = true;
    app.find_query = "let".to_string();
    render_scene("find_bar", 1100.0, 720.0, app);
}

/// Trailing-whitespace tint + column rulers, with content that has trailing
/// spaces on a couple of lines.
#[test]
#[ignore = "GPU render"]
fn scene_trailing_ws_and_rulers() {
    let mut cfg = qa_config();
    cfg.editor.highlight_trailing_whitespace = true;
    cfg.editor.rulers = vec![20, 40];
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = "fn main() {   \n    let x = 1;\n    let y = 2;    \n}\n".to_string();
    t.session_baseline = t.text.clone();
    t.saved_baseline = t.text.clone();
    app.tabs.push(t);
    app.active = 0;
    render_scene("trailing_ws_rulers", 1100.0, 720.0, app);
}

/// Settings window open (Editor section) — checks the settings layout +
/// widths + the new change-bar / occurrence / trailing-ws toggles render.
#[test]
#[ignore = "GPU render"]
fn scene_settings() {
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    app.settings_open = true;
    render_scene("settings", 1100.0, 720.0, app);
}

/// Several tabs incl. a dirty one + a pinned one — checks the tab strip
/// layout, the dirty `*` marker, the pin glyph, and active-tab styling.
#[test]
#[ignore = "GPU render"]
fn scene_tabs() {
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    for (i, name) in ["main.rs", "lib.rs", "notes.md", "config.toml"]
        .iter()
        .enumerate()
    {
        let mut t = EditorTab::scratch();
        t.text = format!("// {name}\n{SAMPLE}");
        t.session_baseline = t.text.clone();
        t.saved_baseline = t.text.clone();
        t.doc_id = crate::grid::DocId(i as u64);
        app.tabs.push(t);
    }
    app.tabs[0].pinned = true;
    // Make tab 2 look dirty (text diverges from the saved doc mirror).
    app.tabs[2].text.push_str("\nunsaved edit\n");
    app.active = 1;
    render_scene("tabs", 1100.0, 720.0, app);
}

/// Top-bar button chrome parity (Fix 1). The LEFT toolbar buttons must now read
/// as FRAMELESS — transparent when idle, matching the RIGHT window-caption
/// buttons — with a persistent accent fill ONLY on a toggled-ON toggle. This
/// scene turns the minimap + word-wrap toggles ON, so the PNG should show those
/// two carrying a low-alpha accent background while `>_`, new/open/save/find/
/// split/⋯ and the OFF toggles are all transparent (no filled button boxes).
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_toolbar_frameless() {
    let mut cfg = qa_config();
    // Two toggles ON so the accent on-fill is visible next to frameless buttons.
    cfg.editor.show_minimap = true;
    cfg.editor.word_wrap = true;
    // Ensure a rich set of quick-access items is on the bar so the frameless
    // treatment is visible across plain buttons AND toggles.
    cfg.toolbar.items = [
        "new",
        "open",
        "save",
        "find",
        "split",
        "minimap",
        "wrap",
        "linenumbers",
        "spellcheck",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    render_scene("toolbar_frameless", 1100.0, 200.0, app);
}

/// Split-view divider (Fix 2). With `grid_enabled` and two open notes, the grid
/// lays them side-by-side; the PNG should now show a thin theme-accent line down
/// the boundary BETWEEN the two panes (instead of the old empty 4 px gap), and
/// no line on the outer edges. Read the vertical seam at the window mid-line.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_split_divider() {
    let mut cfg = qa_config();
    cfg.editor.grid_enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    for (i, (name, body)) in [
        ("left.rs", "// left pane\n"),
        ("right.rs", "// right pane\n"),
    ]
    .iter()
    .enumerate()
    {
        let mut t = EditorTab::scratch();
        t.text = format!("{body}{SAMPLE}");
        t.session_baseline = t.text.clone();
        t.saved_baseline = t.text.clone();
        // Distinct doc ids so the grid lays out two separate panes (sync would
        // assign these anyway; setting them keeps the scene deterministic).
        t.doc_id = crate::grid::DocId(i as u64 + 1);
        let _ = name;
        app.tabs.push(t);
    }
    app.active = 0;
    render_scene("split_divider", 1100.0, 720.0, app);
}

/// Highlight-all-occurrences: a single-line buffer with the word `let`
/// repeated; inject a selection of the FIRST `let` so the other two get the
/// occurrence box. Selection is set on the egui TextEditState between frames.
#[test]
#[ignore = "GPU render"]
fn scene_highlight_occurrences() {
    if !gpu_available() {
        eprintln!("[visual-qa] no GPU adapter; skipping `highlight_occurrences`");
        return;
    }
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = "let aaa = 1; let bbb = 2; let ccc = 3;\n".to_string();
    t.session_baseline = t.text.clone();
    t.saved_baseline = t.text.clone();
    app.tabs.push(t);
    app.active = 0;

    // Inject a selection of the first `let` (chars 0..3) on frame 2 — once the
    // editor's TextEditState exists — from inside the frame closure (where the
    // egui Context is in scope; the Harness exposes no ctx accessor).
    let mut frame = 0u32;
    let mut harness: Harness<'static, ScribeApp> = Harness::builder()
        .with_size(egui::vec2(1100.0, 300.0))
        .wgpu()
        .build_state(
            move |ctx, app: &mut ScribeApp| {
                app.frame_tick(ctx);
                frame += 1;
                if frame == 1 {
                    let id = egui::Id::new("scr1b3-central-editor");
                    if let Some(mut st) = egui::TextEdit::load_state(ctx, id) {
                        st.cursor.set_char_range(Some(egui::text::CCursorRange {
                            primary: egui::text::CCursor::new(3),
                            secondary: egui::text::CCursor::new(0),
                            h_pos: None,
                        }));
                        st.store(ctx, id);
                    }
                    ctx.memory_mut(|m| m.request_focus(id));
                }
            },
            app,
        );
    for _ in 0..5 {
        harness.step();
    }
    let img = harness.render().expect("wgpu render");
    let path = out_dir().join("highlight_occurrences.png");
    img.save(&path).expect("save png");
    eprintln!(
        "[visual-qa] wrote {} ({}x{})",
        path.display(),
        img.width(),
        img.height()
    );
}

/// Spell-check scoping on a REAL `.rs` file: language_hint = "rust" →
/// `SpellScope` restricts the check to comments/strings (check_identifiers
/// defaults false), so keywords (`fn`/`let`/`println`) must NOT be squiggled —
/// only the typos in the comment + the string. (The earlier scratch-tab QA
/// scene had no extension → whole-text fallback → keywords got flagged; this
/// scene proves real-file behavior.)
#[test]
#[ignore = "GPU render"]
fn scene_spellcheck_code() {
    use std::io::Write as _;
    let mut f = tempfile::Builder::new()
        .suffix(".rs")
        .tempfile()
        .expect("temp .rs");
    write!(
        f,
        "fn main() {{\n    // ths sentnce has speling typoz\n    let x = 1;\n    println!(\"helllo wrld\");\n}}\n"
    )
    .unwrap();
    let path = f.path().to_path_buf();
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let tab = EditorTab::from_path(path).expect("open temp .rs");
    app.tabs.push(tab);
    app.active = 0;
    render_scene("spellcheck_code", 1100.0, 400.0, app);
    drop(f); // keep the temp file alive until after the render
}

/// Minimap viewport-indicator accuracy: a LONG document scrolled toward the
/// bottom. The highlight box must overlay the minimap rows whose text equals the
/// editor's visible lines (the fix: content + indicator share one fit-to-height
/// scale). Renders the real frame so the alignment is in the PNG for inspection.
///
/// Each line is numbered so the visible band in the editor can be read off and
/// cross-checked against the highlighted minimap region. Drives `pending_scroll`
/// from inside the frame so the editor scrolls to ~70% before the capture frame.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_minimap_scrolled() {
    if !gpu_available() {
        eprintln!("[visual-qa] no GPU adapter; skipping `minimap_scrolled`");
        return;
    }
    let mut cfg = qa_config();
    cfg.editor.show_minimap = true;
    cfg.editor.word_wrap = false; // exercise the no-wrap P2 mapping path
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    // 400 distinctly-numbered lines so the visible band is legible in the PNG.
    let mut body = String::new();
    for i in 1..=400 {
        body.push_str(&format!(
            "line {i:03}  fn item_{i:03}() {{ /* row {i:03} */ }}\n"
        ));
    }
    t.text = body.clone();
    t.session_baseline = body.clone();
    t.saved_baseline = body;
    app.tabs.push(t);
    app.active = 0;

    let mut frame = 0u32;
    let mut harness: Harness<'static, ScribeApp> = Harness::builder()
        .with_size(egui::vec2(1100.0, 720.0))
        .wgpu()
        .build_state(
            move |ctx, app: &mut ScribeApp| {
                app.frame_tick(ctx);
                frame += 1;
                // Once the editor has reported its real content height, scroll to
                // ~70% of the scrollable range and hold there for the capture.
                if frame >= 2 {
                    let (_off, content_h, view_h) = app.scroll_metrics;
                    let max_off = (content_h - view_h).max(0.0);
                    app.pending_scroll = Some(max_off * 0.7);
                }
            },
            app,
        );
    for _ in 0..6 {
        harness.step();
    }
    let img = harness.render().expect("wgpu render");
    let path = out_dir().join("minimap_scrolled.png");
    img.save(&path).expect("save png");
    eprintln!(
        "[visual-qa] wrote {} ({}x{})",
        path.display(),
        img.width(),
        img.height()
    );
}

/// Tint OFF baseline — a note with visible body text, no window tint. Read this
/// alongside `tint_strong_red` below: the background surfaces (titlebar,
/// toolbar, gutter, status bar) are the theme's dark chrome and the body text is
/// the theme foreground. It is the BEFORE frame for the tint bug fix.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tint_off() {
    let mut cfg = qa_config();
    cfg.window.tint = "#ff0000".to_string();
    cfg.window.tint_strength = 0.0; // OFF
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    render_scene("tint_off", 1100.0, 720.0, app);
}

/// Tint ON, STRONG red (#ff0000 @ 0.8) — the AFTER frame. The fix blends the
/// tint into the BACKGROUND fill colours (`panel_fill` chrome), so the chrome
/// surfaces shift clearly toward red while the body-text glyphs keep their
/// original theme foreground hue (the tint never touches glyph colours). Read
/// this next to `tint_off`: chrome background = clearly red-shifted; the code
/// glyphs (`fn`, `let`, `println!`, identifiers) = unchanged hue.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tint_strong_red() {
    let mut cfg = qa_config();
    cfg.window.tint = "#ff0000".to_string();
    cfg.window.tint_strength = 0.8; // STRONG
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    render_scene("tint_strong_red", 1100.0, 720.0, app);
}

/// Same strong-red tint but the `Enable window tint` toggle is OFF — proves the
/// master switch removes the tint from the main window even with a colour +
/// strength set. Read the PNG: the app background must be the plain (dark) theme
/// colour, not red.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tint_disabled() {
    let mut cfg = qa_config();
    cfg.window.tint = "#ff0000".to_string();
    cfg.window.tint_strength = 0.8;
    cfg.window.tint_enabled = false; // toggle OFF
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    render_scene("tint_disabled", 1100.0, 720.0, app);
}

/// DIAGNOSTIC — strong tint with the SETTINGS WINDOW OPEN (opaque mode). The
/// bug report: the tint appears on the Settings popup but NOT the main app.
/// Read the PNG: the main app chrome + editor well must be tinted AND the
/// Settings window must NOT be tinted.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tint_settings_open() {
    let mut cfg = qa_config();
    cfg.window.tint = "#ff0000".to_string();
    cfg.window.tint_strength = 0.8;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    app.settings_open = true;
    render_scene("tint_settings_open", 1100.0, 720.0, app);
}

/// DIAGNOSTIC — strong tint in GLASS (translucent) mode. Reproduces the mode
/// where the tinted panel fill is composited at reduced opacity, to check
/// whether the main-app tint washes out vs the opaque Settings window.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tint_glass_settings_open() {
    let mut cfg = qa_config();
    cfg.window.tint = "#ff0000".to_string();
    cfg.window.tint_strength = 0.8;
    cfg.window.transparency_enabled = true;
    cfg.window.opacity = 0.5;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    app.settings_open = true;
    render_scene("tint_glass_settings_open", 1100.0, 720.0, app);
}

/// #82 — rotate-ON side tabs MID-DRAG: the drop-insertion hairline must sit in
/// the GAP between two stacked tab chips, never inside a chip's outline. Forces
/// the drag pointer into the gap between chip 0 and chip 1 via the test hook,
/// then renders the REAL frame so the indicator is in the PNG for visual QA.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_rotated_sidetab_drop_indicator() {
    use super::tab_strip_render::{TEST_FORCE_SIDE_TAB_DRAG, TEST_ROTATED_TAB_RECTS};

    fn rotated_app() -> ScribeApp {
        let mut cfg = qa_config();
        cfg.appearance.frameless = false;
        cfg.editor.tab_bar_position = scribe_core::config::TabBarPosition::Left;
        cfg.editor.side_tabs_rotated = true;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs.clear();
        for i in 0..3 {
            let mut t = EditorTab::scratch();
            t.text = format!("document {i}\nbody line\n");
            app.tabs.push(t);
        }
        // Active = the BOTTOM tab so the chip-0/chip-1 gap (where the drop line
        // paints) is flanked by MUTED tabs — the accent drop hairline can't be
        // confused with the active tab's accent outline.
        app.active = 2;
        app
    }

    const W: f32 = 900.0;
    const H: f32 = 600.0;

    // Phase 1 (CPU): render to capture the chip rects; aim the forced pointer at
    // the gap between chip 0 and chip 1.
    TEST_FORCE_SIDE_TAB_DRAG.with(|c| c.set(None));
    TEST_ROTATED_TAB_RECTS.with(|r| r.borrow_mut().clear());
    {
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(W, H))
            .build_state(
                |ctx, app: &mut ScribeApp| app.frame_tick(ctx),
                rotated_app(),
            );
        h.run();
        h.run();
    }
    let rects = TEST_ROTATED_TAB_RECTS.with(|r| r.borrow().clone());
    assert!(
        rects.len() >= 2,
        "need >=2 rotated chips for the drop-indicator scene"
    );
    let pointer = egui::pos2(
        rects[0].center().x,
        (rects[0].center().y + rects[1].center().y) * 0.5,
    );
    TEST_FORCE_SIDE_TAB_DRAG.with(|c| c.set(Some(pointer)));

    // Phase 2 (GPU): render the real frame with the forced drag → the insertion
    // hairline paints in the chip-0/chip-1 gap; saved to PNG for inspection.
    let path = render_scene("rotated_sidetab_drop_indicator", W, H, rotated_app());
    TEST_FORCE_SIDE_TAB_DRAG.with(|c| c.set(None));
    if let Some(p) = path {
        eprintln!("[#82] rotated drop-indicator scene -> {}", p.display());
    }
}

// ---------------------------------------------------------------------------
// v0.4.58 note-tab-bar wave — every dock position + both side variants, so the
// four fixes can be verified in a PNG:
//   Fix 1/2: the "+" button is frameless-until-hover + centred (like a top-bar
//            button), NOT the old grey framed `small_button` slab.
//   Fix 3:   a non-selected tab shows a faint hover fill (hover scenes).
//   Fix 4:   a 1px theme-tinted divider separates adjacent tabs in EVERY
//            position — including left/right (both non-rotated and rotated).
//   Follow-up 1/2: horizontal side-bar titles ellipsise (shrink) / wrap to 2
//            lines (opt-in).
// ---------------------------------------------------------------------------

/// Build a note-tab-bar QA scene app: native (non-frameless) chrome, the given
/// dock position / rotation / 2-line option, and one real file tab per title
/// (real files so `title()` shows distinct names). `pinned` marks tab indices
/// pinned; `active` selects the accented tab.
fn tabbar_scene_app(
    position: scribe_core::config::TabBarPosition,
    rotated: bool,
    two_line: bool,
    titles: &[&str],
    active: usize,
    pinned: &[usize],
) -> ScribeApp {
    let mut cfg = qa_config();
    cfg.appearance.frameless = false;
    cfg.editor.tab_bar_position = position;
    cfg.editor.side_tabs_rotated = rotated;
    cfg.editor.side_tabs_wrap_two_lines = two_line;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    for (i, name) in titles.iter().enumerate() {
        let path = out_dir().join(name);
        std::fs::write(&path, format!("// {name}\ncontent line\n")).expect("write scene file");
        let mut t = EditorTab::from_path(path).expect("open scene file");
        t.doc_id = crate::grid::DocId(i as u64 + 1);
        if pinned.contains(&i) {
            t.pinned = true;
        }
        app.tabs.push(t);
    }
    app.active = active.min(app.tabs.len().saturating_sub(1));
    app
}

/// Like [`render_scene`] but injects a pointer hover at `hover` before the final
/// capture, so a hover-only affordance (Fix 3's non-selected tab highlight)
/// appears in the PNG.
fn render_scene_hover(
    name: &str,
    w: f32,
    h: f32,
    app: ScribeApp,
    hover: egui::Pos2,
) -> Option<std::path::PathBuf> {
    if !gpu_available() {
        eprintln!("[visual-qa] no GPU adapter; skipping `{name}`");
        return None;
    }
    let mut harness: Harness<'static, ScribeApp> = Harness::builder()
        .with_size(egui::vec2(w, h))
        .wgpu()
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    for _ in 0..5 {
        harness.step();
    }
    harness.hover_at(hover);
    for _ in 0..3 {
        harness.step();
    }
    let img = harness.render().expect("kittest wgpu render must succeed");
    let path = out_dir().join(format!("{name}.png"));
    img.save(&path).expect("save visual-qa png");
    eprintln!("[visual-qa] wrote {} (hover)", path.display());
    Some(path)
}

const TABBAR_TITLES: &[&str] = &["main.rs", "lib.rs", "notes.md", "config.toml"];

/// TOP dock — the four fixes' baseline: a horizontal strip with a frameless "+"
/// at the row end and vertical dividers between chips. Read: the "+" has NO grey
/// box at idle; thin lines sit between adjacent tabs.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_top() {
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Top,
        false,
        false,
        TABBAR_TITLES,
        1,
        &[0],
    );
    render_scene("tabbar_top", 1000.0, 240.0, app);
}

/// BOTTOM dock — same horizontal strip, docked above the status bar.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_bottom() {
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Bottom,
        false,
        false,
        TABBAR_TITLES,
        1,
        &[0],
    );
    render_scene("tabbar_bottom", 1000.0, 300.0, app);
}

/// LEFT dock, HORIZONTAL labels (non-rotated). Read: HORIZONTAL dividers between
/// stacked tabs (the left/right-divider fix), the active tab accented, a pinned
/// tab (dimmed grip), and the frameless centred "+" below the column.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_left_horizontal() {
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Left,
        false,
        false,
        TABBAR_TITLES,
        1,
        &[0],
    );
    render_scene("tabbar_left_horizontal", 900.0, 560.0, app);
}

/// RIGHT dock, HORIZONTAL labels — mirror of the left scene; confirms the
/// dividers + frameless "+" also render on the right edge.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_right_horizontal() {
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Right,
        false,
        false,
        TABBAR_TITLES,
        1,
        &[0],
    );
    render_scene("tabbar_right_horizontal", 900.0, 560.0, app);
}

/// LEFT dock, ROTATED (vertical-text) variant. Read: HORIZONTAL dividers between
/// the stacked rotated chips (Fix 4 for the rotated variant) and the frameless
/// centred "+" at the column foot.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_left_rotated() {
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Left,
        true,
        false,
        TABBAR_TITLES,
        1,
        &[0],
    );
    render_scene("tabbar_left_rotated", 900.0, 560.0, app);
}

/// Fix 3 hover — LEFT horizontal bar with the pointer over the (non-selected)
/// SECOND tab. Read: that tab carries a faint hover fill (lighter than the
/// active tab's accent), painted BEHIND its label text. Hover coordinate targets
/// the 2nd row of the left column.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_left_hover() {
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Left,
        false,
        false,
        TABBAR_TITLES,
        0,
        &[],
    );
    // The left column starts just below the top toolbar; the 2nd tab sits a bit
    // lower. (Verified against the rendered PNG.)
    render_scene_hover(
        "tabbar_left_hover",
        900.0,
        560.0,
        app,
        egui::pos2(60.0, 92.0),
    );
}

/// Follow-up 1 — a LONG title on a LEFT horizontal bar. The panel opens at its
/// clamped fit width, but a title wider than that ELLIPSISES on one line
/// ("a-very-long-…"). This proves the truncating galley renders; the shrink
/// interaction is pinned by the `tabbar_resize_tests` interaction test.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_left_narrow_ellipsis() {
    let titles = &[
        "a-very-long-note-title-that-overflows-the-bar.md",
        "short.md",
        "another-fairly-long-filename-here.rs",
    ];
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Left,
        false,
        false,
        titles,
        1,
        &[],
    );
    render_scene("tabbar_left_narrow_ellipsis", 900.0, 480.0, app);
}

/// Follow-up 2 — the SAME long titles with the "Wrap note titles to 2 lines"
/// option ON. Read: a title too long for one line now WRAPS to a second line
/// (chip grows taller); a title too long for even two lines elides the 2nd row.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_left_two_lines() {
    let titles = &[
        "a-very-long-note-title-that-overflows-the-bar.md",
        "short.md",
        "another-fairly-long-filename-here.rs",
    ];
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Left,
        false,
        true, // 2-line wrap ON
        titles,
        1,
        &[],
    );
    render_scene("tabbar_left_two_lines", 900.0, 480.0, app);
}
