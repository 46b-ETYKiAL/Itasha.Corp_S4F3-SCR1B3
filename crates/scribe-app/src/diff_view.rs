//! Line-level diff between two texts (the editor buffer vs the file on disk,
//! or any old/new pair) rendered as a unified diff inside an egui pane.
//!
//! The diff is computed with the [`similar`](https://crates.io/crates/similar)
//! crate's text differ (Myers / Patience under the hood). Everything here is
//! pure Rust, runs entirely on-device, and performs no I/O or network access.
//!
//! Layering:
//!   * [`diff_lines`] is a pure function over `&str` -> `Vec<DiffLine>`. It is
//!     fully unit-testable without egui and carries no rendering concerns.
//!   * [`summary`] derives `(insertions, deletions)` counts from a diff for a
//!     status-line / header segment.
//!   * [`show`] renders a precomputed-or-on-the-fly diff into an [`egui::Ui`].
//!     The renderer is intentionally decoupled from the application's theme:
//!     it accepts [`egui::Color32`] arguments so the caller threads its own
//!     palette (added / removed / context colours) without this module
//!     depending on any app-side type.
//!
//! Cargo dependency (crates/scribe-app/Cargo.toml):
//! ```toml
//! similar = { version = "2", default-features = false, features = ["text"] }
//! ```
//! `default-features = false` + `features = ["text"]` pulls only the line/word
//! text differ and drops the optional `bytes` / `serde` / `inline` / `unicode`
//! extras this view does not need. License: Apache-2.0.
//!
//! This module is `#![forbid(unsafe_code)]`-compatible: it contains no `unsafe`.

use similar::{ChangeTag, TextDiff};

/// The role a single line plays in a unified diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// Line is present and unchanged in both `old` and `new` (context).
    Equal,
    /// Line is present only in `new` (an addition, rendered with `+`).
    Insert,
    /// Line is present only in `old` (a removal, rendered with `-`).
    Delete,
}

/// One row of a unified diff: its kind, the (1-based) source line numbers in
/// each side where applicable, and the line text with any trailing newline
/// stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    /// Whether this line was added, removed, or is unchanged context.
    pub kind: DiffKind,
    /// 1-based line number in `old`. `None` for an [`DiffKind::Insert`].
    pub old_line: Option<usize>,
    /// 1-based line number in `new`. `None` for a [`DiffKind::Delete`].
    pub new_line: Option<usize>,
    /// The line text, with a single trailing `\n` (if any) removed.
    pub text: String,
}

/// Compute a unified line-level diff of `old` vs `new`.
///
/// This is the pure, egui-free core. It builds a [`similar::TextDiff`] over the
/// two inputs split into lines, then walks every change via
/// [`TextDiff::iter_all_changes`], mapping each [`similar::ChangeTag`] to a
/// [`DiffKind`] and tracking running 1-based line numbers for each side.
///
/// The returned vector is in unified-diff order: deletions appear immediately
/// before the insertions that replaced them, with surrounding equal lines as
/// context.
///
/// # Examples
///
/// ```ignore
/// let rows = diff_lines("a\nb\nc\n", "a\nc\nd\n");
/// // "b" deleted, "d" inserted, "a"/"c" are context.
/// ```
pub fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(old, new);
    let mut rows = Vec::new();
    // Running 1-based line cursors for each side.
    let (mut o, mut n) = (0usize, 0usize);

    for change in diff.iter_all_changes() {
        // `change.value()` yields the line text *including* its trailing
        // newline (TextDiff::from_lines is newline-terminated); strip exactly
        // one trailing '\n' for display.
        let text = strip_one_trailing_newline(change.value());

        let row = match change.tag() {
            ChangeTag::Equal => {
                o += 1;
                n += 1;
                DiffLine {
                    kind: DiffKind::Equal,
                    old_line: Some(o),
                    new_line: Some(n),
                    text,
                }
            }
            ChangeTag::Delete => {
                o += 1;
                DiffLine {
                    kind: DiffKind::Delete,
                    old_line: Some(o),
                    new_line: None,
                    text,
                }
            }
            ChangeTag::Insert => {
                n += 1;
                DiffLine {
                    kind: DiffKind::Insert,
                    old_line: None,
                    new_line: Some(n),
                    text,
                }
            }
        };
        rows.push(row);
    }

    rows
}

/// Strip a single trailing `\n` (and an accompanying `\r` for CRLF) from a line
/// produced by the line differ, leaving the visible text.
fn strip_one_trailing_newline(s: &str) -> String {
    let trimmed = s.strip_suffix('\n').unwrap_or(s);
    let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
    trimmed.to_string()
}

/// Count `(insertions, deletions)` across a diff for a header / status segment.
pub fn summary(rows: &[DiffLine]) -> (usize, usize) {
    let insertions = rows.iter().filter(|r| r.kind == DiffKind::Insert).count();
    let deletions = rows.iter().filter(|r| r.kind == DiffKind::Delete).count();
    (insertions, deletions)
}

/// Theme colours threaded in by the caller so this module stays decoupled from
/// the application's palette. All three are plain [`egui::Color32`] values.
#[derive(Debug, Clone, Copy)]
pub struct DiffColors {
    /// Colour for inserted (`+`) lines — typically a green.
    pub insert: egui::Color32,
    /// Colour for deleted (`-`) lines — typically a red.
    pub delete: egui::Color32,
    /// Colour for unchanged context lines — typically a muted foreground.
    pub context: egui::Color32,
}

/// Render a precomputed slice of [`DiffLine`]s into `ui`.
///
/// Use this when the diff was already computed (e.g. to also display a summary
/// header) to avoid diffing twice.
pub fn show_rows(ui: &mut egui::Ui, rows: &[DiffLine], colors: DiffColors) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for row in rows {
                let (sigil, color) = match row.kind {
                    DiffKind::Insert => ('+', colors.insert),
                    DiffKind::Delete => ('-', colors.delete),
                    DiffKind::Equal => (' ', colors.context),
                };
                ui.label(
                    egui::RichText::new(format!("{sigil} {}", row.text))
                        .monospace()
                        .color(color),
                );
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_insert_appends_new_lines() {
        // Going from one line to three: two pure insertions, one context.
        let rows = diff_lines("a\n", "a\nb\nc\n");
        let (ins, del) = summary(&rows);
        assert_eq!(ins, 2, "expected two inserted lines");
        assert_eq!(del, 0, "no deletions expected");

        // The original "a" survives as an Equal/context row.
        assert!(rows
            .iter()
            .any(|r| r.kind == DiffKind::Equal && r.text == "a"));
        // Both new lines are present as inserts with new-side line numbers.
        assert!(rows
            .iter()
            .any(|r| r.kind == DiffKind::Insert && r.text == "b" && r.old_line.is_none()));
        assert!(rows
            .iter()
            .any(|r| r.kind == DiffKind::Insert && r.text == "c" && r.new_line == Some(3)));
    }

    #[test]
    fn pure_delete_removes_old_lines() {
        // Going from three lines to one: two pure deletions, one context.
        let rows = diff_lines("a\nb\nc\n", "a\n");
        let (ins, del) = summary(&rows);
        assert_eq!(ins, 0, "no insertions expected");
        assert_eq!(del, 2, "expected two deleted lines");

        assert!(rows
            .iter()
            .any(|r| r.kind == DiffKind::Delete && r.text == "b" && r.new_line.is_none()));
        assert!(rows
            .iter()
            .any(|r| r.kind == DiffKind::Delete && r.text == "c" && r.old_line == Some(3)));
    }

    #[test]
    fn mixed_insert_and_delete() {
        // "b" removed, "d" added, "a"/"c" unchanged context.
        let rows = diff_lines("a\nb\nc\n", "a\nc\nd\n");
        let (ins, del) = summary(&rows);
        assert_eq!(ins, 1, "expected one insertion (d)");
        assert_eq!(del, 1, "expected one deletion (b)");

        assert!(rows
            .iter()
            .any(|r| r.kind == DiffKind::Delete && r.text == "b"));
        assert!(rows
            .iter()
            .any(|r| r.kind == DiffKind::Insert && r.text == "d"));
        // Both shared lines stay as context.
        assert!(rows
            .iter()
            .any(|r| r.kind == DiffKind::Equal && r.text == "a"));
        assert!(rows
            .iter()
            .any(|r| r.kind == DiffKind::Equal && r.text == "c"));
    }

    #[test]
    fn identical_input_is_all_equal() {
        let rows = diff_lines("x\ny\nz\n", "x\ny\nz\n");
        let (ins, del) = summary(&rows);
        assert_eq!(ins, 0);
        assert_eq!(del, 0);
        assert_eq!(rows.len(), 3, "every line should be a context row");
        assert!(
            rows.iter().all(|r| r.kind == DiffKind::Equal),
            "no changes expected for identical inputs"
        );
        // Line numbers march in lock-step on both sides.
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(r.old_line, Some(i + 1));
            assert_eq!(r.new_line, Some(i + 1));
        }
    }

    #[test]
    fn both_empty_yields_no_rows() {
        let rows = diff_lines("", "");
        assert!(rows.is_empty());
        assert_eq!(summary(&rows), (0, 0));
    }

    #[test]
    fn crlf_trailing_is_stripped() {
        let rows = diff_lines("alpha\r\n", "beta\r\n");
        // Neither the '\r' nor the '\n' should survive into the display text.
        assert!(rows
            .iter()
            .any(|r| r.text == "alpha" && r.kind == DiffKind::Delete));
        assert!(rows
            .iter()
            .any(|r| r.text == "beta" && r.kind == DiffKind::Insert));
    }
}
