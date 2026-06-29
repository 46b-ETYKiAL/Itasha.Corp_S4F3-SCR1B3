//! ENC-1 WIRING — the encoding-preserving reload must actually run on the app's
//! real external-edit reload path (`poll_external_disk_changes`), not just exist
//! as an unused `Document::reload_from_disk`.
//!
//! The fixture is the adversarial ENC-1 byte pattern (`caf\xE9` — a single-byte
//! legacy codepage that is INVALID UTF-8). Under the old UTF-8-only
//! `std::fs::read_to_string`, the external-edit reload silently failed the read
//! and stranded the change. This test fails if the wire is reverted to
//! `read_to_string` (the read errors → no reload → no "v2").

#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn poll_external_reload_preserves_non_utf8_encoding() {
    let mut app = ScribeApp::new_test(Config::default());
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("legacy.txt");

    // Non-UTF-8 content (0xE9): `read_to_string` cannot read this.
    std::fs::write(&p, [b'c', b'a', b'f', 0xE9, b'\n']).unwrap();
    app.open_path(p.clone());
    let i = app.active;
    let enc_before = app.tabs[i].doc.encoding().clone();
    // The buffer opened cleanly and is unmodified (so the reload path, not the
    // unsaved-edit warning path, is taken).
    assert_eq!(app.tabs[i].text, app.tabs[i].disk_text);

    // An external editor rewrites the file with NEW content in the SAME
    // single-byte legacy encoding.
    std::fs::write(&p, [b'c', b'a', b'f', 0xE9, b' ', b'v', b'2', b'\n']).unwrap();
    // Force the change-detection branch deterministically (mtime granularity is
    // unreliable in a fast test).
    app.tabs[i].disk_mtime = None;

    app.poll_external_disk_changes(1_000_000);

    // The wire fired: the buffer reloaded the new content, decoded with the
    // PRESERVED encoding (would be unchanged "café\n" if the read had errored
    // under the old UTF-8-only path).
    assert!(
        app.tabs[i].text.contains("v2"),
        "external-edit reload must update the buffer via the encoding-preserving \
         path, got: {:?}",
        app.tabs[i].text
    );
    assert_eq!(
        app.tabs[i].doc.encoding(),
        &enc_before,
        "reload must preserve the detected encoding, not re-detect/flip it"
    );
    assert!(
        !app.tabs[i].external_change,
        "external_change flag must be cleared once the reload succeeds"
    );
}
