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
pub fn to_html(md: &str) -> String {
    let parser = Parser::new(md);
    let mut body = String::new();
    pulldown_cmark::html::push_html(&mut body, parser);
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
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

/// Emit a sequence of styled inline runs into the current (typically
/// `horizontal_wrapped`) layout.
fn render_runs(ui: &mut egui::Ui, runs: &[MdRun], accent: Color32, muted: Color32) {
    for r in runs {
        if let Some(url) = &r.link {
            // egui handles its own link styling/underline; only colour it.
            ui.hyperlink_to(RichText::new(&r.text).color(accent), url);
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
}
