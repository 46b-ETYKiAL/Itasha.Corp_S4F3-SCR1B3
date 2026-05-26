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
        // Numbers and very short tokens are always "correct".
        if w.len() < 2 || w.chars().any(|c| c.is_ascii_digit()) {
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
}
