//! #107 — Enter auto-indent. The new line copies the current line's leading
//! whitespace so indentation persists (which is what makes the indentation
//! settings visibly do something line-to-line).
use super::newline_with_indent;

#[test]
fn carries_space_indent() {
    let (t, c) = newline_with_indent("    code", 8);
    assert_eq!(t, "    code\n    ");
    assert_eq!(c, 13); // \n + 4 spaces
}

#[test]
fn no_indent_just_breaks_the_line() {
    let (t, c) = newline_with_indent("code", 4);
    assert_eq!(t, "code\n");
    assert_eq!(c, 5);
}

#[test]
fn carries_tab_indent() {
    let (t, c) = newline_with_indent("\tfoo", 4);
    assert_eq!(t, "\tfoo\n\t");
    assert_eq!(c, 6);
}

#[test]
fn indent_is_clamped_to_the_cursor() {
    // Cursor after "ab" on "  abcd" → carry only the line's leading "  ".
    let (t, c) = newline_with_indent("  abcd", 4);
    assert_eq!(t, "  ab\n  cd");
    assert_eq!(c, 7);
}
