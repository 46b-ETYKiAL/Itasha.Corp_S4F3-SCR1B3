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

/// Sort the lines of `text` lexicographically (stable), preserving a trailing
/// newline if the input had one. Empty input is returned unchanged.
pub fn sort_lines(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let had_final_nl = text.ends_with('\n');
    let mut lines: Vec<&str> = text.lines().collect();
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
    fn to_case_upper_lower() {
        assert_eq!(to_case("aB c", true), "AB C");
        assert_eq!(to_case("aB c", false), "ab c");
    }
}
