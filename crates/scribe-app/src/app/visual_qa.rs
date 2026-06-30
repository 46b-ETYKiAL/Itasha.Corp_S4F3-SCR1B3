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
