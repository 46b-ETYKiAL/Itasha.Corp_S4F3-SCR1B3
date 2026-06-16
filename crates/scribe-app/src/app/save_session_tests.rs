//! #81 — prove (not just assert wired) that the Save & Session settings the
//! user couldn't tell were working actually change what hits disk. These run
//! the real open_path → save_active pipeline against a temp file, no GUI
//! focus or timing needed.
use super::ScribeApp;
use scribe_core::Config;

#[test]
fn trim_and_final_newline_on_save_take_effect() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("note.txt");
    std::fs::write(&p, "seed").unwrap();
    let mut cfg = Config::default();
    cfg.editor.trim_trailing_whitespace_on_save = true;
    cfg.editor.final_newline_on_save = true;
    let mut app = ScribeApp::new_test(cfg);
    app.open_path(p.clone());
    let active = app.active;
    app.tabs[active].text = "alpha   \nbeta".into(); // trailing spaces, no final \n
    app.save_active();
    let on_disk = std::fs::read_to_string(&p).unwrap();
    assert!(
        !on_disk.contains("alpha   "),
        "trailing whitespace must be trimmed: {on_disk:?}"
    );
    assert!(on_disk.ends_with('\n'), "a final newline must be ensured");
    assert_eq!(on_disk, "alpha\nbeta\n");
}

#[test]
fn save_hygiene_is_a_noop_when_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("note.txt");
    std::fs::write(&p, "seed").unwrap();
    let mut cfg = Config::default();
    cfg.editor.trim_trailing_whitespace_on_save = false;
    cfg.editor.final_newline_on_save = false;
    let mut app = ScribeApp::new_test(cfg);
    app.open_path(p.clone());
    let active = app.active;
    app.tabs[active].text = "alpha   ".into();
    app.save_active();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "alpha   ",
        "with both toggles off the bytes are written verbatim"
    );
}
