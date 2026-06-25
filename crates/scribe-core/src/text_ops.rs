//! Pure, allocation-light text transforms used by save-time hygiene and the
//! line-operation commands. Every function is a total `&str -> String` (or
//! in-place) transform so it is trivially unit-testable.

/// Remove trailing spaces and tabs from every line, preserving the line's
/// newline. The final line keeps its content; only its trailing blanks go.
pub fn trim_trailing_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (body, nl) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        // Trim only spaces/tabs (not a trailing `\r`) to stay EOL-agnostic.
        out.push_str(body.trim_end_matches([' ', '\t']));
        out.push_str(nl);
    }
    out
}

/// Ensure the text ends with exactly one trailing newline (no-op on empty
/// text). Does not collapse multiple existing trailing blank lines.
pub fn ensure_final_newline(text: &str) -> String {
    if text.is_empty() || text.ends_with('\n') {
        return text.to_string();
    }
    let mut s = text.to_string();
    s.push('\n');
    s
}

/// Split `text` into lines for sorting WITHOUT mutating any line's bytes.
///
/// CORR-03: `str::lines()` strips a `\r` from a `\r\n` pair, so sorting a
/// non-LF-normalized buffer would silently DROP carriage returns (a content
/// mutation under a "sort" operation). Splitting on `'\n'` instead preserves a
/// trailing `\r` verbatim inside each line. A trailing `'\n'` produces a final
/// empty segment which the caller re-appends as the preserved newline rather
/// than treating as a sortable line.
fn split_lines_preserving(text: &str) -> (Vec<&str>, bool) {
    let had_final_nl = text.ends_with('\n');
    let mut lines: Vec<&str> = text.split('\n').collect();
    if had_final_nl {
        // The trailing `\n` yields a final empty element; drop it so it is not
        // sorted as a blank line, then re-append the `\n` after the join.
        lines.pop();
    }
    (lines, had_final_nl)
}

/// Sort the lines of `text` lexicographically (stable), preserving a trailing
/// newline if the input had one. Empty input is returned unchanged.
///
/// CORR-03: line splitting preserves each line's bytes exactly (including a
/// `\r` inside a `\r\n`), so the sort never mutates content — only reorders.
pub fn sort_lines(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let (mut lines, had_final_nl) = split_lines_preserving(text);
    lines.sort();
    let mut out = lines.join("\n");
    if had_final_nl {
        out.push('\n');
    }
    out
}

/// Convert `text` to upper- or (when `upper` is false) lowercase.
pub fn to_case(text: &str, upper: bool) -> String {
    if upper {
        text.to_uppercase()
    } else {
        text.to_lowercase()
    }
}

/// Sort lines lexicographically (stable) AND drop exact duplicate lines,
/// keeping one of each. Preserves a trailing newline if present.
pub fn sort_lines_unique(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    // CORR-03: same byte-preserving split as `sort_lines` (no `\r` eating).
    let (mut lines, had_final_nl) = split_lines_preserving(text);
    lines.sort();
    lines.dedup(); // adjacent dups after sort == all dups
    let mut out = lines.join("\n");
    if had_final_nl {
        out.push('\n');
    }
    out
}

/// Apply `f` to the leading-whitespace run of every line, leaving the rest of
/// each line untouched. Leading whitespace is ASCII space/tab, so the byte
/// split is always on a char boundary. Preserves a trailing newline.
fn convert_indent(text: &str, f: impl Fn(&str) -> String) -> String {
    let had_final_nl = text.ends_with('\n');
    let mapped: Vec<String> = text
        .lines()
        .map(|line| {
            let lead_len = line.len() - line.trim_start_matches([' ', '\t']).len();
            let (lead, rest) = line.split_at(lead_len);
            let mut s = f(lead);
            s.push_str(rest);
            s
        })
        .collect();
    let mut out = mapped.join("\n");
    if had_final_nl {
        out.push('\n');
    }
    out
}

/// Convert leading-indentation tabs to `width` spaces each (indentation only;
/// tabs elsewhere in the line are left alone). `width` is clamped to >= 1.
pub fn tabs_to_spaces(text: &str, width: usize) -> String {
    let spaces = " ".repeat(width.max(1));
    convert_indent(text, |lead| lead.replace('\t', &spaces))
}

/// Convert each run of `width` leading spaces to a tab (indentation only).
/// Leftover spaces that do not fill a full `width`-group are kept as spaces;
/// existing leading tabs are preserved. `width` is clamped to >= 1.
pub fn spaces_to_tabs(text: &str, width: usize) -> String {
    let w = width.max(1);
    convert_indent(text, |lead| {
        let mut out = String::with_capacity(lead.len());
        let mut run = 0usize;
        for ch in lead.chars() {
            if ch == ' ' {
                run += 1;
                if run == w {
                    out.push('\t');
                    run = 0;
                }
            } else {
                // A tab in the leading run: flush pending partial spaces, keep it.
                for _ in 0..run {
                    out.push(' ');
                }
                run = 0;
                out.push('\t');
            }
        }
        for _ in 0..run {
            out.push(' ');
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_removes_trailing_blanks_per_line() {
        let src = "a   \nb\t\t\n  c  \nd";
        assert_eq!(trim_trailing_whitespace(src), "a\nb\n  c\nd");
    }

    #[test]
    fn trim_preserves_leading_indent() {
        assert_eq!(trim_trailing_whitespace("    x   "), "    x");
    }

    #[test]
    fn trim_keeps_blank_lines_as_empty() {
        assert_eq!(trim_trailing_whitespace("a\n   \nb\n"), "a\n\nb\n");
    }

    #[test]
    fn final_newline_added_when_missing() {
        assert_eq!(ensure_final_newline("abc"), "abc\n");
        assert_eq!(ensure_final_newline("abc\n"), "abc\n");
        assert_eq!(ensure_final_newline(""), "");
    }

    #[test]
    fn sort_lines_orders_and_keeps_trailing_nl() {
        assert_eq!(sort_lines("b\na\nc\n"), "a\nb\nc\n");
        assert_eq!(sort_lines("b\na"), "a\nb");
        assert_eq!(sort_lines(""), "");
    }

    #[test]
    fn sort_lines_preserves_embedded_carriage_returns() {
        // CORR-03: `str::lines()` would strip the `\r` from each `\r\n`; the
        // byte-preserving split must keep them. Sorting a CRLF buffer must
        // reorder lines WITHOUT mutating any line's bytes.
        let src = "b\r\na\r\nc\r\n";
        let out = sort_lines(src);
        assert_eq!(out, "a\r\nb\r\nc\r\n", "carriage returns survive the sort");
        // No data mutation: the multiset of bytes is identical (same count of
        // `\r`, `\n`, and letters), only the line order changed.
        assert_eq!(
            out.matches('\r').count(),
            src.matches('\r').count(),
            "no `\\r` dropped"
        );
        // Without a trailing newline, the last line's `\r` is also preserved.
        assert_eq!(sort_lines("b\r\na\r"), "a\r\nb\r");
    }

    #[test]
    fn sort_unique_preserves_embedded_carriage_returns() {
        // CORR-03 (dedup variant): `\r`-bearing lines are compared and emitted
        // byte-exact; an exact CRLF duplicate collapses, distinct ones survive.
        let src = "b\r\na\r\nb\r\nc\r\n";
        let out = sort_lines_unique(src);
        assert_eq!(out, "a\r\nb\r\nc\r\n");
        assert_eq!(out.matches('\r').count(), 3, "one `\\r` per surviving line");
    }

    #[test]
    fn to_case_upper_lower() {
        assert_eq!(to_case("aB c", true), "AB C");
        assert_eq!(to_case("aB c", false), "ab c");
    }

    #[test]
    fn sort_unique_dedups_and_keeps_trailing_newline() {
        assert_eq!(sort_lines_unique("b\na\nb\nc\na\n"), "a\nb\nc\n");
        assert_eq!(sort_lines_unique("b\na"), "a\nb"); // no trailing nl preserved
        assert_eq!(sort_lines_unique(""), "");
        // All-identical collapses to one line.
        assert_eq!(sort_lines_unique("x\nx\nx"), "x");
    }

    #[test]
    fn tabs_to_spaces_only_touches_leading_indent() {
        // Leading tab -> width spaces; a tab AFTER content is untouched.
        assert_eq!(tabs_to_spaces("\tx\ty\n", 4), "    x\ty\n");
        assert_eq!(tabs_to_spaces("\t\tz", 2), "    z");
        // No indent -> unchanged.
        assert_eq!(tabs_to_spaces("plain\n", 4), "plain\n");
        // width clamped to >= 1.
        assert_eq!(tabs_to_spaces("\tx", 0), " x");
    }

    #[test]
    fn spaces_to_tabs_groups_leading_runs() {
        assert_eq!(spaces_to_tabs("    x\n", 4), "\tx\n");
        // Partial trailing group stays as spaces.
        assert_eq!(spaces_to_tabs("      y", 4), "\t  y"); // 4->tab, 2 leftover
                                                           // Spaces after content untouched.
        assert_eq!(spaces_to_tabs("    a  b", 4), "\ta  b");
        // Mixed existing tab in the indent is preserved.
        assert_eq!(spaces_to_tabs("\t    z", 4), "\t\tz");
    }

    #[test]
    fn indent_conversions_round_trip_for_clean_indent() {
        let src = "\tline\n\t\tnested\n";
        let spaced = tabs_to_spaces(src, 4);
        assert_eq!(spaced, "    line\n        nested\n");
        assert_eq!(spaces_to_tabs(&spaced, 4), src);
    }
}
