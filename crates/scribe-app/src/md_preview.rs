//! Markdown preview: `pulldown-cmark` events → a flat, document-order list of
//! styled [`MdBlock`]s that the egui side panel renders as native widgets.
//!
//! **No HTML, no webview, no JavaScript.** Only the common CommonMark subset is
//! rendered (headings, paragraphs, emphasis/strong, inline + fenced code, bullet
//! and ordered lists, blockquotes, links, horizontal rules); anything else
//! degrades to plain text. This keeps the crate `#![forbid(unsafe_code)]` and
//! adds zero attack surface beyond the pure-Rust parser.
//!
//! The design splits cleanly into two halves:
//!   1. [`parse`] — pure `&str -> Vec<MdBlock>`, no egui dependency, fully unit
//!      tested below. This is the load-bearing logic.
//!   2. [`show`] — walks the parsed blocks and emits egui widgets. It contains
//!      no parsing logic, so it cannot be the source of a markdown bug.
//!
//! Built against `pulldown-cmark` 0.13 (the 0.11 → 0.13 split moved every block
//! end-tag onto the [`pulldown_cmark::TagEnd`] enum; `Tag::Heading` carries a
//! `level` field; `Tag::List(Option<u64>)` carries the ordered-list start index;
//! `Tag::Link { dest_url, .. }`; `Tag::CodeBlock(CodeBlockKind)`).

use egui::{Color32, RichText};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Parser, Tag, TagEnd};

/// Render markdown source to a standalone, self-contained HTML document (for the
/// "Export as HTML" command). Uses pulldown-cmark's own HTML writer — pure Rust,
/// no webview, no network. A minimal embedded stylesheet keeps the output
/// readable on its own.
///
/// # Security (SEC-2 — stored XSS in the exported artifact)
///
/// Unlike the in-app preview (which renders a safe block model with no HTML),
/// the exported `.html` is a file the user opens in a **browser**, where any
/// raw HTML or dangerous-scheme URL from an untrusted markdown document would
/// EXECUTE. pulldown-cmark's default writer passes raw inline/block HTML through
/// verbatim and does not filter `javascript:`/`data:` hrefs, so an export of
/// attacker-supplied markdown could carry `<script>`, `<img onerror=…>`, and
/// `javascript:` links into the browser context of the exported file.
///
/// Defense (mirrors the in-app [`is_safe_link_scheme`] allowlist, no new crate):
///   * Raw-HTML passthrough is DISABLED — every [`Event::Html`] /
///     [`Event::InlineHtml`] is dropped from the stream before the writer sees
///     it, so no author-supplied `<script>`/`onerror=`/`<iframe>` survives.
///   * Every [`Tag::Link`]/[`Tag::Image`] destination is run through
///     [`is_safe_link_scheme`]; a disallowed scheme (`javascript:`, `data:`,
///     `file:`, …) is neutralised to an inert `#` anchor so it can never reach
///     the HTML writer as a live URL.
///   * A restrictive `Content-Security-Policy` meta tag is emitted as
///     defense-in-depth: even if a vector ever slipped past the event filter,
///     the browser is told to run no script and load nothing.
pub fn to_html(md: &str) -> String {
    let body = render_safe_html(md);
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta http-equiv=\"Content-Security-Policy\" \
         content=\"default-src 'none'; img-src 'self' http: https: mailto:; \
         style-src 'unsafe-inline'\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <style>\n\
         body{{max-width:46rem;margin:2rem auto;padding:0 1rem;\
         font:16px/1.6 system-ui,sans-serif;color:#1b1b1b}}\n\
         pre,code{{font-family:ui-monospace,monospace}}\n\
         pre{{background:#f4f4f4;padding:.75rem;overflow:auto;border-radius:6px}}\n\
         code{{background:#f4f4f4;padding:.1rem .3rem;border-radius:3px}}\n\
         pre code{{background:none;padding:0}}\n\
         blockquote{{border-left:3px solid #ccc;margin:0;padding-left:1rem;color:#555}}\n\
         table{{border-collapse:collapse}}td,th{{border:1px solid #ccc;padding:.3rem .6rem}}\n\
         </style>\n</head>\n<body>\n{body}</body>\n</html>\n"
    )
}

/// Build the `<body>` HTML from markdown with raw-HTML passthrough disabled and
/// dangerous link/image schemes neutralised (see [`to_html`] for the threat
/// model). Pure `&str -> String`, no IO — unit-tested below.
fn render_safe_html(md: &str) -> String {
    let safe_events = Parser::new(md).filter_map(|ev| match ev {
        // Drop author-supplied raw HTML entirely. This is the `<script>` /
        // `<img onerror=…>` / `<iframe>` vector — markdown that embeds raw HTML
        // must NOT have it survive into a browser-opened export.
        Event::Html(_) | Event::InlineHtml(_) => None,
        // Neutralise dangerous-scheme link/image destinations to an inert `#`
        // anchor before the HTML writer turns them into a live `href`/`src`.
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Link {
            link_type,
            dest_url: neutralise_dest(dest_url),
            title,
            id,
        })),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Image {
            link_type,
            dest_url: neutralise_dest(dest_url),
            title,
            id,
        })),
        other => Some(other),
    });
    let mut body = String::new();
    pulldown_cmark::html::push_html(&mut body, safe_events);
    body
}

/// Replace a link/image destination with an inert `#` anchor when its scheme is
/// not on the [`is_safe_link_scheme`] allowlist; otherwise pass it through. The
/// `#` fragment resolves against the document and can never invoke a protocol
/// handler (`javascript:`, `data:`, `file:`, …).
fn neutralise_dest(dest_url: pulldown_cmark::CowStr<'_>) -> pulldown_cmark::CowStr<'_> {
    if is_safe_link_scheme(&dest_url) {
        dest_url
    } else {
        pulldown_cmark::CowStr::Borrowed("#")
    }
}

/// A renderable block in document order. Inline styling within a block is
/// flattened to a sequence of [`MdRun`]s.
#[derive(Debug, Clone, PartialEq)]
pub enum MdBlock {
    /// `#`..`######` heading. `level` is 1–6.
    Heading { level: u8, text: String },
    /// A run of inline text (a paragraph).
    Paragraph(Vec<MdRun>),
    /// A fenced or indented code block. `lang` is the fence info-string (first
    /// word only), `None` when absent. `code` keeps original line breaks.
    CodeBlock { lang: Option<String>, code: String },
    /// One list item. `depth` is 0-based nesting. `marker` is the rendered
    /// bullet/ordinal prefix (e.g. `"•"` or `"3."`).
    ListItem {
        depth: u8,
        marker: String,
        runs: Vec<MdRun>,
    },
    /// A block-quoted paragraph.
    Quote(Vec<MdRun>),
    /// A horizontal rule (`---`).
    Rule,
}

/// A styled inline run. `link` set means the whole run is a hyperlink.
#[derive(Debug, Clone, PartialEq)]
pub struct MdRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<String>,
}

/// Tracks one open ordered/unordered list level and, for ordered lists, the
/// next ordinal to emit.
#[derive(Clone, Copy)]
struct ListLevel {
    /// `Some(next_index)` for an ordered list, `None` for a bullet list.
    ordinal: Option<u64>,
}

/// Parse markdown into the block model. Never panics; malformed or truncated
/// input yields a best-effort block list.
///
/// The parser is a small state machine over `pulldown-cmark` events: inline
/// events accumulate into `runs`, block-end events flush `runs` into the
/// appropriate [`MdBlock`]. Active inline styles (`bold`/`italic`/`code`/`link`)
/// are tracked as a flat set — CommonMark nests these but flattening to the
/// innermost active style is visually sufficient for a preview pane.
pub fn parse(src: &str) -> Vec<MdBlock> {
    let mut blocks: Vec<MdBlock> = Vec::new();
    let mut runs: Vec<MdRun> = Vec::new();

    let (mut bold, mut italic, code) = (false, false, false);
    let mut link: Option<String> = None;

    // Open list levels (outermost first). Used for depth + ordinal markers.
    let mut lists: Vec<ListLevel> = Vec::new();
    // Stack of open list items as `(depth, marker, flushed)`. An item's own text
    // is flushed (emitted) when a nested list begins, so a parent item appears
    // BEFORE its children in reading order; `flushed` guards against a second
    // emit at item-end.
    let mut pending: Vec<(u8, String, bool)> = Vec::new();
    // Block-quote nesting depth; > 0 means the next flushed runs are a Quote.
    let mut quote_depth: u32 = 0;

    // Fenced/indented code-block accumulation.
    let mut code_lang: Option<String> = None;
    let mut in_code_block = false;
    let mut code_buf = String::new();

    // Flush the current inline `runs` into a run-bearing block, clearing it.
    fn push_run(
        runs: &mut Vec<MdRun>,
        text: &str,
        bold: bool,
        italic: bool,
        code: bool,
        link: &Option<String>,
    ) {
        if !text.is_empty() {
            runs.push(MdRun {
                text: text.to_string(),
                bold,
                italic,
                code,
                link: link.clone(),
            });
        }
    }

    for ev in Parser::new(src) {
        match ev {
            // ---- Headings ---------------------------------------------------
            Event::Start(Tag::Heading { .. }) => runs.clear(),
            Event::End(TagEnd::Heading(level)) => {
                let text: String = runs.drain(..).map(|r| r.text).collect();
                blocks.push(MdBlock::Heading {
                    level: heading_to_u8(level),
                    text,
                });
            }

            // ---- Paragraphs -------------------------------------------------
            Event::Start(Tag::Paragraph) => runs.clear(),
            Event::End(TagEnd::Paragraph) if !runs.is_empty() => {
                let taken = std::mem::take(&mut runs);
                if quote_depth > 0 {
                    blocks.push(MdBlock::Quote(taken));
                } else {
                    blocks.push(MdBlock::Paragraph(taken));
                }
            }

            // ---- Code blocks ------------------------------------------------
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code_block = true;
                code_buf.clear();
                code_lang = match kind {
                    // Info-string can be `rust ignore` — keep the first word only.
                    CodeBlockKind::Fenced(info) => {
                        let first = info.split_whitespace().next().unwrap_or("");
                        if first.is_empty() {
                            None
                        } else {
                            Some(first.to_string())
                        }
                    }
                    CodeBlockKind::Indented => None,
                };
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                // Trim the single trailing newline pulldown-cmark appends.
                let mut code = std::mem::take(&mut code_buf);
                if code.ends_with('\n') {
                    code.pop();
                }
                blocks.push(MdBlock::CodeBlock {
                    lang: code_lang.take(),
                    code,
                });
            }

            // ---- Lists ------------------------------------------------------
            Event::Start(Tag::List(start)) => {
                // A nested list begins inside the current item: flush that item's
                // own text first so the parent is emitted before its children.
                if let Some((depth, marker, flushed)) = pending.last_mut() {
                    if !*flushed && !runs.is_empty() {
                        blocks.push(MdBlock::ListItem {
                            depth: *depth,
                            marker: marker.clone(),
                            runs: std::mem::take(&mut runs),
                        });
                        *flushed = true;
                    }
                }
                lists.push(ListLevel { ordinal: start });
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                // Compute this item's depth + marker now (its ordinal position is
                // known at start); the text accumulates until the item is flushed.
                let depth = lists.len().saturating_sub(1) as u8;
                let marker = match lists.last_mut() {
                    Some(level) => match level.ordinal {
                        Some(n) => {
                            level.ordinal = Some(n + 1);
                            format!("{n}.")
                        }
                        None => "•".to_string(),
                    },
                    None => "•".to_string(),
                };
                runs.clear();
                pending.push((depth, marker, false));
            }
            Event::End(TagEnd::Item) => {
                if let Some((depth, marker, flushed)) = pending.pop() {
                    if !flushed {
                        blocks.push(MdBlock::ListItem {
                            depth,
                            marker,
                            runs: std::mem::take(&mut runs),
                        });
                    } else {
                        // Trailing text after a nested list (uncommon) is dropped
                        // rather than leaking into the next sibling item.
                        runs.clear();
                    }
                }
            }

            // ---- Block quotes ----------------------------------------------
            Event::Start(Tag::BlockQuote(_)) => {
                quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                quote_depth = quote_depth.saturating_sub(1);
            }

            // ---- Inline styling --------------------------------------------
            Event::Start(Tag::Strong) => bold = true,
            Event::End(TagEnd::Strong) => bold = false,
            Event::Start(Tag::Emphasis) => italic = true,
            Event::End(TagEnd::Emphasis) => italic = false,
            Event::Start(Tag::Link { dest_url, .. }) => link = Some(dest_url.to_string()),
            Event::End(TagEnd::Link) => link = None,

            // ---- Leaf content ----------------------------------------------
            Event::Code(s) => push_run(&mut runs, &s, bold, italic, true, &link),
            Event::Text(s) => {
                if in_code_block {
                    code_buf.push_str(&s);
                } else {
                    push_run(&mut runs, &s, bold, italic, code, &link);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if in_code_block {
                    code_buf.push('\n');
                } else {
                    push_run(&mut runs, " ", bold, italic, code, &link);
                }
            }
            Event::Rule => blocks.push(MdBlock::Rule),

            // Everything else (tables, footnotes, HTML, tasks, images) degrades
            // to its text content via the Text events already handled above.
            _ => {}
        }
    }

    // Flush any dangling runs from truncated input (e.g. an unclosed paragraph).
    if !runs.is_empty() {
        blocks.push(MdBlock::Paragraph(runs));
    }

    blocks
}

/// Map a `pulldown-cmark` [`HeadingLevel`] to a 1–6 integer.
fn heading_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Render parsed markdown into an egui [`Ui`] as native widgets.
///
/// `accent` colours headings and links; `muted` colours code. Pass the active
/// theme's colours from the call site. This function parses on every call — for
/// a preview pane that is fine; cache the [`parse`] result if the source is
/// large and unchanged between frames.
pub fn show(ui: &mut egui::Ui, md: &str, accent: Color32, muted: Color32) {
    for block in parse(md) {
        match block {
            MdBlock::Heading { level, text } => {
                let size = match level {
                    1 => 26.0,
                    2 => 22.0,
                    3 => 18.0,
                    _ => 15.0,
                };
                ui.add(egui::Label::new(
                    RichText::new(text).size(size).strong().color(accent),
                ));
                ui.add_space(2.0);
            }
            MdBlock::Paragraph(runs) => {
                ui.horizontal_wrapped(|ui| render_runs(ui, &runs, accent, muted));
                ui.add_space(4.0);
            }
            MdBlock::Quote(runs) => {
                ui.indent("md_quote", |ui| {
                    ui.horizontal_wrapped(|ui| {
                        render_runs(ui, &runs, accent, muted);
                    });
                });
                ui.add_space(4.0);
            }
            MdBlock::CodeBlock { code, .. } => {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.add(egui::Label::new(
                        RichText::new(code).monospace().color(muted),
                    ));
                });
                ui.add_space(4.0);
            }
            MdBlock::ListItem {
                depth,
                marker,
                runs,
            } => {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(depth as f32 * 16.0);
                    ui.label(RichText::new(marker).color(muted));
                    render_runs(ui, &runs, accent, muted);
                });
            }
            MdBlock::Rule => {
                ui.separator();
            }
        }
    }
}

/// S-05 (CWE-79 / CWE-939 — URL-scheme injection). Decide whether a markdown
/// link URL is safe to make CLICKABLE. Only a small allowlist of schemes is
/// permitted; everything else (`javascript:`, `data:`, `file:`, `vbscript:`,
/// …) is rendered as inert text so a malicious markdown document can never
/// hand the user a one-click code-execution / local-file / data-URI vector.
///
/// Allowed:
///   * `http:` / `https:` / `mailto:` (explicit safe schemes)
///   * relative & anchor links (no scheme at all — `./x`, `../x`, `#frag`,
///     `path/page.md`) — these resolve against the document, never a new
///     protocol handler.
///
/// Fail-CLOSED: an unknown scheme is rejected. The check is case-insensitive
/// and tolerant of leading ASCII whitespace / control bytes (the classic
/// `  JavaScript:` and `java\tscript:` obfuscation tricks).
///
/// Pure function — no egui, no IO — so it is exhaustively unit-tested below.
pub(crate) fn is_safe_link_scheme(url: &str) -> bool {
    // Per the WHATWG URL spec, ASCII whitespace and C0 control bytes (NUL,
    // TAB, CR, LF, …) are STRIPPED THROUGHOUT a URL before scheme parsing —
    // not merely from the front. So "java\tscript:" and "  java\nscript:"
    // both collapse to "javascript:" and must be rejected; a naive leading-
    // trim would let the embedded-tab form smuggle past. We remove every
    // ASCII whitespace/control byte, then parse the scheme from what remains.
    let trimmed: String = url
        .chars()
        .filter(|c| !(c.is_ascii_whitespace() || c.is_ascii_control()))
        .collect();

    // Find the scheme delimiter. Per RFC 3986 a scheme is
    // ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) followed by ':'. If there
    // is no ':' before the first '/', '?', '#' or whitespace, there is no
    // scheme → it is a relative/anchor link, which is always safe.
    let mut scheme = String::new();
    let mut has_scheme = false;
    for (i, c) in trimmed.char_indices() {
        if c == ':' {
            // A ':' as the FIRST char, or after a path separator, is not a
            // scheme delimiter (e.g. "./a:b" or "#a:b").
            has_scheme = i > 0;
            break;
        }
        // Anything that can't be part of a scheme means there is no scheme
        // (it's a relative path like "a/b.md" or "page?x=1#y").
        if c == '/' || c == '?' || c == '#' || c.is_ascii_whitespace() {
            break;
        }
        if c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.' {
            scheme.push(c.to_ascii_lowercase());
        } else {
            // A control/other byte inside the would-be scheme: not a valid
            // scheme → treat as relative (and the rest of the pipeline will
            // render it as text/relative, never a protocol handler).
            break;
        }
    }

    if !has_scheme {
        // No scheme → relative or anchor link → safe.
        return true;
    }

    // The first char of a scheme must be ALPHA (RFC 3986). A leading digit
    // means this wasn't a real scheme.
    if scheme
        .chars()
        .next()
        .is_none_or(|c| !c.is_ascii_alphabetic())
    {
        return false;
    }

    matches!(scheme.as_str(), "http" | "https" | "mailto")
}

/// Emit a sequence of styled inline runs into the current (typically
/// `horizontal_wrapped`) layout.
fn render_runs(ui: &mut egui::Ui, runs: &[MdRun], accent: Color32, muted: Color32) {
    for r in runs {
        if let Some(url) = &r.link {
            if is_safe_link_scheme(url) {
                // egui handles its own link styling/underline; only colour it.
                ui.hyperlink_to(RichText::new(&r.text).color(accent), url);
            } else {
                // S-05 — disallowed scheme (javascript:/data:/file:/…). Render
                // the link TEXT as inert, visually-muted styled text so it is
                // NOT clickable and cannot open a protocol handler.
                ui.label(RichText::new(&r.text).color(muted).strikethrough())
                    .on_hover_text("link blocked: unsafe URL scheme");
            }
            continue;
        }
        let mut rt = RichText::new(&r.text);
        if r.bold {
            rt = rt.strong();
        }
        if r.italic {
            rt = rt.italics();
        }
        if r.code {
            rt = rt.monospace().color(muted);
        }
        ui.label(rt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect a run-bearing block's text into one string for assertions.
    fn runs_text(runs: &[MdRun]) -> String {
        runs.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn parses_heading_levels() {
        let b = parse("# One\n\n## Two\n\n### Three\n");
        assert!(matches!(&b[0], MdBlock::Heading { level: 1, text } if text == "One"));
        assert!(matches!(&b[1], MdBlock::Heading { level: 2, text } if text == "Two"));
        assert!(matches!(&b[2], MdBlock::Heading { level: 3, text } if text == "Three"));
    }

    #[test]
    fn parses_paragraph_with_emphasis() {
        let b = parse("Hello **bold** and *italic* text\n");
        match &b[0] {
            MdBlock::Paragraph(runs) => {
                assert_eq!(runs_text(runs), "Hello bold and italic text");
                assert!(runs.iter().any(|r| r.bold && r.text == "bold"));
                assert!(runs.iter().any(|r| r.italic && r.text == "italic"));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parses_inline_and_fenced_code() {
        let b = parse("Use `cargo build` here.\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n");
        // Inline code run.
        match &b[0] {
            MdBlock::Paragraph(runs) => {
                assert!(runs.iter().any(|r| r.code && r.text == "cargo build"));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
        // Fenced block: lang captured, both lines present, no trailing newline.
        match &b[1] {
            MdBlock::CodeBlock { lang, code } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert!(code.contains("let x = 1;"));
                assert!(code.contains("let y = 2;"));
                assert!(!code.ends_with('\n'));
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn parses_ordered_and_bullet_lists() {
        // Ordered list emits incrementing ordinals; bullet list emits "•".
        let b = parse("1. first\n2. second\n3. third\n\n- a\n- b\n");
        let markers: Vec<&str> = b
            .iter()
            .filter_map(|blk| match blk {
                MdBlock::ListItem { marker, .. } => Some(marker.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(markers, vec!["1.", "2.", "3.", "•", "•"]);
    }

    #[test]
    fn to_html_wraps_a_standalone_document() {
        let html = to_html("# Title\n\nSome **bold** text.\n");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.trim_end().ends_with("</html>"));
    }

    #[test]
    fn parses_nested_list_depth() {
        let b = parse("- outer\n    - inner\n");
        let depths: Vec<u8> = b
            .iter()
            .filter_map(|blk| match blk {
                MdBlock::ListItem { depth, .. } => Some(*depth),
                _ => None,
            })
            .collect();
        assert_eq!(depths, vec![0, 1]);
    }

    #[test]
    fn parses_link() {
        let b = parse("See [the site](https://example.com) now\n");
        match &b[0] {
            MdBlock::Paragraph(runs) => {
                let linked = runs
                    .iter()
                    .find(|r| r.link.is_some())
                    .expect("expected a linked run");
                assert_eq!(linked.text, "the site");
                assert_eq!(linked.link.as_deref(), Some("https://example.com"));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parses_blockquote_and_rule() {
        let b = parse("> quoted line\n\n---\n");
        assert!(matches!(&b[0], MdBlock::Quote(runs) if runs_text(runs) == "quoted line"));
        assert!(matches!(&b[1], MdBlock::Rule));
    }

    #[test]
    fn malformed_input_does_not_panic() {
        // Unclosed emphasis, unterminated link, dangling fence — must not panic
        // and must still return some best-effort blocks.
        let _ = parse("**unclosed [link](http://\n\n```\nno close fence");
        let _ = parse("");
        let _ = parse("###### h6 only");
    }

    #[test]
    fn parses_all_six_heading_levels_to_u8() {
        // Covers the H4/H5/H6 arms of heading_to_u8 that the 1-3 test missed.
        let b = parse("#### Four\n\n##### Five\n\n###### Six\n");
        assert!(matches!(&b[0], MdBlock::Heading { level: 4, text } if text == "Four"));
        assert!(matches!(&b[1], MdBlock::Heading { level: 5, text } if text == "Five"));
        assert!(matches!(&b[2], MdBlock::Heading { level: 6, text } if text == "Six"));
    }

    #[test]
    fn heading_to_u8_maps_every_level() {
        assert_eq!(heading_to_u8(HeadingLevel::H1), 1);
        assert_eq!(heading_to_u8(HeadingLevel::H2), 2);
        assert_eq!(heading_to_u8(HeadingLevel::H3), 3);
        assert_eq!(heading_to_u8(HeadingLevel::H4), 4);
        assert_eq!(heading_to_u8(HeadingLevel::H5), 5);
        assert_eq!(heading_to_u8(HeadingLevel::H6), 6);
    }

    #[test]
    fn fenced_code_info_string_keeps_only_first_word() {
        // `rust ignore` is a real CommonMark info-string; only `rust` is the lang.
        let b = parse("```rust ignore\nlet x = 1;\n```\n");
        match &b[0] {
            MdBlock::CodeBlock { lang, code } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert!(code.contains("let x = 1;"));
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn fenced_code_without_lang_has_none() {
        // Bare fence: info-string empty => lang None (the `first.is_empty()` arm).
        let b = parse("```\nplain\n```\n");
        match &b[0] {
            MdBlock::CodeBlock { lang, code } => {
                assert_eq!(*lang, None);
                assert_eq!(code, "plain");
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn indented_code_block_has_no_lang() {
        // A 4-space indented block is CodeBlockKind::Indented => lang None.
        let b = parse("    indented code line\n");
        let has_indented = b.iter().any(|blk| {
            matches!(blk, MdBlock::CodeBlock { lang, code }
                if lang.is_none() && code.contains("indented code line"))
        });
        assert!(has_indented, "expected an indented code block, got {b:?}");
    }

    #[test]
    fn code_block_preserves_internal_newlines_but_trims_trailing() {
        // Exercises the multi-line buffer + the single trailing-newline pop.
        let b = parse("```\na\nb\nc\n```\n");
        match &b[0] {
            MdBlock::CodeBlock { code, .. } => {
                assert_eq!(code, "a\nb\nc");
                assert!(!code.ends_with('\n'));
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn nested_list_flushes_parent_item_before_children() {
        // The parent item's own text must be emitted BEFORE its nested children
        // (the `flushed` flag path in Start(Tag::List)). Parent appears first.
        let b = parse("- parent text\n    - child a\n    - child b\n");
        let items: Vec<(&u8, &str, String)> = b
            .iter()
            .filter_map(|blk| match blk {
                MdBlock::ListItem {
                    depth,
                    marker,
                    runs,
                } => Some((depth, marker.as_str(), runs_text(runs))),
                _ => None,
            })
            .collect();
        assert_eq!(items.len(), 3, "got {items:?}");
        // Parent (depth 0) emitted first, then the two depth-1 children.
        assert_eq!(*items[0].0, 0);
        assert_eq!(items[0].2, "parent text");
        assert_eq!(*items[1].0, 1);
        assert_eq!(items[1].2, "child a");
        assert_eq!(*items[2].0, 1);
        assert_eq!(items[2].2, "child b");
    }

    #[test]
    fn ordered_list_ordinals_increment_from_custom_start() {
        // pulldown-cmark passes the list start index; markers must reflect it.
        let b = parse("5. five\n6. six\n7. seven\n");
        let markers: Vec<&str> = b
            .iter()
            .filter_map(|blk| match blk {
                MdBlock::ListItem { marker, .. } => Some(marker.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(markers, vec!["5.", "6.", "7."]);
    }

    #[test]
    fn nested_blockquote_paragraph_is_a_quote() {
        // quote_depth > 0 at paragraph-end routes runs to MdBlock::Quote.
        let b = parse("> outer\n>\n> still quoted\n");
        assert!(
            b.iter().any(|blk| matches!(blk, MdBlock::Quote(_))),
            "expected a Quote block, got {b:?}"
        );
        // After the quote closes, a plain paragraph is NOT a quote.
        let b2 = parse("> quoted\n\nplain after\n");
        assert!(matches!(&b2[0], MdBlock::Quote(_)));
        assert!(
            b2.iter().any(|blk| matches!(blk, MdBlock::Paragraph(_))),
            "expected a trailing Paragraph, got {b2:?}"
        );
    }

    #[test]
    fn soft_break_joins_lines_with_space() {
        // A soft line break inside a paragraph becomes a single space run.
        let b = parse("line one\nline two\n");
        match &b[0] {
            MdBlock::Paragraph(runs) => {
                assert_eq!(runs_text(runs), "line one line two");
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn hard_break_inside_code_block_keeps_newline() {
        // Inside a fence, a break is a literal newline, not a space.
        let b = parse("```\nfirst\nsecond\n```\n");
        match &b[0] {
            MdBlock::CodeBlock { code, .. } => assert_eq!(code, "first\nsecond"),
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn trailing_runs_from_truncated_paragraph_are_flushed() {
        // A document that ends mid-paragraph (no blank-line close) still emits
        // the dangling runs via the post-loop flush.
        let b = parse("dangling text with no trailing newline close");
        assert!(
            b.iter().any(|blk| matches!(blk, MdBlock::Paragraph(runs)
                if runs_text(runs).contains("dangling text"))),
            "expected the dangling paragraph to be flushed, got {b:?}"
        );
    }

    // --- SEC-2 (CWE-79): exported HTML must not carry executable content ---

    /// Red-first guard for SEC-2: the "Export as HTML" output is opened in a
    /// browser, so author-supplied raw HTML and dangerous-scheme links must be
    /// stripped/neutralised. Against the OLD unfiltered `push_html`, the export
    /// contained a live `<script>`, an `onerror=` attribute, and a `javascript:`
    /// href — this test asserts NONE of them survive after the fix.
    #[test]
    fn to_html_strips_raw_html_and_dangerous_schemes() {
        let md = "Intro text\n\n\
                  <script>alert(1)</script>\n\n\
                  An image: <img src=x onerror=alert(1)>\n\n\
                  A [click me](javascript:alert(1)) link\n\n\
                  A markdown ![pic](javascript:alert(1)) image\n\n\
                  A safe [home](https://example.com) link\n";
        let html = to_html(md);

        // No live <script> element survives (raw HTML dropped). pulldown-cmark
        // escapes any leftover angle brackets to entities, so the literal tag
        // must not appear.
        assert!(
            !html.contains("<script>"),
            "exported HTML still contains a live <script> tag:\n{html}"
        );
        // No event-handler attribute survives (the raw <img onerror=…> is gone).
        assert!(
            !html.contains("onerror="),
            "exported HTML still contains an onerror= handler:\n{html}"
        );
        // No javascript: URI survives in any href/src (link + image dests were
        // neutralised to '#').
        assert!(
            !html.contains("javascript:"),
            "exported HTML still contains a javascript: URI:\n{html}"
        );

        // The safe content is preserved: the body text and the allowlisted
        // https link must still be present + clickable.
        assert!(html.contains("Intro text"), "lost safe body text:\n{html}");
        assert!(
            html.contains("href=\"https://example.com\""),
            "safe https link was incorrectly stripped:\n{html}"
        );
    }

    #[test]
    fn to_html_renders_lists_and_code() {
        let html = to_html("- a\n- b\n\n```\ncode\n```\n");
        assert!(html.contains("<ul>"));
        assert!(html.contains("<li>a</li>"));
        assert!(html.contains("<pre><code>"));
        // The embedded stylesheet is always present.
        assert!(html.contains("font-family:ui-monospace"));
    }

    /// Render the full block model through the real egui `show` path so the
    /// per-variant widget arms (heading sizes, quote indent, code frame, list
    /// indent, rule, links, bold/italic/code runs) are all executed. Headless
    /// via egui_kittest — no GPU. We assert the rendered AccessKit tree exposes
    /// the heading + link text, proving `show`/`render_runs` ran end to end.
    #[test]
    fn show_renders_every_block_variant_headlessly() {
        use egui_kittest::kittest::Queryable as _;
        let md = "# Big Heading\n\n#### Small Heading\n\n\
                  Para with **bold** *italic* and `code` and [a link](https://e.com)\n\n\
                  > a quote\n\n\
                  ```rust\nfn main() {}\n```\n\n\
                  - bullet one\n    - nested two\n\n\
                  1. ordinal one\n\n\
                  ---\n";
        let accent = Color32::from_rgb(0x00, 0xd0, 0xa0);
        let muted = Color32::from_rgb(0x80, 0x80, 0x80);
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(600.0, 800.0))
            .build_ui(move |ui| show(ui, md, accent, muted));
        h.run();
        // The heading text and the link text reached the accessibility tree.
        assert!(h.query_by_label("Big Heading").is_some());
        assert!(h.query_by_label("Small Heading").is_some());
        assert!(h.query_by_label("a link").is_some());
    }

    #[test]
    fn show_empty_source_renders_nothing_without_panic() {
        let mut h = egui_kittest::Harness::builder()
            .build_ui(|ui| show(ui, "", Color32::WHITE, Color32::GRAY));
        h.run();
    }

    // --- S-05 (CWE-79 / CWE-939): markdown link scheme allowlist ---

    #[test]
    fn link_scheme_rejects_dangerous_schemes() {
        // The whole point of the fix: these must NEVER be made clickable.
        assert!(!is_safe_link_scheme("javascript:alert(1)"));
        assert!(!is_safe_link_scheme(
            "data:text/html,<script>alert(1)</script>"
        ));
        assert!(!is_safe_link_scheme("file:///etc/passwd"));
        assert!(!is_safe_link_scheme("vbscript:msgbox(1)"));
        // UNC-ish / other protocol handlers also rejected.
        assert!(!is_safe_link_scheme("ftp://host/x"));
        assert!(!is_safe_link_scheme("smb://attacker/share"));
    }

    #[test]
    fn link_scheme_allows_safe_schemes_and_relative() {
        assert!(is_safe_link_scheme("http://example.com"));
        assert!(is_safe_link_scheme("https://example.com/x?y=1#z"));
        assert!(is_safe_link_scheme("mailto:user@example.com"));
        // Relative / anchor links carry no scheme → always safe.
        assert!(is_safe_link_scheme("./page.md"));
        assert!(is_safe_link_scheme("../other/page.md"));
        assert!(is_safe_link_scheme("page.md"));
        assert!(is_safe_link_scheme("#section-2"));
        assert!(is_safe_link_scheme("path/page?x=1#y"));
        // A relative path that happens to contain a colon AFTER a slash is
        // still relative (no scheme delimiter before the first '/').
        assert!(is_safe_link_scheme("./a:b"));
    }

    #[test]
    fn link_scheme_is_case_insensitive_and_strips_obfuscation() {
        // Case-insensitive.
        assert!(!is_safe_link_scheme("JavaScript:alert(1)"));
        assert!(!is_safe_link_scheme("JAVASCRIPT:alert(1)"));
        assert!(is_safe_link_scheme("HTTPS://example.com"));
        assert!(is_safe_link_scheme("MailTo:user@example.com"));
        // Leading-whitespace trick.
        assert!(!is_safe_link_scheme("   JavaScript:alert(1)"));
        // Leading control bytes (TAB / NEWLINE / CR / NUL) are stripped before
        // scheme parsing — the classic "java\tscript:" smuggle.
        assert!(!is_safe_link_scheme("\tjavascript:alert(1)"));
        assert!(!is_safe_link_scheme("\n\r javascript:alert(1)"));
        assert!(!is_safe_link_scheme("\u{0000}javascript:alert(1)"));
        // Embedded control inside the scheme also fails closed.
        assert!(!is_safe_link_scheme("java\tscript:alert(1)"));
    }

    #[test]
    fn link_scheme_empty_and_degenerate_inputs() {
        // Empty / scheme-only-colon inputs are treated as relative (safe to
        // render — they cannot open a protocol handler).
        assert!(is_safe_link_scheme(""));
        assert!(is_safe_link_scheme(":"));
        // A leading-digit "scheme" is not a valid RFC-3986 scheme → reject so
        // "123:foo" can never be coerced into a handler.
        assert!(!is_safe_link_scheme("1http://x"));
    }
}
