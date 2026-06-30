#![no_main]
//! Fuzz the auto-updater's UNTRUSTED-INPUT path. Two surfaces parse bytes the
//! app does not yet trust:
//!   1. The GitHub Releases API response (`RawRelease` JSON) — parsed to
//!      discover the asset/sig/sha/manifest URLs BEFORE any signature check.
//!   2. The signed update manifest (`latest.json`) — fed to
//!      [`manifest::parse_and_verify`], which runs minisign over the RAW bytes
//!      against the embedded key set BEFORE deserialization (signature-first,
//!      fail-closed). On arbitrary fuzz bytes the verify fails and the call
//!      returns `Err` — the invariant under test is that it NEVER PANICS.
//! A panic on either surface is an update-channel DoS. (The legacy `select_best`
//! highest-semver selector was removed with the Tier-1 fail-closed flow — the
//! manifest is now the only path that decides an install, so the manifest
//! verify/parse surface replaces it here.)
use libfuzzer_sys::fuzz_target;
use scribe_core::update::manifest;
use scribe_core::update::net::RawRelease;
use scribe_core::update::verify::EMBEDDED_PUBLIC_KEYS;

fuzz_target!(|data: &[u8]| {
    // Manifest verify+parse over arbitrary bytes: partition on the first NUL
    // into (json, signature) so both halves take fuzz input; a NUL-free input
    // drives the whole blob as the json with an empty signature. Either way the
    // signature-first gate must reject without panicking (it never reaches serde
    // on unverifiable bytes; verification itself must also be panic-free).
    let (json_bytes, sig_bytes) = match data.iter().position(|&b| b == 0) {
        Some(i) => (&data[..i], &data[i + 1..]),
        None => (data, &b""[..]),
    };
    let sig_str = String::from_utf8_lossy(sig_bytes);
    let _ = manifest::parse_and_verify(json_bytes, &sig_str, EMBEDDED_PUBLIC_KEYS);

    // GitHub Releases JSON discovery surface: both the single-object
    // (`/releases/latest`) and the list (`/releases`) shapes must parse
    // arbitrary UTF-8 without panicking.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = serde_json::from_str::<RawRelease>(s);
        let _ = serde_json::from_str::<Vec<RawRelease>>(s);
    }
});
