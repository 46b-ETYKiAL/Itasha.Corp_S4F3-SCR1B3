#![no_main]
//! Fuzz the auto-updater's UNTRUSTED-INPUT path. The GitHub Releases API
//! response is the only data the app parses BEFORE any signature check can help
//! (we parse the JSON to discover the asset/sig/sha URLs; minisign runs later,
//! on the downloaded tarball). A panic here is an update-channel DoS; a logic
//! bug could steer the download. Invariants: parsing arbitrary bytes as the
//! release JSON must never panic, and `select_best` must never panic on any
//! parsed release list.
use libfuzzer_sys::fuzz_target;
use scribe_core::update::net::{select_best, RawRelease};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    // `/releases/latest` shape — a single release object.
    let _ = serde_json::from_str::<RawRelease>(s);
    // `/releases` shape — a list — then the pure highest-semver decision over it
    // for a couple of real target triples (the path that turns untrusted JSON
    // into "which asset to download").
    if let Ok(releases) = serde_json::from_str::<Vec<RawRelease>>(s) {
        let current = semver::Version::new(0, 4, 0);
        let _ = select_best(&releases, &current, "x86_64-pc-windows-msvc");
        let _ = select_best(&releases, &current, "x86_64-unknown-linux-gnu");
        let _ = select_best(&releases, &current, "aarch64-apple-darwin");
    }
});
