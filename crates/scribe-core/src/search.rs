//! Find / replace engine: literal or regex, case-sensitive or not, whole-word.
//! Returns byte-offset match spans the UI highlights.
//!
//! # Zero-width (empty) match policy (C-03 / R5)
//!
//! Regex patterns such as `x*`, `a?`, `\b`, `^`, and `$` can match an *empty*
//! span (`start == end`) at one or more offsets. A naive `find_iter` /
//! `Regex::replace_all` reports an empty hit at every position, so a find
//! highlights a zero-width span between every character and `replace_all`
//! *injects* the replacement between every character (e.g.
//! `replace_all("abc", "x*", "-")` -> `"-a-b-c-"`). This is the standard
//! "empty match" footgun.
//!
//! Policy: **both `find_all` and `replace_all` skip zero-width matches
//! entirely** — only non-empty spans (`end > start`) are reported or
//! substituted. An empty match carries no selectable text, so for an editor
//! the least-surprising behavior is for find to highlight only real, navigable
//! spans and for replace to substitute only actual matched text, never
//! inject between characters. Non-empty matches (literals, `a+`, capture
//! groups, etc.) are completely unaffected.

use crate::error::{CoreError, Result};
use regex::RegexBuilder;

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
}

fn build_regex(q: &Query) -> Result<regex::Regex> {
    let mut pat = if q.regex {
        q.pattern.clone()
    } else {
        regex::escape(&q.pattern)
    };
    if q.whole_word {
        pat = format!(r"\b(?:{pat})\b");
    }
    RegexBuilder::new(&pat)
        .case_insensitive(!q.case_sensitive)
        .build()
        .map_err(|e| CoreError::Regex(e.to_string()))
}

/// All non-overlapping, **non-empty** matches in `text`.
///
/// Zero-width matches (`start == end`, e.g. from `x*`, `\b`, `^`, `$`) are
/// skipped per the module-level empty-match policy: they carry no selectable
/// text, so an editor must not highlight them. `find_iter` already advances by
/// one codepoint past an empty match (so the scan terminates), so filtering the
/// empty spans out leaves only the real, navigable hits — and the surviving
/// byte offsets always fall on UTF-8 codepoint boundaries.
pub fn find_all(text: &str, q: &Query) -> Result<Vec<Match>> {
    if q.pattern.is_empty() {
        return Ok(Vec::new());
    }
    let re = build_regex(q)?;
    Ok(re
        .find_iter(text)
        .filter(|m| m.end() > m.start())
        .map(|m| Match {
            start: m.start(),
            end: m.end(),
        })
        .collect())
}

/// Replace all **non-empty** matches. For regex queries, `$1` capture refs in
/// `replacement` are honored (regex crate semantics).
///
/// Zero-width matches are skipped per the module-level empty-match policy, so
/// the replacement is never injected between characters. The substitution is
/// driven manually (rather than via [`regex::Regex::replace_all`]) so each
/// match can be filtered on its span before deciding whether to substitute;
/// `Captures::expand` provides the same `$N` / `${name}` expansion semantics as
/// the built-in replacer.
pub fn replace_all(text: &str, q: &Query, replacement: &str) -> Result<String> {
    if q.pattern.is_empty() {
        return Ok(text.to_string());
    }
    let re = build_regex(q)?;

    let mut out = String::with_capacity(text.len());
    let mut last_end = 0usize;
    for caps in re.captures_iter(text) {
        // The overall match is group 0; it always exists for a successful
        // capture, so the `unwrap`-free `get(0)` is guaranteed `Some`.
        let m = caps
            .get(0)
            .expect("regex capture iteration always yields group 0");
        // Skip zero-width matches: copying the unmatched gap between `last_end`
        // and a zero-width match's start (which equals `last_end` when matches
        // are contiguous) plus emitting no replacement leaves the text intact.
        if m.end() == m.start() {
            continue;
        }
        // Copy the text between the previous match and this one verbatim, then
        // expand the replacement (with capture refs) for this match.
        out.push_str(&text[last_end..m.start()]);
        caps.expand(replacement, &mut out);
        last_end = m.end();
    }
    out.push_str(&text[last_end..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(p: &str) -> Query {
        Query {
            pattern: p.into(),
            ..Default::default()
        }
    }

    #[test]
    fn literal_case_insensitive() {
        let m = find_all("Foo foo FOO", &q("foo")).unwrap();
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn case_sensitive() {
        let query = Query {
            pattern: "foo".into(),
            case_sensitive: true,
            ..Default::default()
        };
        assert_eq!(find_all("Foo foo FOO", &query).unwrap().len(), 1);
    }

    #[test]
    fn regex_groups_replace() {
        let query = Query {
            pattern: r"(\w+)@(\w+)".into(),
            regex: true,
            ..Default::default()
        };
        let out = replace_all("a@b c@d", &query, "$2.$1").unwrap();
        assert_eq!(out, "b.a d.c");
    }

    #[test]
    fn whole_word() {
        let query = Query {
            pattern: "cat".into(),
            whole_word: true,
            ..Default::default()
        };
        assert_eq!(find_all("cat category cat", &query).unwrap().len(), 2);
    }

    #[test]
    fn bad_regex_is_error() {
        let query = Query {
            pattern: "(".into(),
            regex: true,
            ..Default::default()
        };
        assert!(find_all("x", &query).is_err());
    }

    // --- Zero-width / empty-match handling (C-03 / R5) -----------------------
    //
    // Policy: a zero-width match (start == end) carries no selectable text, so
    // both `find_all` and `replace_all` SKIP empty matches entirely. Only
    // non-empty spans (end > start) are reported / substituted. This is the
    // least-surprising behavior for an editor: find highlights real, navigable
    // spans; replace substitutes actual matched text and never injects between
    // characters. See the module-level doc and the empty-match filter in
    // `find_all` / `replace_all`.

    fn rq(p: &str) -> Query {
        Query {
            pattern: p.into(),
            regex: true,
            ..Default::default()
        }
    }

    #[test]
    fn zero_width_star_yields_no_matches() {
        // `x*` matches an empty span at every offset under the regex crate.
        // The correct editor behavior is: no selectable hits at all.
        let m = find_all("abc", &rq("x*")).unwrap();
        assert_eq!(
            m,
            Vec::new(),
            "x* must not report empty hits at every offset"
        );
    }

    #[test]
    fn zero_width_star_replace_is_identity() {
        // The footgun: `replace_all("abc", "x*", "-")` previously produced
        // "-a-b-c-". Empty matches must be skipped, leaving the text unchanged.
        let out = replace_all("abc", &rq("x*"), "-").unwrap();
        assert_eq!(out, "abc", "empty matches must not inject replacement");
    }

    #[test]
    fn word_boundary_yields_no_matches() {
        // `\b` is a pure zero-width assertion.
        let m = find_all("a b", &rq(r"\b")).unwrap();
        assert_eq!(m, Vec::new());
        let out = replace_all("a b", &rq(r"\b"), "|").unwrap();
        assert_eq!(out, "a b");
    }

    #[test]
    fn caret_anchor_yields_no_matches() {
        let m = find_all("line", &rq("^")).unwrap();
        assert_eq!(m, Vec::new());
        let out = replace_all("line", &rq("^"), ">").unwrap();
        assert_eq!(out, "line");
    }

    #[test]
    fn dollar_anchor_yields_no_matches() {
        let m = find_all("line", &rq("$")).unwrap();
        assert_eq!(m, Vec::new());
        let out = replace_all("line", &rq("$"), "<").unwrap();
        assert_eq!(out, "line");
    }

    #[test]
    fn multiline_anchors_yield_no_matches() {
        // With the multi-line flag, ^ / $ match at every line boundary, but all
        // are zero-width and must be skipped.
        let m = find_all("a\nb\nc", &rq("(?m)^")).unwrap();
        assert_eq!(m, Vec::new());
        let out = replace_all("a\nb\nc", &rq("(?m)$"), "X").unwrap();
        assert_eq!(out, "a\nb\nc");
    }

    #[test]
    fn star_interleaved_with_literal_keeps_only_real_hits() {
        // `a*` matches "aa", "" (between b and c, etc.), and "a". Only the
        // non-empty runs of 'a' survive.
        let m = find_all("baac", &rq("a*")).unwrap();
        assert_eq!(m, vec![Match { start: 1, end: 3 }]);
        // Replacement substitutes only the real run.
        let out = replace_all("baac", &rq("a*"), "X").unwrap();
        assert_eq!(out, "bXc");
    }

    #[test]
    fn optional_group_skips_empty_alternatives() {
        // `a?` matches "a" then "" repeatedly; only the real "a" counts.
        let m = find_all("xay", &rq("a?")).unwrap();
        assert_eq!(m, vec![Match { start: 1, end: 2 }]);
        let out = replace_all("xay", &rq("a?"), "Z").unwrap();
        assert_eq!(out, "xZy");
    }

    #[test]
    fn multibyte_text_zero_width_unchanged() {
        // Empty matches over multibyte text must not split a UTF-8 codepoint
        // nor inject between graphemes. "café" + "x*".
        let s = "café";
        let m = find_all(s, &rq("x*")).unwrap();
        assert_eq!(m, Vec::new());
        let out = replace_all(s, &rq("x*"), "-").unwrap();
        assert_eq!(out, s);
    }

    #[test]
    fn multibyte_real_match_preserved() {
        // A non-empty match over multibyte text returns correct byte offsets.
        let s = "café"; // 'é' is 2 bytes -> total length 5
        let m = find_all(s, &rq("é")).unwrap();
        assert_eq!(m, vec![Match { start: 3, end: 5 }]);
        let out = replace_all(s, &rq("é"), "e").unwrap();
        assert_eq!(out, "cafe");
    }

    #[test]
    fn empty_input_no_matches() {
        assert_eq!(find_all("", &rq("x*")).unwrap(), Vec::new());
        assert_eq!(replace_all("", &rq("x*"), "-").unwrap(), "");
        assert_eq!(find_all("", &q("foo")).unwrap(), Vec::new());
    }

    #[test]
    fn end_of_string_zero_width_skipped() {
        // `\b` fires at the end-of-string boundary after "word"; zero-width,
        // skipped.
        let out = replace_all("word", &rq(r"d\b"), "D").unwrap();
        // `d\b` is a *non-empty* match ("d") at end-of-string -> substituted.
        assert_eq!(out, "worD");
        // Pure end anchor stays zero-width -> skipped.
        let out2 = replace_all("word", &rq("$"), "!").unwrap();
        assert_eq!(out2, "word");
    }

    // --- Regression lock: ordinary (non-empty) search/replace is UNCHANGED ---

    #[test]
    fn regression_literal_search_unchanged() {
        assert_eq!(find_all("Foo foo FOO", &q("foo")).unwrap().len(), 3);
    }

    #[test]
    fn regression_a_plus_search_unchanged() {
        // `a+` is always non-empty; behavior must be identical to before.
        let m = find_all("baaab", &rq("a+")).unwrap();
        assert_eq!(m, vec![Match { start: 1, end: 4 }]);
        let out = replace_all("baaab", &rq("a+"), "X").unwrap();
        assert_eq!(out, "bXb");
    }

    #[test]
    fn regression_group_replace_unchanged() {
        let query = Query {
            pattern: r"(\w+)@(\w+)".into(),
            regex: true,
            ..Default::default()
        };
        let out = replace_all("a@b c@d", &query, "$2.$1").unwrap();
        assert_eq!(out, "b.a d.c");
    }

    #[test]
    fn regression_whole_word_unchanged() {
        let query = Query {
            pattern: "cat".into(),
            whole_word: true,
            ..Default::default()
        };
        assert_eq!(find_all("cat category cat", &query).unwrap().len(), 2);
    }
}
