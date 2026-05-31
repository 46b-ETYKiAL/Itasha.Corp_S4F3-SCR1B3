#![no_main]
//! Fuzz the TOML config parser. Invariant: `Config::from_toml_str` must never
//! panic on arbitrary input — malformed TOML returns `Err`, and the editor
//! falls back to defaults at the load layer.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = scribe_core::Config::from_toml_str(s);
    }
});
