//! Static, data-driven snippets: a `prefix` expands to `body` on Tab-trigger.
//! Tab-stops `${1}`,`${2}`,`$0` mark caret positions; the first stop becomes the
//! post-expansion caret. No scripting, no interpolation beyond stop markers —
//! a deliberately small, safe surface (NOT a plugin host).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snippet {
    pub prefix: String,
    pub body: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnippetSet {
    #[serde(default)]
    pub snippets: Vec<Snippet>,
}

impl SnippetSet {
    /// Parse a snippet set from TOML. Returns the error text on a parse failure
    /// so the caller can surface it without pulling in the `toml` error type.
    pub fn from_toml(s: &str) -> Result<Self, String> {
        toml::from_str(s).map_err(|e| e.to_string())
    }

    /// The snippet whose `prefix` exactly equals `word`, if any.
    pub fn lookup(&self, word: &str) -> Option<&Snippet> {
        self.snippets.iter().find(|s| s.prefix == word)
    }

    /// Whether any snippet is loaded.
    pub fn is_empty(&self) -> bool {
        self.snippets.is_empty()
    }
}

/// Result of expanding a snippet body: the literal text to insert (stop markers
/// stripped) and the caret char-offset within it (the first `${1}`/`$0`, else end).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    pub text: String,
    /// Char offset into `text` where the caret should land after expansion.
    pub caret_offset: usize,
}

/// Strip tab-stop markers from `body`, returning the literal text + the caret
/// position (lowest-numbered `${N}`/`$N` stop, else `$0`, else end-of-text).
pub fn expand(body: &str) -> Expansion {
    let mut text = String::with_capacity(body.len());
    let mut stops: Vec<(usize, u32)> = Vec::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            // ${N} form.
            if chars.peek() == Some(&'{') {
                chars.next(); // consume '{'
                let mut num = String::new();
                while let Some(&d) = chars.peek() {
                    if d == '}' {
                        chars.next();
                        break;
                    }
                    num.push(d);
                    chars.next();
                }
                if let Ok(n) = num.parse::<u32>() {
                    stops.push((text.chars().count(), n));
                }
                continue;
            } else if chars.peek().map(|d| d.is_ascii_digit()).unwrap_or(false) {
                // $N form.
                let mut num = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() {
                        num.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if let Ok(n) = num.parse::<u32>() {
                    stops.push((text.chars().count(), n));
                }
                continue;
            }
        }
        text.push(c);
    }
    // Caret: the lowest-numbered non-zero stop, else $0, else end.
    stops.sort_by_key(|(_, n)| if *n == 0 { u32::MAX } else { *n });
    let caret_offset = stops
        .first()
        .map(|(off, _)| *off)
        .unwrap_or_else(|| text.chars().count());
    Expansion { text, caret_offset }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_and_expand_with_tabstop() {
        let set = SnippetSet::from_toml(
            "[[snippets]]\nprefix = \"fn\"\nbody = \"fn ${1}() {\\n    $0\\n}\"\n",
        )
        .unwrap();
        let snip = set.lookup("fn").unwrap();
        let exp = expand(&snip.body);
        assert_eq!(exp.text, "fn () {\n    \n}");
        // caret at the ${1} position (char offset 3, right after "fn ").
        assert_eq!(exp.caret_offset, 3);
    }

    #[test]
    fn expand_without_stops_caret_at_end() {
        let e = expand("hello");
        assert_eq!(e.text, "hello");
        assert_eq!(e.caret_offset, 5);
    }

    #[test]
    fn dollar_zero_is_lower_priority_than_numbered_stop() {
        // $0 marks the final caret but a ${1} should win as the first landing.
        let e = expand("a$0b${1}c");
        assert_eq!(e.text, "abc");
        assert_eq!(e.caret_offset, 2); // position of ${1}, after "ab"
    }

    #[test]
    fn unknown_prefix_lookup_is_none() {
        let set = SnippetSet::default();
        assert!(set.lookup("nope").is_none());
        assert!(set.is_empty());
    }
}
