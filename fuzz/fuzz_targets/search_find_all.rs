#![no_main]
//! Fuzz the find/replace engine. Invariant: `find_all` never panics on an
//! arbitrary pattern + text — a malformed regex returns `Err`, a valid one
//! returns byte-offset spans. Exercises the literal-escape, whole-word
//! `\b(?:...)\b` wrapping, and case-insensitive build paths.
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use scribe_core::search::{find_all, Query};

#[derive(Arbitrary, Debug)]
struct Input {
    pattern: String,
    text: String,
    regex: bool,
    case_sensitive: bool,
    whole_word: bool,
}

fuzz_target!(|input: Input| {
    let q = Query {
        pattern: input.pattern,
        regex: input.regex,
        case_sensitive: input.case_sensitive,
        whole_word: input.whole_word,
    };
    let _ = find_all(&input.text, &q);
});
