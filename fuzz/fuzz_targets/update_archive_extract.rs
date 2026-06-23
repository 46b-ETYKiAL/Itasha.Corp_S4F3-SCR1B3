#![no_main]
//! Fuzz the self-updater's `.tar.gz` extraction path — the decompression-bomb +
//! tar-slip (zip-slip / TARmageddon CVE class) attack surface. Even though the
//! production path minisign-verifies the archive BEFORE extraction, archive
//! decompression is the canonical Rust-CVE class, so the extractor is hardened
//! (basename-only join, non-regular-entry reject, `MAX_EXTRACTED_BINARY_BYTES`
//! cap) and must NEVER panic on arbitrary archive bytes: a malformed gzip/tar
//! returns `Err`, a hostile entry is rejected, an oversized member is capped.
//!
//! Drives the real `extract_binary` via the `#[cfg(fuzzing)]` `fuzz_extract_binary`
//! seam (extracts into a fresh temp dir that is removed before returning).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Bound the input — the gzip decoder + tar reader are streaming, but the
    // extraction cap is 512 MiB; keep fuzz iterations fast by capping raw input.
    if data.len() <= 1024 * 1024 {
        scribe_core::update::net::fuzz_extract_binary(data);
    }
});
