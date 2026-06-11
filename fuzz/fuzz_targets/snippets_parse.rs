#![no_main]
//! Parsing an arbitrary `snippets.toml` and expanding its bodies must never
//! panic — malformed TOML returns Err (the app falls back to no snippets) and
//! `expand` walks arbitrary `${N}`/`$0` markers without indexing out of bounds.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    if let Ok(set) = scribe_core::snippets::SnippetSet::from_toml(data) {
        for snip in &set.snippets {
            let _ = scribe_core::snippets::expand(&snip.body);
        }
    }
    // Also fuzz expand directly on the raw input (every byte sequence is a
    // candidate body).
    let _ = scribe_core::snippets::expand(data);
});
