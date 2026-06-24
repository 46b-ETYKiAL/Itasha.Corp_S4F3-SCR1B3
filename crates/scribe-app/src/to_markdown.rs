//! Convert an arbitrary buffer's text to Markdown **source** text.
//!
//! This module is the inverse of `md_preview.rs`: instead of rendering Markdown
//! to a view, it takes the *current edit-buffer text* — which may be HTML, CSV,
//! JSON, TOML, YAML, source code, or plain text — and produces a Markdown
//! **source** string suitable for saving as a `.md` file.
//!
//! The public entry point is the pure function [`to_markdown`]. It is a total
//! `&str -> String` transform: it **never panics** and **always returns valid
//! Markdown**. When a type-specific converter cannot handle its input (malformed
//! HTML/JSON/TOML, ragged CSV), the function degrades gracefully to a fenced
//! code block rather than failing — so a buffer always converts.
//!
//! ## Dispatch (v1 subset)
//!
//! | Extension            | Converter                                              |
//! |----------------------|--------------------------------------------------------|
//! | `md`, `markdown`     | identity pass-through (returned unchanged)             |
//! | `html`, `htm`        | [`htmd::convert`] (html5ever-backed) → fence fallback  |
//! | `csv`                | in-house RFC-4180-ish scanner → GFM table; ragged → fence |
//! | `json`               | `serde_json` pretty-print → ```` ```json ```` fence    |
//! | `toml`               | `toml` re-serialize pretty → ```` ```toml ```` fence   |
//! | `yaml`, `yml`        | ```` ```yaml ```` fenced block (NO yaml parser — see note) |
//! | anything else / none | fenced code block, language inferred from the extension |
//!
//! ### Why no YAML parser
//!
//! The YAML-parsing ecosystem is currently unsuitable for a `#![forbid(unsafe_code)]`,
//! advisory-clean crate: `serde_yaml` is archived/deprecated and `serde_yml` carries
//! `RUSTSEC-2025-0068` (unsound + unmaintained, FFI to unsafe-libyaml). YAML therefore
//! converts via the universal fenced-block path — lossless, zero-dependency, and safe.
//!
//! ## Fence-collision safety
//!
//! [`code_fence`] auto-sizes the outer fence to `longest_backtick_run + 1` backticks
//! (minimum 3). A buffer that itself contains a ```` ``` ```` run therefore cannot
//! break out of its enclosing fence (CommonMark-legal). This is exercised by the
//! `fence_collision` unit test.
//!
//! This module contains no `unsafe` and is compatible with the crate's
//! `#![forbid(unsafe_code)]` attribute.

/// Convert arbitrary buffer text to Markdown source, selecting a converter by
/// the (lowercased) file extension.
///
/// This is a **pure, total** function: it never panics, and every input — valid
/// or malformed — yields valid Markdown. On any type-specific converter error it
/// degrades to a fenced code block.
///
/// `ext` is the file extension **without** the leading dot (e.g. `Some("html")`,
/// `Some("CSV")`, `None`). Matching is case-insensitive.
///
/// # Examples
///
/// ```
/// # use scribe_app::to_markdown::to_markdown;
/// assert_eq!(to_markdown("# already md", Some("md")), "# already md");
/// assert!(to_markdown("fn main() {}", Some("rs")).starts_with("```rust"));
/// ```
pub fn to_markdown(text: &str, ext: Option<&str>) -> String {
    match ext.map(str::to_ascii_lowercase).as_deref() {
        Some("md") | Some("markdown") => text.to_string(), // identity
        Some("html") | Some("htm") => html_to_md(text),
        Some("csv") => csv_to_md(text),
        Some("json") => json_to_md(text),
        Some("toml") => toml_to_md(text),
        // yaml/yml fall through here intentionally — handled by the fence path,
        // which maps the ext to a `yaml` info-string. Everything else (rs, py,
        // txt, unknown, None) also lands here.
        other => code_fence(text, ext_to_lang(other)),
    }
}

// ---------------------------------------------------------------------------
// HTML
// ---------------------------------------------------------------------------

/// HTML → Markdown via `htmd` (html5ever-backed, best-effort, never panics on
/// malformed input). On the rare `Err`, degrade to an `html` fenced block so a
/// save never fails.
///
/// `htmd::convert` has signature `fn convert(html: &str) -> std::io::Result<String>`
/// (verbatim from the crate's `src/lib.rs`).
fn html_to_md(text: &str) -> String {
    htmd::convert(text).unwrap_or_else(|_| code_fence(text, "html"))
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// JSON → pretty-printed `json` fenced block. Parse failure degrades to the raw
/// text in a `json` fence (never an error).
fn json_to_md(text: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => {
            let pretty = serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_string());
            fenced("json", &pretty)
        }
        Err(_) => fenced("json", text),
    }
}

// ---------------------------------------------------------------------------
// TOML
// ---------------------------------------------------------------------------

/// TOML → re-serialized pretty `toml` fenced block. Parse failure (or a serialize
/// failure on an exotic value) degrades to the raw text in a `toml` fence.
///
/// `text.parse::<toml::Value>()` uses `FromStr`; `toml::to_string_pretty` is the
/// same API already used in the repo (`scribe-core/src/config.rs`,
/// `plugin/pinned_keys.rs`).
fn toml_to_md(text: &str) -> String {
    match text.parse::<toml::Value>() {
        Ok(value) => {
            let pretty = toml::to_string_pretty(&value).unwrap_or_else(|_| text.to_string());
            fenced("toml", &pretty)
        }
        Err(_) => fenced("toml", text),
    }
}

// ---------------------------------------------------------------------------
// CSV
// ---------------------------------------------------------------------------

/// CSV → GitHub-Flavored-Markdown table.
///
/// Uses an in-house RFC-4180-ish scanner (no `csv` crate). Honours:
/// - double-quoted fields,
/// - `""` as an escaped quote inside a quoted field,
/// - embedded `,` and newlines inside quoted fields (the scanner tracks
///   in-quote state across line boundaries).
///
/// The first record is the header. If the records are **ragged** (any row's
/// column count differs from the header's) or the input has no usable rows, it
/// falls back to a `csv` fenced block rather than emitting a malformed table —
/// keeping the conversion honest.
///
/// Cell content is escaped for table safety: `|` → `\|`, and any embedded
/// newline → `<br>`.
fn csv_to_md(text: &str) -> String {
    let records = match parse_csv(text) {
        Some(records) => records,
        // Unterminated quote → malformed CSV. Preserve the bytes verbatim in a
        // fenced block rather than fabricate a table from a half-parsed record.
        None => return fenced("csv", text),
    };

    // Drop fully-empty trailing record produced by a final trailing newline.
    let records: Vec<Vec<String>> = records
        .into_iter()
        .filter(|r| !(r.len() == 1 && r[0].is_empty()))
        .collect();

    if records.is_empty() {
        return fenced("csv", text);
    }

    let cols = records[0].len();
    if cols == 0 {
        return fenced("csv", text);
    }

    // Ragged → honest fallback.
    if records.iter().any(|r| r.len() != cols) {
        return fenced("csv", text);
    }

    let mut out = String::new();

    // Header row.
    out.push_str(&render_row(&records[0]));
    out.push('\n');

    // Separator row.
    out.push('|');
    for _ in 0..cols {
        out.push_str(" --- |");
    }
    out.push('\n');

    // Data rows.
    for record in &records[1..] {
        out.push_str(&render_row(record));
        out.push('\n');
    }

    out
}

/// Render one CSV record as a GFM table row: `| a | b | c |`.
fn render_row(record: &[String]) -> String {
    let mut row = String::from("|");
    for cell in record {
        row.push(' ');
        row.push_str(&escape_cell(cell));
        row.push_str(" |");
    }
    row
}

/// Escape a cell for a GFM table: `|` → `\|`, newlines → `<br>`.
fn escape_cell(cell: &str) -> String {
    cell.replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\n', '\r'], "<br>")
}

/// Minimal RFC-4180-ish CSV parser.
///
/// Returns `Some(records)` — each record a `Vec` of field strings — for
/// well-formed input. A field may be quoted with `"`; inside a quoted field,
/// `""` is a literal quote and `,` / newline are literal content. Outside
/// quotes, `,` separates fields and `\n` / `\r\n` separates records.
///
/// Returns `None` when the input ends inside an unterminated quoted field
/// (malformed per RFC 4180); the caller falls back to a lossless fenced block.
fn parse_csv(text: &str) -> Option<Vec<Vec<String>>> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' => {
                    // Lookahead for an escaped quote ("").
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                other => field.push(other),
            }
        } else {
            match c {
                '"' => in_quotes = true,
                ',' => {
                    record.push(std::mem::take(&mut field));
                }
                '\r' => {
                    // Swallow a following '\n' for CRLF; either way, end record.
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                }
                '\n' => {
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                }
                other => field.push(other),
            }
        }
    }

    // A quote that never closed → malformed CSV. Signal the caller to fall
    // back to a lossless fenced block rather than fabricate a table from a
    // half-parsed record.
    if in_quotes {
        return None;
    }

    // Flush the final field/record if the input did not end with a newline.
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }

    Some(records)
}

// ---------------------------------------------------------------------------
// Fenced code blocks (the universal fallback)
// ---------------------------------------------------------------------------

/// Wrap `text` in a fenced code block tagged with `lang`.
///
/// The outer fence is auto-sized to `longest_backtick_run_in_text + 1` backticks
/// (minimum 3) so the body can never break out, even if it contains its own
/// ```` ``` ```` runs. An empty `lang` produces a bare (no-language) fence.
///
/// This is also the universal fallback every other converter degrades to on error.
pub fn code_fence(text: &str, lang: &str) -> String {
    fenced(lang, text)
}

/// Build a fence with an auto-sized backtick run.
fn fenced(lang: &str, body: &str) -> String {
    let fence_len = (longest_backtick_run(body) + 1).max(3);
    let fence = "`".repeat(fence_len);
    let trailing_nl = if body.ends_with('\n') { "" } else { "\n" };
    format!("{fence}{lang}\n{body}{trailing_nl}{fence}\n")
}

/// Length of the longest run of consecutive backticks in `s`.
fn longest_backtick_run(s: &str) -> usize {
    let mut longest = 0usize;
    let mut current = 0usize;
    for c in s.chars() {
        if c == '`' {
            current += 1;
            if current > longest {
                longest = current;
            }
        } else {
            current = 0;
        }
    }
    longest
}

// ---------------------------------------------------------------------------
// Extension → fence-language mapping
// ---------------------------------------------------------------------------

/// Map a (lowercased) file extension to a Markdown fence info-string.
///
/// Returns `""` for unknown extensions / `None` (a bare fence, no language).
fn ext_to_lang(ext: Option<&str>) -> &'static str {
    match ext.map(str::to_ascii_lowercase).as_deref() {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("js") | Some("mjs") | Some("cjs") => "javascript",
        Some("ts") => "typescript",
        Some("tsx") => "tsx",
        Some("jsx") => "jsx",
        Some("toml") => "toml",
        Some("json") => "json",
        Some("yaml") | Some("yml") => "yaml",
        Some("sh") | Some("bash") => "bash",
        Some("ps1") => "powershell",
        Some("c") | Some("h") => "c",
        Some("cpp") | Some("cc") | Some("cxx") | Some("hpp") => "cpp",
        Some("cs") => "csharp",
        Some("go") => "go",
        Some("rb") => "ruby",
        Some("java") => "java",
        Some("kt") | Some("kts") => "kotlin",
        Some("swift") => "swift",
        Some("php") => "php",
        Some("sql") => "sql",
        Some("xml") => "xml",
        Some("css") => "css",
        Some("scss") => "scss",
        Some("lua") => "lua",
        Some("dart") => "dart",
        Some("scala") => "scala",
        Some("hs") => "haskell",
        Some("ml") => "ocaml",
        Some("ex") | Some("exs") => "elixir",
        Some("erl") => "erlang",
        Some("clj") => "clojure",
        Some("zig") => "zig",
        Some("nim") => "nim",
        Some("vim") => "vim",
        Some("dockerfile") => "dockerfile",
        Some("ini") => "ini",
        Some("diff") | Some("patch") => "diff",
        // txt, log, unknown, None → bare fence.
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_basic() {
        let md = to_markdown("<h1>Hi</h1><p>x</p>", Some("html"));
        assert!(md.contains("# Hi"), "expected '# Hi' in:\n{md}");
        assert!(md.contains('x'), "expected paragraph text in:\n{md}");
    }

    #[test]
    fn html_table() {
        let html = "<table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>";
        let md = to_markdown(html, Some("html"));
        // htmd emits a GFM table — assert the pipe-delimited shape appears.
        assert!(md.contains('|'), "expected a GFM table pipe in:\n{md}");
        assert!(
            md.contains('a') && md.contains('b'),
            "expected headers in:\n{md}"
        );
    }

    #[test]
    fn html_malformed_no_panic() {
        // html5ever is a real parser and recovers from unclosed tags; the call
        // must not panic and must return non-empty Markdown.
        let md = to_markdown("<h1>unclosed", Some("html"));
        assert!(!md.is_empty(), "malformed HTML should still convert");
    }

    #[test]
    fn csv_table() {
        let md = to_markdown("a,b\n1,2\n", Some("csv"));
        assert!(md.contains("| a | b |"), "header row missing in:\n{md}");
        assert!(
            md.contains("| --- | --- |"),
            "separator row missing in:\n{md}"
        );
        assert!(md.contains("| 1 | 2 |"), "data row missing in:\n{md}");
    }

    #[test]
    fn csv_quoted_comma() {
        // The quoted "a,b" must remain ONE cell, not split on the inner comma.
        let md = to_markdown("\"a,b\",c\n1,2\n", Some("csv"));
        assert!(
            md.contains("| a,b | c |"),
            "quoted comma not preserved in:\n{md}"
        );
        // 2 columns ⇒ the separator must have exactly two --- groups.
        assert!(
            md.contains("| --- | --- |"),
            "expected 2-col separator in:\n{md}"
        );
    }

    #[test]
    fn csv_ragged_fallback() {
        // Row 2 has 3 columns vs header's 2 ⇒ fall back to a fenced ```csv block.
        let md = to_markdown("a,b\n1,2,3\n", Some("csv"));
        assert!(
            md.starts_with("```csv"),
            "ragged CSV should fence, got:\n{md}"
        );
        assert!(
            !md.contains("| --- |"),
            "ragged CSV must not emit a table:\n{md}"
        );
    }

    #[test]
    fn json_pretty() {
        let md = to_markdown("{\"a\":1,\"b\":[2,3]}", Some("json"));
        assert!(md.starts_with("```json"), "expected json fence, got:\n{md}");
        // Pretty-printing inserts newlines/indentation.
        assert!(
            md.contains("\"a\": 1"),
            "expected pretty-printed key in:\n{md}"
        );
        assert!(md.contains('\n'), "pretty JSON should be multi-line:\n{md}");
    }

    #[test]
    fn json_malformed_raw() {
        // Invalid JSON must NOT error — it lands in a json fence as raw text.
        let raw = "{not valid json,,,}";
        let md = to_markdown(raw, Some("json"));
        assert!(md.starts_with("```json"), "expected json fence, got:\n{md}");
        assert!(
            md.contains(raw),
            "raw malformed text should be preserved:\n{md}"
        );
    }

    #[test]
    fn toml_pretty() {
        let md = to_markdown("b = 2\na = 1\n", Some("toml"));
        assert!(md.starts_with("```toml"), "expected toml fence, got:\n{md}");
        assert!(md.contains("a = 1"), "expected key a in:\n{md}");
        assert!(md.contains("b = 2"), "expected key b in:\n{md}");
    }

    #[test]
    fn toml_malformed_raw() {
        let raw = "this = = = not toml";
        let md = to_markdown(raw, Some("toml"));
        assert!(md.starts_with("```toml"), "expected toml fence, got:\n{md}");
        assert!(
            md.contains(raw),
            "raw malformed toml should be preserved:\n{md}"
        );
    }

    #[test]
    fn code_fence_rust() {
        let md = to_markdown("fn main() {}", Some("rs"));
        assert!(md.starts_with("```rust"), "expected rust fence, got:\n{md}");
        assert!(md.contains("fn main() {}"), "body missing in:\n{md}");
        assert!(md.trim_end().ends_with("```"), "fence not closed in:\n{md}");
    }

    #[test]
    fn fence_collision() {
        // A buffer containing a 3-backtick run must be wrapped in a >=4-backtick
        // fence so the inner run cannot break out.
        let body = "before\n```\ninner fenced\n```\nafter";
        let md = to_markdown(body, Some("rs"));
        assert!(
            md.starts_with("````rust"),
            "expected 4-backtick fence, got:\n{md}"
        );
        assert!(
            md.contains("```\ninner fenced"),
            "inner content lost in:\n{md}"
        );
        // Closing fence is also 4 backticks.
        assert!(
            md.trim_end().ends_with("````"),
            "closing fence wrong in:\n{md}"
        );
    }

    #[test]
    fn fence_collision_escalates() {
        // A 4-backtick run in the body forces a 5-backtick outer fence.
        let body = "````\nquad\n````";
        let out = code_fence(body, "");
        assert!(
            out.starts_with("`````"),
            "expected 5-backtick fence, got:\n{out}"
        );
    }

    #[test]
    fn unknown_ext_plain() {
        let plain = to_markdown("hello world", Some("xyz"));
        assert!(
            plain.starts_with("```\n"),
            "unknown ext should be bare fence:\n{plain}"
        );
        assert!(plain.contains("hello world"), "body missing in:\n{plain}");

        let none = to_markdown("hello world", None);
        assert!(
            none.starts_with("```\n"),
            "None ext should be bare fence:\n{none}"
        );
    }

    #[test]
    fn markdown_identity() {
        let src = "# Title\n\nSome **bold** text with `code`.\n";
        assert_eq!(to_markdown(src, Some("md")), src);
        assert_eq!(to_markdown(src, Some("MARKDOWN")), src);
    }

    #[test]
    fn yaml_fenced() {
        // YAML has no parser — it converts via the universal fenced-block path,
        // tagged `yaml`. Lossless and safe.
        let src = "key: value\nlist:\n  - a\n  - b\n";
        let md = to_markdown(src, Some("yaml"));
        assert!(md.starts_with("```yaml"), "expected yaml fence, got:\n{md}");
        assert!(md.contains("key: value"), "yaml body missing in:\n{md}");

        let md2 = to_markdown(src, Some("yml"));
        assert!(
            md2.starts_with("```yaml"),
            ".yml should also use yaml fence:\n{md2}"
        );
    }

    #[test]
    fn ext_case_insensitive() {
        // Dispatch lowercases the extension.
        assert!(to_markdown("fn x() {}", Some("RS")).starts_with("```rust"));
        assert!(to_markdown("a,b\n1,2\n", Some("CSV")).contains("| a | b |"));
    }

    #[test]
    fn csv_embedded_newline_in_quotes() {
        // A quoted field with an embedded newline stays one cell; the newline is
        // rendered as <br> in the table.
        let md = to_markdown("\"line1\nline2\",b\nx,y\n", Some("csv"));
        assert!(
            md.contains("line1<br>line2"),
            "embedded newline not escaped:\n{md}"
        );
    }

    #[test]
    fn csv_pipe_escaped() {
        let md = to_markdown("a|b,c\n1,2\n", Some("csv"));
        assert!(md.contains("a\\|b"), "pipe not escaped in cell:\n{md}");
    }

    #[test]
    fn csv_unterminated_quote_falls_back_to_fence() {
        // A quote that never closes is malformed CSV (RFC 4180). Rather than
        // fabricate a table from a half-parsed record, the converter must fall
        // back to a lossless fenced block that preserves the bytes verbatim.
        for src in ["\"unterminated", "a,b\nx,\"y", "\"x\ny"] {
            let md = to_markdown(src, Some("csv"));
            assert!(
                md.starts_with("```csv") || md.starts_with("````"),
                "unterminated quote should fence, got:\n{md}"
            );
            assert!(
                md.contains(src),
                "fence must preserve the original bytes:\n{md}"
            );
            assert!(
                !md.contains("| --- |"),
                "malformed CSV must not produce a table:\n{md}"
            );
        }
    }

    #[test]
    fn ext_to_lang_maps_every_known_extension() {
        // Exercise every arm of the ext→lang table so a future edit that breaks a
        // mapping is caught. Each entry must produce a fence tagged with the lang.
        let cases: &[(&str, &str)] = &[
            ("rs", "rust"),
            ("py", "python"),
            ("js", "javascript"),
            ("mjs", "javascript"),
            ("cjs", "javascript"),
            ("ts", "typescript"),
            ("tsx", "tsx"),
            ("jsx", "jsx"),
            ("sh", "bash"),
            ("bash", "bash"),
            ("ps1", "powershell"),
            ("c", "c"),
            ("h", "c"),
            ("cpp", "cpp"),
            ("cc", "cpp"),
            ("cxx", "cpp"),
            ("hpp", "cpp"),
            ("cs", "csharp"),
            ("go", "go"),
            ("rb", "ruby"),
            ("java", "java"),
            ("kt", "kotlin"),
            ("kts", "kotlin"),
            ("swift", "swift"),
            ("php", "php"),
            ("sql", "sql"),
            ("xml", "xml"),
            ("css", "css"),
            ("scss", "scss"),
            ("lua", "lua"),
            ("dart", "dart"),
            ("scala", "scala"),
            ("hs", "haskell"),
            ("ml", "ocaml"),
            ("ex", "elixir"),
            ("exs", "elixir"),
            ("erl", "erlang"),
            ("clj", "clojure"),
            ("zig", "zig"),
            ("nim", "nim"),
            ("vim", "vim"),
            ("dockerfile", "dockerfile"),
            ("ini", "ini"),
            ("diff", "diff"),
            ("patch", "diff"),
        ];
        for (ext, lang) in cases {
            // These exts are NOT special-cased in to_markdown, so they all take
            // the code_fence(ext_to_lang) path and emit a `lang`-tagged fence.
            let md = to_markdown("body line\n", Some(ext));
            assert!(
                md.starts_with(&format!("```{lang}\n")),
                ".{ext} must fence as `{lang}`, got:\n{md}"
            );
        }
    }

    #[test]
    fn json_malformed_degrades_to_raw_json_fence() {
        // Invalid JSON must not error — it degrades to the raw text in a json
        // fence (the converter is honest and lossless).
        let md = to_markdown("{not: valid, json", Some("json"));
        assert!(md.starts_with("```json\n"), "got:\n{md}");
        assert!(
            md.contains("{not: valid, json"),
            "raw bytes preserved:\n{md}"
        );
    }

    #[test]
    fn csv_empty_input_falls_back_to_fence_not_a_table() {
        // Empty / whitespace-only CSV has no usable rows → fenced block, never an
        // empty table.
        for src in ["", "\n", "\n\n"] {
            let md = to_markdown(src, Some("csv"));
            assert!(md.starts_with("```csv"), "empty CSV must fence, got:\n{md}");
            assert!(!md.contains("| --- |"), "no table for empty CSV:\n{md}");
        }
    }

    #[test]
    fn csv_escaped_double_quote_becomes_a_literal_quote_in_cell() {
        // Inside a quoted field, `""` is the RFC-4180 escape for a single literal
        // `"`. The cell `"say ""hi"""` must render as `say "hi"`, NOT split or
        // lose the quotes — exercising the `""` lookahead in the scanner.
        let md = to_markdown("\"say \"\"hi\"\"\",b\n1,2\n", Some("csv"));
        assert!(
            md.contains("| say \"hi\" | b |"),
            "escaped double-quote not unescaped to a literal quote:\n{md}"
        );
        // Still a 2-column table (the escaped quotes did not break field parsing).
        assert!(md.contains("| --- | --- |"), "expected 2-col table:\n{md}");
    }

    #[test]
    fn csv_crlf_line_endings_split_records() {
        // CRLF (\r\n) record terminators must split rows exactly like LF — the
        // scanner swallows the \n after a \r. A Windows-authored CSV must produce
        // the same table a Unix one does.
        let md = to_markdown("a,b\r\n1,2\r\n3,4\r\n", Some("csv"));
        assert!(md.contains("| a | b |"), "header row missing in:\n{md}");
        assert!(md.contains("| 1 | 2 |"), "first data row missing in:\n{md}");
        assert!(
            md.contains("| 3 | 4 |"),
            "second data row missing in:\n{md}"
        );
        // Exactly two columns — a stray \r must NOT leak into a cell as content.
        assert!(md.contains("| --- | --- |"), "expected 2-col table:\n{md}");
        assert!(
            !md.contains('\r'),
            "carriage return leaked into output:\n{md:?}"
        );
    }

    #[test]
    fn csv_without_trailing_newline_flushes_the_final_record() {
        // Input that does NOT end with a newline still flushes its final
        // field+record (the `!field.is_empty() || !record.is_empty()` flush
        // branch) — the last row must not be dropped.
        let md = to_markdown("a,b\n1,2", Some("csv"));
        assert!(md.contains("| a | b |"), "header missing in:\n{md}");
        assert!(
            md.contains("| 1 | 2 |"),
            "final no-newline record was dropped:\n{md}"
        );
    }
}
