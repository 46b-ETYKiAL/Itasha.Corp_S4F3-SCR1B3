//! CORR-01 fix: tab-stop-aware caret / selection / click x-positioning.
//!
//! ## Root cause
//!
//! The original caret vline, selection band, secondary carets, bracket-match
//! boxes, whitespace markers, and mouse hit-testing all computed a screen x as
//! `col * char_w`, where `col` is a **char** column and `char_w` is the
//! monospace glyph advance. That is correct only when every glyph advances by
//! exactly `char_w`. A horizontal-tab (`\t`) is a single char but egui's
//! [`LayoutJob`] advances it to the next **tab stop** (a multiple of the
//! configured tab width, wider than one `char_w`). So on any line containing
//! tab characters — opened Go / Makefile / tab-formatted source — the caret and
//! selection drew to the LEFT of the actual glyphs and a mouse click resolved to
//! the WRONG column.
//!
//! ## Fix
//!
//! The source of truth for an x-position is the laid-out [`Galley`], not
//! arithmetic on `char_w`. egui lays the tabs out to their tab stops, so the
//! galley's own glyph positions already account for tab advance (and for any
//! non-uniform glyph widths). This module maps:
//!
//! * (char column) -> relative x via [`Galley::pos_from_cursor`], and
//! * (click x) -> char column via [`Galley::cursor_from_pos`].
//!
//! Both are routed through the SAME galley the row paints, so forward (caret /
//! selection) and inverse (click) mapping are mutually consistent and honour
//! tab stops by construction.
//!
//! A tab-free line still resolves to `col * char_w` *through the galley* (every
//! glyph advances uniformly), so the monospace fast path is preserved for the
//! common case without a separate code branch.

use egui::text::CCursor;
use egui::Galley;
use std::sync::Arc;

/// Relative x (in points, from the galley's local origin) of the left edge of
/// char column `col` on a single-row line `galley`.
///
/// Uses [`Galley::pos_from_cursor`] so the value accounts for tab-stop advance
/// and any non-uniform glyph width. `col` is a CHAR column (matching the
/// editor's `caret_col` / selection-column semantics and ropey's char indexing),
/// which is exactly what [`CCursor::index`] means.
///
/// A `col` past the line end clamps to the line-end x (egui clamps the cursor),
/// so a selection/caret never reads past the laid-out row.
pub(crate) fn col_to_rel_x(galley: &Galley, col: usize) -> f32 {
    galley.pos_from_cursor(CCursor::new(col)).left()
}

/// Inverse of [`col_to_rel_x`]: map a relative x (in points, from the galley's
/// local origin) to the char column whose boundary is nearest that x.
///
/// Uses [`Galley::cursor_from_pos`], which walks the row's glyph advances (tab
/// stops included) and returns the nearest char boundary — the tab-aware
/// replacement for `((x) / char_w).round()`.
pub(crate) fn rel_x_to_col(galley: &Galley, rel_x: f32) -> usize {
    galley.cursor_from_pos(egui::vec2(rel_x, 0.0)).index
}

/// Lay out one line of text into a single-row [`Galley`] with the editor's
/// monospace font, for x-mapping. Shares the font + color the row paints with,
/// so the mapping galley's metrics match the painted galley's metrics exactly.
///
/// `no_wrap` layout keeps the line on one row so column<->x is a simple
/// single-row lookup (the editor never soft-wraps a row for caret math).
pub(crate) fn layout_line(
    ui: &egui::Ui,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
) -> Arc<Galley> {
    ui.painter().layout_no_wrap(text.to_string(), font, color)
}

#[cfg(test)]
#[allow(deprecated)] // egui 0.34 deprecated Context::run + CentralPanel::show for
                     // non-test paths; the headless layout harness here uses them
                     // exactly as the sibling mod.rs test module does.
mod tests {
    use super::*;
    use egui::{Color32, FontId};

    /// Build a real egui context + a laid-out galley for `line`, returning the
    /// galley and the monospace advance `char_w` the OLD math used. Runs inside
    /// a `Context::run` frame so `ui.fonts` is available.
    fn galley_and_char_w(line: &str) -> (Arc<Galley>, f32) {
        let ctx = egui::Context::default();
        let font = FontId::monospace(14.0);
        let mut out: Option<(Arc<Galley>, f32)> = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let char_w = ui
                    .painter()
                    .layout_no_wrap("M".to_string(), font.clone(), Color32::WHITE)
                    .size()
                    .x
                    .max(1.0);
                let galley = layout_line(ui, line, font.clone(), Color32::WHITE);
                out = Some((galley, char_w));
            });
        });
        out.expect("frame ran")
    }

    /// THE CORR-01 BITE: on a line with leading tabs, the galley x of the column
    /// AFTER the tabs must NOT equal `col * char_w` (the old math), and MUST sit
    /// at (or past) the tab stop the tabs advance to. This fails against the old
    /// `col*char_w` arithmetic and passes with the galley mapping.
    #[test]
    fn tab_column_x_is_past_naive_col_times_char_w() {
        // Two leading tabs then "let x = 1;". Column 2 == the 'l', which is laid
        // out AFTER both tabs advanced to their tab stops.
        let line = "\t\tlet x = 1;";
        let (galley, char_w) = galley_and_char_w(line);

        let col = 2; // first char after the two tabs ('l')
        let galley_x = col_to_rel_x(&galley, col);
        let naive_x = col as f32 * char_w; // the OLD (buggy) math

        // The OLD math underestimates: two tabs advance well past 2 * char_w.
        assert!(
            galley_x > naive_x + char_w,
            "tab stops must push column {col} far right of the naive {naive_x} \
             (got galley_x={galley_x}, char_w={char_w}); the old col*char_w math \
             is the CORR-01 bug"
        );

        // A tab advances to a multiple of the tab width; with tab_width == N
        // char-widths, two tabs land the column at >= 2 tab stops. The galley x
        // must be a positive multiple-ish of char_w, never the naive value.
        assert!(
            (galley_x - naive_x).abs() > 0.5,
            "galley x must differ from naive col*char_w by more than rounding"
        );
    }

    /// Round-trip: a click at the galley x of a post-tab column must inverse-map
    /// back to that exact char column. With the OLD math a click at the glyph's
    /// true x would resolve to `(x/char_w).round()` — a column too far right —
    /// so this round-trip only holds through the galley.
    #[test]
    fn click_at_post_tab_glyph_maps_back_to_its_column() {
        let line = "a\tb\tc";
        let (galley, _char_w) = galley_and_char_w(line);

        // Columns: 0='a' 1='\t' 2='b' 3='\t' 4='c'. Check the chars after tabs.
        for col in [2usize, 4usize] {
            let x = col_to_rel_x(&galley, col);
            let back = rel_x_to_col(&galley, x);
            assert_eq!(
                back, col,
                "click at the galley x of column {col} must map back to column {col} \
                 (round-trip through the tab-aware galley)"
            );
        }
    }

    /// A click at the true x of a post-tab glyph does NOT map back to the column
    /// the OLD naive inverse `(x/char_w).round()` would have produced — the
    /// concrete proof the old click math landed on the wrong column.
    #[test]
    fn naive_inverse_lands_on_wrong_column_for_tabbed_line() {
        let line = "\tx"; // tab then 'x' at column 1
        let (galley, char_w) = galley_and_char_w(line);

        let col = 1;
        let true_x = col_to_rel_x(&galley, col); // x of 'x', after the tab stop
        let naive_col = (true_x / char_w).round() as usize; // OLD inverse
        let galley_col = rel_x_to_col(&galley, true_x); // NEW inverse

        assert_eq!(galley_col, col, "galley inverse lands on the real column");
        assert_ne!(
            naive_col, col,
            "the OLD (x/char_w).round() inverse lands on the WRONG column \
             for a tabbed line (naive={naive_col}, real={col}) — the CORR-01 click bug"
        );
    }

    /// A tab-free line maps monotonically and round-trips for every column (the
    /// common case is preserved through the galley). x strictly increases with
    /// the column, and the galley inverse recovers the column from its x.
    ///
    /// (We assert galley-internal monotonicity + round-trip rather than equality
    /// with the "M"-measured `char_w`: the bare headless `Context::default()`
    /// font is not strictly mono-advance, and the galley — not `char_w` — is the
    /// authority for both forward and inverse mapping.)
    #[test]
    fn tab_free_line_is_monotonic_and_round_trips() {
        let line = "hello";
        let (galley, _char_w) = galley_and_char_w(line);
        let mut prev = f32::NEG_INFINITY;
        for col in 0..=5 {
            let x = col_to_rel_x(&galley, col);
            assert!(x > prev, "x must strictly increase with column {col}");
            prev = x;
            assert_eq!(
                rel_x_to_col(&galley, x),
                col,
                "tab-free column {col} must round-trip through the galley"
            );
        }
    }
}
