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
use syntect::highlighting::{
    HighlightIterator, HighlightState, Highlighter as SynHighlighter, Style as SynStyle, ThemeSet,
};
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

/// Above this buffer size, skip highlighting entirely (the caller paints plain
/// text). egui itself lays out + tessellates every row regardless of coloring,
/// so a multi-MB file is heavy no matter what; this is the safety cap so it can
/// never stall on the highlight pass.
///
/// Public so a caller (the rope editor) can branch on the SAME threshold: under
/// the cap it uses the whole-document incremental path (cross-line correct +
/// edit-gen cached); over the cap it falls back to a viewport-only approximate
/// highlight for the huge-file browse view. Single source of truth.
pub const MAX_HIGHLIGHT_BYTES: usize = 4 * 1024 * 1024;

/// Re-highlighting resumes from the nearest snapshot at/below the edited line.
/// We snapshot the syntect parse/highlight state every STRIDE lines (not every
/// line) so memory stays O(lines/STRIDE) while a single edit replays at most
/// STRIDE extra lines to rebuild the entering state.
const HL_SNAPSHOT_STRIDE: usize = 256;

/// One cached line in the incremental highlighter: its content hash (to detect a
/// change) and its computed spans.
#[derive(Clone)]
struct LineHl {
    text_hash: u64,
    spans: Vec<HlSpan>,
}

/// Incremental syntect-highlight cache for ONE editable buffer. Reused across
/// keystrokes so only the lines from the first edit downward are re-highlighted
/// (syntect parsing is stateful per line — block comments/strings carry across
/// lines — so we snapshot the state every [`HL_SNAPSHOT_STRIDE`] lines and
/// resume from the nearest one). The result is byte-identical to a full pass
/// (property-tested). The host holds one of these per focused buffer; it resets
/// when the language or theme changes. The tree-sitter (Rust) path does NOT use
/// this — it re-parses whole-document (already fast).
#[derive(Default)]
pub struct IncrementalHighlightState {
    /// `(ext, theme_name)` identity; a change resets the cache.
    key: Option<(Option<String>, String)>,
    /// One entry per source line, in order.
    lines: Vec<LineHl>,
    /// `(line_index, parse_state_entering_line, highlight_state_entering_line)`
    /// at every `HL_SNAPSHOT_STRIDE`-th line, ascending by line index.
    snapshots: Vec<(usize, ParseState, HighlightState)>,
}

impl IncrementalHighlightState {
    fn spans(&self) -> Vec<Vec<HlSpan>> {
        self.lines.iter().map(|l| l.spans.clone()).collect()
    }
    fn clear(&mut self) {
        self.lines.clear();
        self.snapshots.clear();
        self.key = None;
    }
}

#[inline]
fn hash_line(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

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

/// Representative syntect scope for a tree-sitter capture name (#104), so the
/// tree-sitter palette can be derived from the active syntect theme.
fn capture_to_scope(name: &str) -> &'static str {
    if name.starts_with("keyword") {
        "keyword.control"
    } else if name.starts_with("function") || name.starts_with("constructor") {
        "entity.name.function"
    } else if name.starts_with("type") {
        "entity.name.type"
    } else if name.starts_with("string") || name.starts_with("escape") || name.starts_with("char") {
        "string.quoted.double"
    } else if name.starts_with("comment") {
        "comment.line"
    } else if name.starts_with("constant")
        || name.starts_with("number")
        || name.starts_with("float")
    {
        "constant.numeric"
    } else if name.starts_with("attribute") || name.starts_with("property") {
        "entity.other.attribute-name"
    } else if name.starts_with("operator") || name.starts_with("punctuation") {
        "punctuation"
    } else if name.starts_with("label") || name.starts_with("tag") {
        "entity.name.tag"
    } else {
        "source"
    }
}

/// Foreground RGB the syntect `theme` assigns to the scope most representative
/// of a tree-sitter capture `name` — so the tree-sitter highlight path matches
/// the syntect path under the same theme (#104).
fn color_from_theme(theme: &syntect::highlighting::Theme, name: &str) -> [u8; 3] {
    use std::str::FromStr;
    let hl = syntect::highlighting::Highlighter::new(theme);
    let stack = ScopeStack::from_str(capture_to_scope(name)).unwrap_or_default();
    let c = hl.style_for_stack(stack.as_slice()).foreground;
    [c.r, c.g, c.b]
}

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
    /// tree-sitter Rust grammar + highlight query, compiled LAZILY on the first
    /// Rust-file highlight (not at construction) so cold start never pays the
    /// grammar+query compile when the user opens a non-Rust file or an empty
    /// buffer. Inner `None` = the build failed (we then fall back to syntect for
    /// Rust). `OnceLock` (not `cell::OnceCell`) preserves the field's original
    /// `Send`/`Sync` so `Highlighter`'s thread-safety is unchanged.
    ts_rust: std::sync::OnceLock<Option<HighlightConfiguration>>,
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
        let mut themes = ThemeSet::load_defaults();
        Self::add_bundled_themes(&mut themes);
        Self {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            themes,
            // A dark base theme; the app re-tints via its own Theme for chrome.
            theme_name: "base16-eighties.dark".to_string(),
            // Deferred: the Rust grammar+query is compiled on first use, not here
            // (see `ts_rust()` + the field doc). Keeps cold start off the
            // grammar-compile path.
            ts_rust: std::sync::OnceLock::new(),
            ts_colors: HL_NAMES.iter().map(|n| color_for(n)).collect(),
        }
    }

    /// The lazily-built tree-sitter Rust config. Compiles the grammar+query on
    /// the FIRST call (then caches it), so opening a non-Rust file or an empty
    /// buffer never pays the compile at startup. `None` if the build failed.
    fn ts_rust(&self) -> Option<&HighlightConfiguration> {
        self.ts_rust.get_or_init(build_rust_config).as_ref()
    }

    /// Merge the bundled BRAND note (syntax) colour themes (#26) into `themes`,
    /// alongside the syntect defaults. Each is an ORIGINAL SCR1B3 theme (no
    /// third-party licence) built PROGRAMMATICALLY via syntect's public `Theme`
    /// API — this avoids enabling syntect's `plist-load` feature (and pulling in
    /// the `plist` dependency) just to parse a `.tmTheme` at runtime.
    fn add_bundled_themes(themes: &mut ThemeSet) {
        use std::str::FromStr;
        use syntect::highlighting::{
            Color as SynColor, ScopeSelectors, StyleModifier, Theme as SynTheme, ThemeItem,
            ThemeSettings,
        };

        fn col(r: u8, g: u8, b: u8) -> SynColor {
            SynColor { r, g, b, a: 0xFF }
        }
        // One scope→colour rule. An unparsable selector falls back to the empty
        // selector (matches nothing) rather than panicking — the global
        // foreground still covers that token.
        fn item(scope: &str, c: SynColor) -> ThemeItem {
            ThemeItem {
                scope: ScopeSelectors::from_str(scope).unwrap_or_default(),
                style: StyleModifier {
                    foreground: Some(c),
                    background: None,
                    font_style: None,
                },
            }
        }
        // (background, foreground, caret, selection, line_highlight) + the 8
        // syntax rules in a fixed order: comment, keyword, string, constant,
        // function, type, variable, punctuation.
        #[allow(clippy::too_many_arguments)]
        fn theme(
            name: &str,
            bg: SynColor,
            fg: SynColor,
            caret: SynColor,
            sel: SynColor,
            line: SynColor,
            rules: [SynColor; 8],
        ) -> SynTheme {
            let settings = ThemeSettings {
                foreground: Some(fg),
                background: Some(bg),
                caret: Some(caret),
                selection: Some(sel),
                line_highlight: Some(line),
                ..Default::default()
            };
            let scopes = vec![
                item("comment", rules[0]),
                item("keyword, storage, keyword.control", rules[1]),
                item("string, string.quoted", rules[2]),
                item(
                    "constant.numeric, constant.language, constant.character",
                    rules[3],
                ),
                item("entity.name.function, support.function", rules[4]),
                item(
                    "entity.name.type, storage.type, support.type, support.class",
                    rules[5],
                ),
                item("variable, variable.parameter", rules[6]),
                item("keyword.operator, punctuation", rules[7]),
            ];
            SynTheme {
                name: Some(name.to_string()),
                author: Some("SCR1B3".to_string()),
                settings,
                scopes,
            }
        }

        // Wired Noir — cyan-on-near-black (mirrors the wired-noir chrome theme).
        themes.themes.insert(
            "Wired Noir".to_string(),
            theme(
                "Wired Noir",
                col(0x0A, 0x0E, 0x14),
                col(0xC8, 0xD6, 0xDC),
                col(0x00, 0xFF, 0xFE),
                col(0x13, 0x35, 0x4A),
                col(0x10, 0x16, 0x1F),
                [
                    col(0x5A, 0x68, 0x73),
                    col(0x00, 0xFF, 0xFE),
                    col(0x6F, 0xD7, 0xC1),
                    col(0xF9, 0x91, 0x57),
                    col(0x66, 0x99, 0xCC),
                    col(0xFF, 0xCC, 0x66),
                    col(0xC8, 0xD6, 0xDC),
                    col(0xA0, 0x9F, 0x93),
                ],
            ),
        );
        // Phosphor Amber — amber/green CRT phosphor on black.
        themes.themes.insert(
            "Phosphor Amber".to_string(),
            theme(
                "Phosphor Amber",
                col(0x0B, 0x0A, 0x06),
                col(0xE8, 0xC1, 0x70),
                col(0xFF, 0xB0, 0x00),
                col(0x3A, 0x2E, 0x10),
                col(0x15, 0x12, 0x0A),
                [
                    col(0x6B, 0x5A, 0x36),
                    col(0xFF, 0xB0, 0x00),
                    col(0x9F, 0xE0, 0x8F),
                    col(0xFF, 0x7A, 0x3C),
                    col(0xFF, 0xD4, 0x79),
                    col(0xFF, 0xE6, 0xA8),
                    col(0xE8, 0xC1, 0x70),
                    col(0x9C, 0x8A, 0x55),
                ],
            ),
        );
        // Operator Violet — the brand OPERATOR VIOLET (#A020FF) on deep plum.
        themes.themes.insert(
            "Operator Violet".to_string(),
            theme(
                "Operator Violet",
                col(0x0E, 0x0A, 0x14),
                col(0xD6, 0xC8, 0xE6),
                col(0xA0, 0x20, 0xFF),
                col(0x2A, 0x18, 0x40),
                col(0x15, 0x10, 0x1F),
                [
                    col(0x6A, 0x5A, 0x80),
                    col(0xA0, 0x20, 0xFF),
                    col(0xC7, 0xA6, 0xFF),
                    col(0xFF, 0x77, 0xC8),
                    col(0x9D, 0x7B, 0xFF),
                    col(0xE0, 0xB3, 0xFF),
                    col(0xD6, 0xC8, 0xE6),
                    col(0x8C, 0x7A, 0xA0),
                ],
            ),
        );
    }

    /// Number of bundled syntect languages (sanity / about-box).
    pub fn language_count(&self) -> usize {
        self.syntaxes.syntaxes().len()
    }

    /// Number of languages served by a native tree-sitter grammar. Does NOT
    /// force the lazy grammar build: if it hasn't been built yet, report the
    /// expected 1 (the embedded Rust grammar), so an about-box query never pays
    /// the compile; once built, report the real result.
    pub fn tree_sitter_language_count(&self) -> usize {
        self.ts_rust.get().map_or(1, |o| o.is_some() as usize)
    }

    /// The bundled note (syntax) colour themes, sorted for a stable picker order
    /// (#104). These are the editor text colour schemes — independent of the app
    /// chrome theme.
    pub fn theme_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.themes.themes.keys().cloned().collect();
        v.sort();
        v
    }

    /// The active note colour theme name.
    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }

    /// Set the note colour theme (#104). Unknown names are ignored (keeps the
    /// current theme). Re-derives the tree-sitter palette from the chosen theme
    /// so BOTH the syntect and tree-sitter highlight paths follow it uniformly.
    pub fn set_theme(&mut self, name: &str) {
        if !self.themes.themes.contains_key(name) {
            return;
        }
        self.theme_name = name.to_string();
        let theme = &self.themes.themes[name];
        self.ts_colors = HL_NAMES
            .iter()
            .map(|n| color_from_theme(theme, n))
            .collect();
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

    /// Incremental form of [`highlight_document`](Self::highlight_document): reuses
    /// `cache` so only the lines from the first change downward are re-highlighted
    /// (the per-keystroke "re-highlight the whole buffer" cost is removed for the
    /// syntect path). Output is byte-identical to a full pass — property-tested.
    /// Rust still uses the fast whole-document tree-sitter path; the cache covers
    /// the syntect path. Above [`MAX_HIGHLIGHT_BYTES`] highlighting is skipped.
    pub fn highlight_document_incremental(
        &self,
        text: &str,
        ext: Option<&str>,
        cache: &mut IncrementalHighlightState,
    ) -> Vec<Vec<HlSpan>> {
        if text.len() > MAX_HIGHLIGHT_BYTES {
            cache.clear();
            return Vec::new();
        }
        if matches!(ext, Some("rs")) {
            if let Some(spans) = self.highlight_tree_sitter(text) {
                cache.clear();
                return spans;
            }
        }
        self.highlight_syntect_incremental(text, ext, cache)
    }

    /// syntect incremental pass — resumes from the nearest snapshot at/below the
    /// first changed line and re-highlights downward. See
    /// [`IncrementalHighlightState`].
    fn highlight_syntect_incremental(
        &self,
        text: &str,
        ext: Option<&str>,
        cache: &mut IncrementalHighlightState,
    ) -> Vec<Vec<HlSpan>> {
        let syntax = self.syntax_for(ext);
        let theme = &self.themes.themes[&self.theme_name];
        let highlighter = SynHighlighter::new(theme);

        // Reset the whole cache when the language or theme changes.
        let this_key = (ext.map(str::to_string), self.theme_name.clone());
        if cache.key.as_ref() != Some(&this_key) {
            cache.lines.clear();
            cache.snapshots.clear();
            cache.key = Some(this_key);
        }

        let new_lines: Vec<&str> = LinesWithEndings::from(text).collect();

        // First line whose content differs from the cache.
        let mut dirty = 0usize;
        while dirty < new_lines.len()
            && dirty < cache.lines.len()
            && cache.lines[dirty].text_hash == hash_line(new_lines[dirty])
        {
            dirty += 1;
        }
        // Unchanged (same content AND same line count) → reuse wholesale.
        if dirty == new_lines.len() && new_lines.len() == cache.lines.len() {
            return cache.spans();
        }

        // Resume from the nearest snapshot whose line index is <= the edit line.
        let (start_line, mut ps, mut hs) =
            match cache.snapshots.iter().rposition(|(li, _, _)| *li <= dirty) {
                Some(i) => {
                    let (li, ps, hs) = &cache.snapshots[i];
                    (*li, ps.clone(), hs.clone())
                }
                None => (
                    0,
                    ParseState::new(syntax),
                    HighlightState::new(&highlighter, ScopeStack::new()),
                ),
            };

        // Drop snapshots past the resume point (rebuilt below) and the cached
        // spans from the edit line onward (the prefix [0, dirty) is reused).
        cache.snapshots.retain(|(li, _, _)| *li <= start_line);
        cache.lines.truncate(dirty);

        for (offset, &line) in new_lines[start_line..].iter().enumerate() {
            let li = start_line + offset;
            // Snapshot the ENTERING state at stride boundaries.
            if li % HL_SNAPSHOT_STRIDE == 0 && !cache.snapshots.iter().any(|(x, _, _)| *x == li) {
                cache.snapshots.push((li, ps.clone(), hs.clone()));
            }
            let ops = ps.parse_line(line, &self.syntaxes);
            if li >= dirty {
                // Changed / new line → recompute its spans.
                let mut spans = Vec::new();
                if let Ok(ops) = ops {
                    let mut byte = 0usize;
                    for (style, piece) in HighlightIterator::new(&mut hs, &ops, line, &highlighter)
                    {
                        let len = piece.len();
                        spans.push(span_from(style, byte..byte + len));
                        byte += len;
                    }
                }
                cache.lines.push(LineHl {
                    text_hash: hash_line(line),
                    spans,
                });
            } else if let Ok(ops) = ops {
                // Unchanged line before the edit → advance state only (spans kept).
                for _ in HighlightIterator::new(&mut hs, &ops, line, &highlighter) {}
            }
        }

        cache.spans()
    }

    /// tree-sitter highlight pass → per-line spans. `None` if the grammar is
    /// unavailable or the pass errors (caller falls back to syntect).
    ///
    /// P-03 NOTE — this is a WHOLE-DOCUMENT reparse, not a `Tree::edit`
    /// incremental reparse. The `tree_sitter_highlight::Highlighter::highlight`
    /// convenience API takes `source: &[u8]` with NO old-tree hook: it builds a
    /// fresh `Parser`/`Tree` internally and discards it, so there is no seam to
    /// feed a `Tree::edit`'d old tree through. True per-keystroke incremental
    /// reparse would require abandoning this convenience layer and
    /// reimplementing the highlight-query traversal against a manually-managed
    /// `tree_sitter::Parser` + persisted `Tree` + `QueryCursor` — a large,
    /// correctness-risky rewrite of the span-tiling. It is deliberately NOT
    /// half-wired here. Instead the per-FRAME cost (the dominant P-02 leak: this
    /// 700+ms pass ran every egui repaint) is removed by the edit-generation
    /// highlight cache in `scribe_render::rope_editor` — Rust highlighting now
    /// runs once per EDIT, not once per frame, which subsumes the worst cost.
    /// `Tree::edit` incremental reparse remains the open follow-up optimization.
    fn highlight_tree_sitter(&self, text: &str) -> Option<Vec<Vec<HlSpan>>> {
        let cfg = self.ts_rust()?;
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
        let cfg = self.ts_rust()?;
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
    fn note_theme_switch_is_applied_and_validated() {
        // #104 — the note colour theme can be switched; unknown names are
        // ignored (the current theme stays).
        let mut hl = Highlighter::new();
        assert!(hl
            .theme_names()
            .contains(&"base16-eighties.dark".to_string()));
        hl.set_theme("Solarized (dark)");
        assert_eq!(hl.theme_name(), "Solarized (dark)");
        hl.set_theme("no-such-theme");
        assert_eq!(hl.theme_name(), "Solarized (dark)", "unknown theme ignored");
    }

    #[test]
    fn bundled_brand_themes_load_and_apply() {
        // #26 — the 3 bundled brand note themes parse, register, and are
        // selectable (set_theme accepts them, unlike an unknown name). This also
        // guards `add_bundled_themes` against a malformed .tmTheme asset.
        let mut hl = Highlighter::new();
        let names = hl.theme_names();
        for brand in ["Wired Noir", "Phosphor Amber", "Operator Violet"] {
            assert!(names.contains(&brand.to_string()), "{brand} registered");
            hl.set_theme(brand);
            assert_eq!(hl.theme_name(), brand, "{brand} is selectable");
        }
        // The brand themes genuinely recolour vs a default theme.
        let mut a = Highlighter::new();
        a.set_theme("Operator Violet");
        let mut b = Highlighter::new();
        b.set_theme("InspiredGitHub");
        assert_ne!(
            a.highlight_document("let x = 1;\n", Some("rs")),
            b.highlight_document("let x = 1;\n", Some("rs")),
            "a brand note theme must recolour the text"
        );
    }

    #[test]
    fn different_note_themes_recolour_the_text() {
        // #104 — switching the note theme actually changes the highlight colours
        // (both the syntect and tree-sitter paths derive from it).
        let mut a = Highlighter::new();
        a.set_theme("base16-ocean.dark");
        let mut b = Highlighter::new();
        b.set_theme("InspiredGitHub");
        let ca = a.highlight_document("let x = 1;\n", Some("rs"));
        let cb = b.highlight_document("let x = 1;\n", Some("rs"));
        assert_ne!(ca, cb, "a different note theme must recolour the text");
    }

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

    #[test]
    fn incremental_highlight_matches_full_pass() {
        // The incremental highlighter MUST produce byte-identical spans to a full
        // syntect pass after any edit — that is the whole correctness contract.
        // Uses Python (triple-quoted strings span lines) so a mid-file edit
        // changes the parse state of following lines, exercising state resume.
        let h = Highlighter::new();
        let ext = Some("py");
        let full = |t: &str| h.highlight_syntect(t, ext);
        let mut cache = IncrementalHighlightState::default();

        // 1. Fresh build.
        let t0 = "x = 1\ny = '''multi\nline string'''\nz = 2\n";
        assert_eq!(
            h.highlight_document_incremental(t0, ext, &mut cache),
            full(t0)
        );
        // 2. Middle edit that changes multi-line string state propagation.
        let t1 = "x = 1\ny = 'short'\nline string'''\nz = 2\n";
        assert_eq!(
            h.highlight_document_incremental(t1, ext, &mut cache),
            full(t1),
            "must equal full after a state-changing middle edit"
        );
        // 3. Append.
        let t2 = format!("{t1}w = 3\nv = 4\n");
        assert_eq!(
            h.highlight_document_incremental(&t2, ext, &mut cache),
            full(&t2)
        );
        // 4. Edit the FIRST line (resume from line 0).
        let t3 = t2.replacen("x = 1", "x = 999", 1);
        assert_eq!(
            h.highlight_document_incremental(&t3, ext, &mut cache),
            full(&t3)
        );
        // 5. Delete a line.
        let t4 = t3.replacen("z = 2\n", "", 1);
        assert_eq!(
            h.highlight_document_incremental(&t4, ext, &mut cache),
            full(&t4)
        );

        // 6. A 600-line file (spans multiple snapshot strides), then a deep edit
        //    that must resume from the nearest snapshot, not the top.
        let mut big = String::from("s = '''\n");
        for i in 0..600 {
            big.push_str(&format!("body line {i}\n"));
        }
        big.push_str("'''\nend = 1\n");
        let mut cache2 = IncrementalHighlightState::default();
        assert_eq!(
            h.highlight_document_incremental(&big, ext, &mut cache2),
            full(&big)
        );
        let big2 = big.replacen("body line 400", "body line 400 EDITED", 1);
        assert_eq!(
            h.highlight_document_incremental(&big2, ext, &mut cache2),
            full(&big2),
            "must equal full after a deep edit (snapshot-stride resume)"
        );
    }

    // ---- tile_into_lines: the pure absolute-span → per-line-span splitter ----
    // (driven directly so the line-segmentation + cross-line-span split logic is
    // exercised independently of which highlight engine produced the spans.)

    const RED: [u8; 3] = [0xff, 0, 0];
    const GRN: [u8; 3] = [0, 0xff, 0];

    #[test]
    fn tile_empty_text_yields_no_lines() {
        assert!(tile_into_lines("", &[]).is_empty());
    }

    #[test]
    fn tile_single_line_no_trailing_newline_is_one_line() {
        // "abc" with a span over "ab": one line, one relative span 0..2.
        let out = tile_into_lines("abc", &[(0, 2, RED)]);
        assert_eq!(out.len(), 1, "a final line without \\n is its own line");
        assert_eq!(out[0].len(), 1);
        assert_eq!(out[0][0].range, 0..2);
        assert_eq!(out[0][0].color, RED);
    }

    #[test]
    fn tile_relativizes_spans_to_each_line_start() {
        // "ab\ncd\n" → two lines at absolute [0,3) and [3,6). A span over the
        // second line's "cd" (absolute 3..5) must become relative 0..2 on line 1.
        let out = tile_into_lines("ab\ncd\n", &[(3, 5, GRN)]);
        assert_eq!(out.len(), 2);
        assert!(out[0].is_empty(), "line 0 has no span");
        assert_eq!(out[1].len(), 1);
        assert_eq!(out[1][0].range, 0..2, "absolute 3..5 -> line-relative 0..2");
        assert_eq!(out[1][0].color, GRN);
    }

    #[test]
    fn tile_splits_a_span_that_crosses_a_line_boundary() {
        // A single absolute span 1..5 over "ab\ncd\n" straddles the newline: it
        // must be clipped into a piece on line 0 (1..3 incl. the \n) and a piece
        // on line 1 (0..2), each relative to its own line.
        let out = tile_into_lines("ab\ncd\n", &[(1, 5, RED)]);
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0].len(),
            1,
            "line 0 carries the head of the crossing span"
        );
        assert_eq!(out[0][0].range, 1..3, "1..3 = 'b' + the trailing newline");
        assert_eq!(out[1].len(), 1, "line 1 carries the tail");
        assert_eq!(out[1][0].range, 0..2, "tail relative to line 1 start");
    }

    #[test]
    fn tile_drops_degenerate_and_empty_spans() {
        // s >= e spans contribute nothing (the `if s >= e { continue }` guard).
        let out = tile_into_lines("abc\n", &[(2, 2, RED), (3, 1, GRN)]);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].is_empty(),
            "zero-width / inverted spans produce no output"
        );
    }

    #[test]
    fn tile_spans_in_final_line_without_newline() {
        // "x\nyz" → line 0 "x\n" [0,2), line 1 "yz" [2,4) with NO trailing \n.
        // A span over the last line's "z" (absolute 3..4) lands on line 1 at 1..2.
        let out = tile_into_lines("x\nyz", &[(3, 4, GRN)]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1][0].range, 1..2);
    }

    // ---- pure helpers around the incremental cache (previously uncovered) ----

    #[test]
    fn incremental_state_clear_resets_and_a_fresh_pass_rebuilds() {
        // `clear()` drops cached lines, snapshots, and the (ext, theme) key so the
        // next pass is a full rebuild. We prove the post-clear pass still matches
        // a full syntect pass (the cache's correctness contract holds after a
        // reset, not just on first build).
        let h = Highlighter::new();
        let ext = Some("py");
        let mut cache = IncrementalHighlightState::default();
        let t = "a = 1\nb = 2\n";
        let _ = h.highlight_document_incremental(t, ext, &mut cache);
        cache.clear();
        assert!(cache.lines.is_empty(), "clear empties the line cache");
        assert!(cache.snapshots.is_empty(), "clear empties the snapshots");
        assert!(
            cache.key.is_none(),
            "clear forgets the (ext, theme) identity"
        );
        // A pass after clear rebuilds and equals a full pass.
        assert_eq!(
            h.highlight_document_incremental(t, ext, &mut cache),
            h.highlight_syntect(t, ext),
            "a post-clear pass is byte-identical to a full pass"
        );
    }

    #[test]
    fn incremental_skips_and_clears_above_the_size_cap() {
        // Above MAX_HIGHLIGHT_BYTES the incremental path skips highlighting
        // entirely (returns empty) AND clears the cache so stale spans can't leak
        // into a later under-cap pass.
        let h = Highlighter::new();
        let ext = Some("py");
        let mut cache = IncrementalHighlightState::default();
        // Prime the cache with a small pass first.
        let _ = h.highlight_document_incremental("x = 1\n", ext, &mut cache);
        assert!(!cache.lines.is_empty(), "primed cache is non-empty");
        // Now feed an over-cap buffer.
        let huge = "a\n".repeat(MAX_HIGHLIGHT_BYTES); // ~2x the cap in bytes
        let out = h.highlight_document_incremental(&huge, ext, &mut cache);
        assert!(out.is_empty(), "over-cap input is not highlighted");
        assert!(cache.lines.is_empty(), "the cache is cleared on the skip");
    }

    #[test]
    fn highlighter_default_equals_new() {
        // `Default for Highlighter` must construct the same wired highlighter as
        // `new()` — same bundled languages + selectable brand themes.
        let d = Highlighter::default();
        let n = Highlighter::new();
        assert_eq!(d.language_count(), n.language_count());
        assert_eq!(d.theme_name(), n.theme_name());
        assert!(d.theme_names().contains(&"Operator Violet".to_string()));
    }
}
