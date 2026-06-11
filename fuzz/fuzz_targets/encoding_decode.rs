#![no_main]
//! Decoding arbitrary bytes must never panic — the editor opens any file, and
//! decode is lossy-by-design so it always yields a String. Re-encoding the
//! decoded text with the detected encoding must also never panic.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (text, enc) = scribe_core::encoding::decode(data);
    let _ = scribe_core::encoding::encode_checked(&text, &enc);
});
