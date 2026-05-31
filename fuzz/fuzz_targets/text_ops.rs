#![no_main]
//! Fuzz the save-time text transforms. Invariant: none of them panic on
//! arbitrary UTF-8 (multi-byte chars, lone `\r`, mixed EOLs, no final newline).
use libfuzzer_sys::fuzz_target;
use scribe_core::text_ops;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = text_ops::trim_trailing_whitespace(s);
        let _ = text_ops::ensure_final_newline(s);
        let _ = text_ops::sort_lines(s);
        let _ = text_ops::to_case(s, true);
        let _ = text_ops::to_case(s, false);
    }
});
