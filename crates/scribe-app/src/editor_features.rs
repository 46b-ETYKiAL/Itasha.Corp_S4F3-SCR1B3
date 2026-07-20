//! Editor power-features that are pure data transforms — kept out of the egui
//! UI layer so they are unit-testable without a render context.
//!
//! * [`fold_regions`] — brace-balanced foldable line ranges for the fold gutter.
//! * [`project_folded`] — collapse folded regions into a display string + a
//!   display-line → source-line map (for a read-only folded preview).
//! * [`word_completions`] — local "dabbrev"-style identifier completion that
//!   powers the completion popup with zero network/LSP dependency. LSP items,
//!   when available, are merged on top by the caller.
//! * [`symbol_scopes`] — brace-delimited definition scopes (fn/struct/impl/…)
//!   that drive the breadcrumbs bar (F-033) and sticky-scroll headers (F-034).

use std::collections::BTreeSet;

/// A foldable region: the line that owns the opening brace, and the last line
/// of the region (the line with the matching closing brace). Both are 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldRegion {
    /// Line carrying the `{` — the line that stays visible with a ▾/▸ toggle.
    pub start_line: usize,
    /// Line carrying the matching `}` — last line hidden when folded.
    pub end_line: usize,
}

impl FoldRegion {
    /// Number of lines hidden when this region is folded (the body, excluding
    /// the header line that remains visible).
    pub fn hidden_len(&self) -> usize {
        self.end_line.saturating_sub(self.start_line)
    }
}

/// Compute brace-balanced foldable regions spanning more than one line.
///
/// Heuristic (language-agnostic, good enough until tree-sitter node ranges
/// drive it): track `{`/`}` nesting outside of string and line-comment context,
/// pairing each `{` with its matching `}`. Only multi-line pairs are returned,
/// ordered by `start_line`.
pub fn fold_regions(text: &str) -> Vec<FoldRegion> {
    let mut stack: Vec<usize> = Vec::new(); // line index of each open brace
    let mut out: Vec<FoldRegion> = Vec::new();
    let mut in_string: Option<char> = None;
    let mut prev = '\0';

    for (line_idx, line) in text.split_inclusive('\n').enumerate() {
        let mut line_comment = false;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if line_comment {
                break;
            }
            match in_string {
                Some(q) => {
                    // End of string unless escaped.
                    if c == q && prev != '\\' {
                        in_string = None;
                    }
                }
                None => match c {
                    '"' | '\'' => in_string = Some(c),
                    '/' if chars.peek() == Some(&'/') => line_comment = true,
                    '{' => stack.push(line_idx),
                    '}' => {
                        if let Some(open) = stack.pop() {
                            if line_idx > open {
                                out.push(FoldRegion {
                                    start_line: open,
                                    end_line: line_idx,
                                });
                            }
                        }
                    }
                    _ => {}
                },
            }
            prev = c;
        }
        // Newlines never continue a single-line string for this heuristic.
        in_string = None;
        prev = '\0';
    }

    out.sort_by_key(|r| r.start_line);
    out
}

/// Language-aware foldable regions (P2-4): markdown / text notes fold by
/// heading SECTION (a heading line to the line before the next same-or-higher
/// heading), everything else folds by brace balance. Reuses the existing fold
/// gutter + [`project_folded`] verbatim — only the region source differs.
pub fn fold_regions_for(text: &str, lang: Option<&str>) -> Vec<FoldRegion> {
    let is_note = matches!(
        lang,
        Some("md") | Some("markdown") | Some("txt") | Some("text")
    );
    if !is_note {
        return fold_regions(text);
    }
    scribe_core::md_ops::heading_fold_regions(text)
        .into_iter()
        .map(|(start_line, end_line)| FoldRegion {
            start_line,
            end_line,
        })
        .collect()
}

/// Project `text` with the given folded regions collapsed. Each folded region
/// keeps its header line (with a ` …` marker appended) and drops the body
/// through `end_line`. Returns the display string and a map from each display
/// line to its source line index.
///
/// `folded` holds the `start_line` of every region the user has collapsed; only
/// regions present in `regions` are honoured (so stale fold state is ignored).
pub fn project_folded(
    text: &str,
    regions: &[FoldRegion],
    folded: &BTreeSet<usize>,
) -> (String, Vec<usize>) {
    // Lines hidden by an active fold (header excluded).
    let mut hidden: BTreeSet<usize> = BTreeSet::new();
    let mut header_of: std::collections::BTreeMap<usize, usize> = Default::default();
    for r in regions {
        if folded.contains(&r.start_line) {
            for l in (r.start_line + 1)..=r.end_line {
                hidden.insert(l);
            }
            header_of.insert(r.start_line, r.end_line);
        }
    }

    let mut display = String::new();
    let mut map: Vec<usize> = Vec::new();
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    for (idx, line) in lines.iter().enumerate() {
        if hidden.contains(&idx) {
            continue;
        }
        let body = line.strip_suffix('\n').unwrap_or(line);
        if header_of.contains_key(&idx) {
            display.push_str(body);
            display.push_str(" …");
        } else {
            display.push_str(body);
        }
        // Preserve original newline presence.
        if line.ends_with('\n') {
            display.push('\n');
        }
        map.push(idx);
    }
    (display, map)
}

/// Extract distinct identifier-like words from `text` that start with `prefix`
/// (case-sensitive), excluding `prefix` itself, ordered shortest-first then
/// lexicographically. A word is `[A-Za-z_][A-Za-z0-9_]*`. Returns at most
/// `limit` suggestions.
pub fn word_completions(text: &str, prefix: &str, limit: usize) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, seen: &mut BTreeSet<String>| {
        if !cur.is_empty() {
            if cur.len() > prefix.len() && cur.starts_with(prefix) {
                seen.insert(std::mem::take(cur));
            } else {
                cur.clear();
            }
        }
    };
    for c in text.chars() {
        if c == '_' || c.is_ascii_alphanumeric() {
            // Identifiers cannot start with a digit.
            if cur.is_empty() && c.is_ascii_digit() {
                // Skip the rest of this numeric token.
                cur.push('\0');
                continue;
            }
            if !cur.starts_with('\0') {
                cur.push(c);
            }
        } else {
            flush(&mut cur, &mut seen);
            cur.clear();
        }
    }
    flush(&mut cur, &mut seen);

    let mut v: Vec<String> = seen.into_iter().collect();
    v.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    v.truncate(limit);
    v
}

/// Find the start byte of the identifier ending at byte offset `cursor` in
/// `text` (the prefix being typed). Returns `(prefix_start, prefix_str)`.
pub fn prefix_before(text: &str, cursor: usize) -> (usize, String) {
    let bytes = text.as_bytes();
    let mut start = cursor.min(text.len());
    while start > 0 {
        let b = bytes[start - 1];
        let is_word = b == b'_' || b.is_ascii_alphanumeric();
        if !is_word {
            break;
        }
        start -= 1;
    }
    (start, text[start..cursor.min(text.len())].to_string())
}

/// A brace-delimited definition scope (a `fn`/`struct`/`impl`/… block).
/// Lines are 0-based. `depth` is the brace-nesting depth of the header
/// (0 = top level) so callers can render nesting without recomputing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolScope {
    /// 0-based line carrying the definition header (and its opening brace).
    pub start_line: usize,
    /// 0-based line carrying the matching closing brace.
    pub end_line: usize,
    /// Concise label, e.g. `fn parse`, `impl Display`, `struct Point`.
    pub label: String,
    /// Brace-nesting depth of the header line (0 = outermost).
    pub depth: usize,
}

/// Definition-introducing keywords the breadcrumb scanner recognises. A
/// brace-opening line is only treated as a symbol scope when one of these
/// appears as a whole word before the `{` — so control-flow blocks
/// (`if`/`for`/`while`/`match`/`loop`/`else`) and closures are excluded.
const DEF_KEYWORDS: &[&str] = &[
    "fn",
    "struct",
    "enum",
    "trait",
    "impl",
    "mod",
    "class",
    "interface",
    "namespace",
    "macro_rules",
    "function",
];

/// Extract the concise label for a definition header line, or `None` when the
/// line opens a brace block that is not a definition (control flow, closure,
/// bare block). The label is `"<keyword> <name>"`, the name being the next
/// identifier-ish token after the keyword (trimmed of `(`, `<`, `{`, `:`).
fn label_for_def(line: &str) -> Option<String> {
    // Tokenise on whitespace; tolerate `macro_rules!` and `impl<T>`.
    let tokens: Vec<&str> = line.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        // Strip a trailing `!` so `macro_rules!` matches `macro_rules`.
        let kw = tok.trim_end_matches('!');
        if !DEF_KEYWORDS.contains(&kw) {
            continue;
        }
        // Found the keyword. The name is the next token, cleaned of the
        // punctuation that commonly butts against it.
        let name = tokens.get(i + 1).map(|n| {
            n.trim_matches(|c: char| {
                c == '(' || c == '{' || c == '<' || c == ':' || c == ';' || c == '!'
            })
            .split(['(', '{', '<', ':', ';'])
            .next()
            .unwrap_or("")
        });
        return Some(match name {
            Some(n) if !n.is_empty() => format!("{kw} {n}"),
            _ => kw.to_string(),
        });
    }
    None
}

/// Discover brace-delimited definition scopes, ordered by `start_line`.
///
/// Reuses the same string/line-comment-aware brace scanner as
/// [`fold_regions`], but records each opening brace's line text so a closed
/// pair can be classified as a definition (via [`label_for_def`]) or
/// discarded. Single-line definitions and non-definition blocks are skipped.
/// Brace-based by design — indentation-delimited languages (Python) are not
/// covered by this heuristic.
pub fn symbol_scopes(text: &str) -> Vec<SymbolScope> {
    // Stack entries: (line index of `{`, trimmed header text at that line).
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut out: Vec<SymbolScope> = Vec::new();
    let mut in_string: Option<char> = None;
    let mut prev = '\0';

    for (line_idx, line) in text.split_inclusive('\n').enumerate() {
        let mut line_comment = false;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if line_comment {
                break;
            }
            match in_string {
                Some(q) => {
                    if c == q && prev != '\\' {
                        in_string = None;
                    }
                }
                None => match c {
                    '"' | '\'' => in_string = Some(c),
                    '/' if chars.peek() == Some(&'/') => line_comment = true,
                    '{' => stack.push((line_idx, line.trim().to_string())),
                    '}' => {
                        if let Some((open, header)) = stack.pop() {
                            if line_idx > open {
                                if let Some(label) = label_for_def(&header) {
                                    out.push(SymbolScope {
                                        start_line: open,
                                        end_line: line_idx,
                                        label,
                                        depth: stack.len(),
                                    });
                                }
                            }
                        }
                    }
                    _ => {}
                },
            }
            prev = c;
        }
        in_string = None;
        prev = '\0';
    }

    out.sort_by_key(|s| s.start_line);
    out
}

/// Symbols for the Go-to-symbol modal (P1-1): for markdown / plain-text notes,
/// the ATX heading outline; for everything else, the brace-delimited definition
/// scopes. `lang` is the active document's language hint (extension).
///
/// This lets the existing Ctrl+Shift+O modal double as a document outline for
/// prose — the heading data comes from [`scribe_core::md_ops::heading_outline`],
/// so there is no new panel or UI subsystem. Heading levels map to the modal's
/// `depth` indent (level 1 = depth 0).
pub fn outline_symbols(text: &str, lang: Option<&str>) -> Vec<SymbolScope> {
    let is_note = matches!(
        lang,
        Some("md") | Some("markdown") | Some("txt") | Some("text")
    );
    if !is_note {
        return symbol_scopes(text);
    }
    let headings = scribe_core::md_ops::heading_outline(text);
    // Each heading's scope runs to the line before the next same-or-higher
    // heading (so `breadcrumb_at`/`sticky_chain_at` still make sense if reused).
    let total_lines = text.split('\n').count();
    let mut out: Vec<SymbolScope> = Vec::with_capacity(headings.len());
    for (i, h) in headings.iter().enumerate() {
        let mut end = total_lines.saturating_sub(1);
        for next in &headings[i + 1..] {
            if next.level <= h.level {
                end = next.line.saturating_sub(1).max(h.line);
                break;
            }
        }
        let title = if h.title.is_empty() {
            format!("{} (untitled)", "#".repeat(h.level as usize))
        } else {
            h.title.clone()
        };
        out.push(SymbolScope {
            start_line: h.line,
            end_line: end,
            label: title,
            depth: (h.level as usize).saturating_sub(1),
        });
    }
    out
}

/// The chain of definition scopes enclosing a 0-based `line`, outermost first.
/// This is the breadcrumb path (F-033) — e.g. `[mod foo, impl Bar, fn baz]`.
pub fn breadcrumb_at(scopes: &[SymbolScope], line: usize) -> Vec<&SymbolScope> {
    let mut chain: Vec<&SymbolScope> = scopes
        .iter()
        .filter(|s| s.start_line <= line && line <= s.end_line)
        .collect();
    chain.sort_by_key(|s| s.depth);
    chain
}

/// The definition headers to pin at the top of the viewport (F-034): every
/// scope whose header has scrolled above `first_visible_line` while its body
/// still spans it, outermost first. Capped at `max` so deep nesting can't eat
/// the viewport.
pub fn sticky_chain_at(
    scopes: &[SymbolScope],
    first_visible_line: usize,
    max: usize,
) -> Vec<&SymbolScope> {
    let mut chain: Vec<&SymbolScope> = scopes
        .iter()
        .filter(|s| s.start_line < first_visible_line && first_visible_line <= s.end_line)
        .collect();
    chain.sort_by_key(|s| s.depth);
    chain.truncate(max);
    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_before_walks_to_buffer_start() {
        // A prefix that walks to column 0: clean `while start > 0` stops at 0; the
        // `>=` mutant reads bytes[0-1] and underflow-panics. Kills 203:17.
        assert_eq!(prefix_before("foo", 3), (0, "foo".to_string()));
    }

    #[test]
    fn label_for_def_empty_name_falls_back_to_keyword() {
        // Anonymous block header: the name token cleans to "" -> label is the bare
        // keyword. The `if !n.is_empty() -> if true` guard mutant emits "impl "
        // (trailing space) instead. Kills 271:24.
        assert_eq!(label_for_def("impl {"), Some("impl".to_string()));
        assert_eq!(label_for_def("fn {"), Some("fn".to_string()));
    }

    #[test]
    fn fold_regions_string_end_exposes_brace() {
        // A `{` inside a string is ignored; a `}` on the next line pops nothing ->
        // empty. The string-end mutants close the string early, exposing the `{`.
        // Kills 55:26, 55:31.
        assert!(fold_regions("open = \"x{y\";\nclose }\n").is_empty());
    }

    #[test]
    fn fold_regions_open_string_swallows_brace() {
        // `"q"` closes, ` { ` opens line0, `}` line2 -> region (0,2). The
        // `prev != '\\'` -> `==` mutant never closes the string. Kills 55:39.
        let r = fold_regions("x = \"q\" {\n body;\n}\n");
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].start_line, r[0].end_line), (0, 2));
    }

    #[test]
    fn fold_regions_ignores_braces_in_string_body() {
        // Deleting the string-start arm treats in-string braces as real. Kills 60:21.
        assert!(fold_regions("x = \"{\"\n}\n").is_empty());
    }

    #[test]
    fn fold_regions_line_comment_hides_brace() {
        // `// }` is a comment -> `}` ignored -> region (0,3). The comment-guard
        // false / `!=` mutants treat it as code. Kills 61:28-false, 61:41.
        let r = fold_regions("a {\n// }\nb;\n}\n");
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].start_line, r[0].end_line), (0, 3));
    }

    #[test]
    fn fold_regions_single_slash_is_not_comment() {
        // A single `/` (peek is space) is NOT a comment -> `}` pops -> region (0,1).
        // The comment-guard true mutant swallows it. Kills 61:28-true.
        let r = fold_regions("x {\na / b }\n");
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].start_line, r[0].end_line), (0, 1));
    }

    #[test]
    fn symbol_scopes_string_end_exposes_brace() {
        // Same scanner as fold_regions but scope-gated by label_for_def. Kills
        // 302:26, 302:31.
        assert!(symbol_scopes("fn a() {\ns = \"x}\"\n").is_empty());
    }

    #[test]
    fn symbol_scopes_open_string_swallows_close() {
        // Kills 302:39.
        let s = symbol_scopes("fn a() {\nx;\n\"q\" }\n");
        assert_eq!(
            s.iter().map(|x| x.label.as_str()).collect::<Vec<_>>(),
            vec!["fn a"]
        );
        assert_eq!((s[0].start_line, s[0].end_line), (0, 2));
    }

    #[test]
    fn symbol_scopes_string_arm_hides_brace() {
        // Kills 307:21.
        let s = symbol_scopes("fn a() {\nx = \"{\"\ny;\n}\n");
        assert_eq!(
            s.iter().map(|x| x.label.as_str()).collect::<Vec<_>>(),
            vec!["fn a"]
        );
        assert_eq!(s[0].end_line, 3);
    }

    #[test]
    fn symbol_scopes_line_comment_hides_close() {
        // Kills 308:28-false, 308:41.
        let s = symbol_scopes("fn a() {\nx; // }\ny;\n}\n");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].end_line, 3);
    }

    #[test]
    fn symbol_scopes_single_slash_is_not_comment() {
        // Kills 308:28-true.
        let s = symbol_scopes("fn a() {\nz = a / b }\n");
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].start_line, s[0].end_line), (0, 1));
    }

    #[test]
    fn outline_symbols_section_end_spans_to_next_peer() {
        // "# A"(l1,line0), "## B"(l2,line1), "# C"(l1,line2). A's section runs to
        // the line before the next level-1 heading (C@2) => 1. The `+ -> *`
        // collapses every end to its own start; `<= -> >` breaks at the nested B;
        // `+ -> -` panics at i=0. Kills 360:33 (x2), 361:27.
        let out = outline_symbols("# A\n## B\n# C\n", Some("md"));
        let a = out.iter().find(|s| s.label == "A").expect("heading A");
        assert_eq!(
            a.end_line, 1,
            "A must span to just before the next peer heading"
        );
        assert!(a.end_line > a.start_line);
    }

    #[test]
    fn folds_multiline_brace_pairs() {
        let src = "fn a() {\n    body;\n}\nfn b() {}\n";
        let regions = fold_regions(src);
        // Only the multi-line `fn a` body folds; `fn b() {}` is single-line.
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].start_line, 0);
        assert_eq!(regions[0].end_line, 2);
        assert_eq!(regions[0].hidden_len(), 2);
    }

    #[test]
    fn nested_braces_pair_correctly() {
        let src = "a {\n  b {\n    c;\n  }\n}\n";
        let regions = fold_regions(src);
        assert_eq!(regions.len(), 2);
        // Sorted by start_line: outer (0..4) then inner (1..3).
        assert_eq!((regions[0].start_line, regions[0].end_line), (0, 4));
        assert_eq!((regions[1].start_line, regions[1].end_line), (1, 3));
    }

    #[test]
    fn braces_in_strings_and_comments_are_ignored() {
        let src = "let s = \"{ not a fold\";\n// } also not\nok;\n";
        assert!(fold_regions(src).is_empty());
    }

    #[test]
    fn project_folded_collapses_body() {
        let src = "fn a() {\n    body;\n}\ntail;\n";
        let regions = fold_regions(src);
        let folded: BTreeSet<usize> = [0usize].into_iter().collect();
        let (disp, map) = project_folded(src, &regions, &folded);
        // Header + tail only; body lines 1 and 2 hidden.
        assert_eq!(disp, "fn a() { …\ntail;\n");
        assert_eq!(map, vec![0, 3]);
    }

    #[test]
    fn project_unfolded_is_identity_map() {
        let src = "a\nb\nc\n";
        let (disp, map) = project_folded(src, &[], &BTreeSet::new());
        assert_eq!(disp, src);
        assert_eq!(map, vec![0, 1, 2]);
    }

    #[test]
    fn completions_prefix_match_shortest_first() {
        // "valuate" does NOT match "value" (valu-A-te); excluded by design.
        let src = "value valuer valuate value_x other";
        let got = word_completions(src, "value", 10);
        assert_eq!(got, vec!["valuer", "value_x"]);
    }

    #[test]
    fn completions_exclude_exact_prefix_and_nonmatches() {
        let src = "foo foobar baz";
        let got = word_completions(src, "foo", 10);
        assert_eq!(got, vec!["foobar"]);
    }

    #[test]
    fn completions_ignore_numeric_tokens() {
        let src = "var123 12345 var_a";
        let got = word_completions(src, "var", 10);
        assert_eq!(got, vec!["var_a", "var123"]);
    }

    #[test]
    fn completions_respect_limit() {
        let src = "aa ab ac ad ae af";
        let got = word_completions(src, "a", 3);
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn prefix_before_cursor() {
        let text = "let foo.bar";
        let (start, pre) = prefix_before(text, text.len());
        assert_eq!(pre, "bar");
        assert_eq!(start, 8);
        // Cursor in the middle of `foo`.
        let (s2, p2) = prefix_before(text, 6);
        assert_eq!(p2, "fo");
        assert_eq!(s2, 4);
    }

    // ---- symbol_scopes / breadcrumb_at / sticky_chain_at (F-033 / F-034) ----

    #[test]
    fn symbol_scopes_finds_nested_defs() {
        let src =
            "mod foo {\n    impl Bar {\n        fn baz() {\n            x;\n        }\n    }\n}\n";
        let scopes = symbol_scopes(src);
        let labels: Vec<&str> = scopes.iter().map(|s| s.label.as_str()).collect();
        // Ordered by start_line: mod (0), impl (1), fn (2).
        assert_eq!(labels, vec!["mod foo", "impl Bar", "fn baz"]);
        assert_eq!(scopes[0].depth, 0);
        assert_eq!(scopes[1].depth, 1);
        assert_eq!(scopes[2].depth, 2);
        assert_eq!((scopes[2].start_line, scopes[2].end_line), (2, 4));
    }

    #[test]
    fn symbol_scopes_excludes_control_flow_and_closures() {
        // `if`, `for`, `match`, and a closure all open braces but are NOT defs.
        let src = "fn run() {\n    if x {\n        y;\n    }\n    for i in 0..3 {\n        z;\n    }\n    let c = || {\n        w;\n    };\n}\n";
        let scopes = symbol_scopes(src);
        let labels: Vec<&str> = scopes.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, vec!["fn run"], "only the fn is a symbol scope");
    }

    #[test]
    fn symbol_scopes_handles_struct_and_single_line_is_skipped() {
        // `struct Point { x: i32 }` is single-line → skipped (no nesting).
        let src = "struct Point { x: i32 }\nfn area() {\n    1;\n}\n";
        let scopes = symbol_scopes(src);
        let labels: Vec<&str> = scopes.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, vec!["fn area"]);
    }

    #[test]
    fn breadcrumb_at_returns_outermost_first() {
        let src = "mod foo {\n    impl Bar {\n        fn baz() {\n            here;\n        }\n    }\n}\n";
        let scopes = symbol_scopes(src);
        // Line 3 is `here;` — enclosed by all three.
        let crumbs: Vec<&str> = breadcrumb_at(&scopes, 3)
            .iter()
            .map(|s| s.label.as_str())
            .collect();
        assert_eq!(crumbs, vec!["mod foo", "impl Bar", "fn baz"]);
        // A line outside any scope yields an empty path.
        assert!(breadcrumb_at(&scopes, 99).is_empty());
    }

    #[test]
    fn sticky_chain_pins_enclosing_headers_above_viewport() {
        let src = "mod foo {\n    impl Bar {\n        fn baz() {\n            a;\n            b;\n            c;\n        }\n    }\n}\n";
        let scopes = symbol_scopes(src);
        // Viewport top at line 4 (`b;`): all three headers (lines 0/1/2) are
        // above it and still enclose it → pin all three, outermost first.
        let pinned: Vec<&str> = sticky_chain_at(&scopes, 4, 5)
            .iter()
            .map(|s| s.label.as_str())
            .collect();
        assert_eq!(pinned, vec!["mod foo", "impl Bar", "fn baz"]);
        // The `max` cap truncates the deepest entries.
        let capped = sticky_chain_at(&scopes, 4, 2);
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].label, "mod foo");
    }

    #[test]
    fn sticky_chain_empty_when_header_is_visible() {
        let src = "fn baz() {\n    a;\n}\n";
        let scopes = symbol_scopes(src);
        // Viewport top at line 0 — the header itself is visible, nothing to pin.
        assert!(sticky_chain_at(&scopes, 0, 5).is_empty());
    }
}
