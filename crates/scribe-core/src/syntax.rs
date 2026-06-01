//! Syntax highlighting.
//!
//! Two engines sit behind one engine-agnostic API (`Highlighter` + `HlSpan`):
//!
//! * **tree-sitter** — used for languages that ship a native grammar (Rust
//!   today). Produces a concrete-syntax-tree highlight pass via
//!   `tree_sitter_highlight`, which yields `Source` events tiling the whole
//!   input; we split those at line boundaries into the per-line span shape the
//!   renderer expects.
//! * **syntect** — pure-Rust fallback covering ~100 bundled languages with zero
//!   C-grammar build step (aligned with the not-bloated + deliverability
//!   constraints).
//!
//! Callers never see which engine ran. Adding a grammar is: add the dep, add a
//! `HighlightConfiguration` in `new()`, and route the extension in
//! `highlight_document`. See ADR-0001.

use crate::spell::{ClassifiedSpan, SpanClass};
use std::ops::Range;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SynStyle, ThemeSet};
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};
use syntect::util::LinesWithEndings;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter as TsHighlighter};

/// A colored span of a line: byte range within the line + RGB color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlSpan {
    pub range: Range<usize>,
    pub color: [u8; 3],
    pub bold: bool,
    pub italic: bool,
}

/// Default editor foreground (base16-eighties text tone). Used for any byte not
/// claimed by a more specific highlight capture.
const DEFAULT_FG: [u8; 3] = [0xd3, 0xd0, 0xc8];

/// Recognised tree-sitter highlight capture names. `configure()` maps each
/// query capture to the longest matching entry here; unmatched captures fall
/// back to `DEFAULT_FG`. Order is the index space the `Highlight(idx)` events
/// reference, so `TS_COLORS` is built parallel to this list.
const HL_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "escape",
    "function",
    "function.builtin",
    "function.method",
    "keyword",
    "label",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "string",
    "string.escape",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

/// Map a capture name to an RGB color by longest-meaningful prefix.
fn color_for(name: &str) -> [u8; 3] {
    if name.starts_with("keyword") {
        [0xcc, 0x99, 0xcc] // magenta
    } else if name.starts_with("function") || name.starts_with("constructor") {
        [0x66, 0x99, 0xcc] // blue
    } else if name.starts_with("type") {
        [0xff, 0xcc, 0x66] // yellow
    } else if name.starts_with("string") || name.starts_with("escape") || name.starts_with("char") {
        [0x99, 0xcc, 0x99] // green
    } else if name.starts_with("comment") {
        [0x74, 0x73, 0x69] // grey
    } else if name.starts_with("constant")
        || name.starts_with("number")
        || name.starts_with("float")
    {
        [0xf9, 0x91, 0x57] // orange
    } else if name.starts_with("attribute") || name.starts_with("property") {
        [0x66, 0xcc, 0xcc] // cyan
    } else if name.starts_with("operator") || name.starts_with("punctuation") {
        [0xa0, 0x9f, 0x93] // muted
    } else if name.starts_with("label") || name.starts_with("tag") {
        [0xf2, 0x77, 0x7a] // red
    } else {
        DEFAULT_FG
    }
}

/// Map a highlighter scope/capture name to the spellcheck [`SpanClass`].
///
/// The rule is substring-based so it works for both the dotted tree-sitter
/// capture names (`"comment"`, `"string"`, `"string.escape"`) and the
/// space-separated syntect scope stacks (`"source.rust comment.line.double-slash.rust"`):
///
/// * contains `"comment"` → [`SpanClass::Comment`]
/// * else contains `"string"` or `"char"` → [`SpanClass::String`]
/// * else a name carrying identifier-ish semantics (`variable`, `function`,
///   `type`, `constant`, `property`, `entity.name`, `support`) →
///   [`SpanClass::Identifier`]
/// * everything else (keywords, operators, punctuation, whitespace) →
///   [`SpanClass::Other`]
///
/// Comment is checked first so a doc-comment scope that also mentions a
/// keyword classifies as a comment.
pub fn classify_scope_name(name: &str) -> SpanClass {
    if name.contains("comment") {
        SpanClass::Comment
    } else if name.contains("string") || name.contains("char") {
        SpanClass::String
    } else if name.contains("variable")
        || name.contains("function")
        || name.contains("constructor")
        || name.contains("type")
        || name.contains("constant")
        || name.contains("property")
        || name.contains("entity.name")
        || name.contains("support")
        || name.contains("identifier")
    {
        SpanClass::Identifier
    } else {
        SpanClass::Other
    }
}

pub struct Highlighter {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
    theme_name: String,
    /// tree-sitter Rust grammar + highlight query, compiled once. `None` if the
    /// grammar/query failed to build (we then fall back to syntect for Rust).
    ts_rust: Option<HighlightConfiguration>,
    /// Color per `HL_NAMES` index, used to resolve `Highlight(idx)` events.
    ts_colors: Vec<[u8; 3]>,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            themes: ThemeSet::load_defaults(),
            // A dark base theme; the app re-tints via its own Theme for chrome.
            theme_name: "base16-eighties.dark".to_string(),
            ts_rust: build_rust_config(),
            ts_colors: HL_NAMES.iter().map(|n| color_for(n)).collect(),
        }
    }

    /// Number of bundled syntect languages (sanity / about-box).
    pub fn language_count(&self) -> usize {
        self.syntaxes.syntaxes().len()
    }

    /// Number of languages served by a native tree-sitter grammar.
    pub fn tree_sitter_language_count(&self) -> usize {
        self.ts_rust.is_some() as usize
    }

    /// Resolve a syntect syntax by file extension/token, falling back to plain.
    fn syntax_for<'a>(&'a self, ext: Option<&str>) -> &'a syntect::parsing::SyntaxReference {
        ext.and_then(|e| self.syntaxes.find_syntax_by_extension(e))
            .or_else(|| Some(self.syntaxes.find_syntax_plain_text()))
            .unwrap()
    }

    /// Highlight an entire (small/medium) document into per-line spans.
    /// For huge files the caller should only pass the visible window.
    ///
    /// Routes to the tree-sitter backend for languages with a native grammar,
    /// else syntect. Both engines return the same per-line span shape.
    pub fn highlight_document(&self, text: &str, ext: Option<&str>) -> Vec<Vec<HlSpan>> {
        if matches!(ext, Some("rs")) {
            if let Some(spans) = self.highlight_tree_sitter(text) {
                return spans;
            }
        }
        self.highlight_syntect(text, ext)
    }

    /// tree-sitter highlight pass → per-line spans. `None` if the grammar is
    /// unavailable or the pass errors (caller falls back to syntect).
    fn highlight_tree_sitter(&self, text: &str) -> Option<Vec<Vec<HlSpan>>> {
        let cfg = self.ts_rust.as_ref()?;
        let mut ts = TsHighlighter::new();
        let src = text.as_bytes();
        let events = ts.highlight(cfg, src, None, |_| None).ok()?;

        // tree-sitter-highlight emits `Source` events spanning the WHOLE input
        // (a byte not under any capture has an empty highlight stack → default
        // color), so these absolute spans tile the document contiguously.
        let mut abs: Vec<(usize, usize, [u8; 3])> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for ev in events {
            match ev.ok()? {
                HighlightEvent::HighlightStart(h) => stack.push(h.0),
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    if end > start {
                        let color = stack
                            .last()
                            .and_then(|&i| self.ts_colors.get(i).copied())
                            .unwrap_or(DEFAULT_FG);
                        abs.push((start, end, color));
                    }
                }
            }
        }
        Some(tile_into_lines(text, &abs))
    }

    /// syntect highlight pass → per-line spans.
    fn highlight_syntect(&self, text: &str, ext: Option<&str>) -> Vec<Vec<HlSpan>> {
        let syntax = self.syntax_for(ext);
        let theme = &self.themes.themes[&self.theme_name];
        let mut hl = HighlightLines::new(syntax, theme);
        let mut out = Vec::new();
        for line in LinesWithEndings::from(text) {
            let mut spans = Vec::new();
            if let Ok(ranges) = hl.highlight_line(line, &self.syntaxes) {
                let mut offset = 0usize;
                for (style, piece) in ranges {
                    let len = piece.len();
                    spans.push(span_from(style, offset..offset + len));
                    offset += len;
                }
            }
            out.push(spans);
        }
        out
    }

    /// Classify the whole document into absolute byte spans tagged with a
    /// [`SpanClass`] (comment / string / identifier / other) for spellcheck
    /// scoping ([`crate::spell::check_text_scoped`]).
    ///
    /// Routes to the same backend as [`highlight_document`](Self::highlight_document)
    /// — tree-sitter for native-grammar languages (Rust today), else syntect —
    /// so the classification matches what the user sees highlighted. Returns an
    /// empty `Vec` when no syntax info can be derived; the scoped checker treats
    /// that as "no scoping" and falls back to whole-text checking, so an empty
    /// result never silently disables spellcheck.
    pub fn classify_document(&self, text: &str, ext: Option<&str>) -> Vec<ClassifiedSpan> {
        if matches!(ext, Some("rs")) {
            if let Some(spans) = self.classify_tree_sitter(text) {
                return spans;
            }
        }
        self.classify_syntect(text, ext)
    }

    /// tree-sitter classification pass → absolute classified spans. `None` if
    /// the grammar is unavailable or the pass errors (caller falls back to
    /// syntect). Mirrors [`highlight_tree_sitter`](Self::highlight_tree_sitter)
    /// but maps each capture's `HL_NAMES` entry to a [`SpanClass`].
    fn classify_tree_sitter(&self, text: &str) -> Option<Vec<ClassifiedSpan>> {
        let cfg = self.ts_rust.as_ref()?;
        let mut ts = TsHighlighter::new();
        let src = text.as_bytes();
        let events = ts.highlight(cfg, src, None, |_| None).ok()?;

        let mut out: Vec<ClassifiedSpan> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for ev in events {
            match ev.ok()? {
                HighlightEvent::HighlightStart(h) => stack.push(h.0),
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    if end > start {
                        let class = stack
                            .last()
                            .and_then(|&i| HL_NAMES.get(i))
                            .map(|name| classify_scope_name(name))
                            .unwrap_or(SpanClass::Other);
                        push_classified(&mut out, start, end, class);
                    }
                }
            }
        }
        Some(out)
    }

    /// syntect classification pass → absolute classified spans. Uses the
    /// `ParseState`/`ScopeStack` parsing API (NOT the color-only
    /// `HighlightLines`) so real scope names are available to
    /// [`classify_scope_name`]. The top-of-stack scope wins, matching the
    /// "most specific highlight" rule the tree-sitter path uses.
    fn classify_syntect(&self, text: &str, ext: Option<&str>) -> Vec<ClassifiedSpan> {
        let syntax = self.syntax_for(ext);
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut out: Vec<ClassifiedSpan> = Vec::new();
        let mut line_start = 0usize; // absolute byte offset of the current line

        for line in LinesWithEndings::from(text) {
            let Ok(ops) = state.parse_line(line, &self.syntaxes) else {
                line_start += line.len();
                continue;
            };
            // Walk the (offset, op) ops, emitting a classified span for each
            // run between consecutive offsets using the scope stack in force.
            let mut prev = 0usize; // offset within `line`
            for (offset, op) in ops {
                if offset > prev {
                    let class = scope_stack_class(&stack);
                    push_classified(&mut out, line_start + prev, line_start + offset, class);
                }
                let _ = stack.apply(&op);
                prev = offset;
            }
            // Tail of the line after the last op.
            if prev < line.len() {
                let class = scope_stack_class(&stack);
                push_classified(&mut out, line_start + prev, line_start + line.len(), class);
            }
            line_start += line.len();
        }
        out
    }
}

/// Classify a syntect scope stack by its most-specific (top) scope. Walks from
/// the top down so the innermost scope (e.g. `comment.line` over `source.rust`)
/// decides the class; falls back to `Other` for a bare/source-only stack.
fn scope_stack_class(stack: &ScopeStack) -> SpanClass {
    for scope in stack.as_slice().iter().rev() {
        let name = scope.build_string();
        let class = classify_scope_name(&name);
        if class != SpanClass::Other {
            return class;
        }
    }
    SpanClass::Other
}

/// Append a classified span, coalescing with the previous one when it is
/// byte-contiguous and the same class — keeps the span list compact so the
/// linear lookup in `check_text_scoped` stays cheap.
fn push_classified(out: &mut Vec<ClassifiedSpan>, start: usize, end: usize, class: SpanClass) {
    if end <= start {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.end == start && last.class == class {
            last.end = end;
            return;
        }
    }
    out.push(ClassifiedSpan { start, end, class });
}

/// Build the Rust tree-sitter highlight configuration. Returns `None` on any
/// grammar/query construction failure so the caller can fall back to syntect.
fn build_rust_config() -> Option<HighlightConfiguration> {
    let language = tree_sitter::Language::new(tree_sitter_rust::LANGUAGE);
    let mut cfg = HighlightConfiguration::new(
        language,
        "rust",
        tree_sitter_rust::HIGHLIGHTS_QUERY,
        "", // injections query
        "", // locals query
    )
    .ok()?;
    cfg.configure(HL_NAMES);
    Some(cfg)
}

/// Split a set of contiguous, ordered absolute byte spans into per-line spans
/// whose ranges are relative to each line's start. Line segmentation matches
/// `str::split_inclusive('\n')` (each line keeps its trailing newline; a final
/// line without a newline is its own line; empty text yields zero lines), so
/// the result indexes 1:1 with the renderer's line walk.
fn tile_into_lines(text: &str, spans: &[(usize, usize, [u8; 3])]) -> Vec<Vec<HlSpan>> {
    let mut line_ranges: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            line_ranges.push((start, i + 1));
            start = i + 1;
        }
    }
    if start < text.len() {
        line_ranges.push((start, text.len()));
    }

    let mut out: Vec<Vec<HlSpan>> = vec![Vec::new(); line_ranges.len()];
    for &(s, e, color) in spans {
        if s >= e {
            continue;
        }
        // First line whose start byte is <= s.
        let mut li = line_ranges
            .partition_point(|&(ls, _)| ls <= s)
            .saturating_sub(1);
        while li < line_ranges.len() {
            let (ls, le) = line_ranges[li];
            if ls >= e {
                break;
            }
            let cs = s.max(ls);
            let ce = e.min(le);
            if ce > cs {
                out[li].push(HlSpan {
                    range: (cs - ls)..(ce - ls),
                    color,
                    bold: false,
                    italic: false,
                });
            }
            if le >= e {
                break;
            }
            li += 1;
        }
    }
    out
}

fn span_from(style: SynStyle, range: Range<usize>) -> HlSpan {
    use syntect::highlighting::FontStyle;
    HlSpan {
        range,
        color: [style.foreground.r, style.foreground.g, style.foreground.b],
        bold: style.font_style.contains(FontStyle::BOLD),
        italic: style.font_style.contains(FontStyle::ITALIC),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_many_languages() {
        let h = Highlighter::new();
        assert!(
            h.language_count() > 50,
            "expected bundled syntaxes, got {}",
            h.language_count()
        );
    }

    #[test]
    fn tree_sitter_rust_grammar_is_wired() {
        let h = Highlighter::new();
        assert_eq!(
            h.tree_sitter_language_count(),
            1,
            "tree-sitter Rust grammar should compile and configure"
        );
    }

    #[test]
    fn rust_uses_tree_sitter_and_colors_keywords() {
        let h = Highlighter::new();
        let src = "fn main() {\n    let x = 1;\n}\n";
        let lines = h.highlight_document(src, Some("rs"));
        assert_eq!(lines.len(), 3, "three newlines => three lines");
        // Spans tile each line contiguously.
        for (li, (line, spans)) in src.split_inclusive('\n').zip(&lines).enumerate() {
            let covered: usize = spans.iter().map(|s| s.range.end - s.range.start).sum();
            assert_eq!(
                covered,
                line.len(),
                "line {li} spans must tile the whole line ({covered} != {})",
                line.len()
            );
        }
        // The `fn` keyword on line 0 should not be default-fg.
        let kw = &lines[0][0];
        assert_eq!(kw.range, 0..2, "first span covers `fn`");
        assert_ne!(kw.color, DEFAULT_FG, "`fn` keyword should be colored");
    }

    #[test]
    fn line_count_matches_split_inclusive() {
        let h = Highlighter::new();
        for src in ["", "a", "a\n", "a\nb", "a\nb\n", "fn x(){}\n"] {
            let expected = src.split_inclusive('\n').count();
            let lines = h.highlight_document(src, Some("rs"));
            assert_eq!(lines.len(), expected, "line count mismatch for {src:?}");
        }
    }

    #[test]
    fn highlights_rust_keywords_distinctly() {
        let h = Highlighter::new();
        let lines = h.highlight_document("fn main() {}\n", Some("rs"));
        assert_eq!(lines.len(), 1);
        let spans = &lines[0];
        assert!(!spans.is_empty());
    }

    #[test]
    fn unknown_extension_falls_back_plain() {
        let h = Highlighter::new();
        let lines = h.highlight_document("just text\n", Some("zzznope"));
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn syntect_path_still_works_for_python() {
        let h = Highlighter::new();
        let lines = h.highlight_document("def f():\n    pass\n", Some("py"));
        assert_eq!(lines.len(), 2);
        assert!(!lines[0].is_empty());
    }

    // ---- scope classification (classify_scope_name / classify_document) ----

    #[test]
    fn classify_scope_name_buckets() {
        // tree-sitter dotted names
        assert_eq!(classify_scope_name("comment"), SpanClass::Comment);
        assert_eq!(classify_scope_name("string"), SpanClass::String);
        assert_eq!(classify_scope_name("string.escape"), SpanClass::String);
        assert_eq!(classify_scope_name("variable"), SpanClass::Identifier);
        assert_eq!(
            classify_scope_name("function.method"),
            SpanClass::Identifier
        );
        assert_eq!(classify_scope_name("type.builtin"), SpanClass::Identifier);
        assert_eq!(classify_scope_name("keyword"), SpanClass::Other);
        assert_eq!(classify_scope_name("operator"), SpanClass::Other);
        assert_eq!(classify_scope_name("punctuation.bracket"), SpanClass::Other);
        // syntect space-separated stacks
        assert_eq!(
            classify_scope_name("source.rust comment.line.double-slash.rust"),
            SpanClass::Comment
        );
        assert_eq!(
            classify_scope_name("source.rust string.quoted.double.rust"),
            SpanClass::String
        );
        // comment wins over a co-occurring keyword token
        assert_eq!(
            classify_scope_name("comment.block keyword"),
            SpanClass::Comment
        );
    }

    /// Spans returned by classify_document tile the document contiguously and
    /// in order (no gaps, no overlaps) — the invariant check_text_scoped relies
    /// on for its start-byte lookup.
    fn assert_tiles(spans: &[ClassifiedSpan], len: usize) {
        let mut at = 0usize;
        for s in spans {
            assert_eq!(s.start, at, "span gap/overlap at {at}: {spans:?}");
            assert!(s.end > s.start);
            at = s.end;
        }
        assert_eq!(at, len, "spans must cover the whole document");
    }

    #[test]
    fn classify_rust_marks_comment_string_identifier() {
        let h = Highlighter::new();
        let src = "fn brokin() {\n    // mispel here\n    let s = \"wronng\";\n}\n";
        let spans = h.classify_document(src, Some("rs"));
        assert!(!spans.is_empty(), "rust should classify via tree-sitter");
        assert_tiles(&spans, src.len());

        // The class covering each deliberately-placed word matches.
        let class_at = |needle: &str| -> SpanClass {
            let off = src.find(needle).unwrap();
            spans
                .iter()
                .find(|s| off >= s.start && off < s.end)
                .map(|s| s.class)
                .unwrap()
        };
        assert_eq!(class_at("mispel"), SpanClass::Comment);
        assert_eq!(class_at("wronng"), SpanClass::String);
        assert_eq!(class_at("brokin"), SpanClass::Identifier);
    }

    #[test]
    fn classify_python_via_syntect_marks_comment_and_string() {
        let h = Highlighter::new();
        let src = "x = \"wronng\"  # mispel\n";
        let spans = h.classify_document(src, Some("py"));
        assert!(!spans.is_empty(), "python should classify via syntect");
        assert_tiles(&spans, src.len());

        let class_at = |needle: &str| -> SpanClass {
            let off = src.find(needle).unwrap();
            spans
                .iter()
                .find(|s| off >= s.start && off < s.end)
                .map(|s| s.class)
                .unwrap()
        };
        assert_eq!(class_at("wronng"), SpanClass::String);
        assert_eq!(class_at("mispel"), SpanClass::Comment);
    }

    #[test]
    fn classify_end_to_end_scopes_spellcheck() {
        // Wire the real highlighter classification into the real scoped checker
        // and confirm comments-only isolates the comment misspelling.
        use crate::spell::{check_text_scoped, HashSetEngine, SpellScope};
        let h = Highlighter::new();
        let engine = HashSetEngine::from_word_list("let\nfn\nhere\n");
        let src = "fn run() {\n    // mispel here\n    let v = \"wronng\";\n}\n";
        let spans = h.classify_document(src, Some("rs"));

        // Comments only -> "mispel" flagged (a misspelling), "here" is known,
        // "wronng" (string) is NOT checked.
        let out = check_text_scoped(&engine, src, &spans, SpellScope::new(true, false, false));
        let words: Vec<&str> = out.iter().map(|m| m.word.as_str()).collect();
        assert!(
            words.contains(&"mispel"),
            "comment misspelling found: {words:?}"
        );
        assert!(
            !words.contains(&"wronng"),
            "string word must be excluded: {words:?}"
        );
    }

    #[test]
    fn classify_empty_text_is_empty() {
        let h = Highlighter::new();
        assert!(h.classify_document("", Some("rs")).is_empty());
        assert!(h.classify_document("", Some("py")).is_empty());
    }
}
