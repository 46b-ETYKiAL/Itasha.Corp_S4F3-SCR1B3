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

use std::ops::Range;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SynStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
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
}
