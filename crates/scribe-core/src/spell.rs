//! Privacy-respecting spellcheck. **Fully offline, telemetry-free, no network.**
//!
//! v1 engine is a pure-Rust dictionary (`HashSet`) + Damerau-Levenshtein
//! suggestions behind a `SpellEngine` trait, so a Hunspell-affix backend
//! (`spellbook`) can be slotted in later without changing callers (ADR-0007).
//! Code-aware: the app only feeds comment/string text here (decided by syntect
//! scopes), and identifiers are split camelCase/snake_case before checking.
//!
//! There is intentionally NO network code in this module — the privacy
//! guarantee is structural, not configurational.

use std::collections::HashSet;

/// A misspelled word + suggestions, located by byte range within the text fed
/// to the checker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Misspelling {
    pub word: String,
    pub start: usize,
    pub end: usize,
    pub suggestions: Vec<String>,
}

/// Pluggable spellcheck backend. v1 = `HashSetEngine`; a Hunspell backend can
/// implement this trait later.
pub trait SpellEngine {
    fn is_correct(&self, word: &str) -> bool;
    fn suggest(&self, word: &str, max: usize) -> Vec<String>;
}

/// Pure-Rust dictionary engine. Case-insensitive membership; edit-distance
/// suggestions ranked by Damerau-Levenshtein distance then length proximity.
pub struct HashSetEngine {
    words: HashSet<String>,
    /// Words the user added ("add to dictionary") or chose to ignore.
    user_words: HashSet<String>,
}

impl HashSetEngine {
    /// Build from a newline-separated word list (one word per line, comments
    /// with `#` ignored). Words are lowercased.
    pub fn from_word_list(list: &str) -> Self {
        let words = list
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.to_lowercase())
            .collect();
        Self {
            words,
            user_words: HashSet::new(),
        }
    }

    /// The compiled-in default en_US dictionary (public-domain common words).
    pub fn bundled_en_us() -> Self {
        Self::from_word_list(include_str!("../assets/dict/en_US.txt"))
    }

    pub fn word_count(&self) -> usize {
        self.words.len() + self.user_words.len()
    }

    /// Add a word to the user dictionary (persisted by the caller).
    pub fn add_user_word(&mut self, word: &str) {
        self.user_words.insert(word.to_lowercase());
    }

    pub fn load_user_words(&mut self, list: &str) {
        for w in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
            self.user_words.insert(w.to_lowercase());
        }
    }
}

impl SpellEngine for HashSetEngine {
    fn is_correct(&self, word: &str) -> bool {
        let w = word.to_lowercase();
        // Numbers and very short tokens are always "correct". Count CHARS, not
        // bytes: `w.len() < 2` would still check a single 2-byte letter (`é`, `я`)
        // against the dictionary and flag it misspelled, while skipping 1-byte
        // ASCII — an inconsistency for non-Latin scripts.
        if w.chars().count() < 2 || w.chars().any(|c| c.is_ascii_digit()) {
            return true;
        }
        self.words.contains(&w) || self.user_words.contains(&w)
    }

    fn suggest(&self, word: &str, max: usize) -> Vec<String> {
        let w = word.to_lowercase();
        let mut scored: Vec<(usize, &String)> = self
            .words
            .iter()
            .filter(|cand| (cand.len() as isize - w.len() as isize).abs() <= 2)
            .map(|cand| (damerau_levenshtein(&w, cand), cand))
            .filter(|(d, _)| *d > 0 && *d <= 2)
            .collect();
        scored
            .sort_by_key(|(d, cand)| (*d, (cand.len() as isize - w.len() as isize).unsigned_abs()));
        scored
            .into_iter()
            .take(max)
            .map(|(_, c)| c.clone())
            .collect()
    }
}

/// Spellcheck a block of plain prose (already extracted from comments/strings).
/// `enabled = false` short-circuits to an empty result (zero work).
pub fn check_text(engine: &dyn SpellEngine, text: &str, enabled: bool) -> Vec<Misspelling> {
    if !enabled {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (start, token) in word_tokens(text) {
        if !engine.is_correct(token) {
            out.push(Misspelling {
                word: token.to_string(),
                start,
                end: start + token.len(),
                suggestions: engine.suggest(token, 5),
            });
        }
    }
    out
}

/// Token-class scoping for [`check_text_scoped`]. Each flag opts that class of
/// source text into spellchecking. Mirrors the three `SpellcheckConfig` bools
/// (`check_comments` / `check_strings` / `check_identifiers`) 1:1.
///
/// When **all three** are false the scoped checker falls back to whole-text
/// checking (the historic [`check_text`] behaviour) — "scope to nothing" is
/// treated as "no scoping requested", never as "check nothing", so toggling
/// every class off can't silently disable spellcheck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellScope {
    pub comments: bool,
    pub strings: bool,
    pub identifiers: bool,
}

impl SpellScope {
    /// Build a scope from the three `SpellcheckConfig` booleans.
    pub fn new(comments: bool, strings: bool, identifiers: bool) -> Self {
        Self {
            comments,
            strings,
            identifiers,
        }
    }

    /// True when no class is selected — the "no scoping requested" sentinel
    /// that makes [`check_text_scoped`] fall back to whole-text checking.
    pub fn is_empty(self) -> bool {
        !self.comments && !self.strings && !self.identifiers
    }

    /// Whether `class` is opted into checking by this scope. The catch-all
    /// [`SpanClass::Other`] (operators/punctuation/whitespace/keywords) is
    /// never checked under scoping.
    fn includes(self, class: SpanClass) -> bool {
        match class {
            SpanClass::Comment => self.comments,
            SpanClass::String => self.strings,
            SpanClass::Identifier => self.identifiers,
            SpanClass::Other => false,
        }
    }
}

/// The token class a highlighted span belongs to, derived from the
/// highlighter's scope names (see [`crate::syntax::classify_document`] and
/// [`crate::syntax::classify_scope_name`]). `Other` is the catch-all for
/// everything not worth spellchecking (operators, punctuation, keywords,
/// whitespace).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanClass {
    Comment,
    String,
    Identifier,
    Other,
}

/// A classified byte span over the WHOLE document: `[start, end)` byte range
/// (absolute, document-relative) tagged with its [`SpanClass`]. Produced by
/// [`crate::syntax::classify_document`]; consumed by [`check_text_scoped`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedSpan {
    pub start: usize,
    pub end: usize,
    pub class: SpanClass,
}

/// Spellcheck `text`, restricting the checked regions to the token classes
/// selected by `scope`, using pre-computed `spans` from the highlighter.
///
/// Semantics (matching the privacy/no-regression contract):
///
/// * `scope.is_empty()` **or** `spans.is_empty()` → fall back to whole-text
///   checking, identical to [`check_text`]`(engine, text, true)`. This is the
///   no-regression path: when no syntax info is available, or the user opted
///   no class in, the whole buffer is checked exactly as before.
/// * otherwise → only word tokens whose byte offset lands inside a span whose
///   class is opted in by `scope` are checked. Tokens in `Other` spans, or in
///   classes the user toggled off, are skipped.
///
/// Misspelling offsets are document-relative (the same coordinate space as the
/// input `text` and the `spans`), so callers map them straight back onto the
/// buffer.
pub fn check_text_scoped(
    engine: &dyn SpellEngine,
    text: &str,
    spans: &[ClassifiedSpan],
    scope: SpellScope,
) -> Vec<Misspelling> {
    // No scoping requested, or no syntax info → check the whole text (the
    // historic behaviour; must not regress).
    if scope.is_empty() || spans.is_empty() {
        return check_text(engine, text, true);
    }

    let mut out = Vec::new();
    for (start, token) in word_tokens(text) {
        // A token is checked iff its START byte falls inside an opted-in span.
        // Word tokens are runs of alphabetic chars and never straddle a
        // class boundary the highlighter draws (scopes split on the same
        // non-alphabetic punctuation `word_tokens` splits on), so the start
        // byte is a faithful locator.
        if !span_class_at(spans, start).is_some_and(|c| scope.includes(c)) {
            continue;
        }
        if !engine.is_correct(token) {
            out.push(Misspelling {
                word: token.to_string(),
                start,
                end: start + token.len(),
                suggestions: engine.suggest(token, 5),
            });
        }
    }
    out
}

/// Class of the span covering byte `offset`, or `None` if no span covers it.
/// Linear scan — spans are few per document line and the caller already walks
/// the token stream linearly; a binary search would not pay for itself at the
/// span counts a highlighter produces.
fn span_class_at(spans: &[ClassifiedSpan], offset: usize) -> Option<SpanClass> {
    spans
        .iter()
        .find(|s| offset >= s.start && offset < s.end)
        .map(|s| s.class)
}

/// Extract word tokens (runs of alphabetic chars) with their byte offsets.
/// Splits on non-alphabetic boundaries; the caller handles camelCase/snake_case
/// upstream via `split_identifier` when checking identifiers.
fn word_tokens(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        if c.is_alphabetic() {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            out.push((s, &text[s..i]));
        }
    }
    if let Some(s) = start {
        out.push((s, &text[s..]));
    }
    out
}

/// Split an identifier into sub-words: `parseHTTPResponse` / `parse_http_response`
/// -> ["parse","HTTP","response"] (case preserved for display; checked lowercased).
pub fn split_identifier(ident: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in ident.split(|c: char| c == '_' || c == '-' || !c.is_alphanumeric()) {
        if part.is_empty() {
            continue;
        }
        // camelCase / PascalCase / acronym boundaries
        let chars: Vec<char> = part.chars().collect();
        let mut word_start = 0;
        for i in 1..chars.len() {
            let prev = chars[i - 1];
            let cur = chars[i];
            let next = chars.get(i + 1).copied();
            let boundary = (prev.is_lowercase() && cur.is_uppercase())
                || (prev.is_uppercase()
                    && cur.is_uppercase()
                    && next.is_some_and(|n| n.is_lowercase()));
            if boundary {
                words.push(chars[word_start..i].iter().collect());
                word_start = i;
            }
        }
        words.push(chars[word_start..].iter().collect());
    }
    words
        .into_iter()
        .filter(|w: &String| !w.is_empty())
        .collect()
}

/// Damerau-Levenshtein edit distance (optimal string alignment variant).
fn damerau_levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (la, lb) = (a.len(), b.len());
    if la == 0 {
        return lb;
    }
    if lb == 0 {
        return la;
    }
    let mut d = vec![vec![0usize; lb + 1]; la + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=la {
        for j in 1..=lb {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            let mut m = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                m = m.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = m;
        }
    }
    d[la][lb]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> HashSetEngine {
        HashSetEngine::from_word_list(
            "the\nquick\nbrown\nfox\nhello\nworld\nfunction\nreturn\ncolor\ncolour\n",
        )
    }

    #[test]
    fn correct_word_passes() {
        assert!(engine().is_correct("hello"));
        assert!(engine().is_correct("HELLO")); // case-insensitive
    }

    #[test]
    fn misspelling_detected_with_suggestion() {
        let e = engine();
        assert!(!e.is_correct("helllo"));
        let s = e.suggest("helllo", 5);
        assert!(s.contains(&"hello".to_string()), "suggestions: {s:?}");
    }

    #[test]
    fn toggle_off_is_noop() {
        let e = engine();
        assert!(check_text(&e, "zzzz wrongg", false).is_empty());
        assert!(!check_text(&e, "zzzz wrongg", true).is_empty());
    }

    #[test]
    fn numbers_and_short_tokens_ignored() {
        let e = engine();
        assert!(e.is_correct("x"));
        assert!(e.is_correct("abc123"));
    }

    #[test]
    fn single_multibyte_letter_is_treated_as_short_token() {
        // `é` / `я` are single CHARS but 2 BYTES each. The old `w.len() < 2`
        // byte check did NOT skip them, so a lone non-Latin letter was looked up
        // in the (English) dictionary and flagged misspelled — inconsistent with
        // the 1-byte ASCII case which WAS skipped. Char-count makes both consistent.
        let e = engine();
        assert!(e.is_correct("\u{e9}")); // é — single char, must be ignored
        assert!(e.is_correct("\u{44f}")); // я — single char, must be ignored
    }

    #[test]
    fn user_word_suppresses_misspelling() {
        let mut e = engine();
        assert!(!e.is_correct("zaxby"));
        e.add_user_word("zaxby");
        assert!(e.is_correct("zaxby"));
    }

    #[test]
    fn identifier_splitting() {
        assert_eq!(
            split_identifier("parseHTTPResponse"),
            vec!["parse", "HTTP", "Response"]
        );
        assert_eq!(
            split_identifier("parse_http_response"),
            vec!["parse", "http", "response"]
        );
        assert_eq!(
            split_identifier("snake_case_word"),
            vec!["snake", "case", "word"]
        );
    }

    #[test]
    fn word_offsets_correct() {
        let toks = word_tokens("hi  world");
        assert_eq!(toks, vec![(0, "hi"), (4, "world")]);
    }

    #[test]
    fn damerau_transposition() {
        assert_eq!(damerau_levenshtein("teh", "the"), 1); // single transposition
        assert_eq!(damerau_levenshtein("abc", "abc"), 0);
    }

    // ---- scoped spellcheck (check_text_scoped) ----

    /// Engine that only knows "hello"/"world" — every other alphabetic token
    /// is a misspelling, which makes the scoping assertions crisp.
    fn scoped_engine() -> HashSetEngine {
        HashSetEngine::from_word_list("hello\nworld\n")
    }

    /// Locate the byte range of `needle` in `text` and tag it `class`.
    fn span(text: &str, needle: &str, class: SpanClass) -> ClassifiedSpan {
        let start = text.find(needle).expect("needle present in text");
        ClassifiedSpan {
            start,
            end: start + needle.len(),
            class,
        }
    }

    #[test]
    fn scope_all_false_checks_whole_text() {
        // All flags off == "no scoping requested" -> whole-text fallback,
        // identical to check_text(.., true). MUST NOT regress.
        let e = scoped_engine();
        let text = "wrongg badd hello";
        let none = SpellScope::new(false, false, false);
        let scoped = check_text_scoped(&e, text, &[], none);
        let whole = check_text(&e, text, true);
        assert_eq!(scoped, whole);
        // Two misspellings: "wrongg", "badd".
        assert_eq!(scoped.len(), 2);
        assert_eq!(scoped[0].word, "wrongg");
        assert_eq!(scoped[1].word, "badd");
    }

    #[test]
    fn empty_spans_falls_back_to_whole_text() {
        // Even with scoping requested, NO syntax info -> whole-text fallback.
        let e = scoped_engine();
        let text = "wrongg hello";
        let scope = SpellScope::new(true, true, true);
        let scoped = check_text_scoped(&e, text, &[], scope);
        assert_eq!(scoped, check_text(&e, text, true));
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].word, "wrongg");
    }

    #[test]
    fn comments_only_scopes_to_comment_text() {
        let e = scoped_engine();
        //            0123456789...
        let text = "identx commz strz"; // identx | commz | strz
        let spans = [
            span(text, "identx", SpanClass::Identifier),
            span(text, "commz", SpanClass::Comment),
            span(text, "strz", SpanClass::String),
        ];
        let only_comments = SpellScope::new(true, false, false);
        let out = check_text_scoped(&e, text, &spans, only_comments);
        // Only the comment token "commz" is checked (and it IS misspelled).
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].word, "commz");
        // Offset is document-relative and lands on "commz".
        assert_eq!(&text[out[0].start..out[0].end], "commz");
    }

    #[test]
    fn strings_only_scopes_to_string_literals() {
        let e = scoped_engine();
        let text = "identx commz strz";
        let spans = [
            span(text, "identx", SpanClass::Identifier),
            span(text, "commz", SpanClass::Comment),
            span(text, "strz", SpanClass::String),
        ];
        let only_strings = SpellScope::new(false, true, false);
        let out = check_text_scoped(&e, text, &spans, only_strings);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].word, "strz");
    }

    #[test]
    fn identifiers_only_scopes_to_identifier_text() {
        let e = scoped_engine();
        let text = "identx commz strz";
        let spans = [
            span(text, "identx", SpanClass::Identifier),
            span(text, "commz", SpanClass::Comment),
            span(text, "strz", SpanClass::String),
        ];
        let only_idents = SpellScope::new(false, false, true);
        let out = check_text_scoped(&e, text, &spans, only_idents);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].word, "identx");
    }

    #[test]
    fn other_class_text_is_never_checked_under_scoping() {
        // A misspelled word sitting in an `Other` span (e.g. a keyword region)
        // is skipped no matter which flags are on.
        let e = scoped_engine();
        let text = "kwordz strz";
        let spans = [
            span(text, "kwordz", SpanClass::Other),
            span(text, "strz", SpanClass::String),
        ];
        let all = SpellScope::new(true, true, true);
        let out = check_text_scoped(&e, text, &spans, all);
        // Only "strz" (String, opted in) is checked; "kwordz" (Other) skipped.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].word, "strz");
    }

    #[test]
    fn mixed_snippet_each_class_isolated() {
        // A mixed Rust-like snippet: a misspelled word in a comment, a string,
        // and an identifier each. Verify each flag isolates exactly its class.
        let e = scoped_engine();
        //          identifier    comment        string
        let text = "fn brokin() { /* mispel */ \"wronng\" }";
        let spans = [
            span(text, "brokin", SpanClass::Identifier),
            span(text, "mispel", SpanClass::Comment),
            span(text, "wronng", SpanClass::String),
            // The `fn`/braces/parens are Other — never checked.
            span(text, "fn", SpanClass::Other),
        ];

        // Comments only -> just "mispel".
        let c = check_text_scoped(&e, text, &spans, SpellScope::new(true, false, false));
        assert_eq!(
            c.iter().map(|m| m.word.as_str()).collect::<Vec<_>>(),
            ["mispel"]
        );

        // Strings only -> just "wronng".
        let s = check_text_scoped(&e, text, &spans, SpellScope::new(false, true, false));
        assert_eq!(
            s.iter().map(|m| m.word.as_str()).collect::<Vec<_>>(),
            ["wronng"]
        );

        // Identifiers only -> just "brokin".
        let i = check_text_scoped(&e, text, &spans, SpellScope::new(false, false, true));
        assert_eq!(
            i.iter().map(|m| m.word.as_str()).collect::<Vec<_>>(),
            ["brokin"]
        );

        // Comments + strings -> "mispel" then "wronng" (document order).
        let cs = check_text_scoped(&e, text, &spans, SpellScope::new(true, true, false));
        assert_eq!(
            cs.iter().map(|m| m.word.as_str()).collect::<Vec<_>>(),
            ["mispel", "wronng"]
        );
    }

    #[test]
    fn scope_helpers() {
        assert!(SpellScope::new(false, false, false).is_empty());
        assert!(!SpellScope::new(false, false, true).is_empty());
        let s = SpellScope::new(true, false, true);
        assert!(s.includes(SpanClass::Comment));
        assert!(!s.includes(SpanClass::String));
        assert!(s.includes(SpanClass::Identifier));
        assert!(!s.includes(SpanClass::Other));
    }

    #[test]
    fn token_outside_any_span_is_skipped_under_scoping() {
        // A token landing in a gap (no covering span) is not in any opted-in
        // class -> skipped. Only the covered, opted-in token is checked.
        let e = scoped_engine();
        let text = "gapword commz";
        let spans = [span(text, "commz", SpanClass::Comment)];
        let out = check_text_scoped(&e, text, &spans, SpellScope::new(true, true, true));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].word, "commz");
    }
}
