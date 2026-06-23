#![no_main]
//! Fuzz the theme TOML parser. A user can drop an arbitrary `theme.toml` into
//! the themes dir; `Theme::from_toml_str` must never panic on arbitrary input —
//! malformed TOML returns `Err` (the app falls back to a bundled theme), valid
//! TOML returns a `Theme`. The `parse_hex` colour decoder (reached via the
//! palette tables) must also never panic on garbage colour strings.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    if let Ok(theme) = scribe_core::Theme::from_toml_str(data) {
        // Walk the parsed theme's colour lookups — every `ui`/`syntax_color`
        // query must be total. Re-serializing must also never panic.
        let fallback = scribe_core::theme::Rgba::new(0, 0, 0, 255);
        let _ = theme.to_toml_string();
        let _ = theme.ui("editor.fg", fallback);
        let _ = theme.syntax_color("keyword", fallback);
    }
    // Also drive parse_hex directly on the raw input — the colour decoder is the
    // byte-level surface a malformed palette value reaches.
    let _ = scribe_core::theme::Rgba::parse_hex(data);
});
