//! Find / replace engine: literal or regex, case-sensitive or not, whole-word.
//! Returns byte-offset match spans the UI highlights.

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

/// All non-overlapping matches in `text`.
pub fn find_all(text: &str, q: &Query) -> Result<Vec<Match>> {
    if q.pattern.is_empty() {
        return Ok(Vec::new());
    }
    let re = build_regex(q)?;
    Ok(re
        .find_iter(text)
        .map(|m| Match {
            start: m.start(),
            end: m.end(),
        })
        .collect())
}

/// Replace all matches. For regex queries, `$1` capture refs in `replacement`
/// are honored (regex crate semantics).
pub fn replace_all(text: &str, q: &Query, replacement: &str) -> Result<String> {
    if q.pattern.is_empty() {
        return Ok(text.to_string());
    }
    let re = build_regex(q)?;
    Ok(re.replace_all(text, replacement).into_owned())
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
}
