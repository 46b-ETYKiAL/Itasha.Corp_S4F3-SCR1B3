//! Plaintext URL autolink scanner.
//!
//! A tight, dependency-free scanner that finds `http://` / `https://` URLs in a
//! single line of text and returns their byte ranges. Deliberately NARROW: only
//! the two web schemes are recognised. The editor opens only http/https (see
//! [`is_clickable_url`]), so a `file:` / `javascript:` / `data:` literal in a
//! user's file is never even *detected* as a link — defence in depth against the
//! classic local-scheme / protocol-handler abuse vectors (a URL in a file is
//! untrusted data; AES Clause 9). Pure + allocation-light so it can run inside
//! the per-line highlight loop and be fuzzed in isolation.

use std::ops::Range;

/// Characters that terminate a URL when encountered (in addition to any
/// whitespace or ASCII control char). None of these are ever valid inside a bare
/// URL embedded in running text.
const URL_DELIMITERS: &[char] = &['<', '>', '"', '\'', '`', '\\', '^', '{', '}', '|'];

/// True if `c` can be part of a URL body.
fn is_url_body(c: char) -> bool {
    !c.is_whitespace() && !c.is_control() && !URL_DELIMITERS.contains(&c)
}

/// Case-insensitive scheme match at the start of `s`. Returns the scheme length
/// (`7` for `http://`, `8` for `https://`) or `None`.
fn scheme_len_at(s: &str) -> Option<usize> {
    fn ieq(b: &[u8], pat: &[u8]) -> bool {
        b.len() >= pat.len() && b[..pat.len()].eq_ignore_ascii_case(pat)
    }
    let b = s.as_bytes();
    if ieq(b, b"https://") {
        Some(8)
    } else if ieq(b, b"http://") {
        Some(7)
    } else {
        None
    }
}

/// Trim trailing sentence punctuation and *unbalanced* closing brackets from the
/// candidate URL `line[start..end]`, returning the trimmed end byte. A balanced
/// `)` / `]` / `}` (e.g. a Wikipedia `..._(disambiguation)` path) is kept.
fn trim_trailing(line: &str, start: usize, mut end: usize) -> usize {
    loop {
        let seg = &line[start..end];
        let Some(last) = seg.chars().next_back() else {
            break;
        };
        let strip = match last {
            '.' | ',' | ';' | ':' | '!' | '?' => true,
            ')' | ']' | '}' => {
                let (open, close) = match last {
                    ')' => ('(', ')'),
                    ']' => ('[', ']'),
                    _ => ('{', '}'),
                };
                seg.matches(close).count() > seg.matches(open).count()
            }
            _ => false,
        };
        if strip {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    end
}

/// A URL is only credible if its authority has a `.` (a domain or IP) or is
/// `localhost` (optionally with a port). Rejects a bare `http://` and
/// `http://nope`.
fn authority_ok(after_scheme: &str) -> bool {
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return false;
    }
    // Strip any `user:pass@` userinfo, then the `:port` suffix.
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = host_port.split(':').next().unwrap_or(host_port);
    host.contains('.') || host.eq_ignore_ascii_case("localhost")
}

/// Scan `line` for `http://` / `https://` URLs, returning their byte ranges
/// within `line`. URLs do not span newlines, so callers pass one line at a time
/// (a trailing `\n` simply terminates the body). Trailing sentence punctuation
/// and unbalanced closing brackets are trimmed, so "see http://x.com." does not
/// capture the period.
pub fn detect_urls(line: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut search_from = 0usize;
    for (i, c) in line.char_indices() {
        if i < search_from {
            continue;
        }
        if c != 'h' && c != 'H' {
            continue;
        }
        let Some(scheme) = scheme_len_at(&line[i..]) else {
            continue;
        };
        let body_start = i + scheme;
        let mut end = body_start;
        for (off, ch) in line[body_start..].char_indices() {
            if is_url_body(ch) {
                end = body_start + off + ch.len_utf8();
            } else {
                break;
            }
        }
        let end = trim_trailing(line, body_start, end);
        if end > body_start && authority_ok(&line[body_start..end]) {
            out.push(i..end);
            search_from = end;
        }
    }
    out
}

/// Whether `url` is an http/https URL the editor may hand to the browser-open
/// call. Defence-in-depth: [`detect_urls`] only emits http/https ranges, but the
/// click handler re-checks here so no other scheme (`file:`, `javascript:`,
/// `data:`, `mailto:`, custom protocol handlers) can ever be opened.
pub fn is_clickable_url(url: &str) -> bool {
    scheme_len_at(url).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(line: &str) -> Vec<String> {
        detect_urls(line)
            .into_iter()
            .map(|r| line[r].to_string())
            .collect()
    }

    #[test]
    fn detects_http_and_https() {
        assert_eq!(urls("go to http://example.com now"), ["http://example.com"]);
        assert_eq!(
            urls("see https://example.com/path?q=1"),
            ["https://example.com/path?q=1"]
        );
    }

    #[test]
    fn scheme_match_is_case_insensitive() {
        assert_eq!(urls("HTTP://Example.com"), ["HTTP://Example.com"]);
        assert_eq!(urls("HtTpS://Example.com"), ["HtTpS://Example.com"]);
    }

    #[test]
    fn trims_trailing_sentence_punctuation() {
        assert_eq!(urls("see http://x.com."), ["http://x.com"]);
        assert_eq!(urls("(see http://x.com)"), ["http://x.com"]);
        assert_eq!(urls("link: https://x.com, then"), ["https://x.com"]);
        assert_eq!(urls("really? https://x.com?"), ["https://x.com"]);
    }

    #[test]
    fn keeps_balanced_parens_in_path() {
        // A balanced trailing paren is part of the URL (Wikipedia-style).
        assert_eq!(
            urls("https://en.wikipedia.org/wiki/Rust_(programming_language)"),
            ["https://en.wikipedia.org/wiki/Rust_(programming_language)"]
        );
    }

    #[test]
    fn rejects_bare_scheme_and_no_dot_host() {
        assert!(detect_urls("http://").is_empty());
        assert!(detect_urls("https://   trailing").is_empty());
        assert!(
            detect_urls("http://nope").is_empty(),
            "no dot, not localhost"
        );
    }

    #[test]
    fn accepts_localhost_with_port() {
        assert_eq!(
            urls("dev server http://localhost:8080/app"),
            ["http://localhost:8080/app"]
        );
    }

    #[test]
    fn non_ascii_tail_is_captured_to_word_boundary() {
        // Unicode host/path chars are URL body; whitespace ends it.
        assert_eq!(
            urls("見て https://例え.テスト/道 です"),
            ["https://例え.テスト/道"]
        );
    }

    #[test]
    fn no_match_lines_yield_empty() {
        assert!(detect_urls("just some plain text, no link here").is_empty());
        assert!(detect_urls("").is_empty());
        assert!(detect_urls("ftp://example.com not web").is_empty());
    }

    #[test]
    fn multiple_urls_on_one_line() {
        assert_eq!(
            urls("a http://one.com b https://two.org c"),
            ["http://one.com", "https://two.org"]
        );
    }

    #[test]
    fn click_scheme_allowlist_rejects_dangerous_schemes() {
        assert!(is_clickable_url("http://example.com"));
        assert!(is_clickable_url("https://example.com"));
        assert!(!is_clickable_url("file:///etc/passwd"));
        assert!(!is_clickable_url("javascript:alert(1)"));
        assert!(!is_clickable_url("data:text/html,<script>"));
        assert!(!is_clickable_url("mailto:a@b.com"));
        assert!(!is_clickable_url("ftp://example.com"));
        assert!(!is_clickable_url(""));
    }

    #[test]
    fn ranges_are_char_boundaries() {
        // Every returned range must slice cleanly (no panic) even with multibyte.
        let line = "δ https://例え.com/π α http://b.com";
        for r in detect_urls(line) {
            let _ = &line[r]; // would panic if not a char boundary
        }
    }
}
