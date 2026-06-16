//! Wave-3 perf: the minimap + spellcheck caches now key off a per-tab
//! `edit_gen` counter instead of re-hashing the whole buffer every frame.
//! Correctness hinges on EVERY text mutation advancing the counter — these
//! tests pin the Rust-side mutation funnels (Class A + Class B). The egui /
//! rope write-back paths (Class C/D) require a live frame and are covered by
//! the `out.response.changed()` / `resp.content_changed` hooks directly.
use super::{use_rope_editor, ScribeApp};
use scribe_core::Config;

fn gen(app: &ScribeApp) -> u64 {
    app.tabs[app.active].edit_gen
}

#[test]
fn use_rope_editor_decision_matrix() {
    // The experimental opt-in forces the rope editor regardless of size.
    assert!(use_rope_editor(true, 0, 0));
    assert!(use_rope_editor(true, 10, 16 * 1024 * 1024));
    // threshold 0 disables auto-promotion no matter how big the buffer is.
    assert!(!use_rope_editor(false, usize::MAX, 0));
    // Below the threshold → the canonical egui TextEdit path.
    assert!(!use_rope_editor(false, 1024, 16 * 1024 * 1024));
    // At or above the threshold → the viewport-culled rope path.
    assert!(use_rope_editor(false, 16 * 1024 * 1024, 16 * 1024 * 1024));
    assert!(use_rope_editor(false, 32 * 1024 * 1024, 16 * 1024 * 1024));
}

#[test]
fn set_text_advances_edit_gen() {
    let mut app = ScribeApp::new_test(Config::default());
    let g0 = gen(&app);
    app.tabs[app.active].set_text("hello\nworld\n".to_string());
    assert!(
        gen(&app) > g0,
        "set_text (Class A funnel) must bump edit_gen"
    );
}

#[test]
fn direct_edit_commands_advance_edit_gen() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[app.active].text = "alpha\nbravo\ncharlie\n".to_string();
    app.last_cursor_line_col = Some((2, 1)); // 1-based line 2 (bravo)

    let g = gen(&app);
    app.duplicate_cursor_line();
    assert!(gen(&app) > g, "duplicate_cursor_line must bump edit_gen");

    let g = gen(&app);
    app.move_cursor_line(1);
    assert!(gen(&app) > g, "move_cursor_line must bump edit_gen");

    let g = gen(&app);
    app.join_cursor_line_with_next();
    assert!(
        gen(&app) > g,
        "join_cursor_line_with_next must bump edit_gen"
    );

    app.find_query = "a".to_string();
    app.replace_query = "X".to_string();
    let g = gen(&app);
    app.replace_in_active(true);
    assert!(gen(&app) > g, "replace_in_active must bump edit_gen");
}
