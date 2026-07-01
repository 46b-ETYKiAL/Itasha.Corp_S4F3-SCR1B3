//! Pure markdown / note text transforms — the engine half of the note-usability
//! features (GFM task checkboxes, smart list continuation, list-aware
//! indent/outdent, emphasis wrap, heading outline, reading-time, title-case,
//! smart-link paste, table formatting, auto-pair).
//!
//! Every function here is a total `&str`-transform with no egui/IO dependency,
//! so the whole module is unit-testable headless. The thin app glue in
//! `scribe-app` loads the live `TextEdit` caret range, calls one of these, and
//! writes the result back. Mirrors the `text_ops.rs` style (allocation-light,
//! trailing-newline-preserving, byte-exact on the untouched span).

// ---------------------------------------------------------------------------
// List-marker parsing (shared backbone)
// ---------------------------------------------------------------------------

/// What kind of list marker leads a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerKind {
    /// `-`, `*`, or `+` bullet. The char is the bullet glyph.
    Bullet(char),
    /// Ordered marker `N.` / `N)` — `num` is the parsed ordinal, `delim` the
    /// `.`/`)` punctuation that follows it.
    Ordered { num: u64, delim: char },
}

/// A parsed leading list marker. Byte indices are into the original line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListMarker {
    /// Leading whitespace run (spaces/tabs) before the marker.
    pub indent: String,
    /// The marker token verbatim (`-`, `*`, `+`, `12.`, `3)`).
    pub token: String,
    pub kind: MarkerKind,
    /// The content after the marker and its single following space (the item's
    /// own text; may itself begin with a `[ ]` task box).
    pub content: String,
}

/// Parse the leading list marker of a single line (no trailing `\n`). Returns
/// `None` when the line is not a bullet/ordered list item.
///
/// Accepts `-`/`*`/`+` bullets and `N.`/`N)` ordered markers, each followed by
/// at least one space. The marker must be the first non-whitespace content.
pub fn parse_list_marker(line: &str) -> Option<ListMarker> {
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let (indent, rest) = line.split_at(indent_len);
    let mut chars = rest.char_indices();
    let first = chars.next()?.1;

    // Bullet: a single -/*/+ then a space.
    if matches!(first, '-' | '*' | '+') {
        let after = &rest[first.len_utf8()..];
        // Must be followed by a space (or be the entire rest = an empty bullet).
        if after.is_empty() {
            return Some(ListMarker {
                indent: indent.to_string(),
                token: first.to_string(),
                kind: MarkerKind::Bullet(first),
                content: String::new(),
            });
        }
        let sp = after.len() - after.trim_start_matches(' ').len();
        if sp == 0 {
            return None; // `-x` is not a list item
        }
        return Some(ListMarker {
            indent: indent.to_string(),
            token: first.to_string(),
            kind: MarkerKind::Bullet(first),
            content: after[sp..].to_string(),
        });
    }

    // Ordered: one or more ASCII digits, then `.` or `)`, then a space (or EOL).
    if first.is_ascii_digit() {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let after_digits = &rest[digits.len()..];
        let delim = after_digits.chars().next()?;
        if delim != '.' && delim != ')' {
            return None;
        }
        let after = &after_digits[delim.len_utf8()..];
        let num: u64 = digits.parse().ok()?;
        if after.is_empty() {
            return Some(ListMarker {
                indent: indent.to_string(),
                token: format!("{digits}{delim}"),
                kind: MarkerKind::Ordered { num, delim },
                content: String::new(),
            });
        }
        let sp = after.len() - after.trim_start_matches(' ').len();
        if sp == 0 {
            return None;
        }
        return Some(ListMarker {
            indent: indent.to_string(),
            token: format!("{digits}{delim}"),
            kind: MarkerKind::Ordered { num, delim },
            content: after[sp..].to_string(),
        });
    }

    None
}

/// If `content` begins with a GFM task box (`[ ]`, `[x]`, `[X]`) followed by a
/// space or end-of-content, return `(checked, rest_after_box)`.
fn parse_task_box(content: &str) -> Option<(bool, &str)> {
    let bytes = content.as_bytes();
    if bytes.len() < 3 || bytes[0] != b'[' || bytes[2] != b']' {
        return None;
    }
    let checked = match bytes[1] {
        b' ' => false,
        b'x' | b'X' => true,
        _ => return None,
    };
    let rest = &content[3..];
    if rest.is_empty() || rest.starts_with(' ') {
        Some((checked, rest.trim_start_matches(' ')))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// P0-1 — GFM task checkbox toggle + progress
// ---------------------------------------------------------------------------

/// Toggle GFM task checkboxes on the inclusive 0-based line range
/// `[line_lo, line_hi]`.
///
/// For each touched line:
///   * a task item (`- [ ] foo` / `- [x] foo`) flips `[ ]`↔`[x]`;
///   * a plain list item (`- foo`) gains a fresh unchecked box (`- [ ] foo`);
///   * anything else is left untouched.
///
/// Returns the new text, or `None` when nothing changed (so the caller can
/// surface a "no task on this line" toast). A trailing newline is preserved.
pub fn toggle_task_on_lines(text: &str, line_lo: usize, line_hi: usize) -> Option<String> {
    let had_final_nl = text.ends_with('\n');
    let mut lines: Vec<String> = split_lines(text);
    let mut changed = false;
    let lo = line_lo.min(line_hi);
    let hi = line_lo.max(line_hi);
    for idx in lo..=hi {
        let Some(line) = lines.get_mut(idx) else {
            continue;
        };
        if let Some(new_line) = toggle_task_one_line(line) {
            *line = new_line;
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    Some(join_lines(lines, had_final_nl))
}

/// Toggle / insert a task box on a single line. `None` when the line is not a
/// list item (so it cannot host a checkbox).
fn toggle_task_one_line(line: &str) -> Option<String> {
    let m = parse_list_marker(line)?;
    let prefix = format!("{}{} ", m.indent, m.token);
    match parse_task_box(&m.content) {
        Some((checked, rest)) => {
            let box_str = if checked { "[ ]" } else { "[x]" };
            if rest.is_empty() {
                Some(format!("{prefix}{box_str}"))
            } else {
                Some(format!("{prefix}{box_str} {rest}"))
            }
        }
        None => {
            // Plain list item → add a fresh unchecked box.
            if m.content.is_empty() {
                Some(format!("{prefix}[ ]"))
            } else {
                Some(format!("{prefix}[ ] {}", m.content))
            }
        }
    }
}

/// Count `(done, total)` GFM task items in `text` (checked vs all task lines).
pub fn tasks_progress(text: &str) -> (usize, usize) {
    let mut done = 0;
    let mut total = 0;
    for line in text.split('\n') {
        if let Some(m) = parse_list_marker(line) {
            if let Some((checked, _)) = parse_task_box(&m.content) {
                total += 1;
                if checked {
                    done += 1;
                }
            }
        }
    }
    (done, total)
}

// ---------------------------------------------------------------------------
// P0-2 — Smart list continuation on Enter
// ---------------------------------------------------------------------------

/// The decision for what Enter should do at the end of a list line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListContinuation {
    /// Marker text to insert on the new line (already including indent + a
    /// trailing space). `None` when the current line is not a list item.
    pub marker_to_insert: Option<String>,
    /// True when the current line is an *empty* list item: Enter should clear
    /// the dangling marker (terminating the list) instead of continuing it.
    pub clear_current_line: bool,
}

/// Compute the smart-list continuation for the current line (no trailing `\n`).
///
/// * A non-empty list item → continue the same marker on the next line
///   (`-`/`*`/`+` verbatim, ordered ordinal incremented, a fresh **unchecked**
///   `[ ]` box for task items).
/// * An empty list item (marker with no content) → `clear_current_line` so the
///   marker is removed, the universally-expected "Enter exits the list".
/// * Not a list item → all-`None`/false (caller falls back to plain newline).
pub fn continue_list_marker(line: &str) -> ListContinuation {
    let Some(m) = parse_list_marker(line) else {
        return ListContinuation::default();
    };

    let task = parse_task_box(&m.content);
    // "Empty" = no text content. For a task line that means an empty box body.
    let is_empty = match &task {
        Some((_, rest)) => rest.is_empty(),
        None => m.content.is_empty(),
    };
    if is_empty {
        return ListContinuation {
            marker_to_insert: None,
            clear_current_line: true,
        };
    }

    let mut marker = match &m.kind {
        MarkerKind::Bullet(c) => format!("{}{} ", m.indent, c),
        MarkerKind::Ordered { num, delim } => {
            format!("{}{}{} ", m.indent, num.saturating_add(1), delim)
        }
    };
    if task.is_some() {
        marker.push_str("[ ] ");
    }
    ListContinuation {
        marker_to_insert: Some(marker),
        clear_current_line: false,
    }
}

// ---------------------------------------------------------------------------
// P0-3 — List-aware indent / outdent (+ ordered renumber)
// ---------------------------------------------------------------------------

/// Indent (`dir > 0`) or outdent (`dir < 0`) every list item on the inclusive
/// 0-based line range `[line_lo, line_hi]` by one `width`-space level. Ordered
/// siblings are renumbered afterwards so each contiguous ordered run at a given
/// indent restarts from its first ordinal.
///
/// Lines that are not list items are left untouched. Returns `None` when
/// nothing changed (e.g. the selection holds no list item, or an outdent at
/// column 0). A trailing newline is preserved.
pub fn indent_list_lines(
    text: &str,
    line_lo: usize,
    line_hi: usize,
    width: usize,
    dir: i32,
) -> Option<String> {
    let w = width.max(1);
    let pad = " ".repeat(w);
    let had_final_nl = text.ends_with('\n');
    let mut lines: Vec<String> = split_lines(text);
    let lo = line_lo.min(line_hi);
    let hi = line_lo.max(line_hi);
    let mut changed = false;

    for idx in lo..=hi {
        let Some(line) = lines.get_mut(idx) else {
            continue;
        };
        if parse_list_marker(line).is_none() {
            continue;
        }
        if dir > 0 {
            *line = format!("{pad}{line}");
            changed = true;
        } else if dir < 0 {
            // Remove up to `w` leading spaces, or a single leading tab.
            if let Some(stripped) = line.strip_prefix('\t') {
                *line = stripped.to_string();
                changed = true;
            } else {
                let lead = line.len() - line.trim_start_matches(' ').len();
                let drop = lead.min(w);
                if drop > 0 {
                    *line = line[drop..].to_string();
                    changed = true;
                }
            }
        }
    }
    if !changed {
        return None;
    }
    renumber_ordered_lists(&mut lines);
    Some(join_lines(lines, had_final_nl))
}

/// Renumber every contiguous ordered-list run in `lines` so each run (same
/// indentation width) counts up from its first item's ordinal. Bullet items and
/// blank lines break a run. In-place; pure modulo the `lines` mutation.
fn renumber_ordered_lists(lines: &mut [String]) {
    // Per indentation-width, the next ordinal to emit. A blank line or a shallow
    // line resets deeper levels.
    use std::collections::BTreeMap;
    let mut next: BTreeMap<usize, u64> = BTreeMap::new();

    for line in lines.iter_mut() {
        if line.trim().is_empty() {
            next.clear();
            continue;
        }
        let Some(m) = parse_list_marker(line) else {
            next.clear();
            continue;
        };
        let indent_w = m.indent.chars().count();
        // Any deeper level is no longer contiguous once we see this line.
        next.retain(|k, _| *k <= indent_w);
        match m.kind {
            MarkerKind::Ordered { delim, .. } => {
                let n = next.entry(indent_w).or_insert_with(|| {
                    // First item of this run keeps its own starting ordinal.
                    if let MarkerKind::Ordered { num, .. } = m.kind {
                        num
                    } else {
                        1
                    }
                });
                let new_token = format!("{}{}{}", m.indent, *n, delim);
                let body = if m.content.is_empty() {
                    new_token
                } else {
                    format!("{new_token} {}", m.content)
                };
                *line = body;
                *n = n.saturating_add(1);
            }
            MarkerKind::Bullet(_) => {
                // A bullet at this indent breaks any ordered run at this level.
                next.remove(&indent_w);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// P0-4 — Emphasis toggle (wrap / unwrap a selection)
// ---------------------------------------------------------------------------

/// Toggle a markdown inline marker (`**`, `*`, `` ` ``, `~~`) around the char
/// range `[lo, hi)` of `text`.
///
/// * Already wrapped (the marker sits immediately outside or inside the
///   selection) → unwrap.
/// * Otherwise → wrap. An empty selection inserts the pair and returns a caret
///   range positioned between the two markers.
///
/// Returns `(new_text, new_lo, new_hi)` as char indices selecting the inner
/// content (so the caller can restore a sensible selection).
pub fn toggle_wrap(text: &str, lo: usize, hi: usize, marker: &str) -> (String, usize, usize) {
    // Normalise a possibly-reversed range from the ORIGINAL endpoints. (Computing
    // `hi` from the already-minimised `lo` collapsed a reversed range to an empty
    // one instead of swapping — latent, since callers pre-normalise today.)
    let (lo, hi) = (lo.min(hi), lo.max(hi));
    let chars: Vec<char> = text.chars().collect();
    let mlen = marker.chars().count();
    let sel: String = chars[lo..hi.min(chars.len())].iter().collect();

    // Already wrapped *inside* the selection: `**foo**` fully selected.
    if sel.chars().count() >= 2 * mlen && sel.starts_with(marker) && sel.ends_with(marker) {
        let inner: String = sel[marker.len()..sel.len() - marker.len()].to_string();
        let new = splice_chars(&chars, lo, hi, &inner);
        return (new, lo, lo + inner.chars().count());
    }

    // Already wrapped *outside* the selection: markers sit just left/right.
    let left_ok = lo >= mlen && chars[lo - mlen..lo].iter().collect::<String>() == marker;
    let right_ok =
        hi + mlen <= chars.len() && chars[hi..hi + mlen].iter().collect::<String>() == marker;
    if left_ok && right_ok && hi >= lo {
        // Remove the outer markers.
        let mut out = String::new();
        out.extend(&chars[..lo - mlen]);
        out.extend(&chars[lo..hi]);
        out.extend(&chars[hi + mlen..]);
        return (out, lo - mlen, hi - mlen);
    }

    // Not wrapped → wrap. Caret/selection ends up around the inner content.
    let wrapped = format!("{marker}{sel}{marker}");
    let new = splice_chars(&chars, lo, hi, &wrapped);
    (new, lo + mlen, lo + mlen + sel.chars().count())
}

/// Replace the char range `[lo, hi)` of `chars` with `ins`, returning a new
/// String.
fn splice_chars(chars: &[char], lo: usize, hi: usize, ins: &str) -> String {
    let hi = hi.min(chars.len());
    let mut out = String::new();
    out.extend(&chars[..lo.min(chars.len())]);
    out.push_str(ins);
    out.extend(&chars[hi..]);
    out
}

// ---------------------------------------------------------------------------
// P1-1 — Markdown heading outline
// ---------------------------------------------------------------------------

/// One heading in a markdown document outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadingItem {
    /// 0-based source line of the heading.
    pub line: usize,
    /// Heading level 1–6.
    pub level: u8,
    /// Heading title text (the `#`s and surrounding whitespace stripped).
    pub title: String,
}

/// Extract the ATX heading outline (`#`..`######`) of a markdown document.
///
/// Headings inside fenced code blocks (``` / ~~~) are ignored so a `# comment`
/// in a code sample is not mistaken for a heading. Pure line scan — no parser
/// dependency, which keeps this in the engine crate with zero new deps.
pub fn heading_outline(text: &str) -> Vec<HeadingItem> {
    let mut out = Vec::new();
    let mut fence: Option<char> = None;
    for (line_idx, raw) in text.split('\n').enumerate() {
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim_start();
        // Fenced code-block tracking (``` or ~~~, 3+ of the same char).
        if let Some(fc) = fence {
            if is_fence_line(trimmed, fc) {
                fence = None;
            }
            continue;
        }
        if let Some(fc) = fence_open_char(trimmed) {
            fence = Some(fc);
            continue;
        }
        // ATX heading: 1–6 `#`, then a space (or end-of-line).
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) {
            let after = &trimmed[hashes..];
            if after.is_empty() || after.starts_with(' ') {
                let title = after.trim().trim_end_matches('#').trim().to_string();
                out.push(HeadingItem {
                    line: line_idx,
                    level: hashes as u8,
                    title,
                });
            }
        }
    }
    out
}

/// The fence char if `trimmed` opens a fenced code block, else `None`.
fn fence_open_char(trimmed: &str) -> Option<char> {
    ['`', '~']
        .into_iter()
        .find(|&fc| trimmed.chars().take_while(|c| *c == fc).count() >= 3)
}

/// True when `trimmed` is a closing fence of char `fc` (3+ of `fc`).
fn is_fence_line(trimmed: &str, fc: char) -> bool {
    trimmed.chars().take_while(|c| *c == fc).count() >= 3
        && trimmed.chars().all(|c| c == fc || c == ' ')
}

// ---------------------------------------------------------------------------
// P1-3 — Reading time
// ---------------------------------------------------------------------------

/// Estimated reading minutes for a word count (225 wpm, the common prose
/// average). Always at least 1 minute when there is any text.
pub fn reading_time_minutes(words: usize) -> usize {
    if words == 0 {
        0
    } else {
        words.div_ceil(225).max(1)
    }
}

// ---------------------------------------------------------------------------
// P1-4 — Title case
// ---------------------------------------------------------------------------

/// Convert `text` to Title Case: the first letter of every whitespace-delimited
/// word is upper-cased, the rest lower-cased. Whitespace runs are preserved
/// verbatim (so newlines/tabs survive).
pub fn to_title_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at_word_start = true;
    for ch in text.chars() {
        if ch.is_whitespace() {
            out.push(ch);
            at_word_start = true;
        } else if at_word_start {
            out.extend(ch.to_uppercase());
            at_word_start = false;
        } else {
            out.extend(ch.to_lowercase());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// P1-2 — Smart paste: URL over selection → markdown link
// ---------------------------------------------------------------------------

/// True when `s` looks like a single safe URL to linkify on paste: a single
/// token (no internal whitespace) that begins with `http://`, `https://`, or
/// `mailto:`.
pub fn looks_like_url(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || t.chars().any(char::is_whitespace) {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    (lower.starts_with("http://") && t.len() > 7)
        || (lower.starts_with("https://") && t.len() > 8)
        || (lower.starts_with("mailto:") && t.len() > 7)
}

/// Build a markdown link `[selection](url)` from a paste of `url` over the
/// active `selection`.
pub fn make_markdown_link(selection: &str, url: &str) -> String {
    format!("[{}]({})", selection, url.trim())
}

// ---------------------------------------------------------------------------
// P2-1 — Markdown table formatter
// ---------------------------------------------------------------------------

/// Find the contiguous pipe-table block (a run of lines each containing a `|`)
/// that includes 0-based `line`. Returns `(first_line, last_line)` inclusive,
/// or `None` when the cursor line is not part of a pipe table.
pub fn table_block_bounds(text: &str, line: usize) -> Option<(usize, usize)> {
    let lines: Vec<&str> = text.split('\n').collect();
    let is_row = |l: &str| l.contains('|') && !l.trim().is_empty();
    if line >= lines.len() || !is_row(lines[line]) {
        return None;
    }
    let mut lo = line;
    while lo > 0 && is_row(lines[lo - 1]) {
        lo -= 1;
    }
    let mut hi = line;
    while hi + 1 < lines.len() && is_row(lines[hi + 1]) {
        hi += 1;
    }
    if hi == lo {
        return None; // a single `|` line is not a table
    }
    Some((lo, hi))
}

/// Format a markdown pipe-table block: split each row on `|`, compute the max
/// display width per column, and re-pad every cell so the columns align. The
/// delimiter row (`---`/`:--:`) keeps its alignment colons and is re-dashed to
/// the column width.
///
/// `block` is the verbatim table text (newline-separated rows). Returns the
/// reformatted block (no trailing newline added).
pub fn format_markdown_table(block: &str) -> String {
    let rows: Vec<Vec<String>> = block
        .split('\n')
        .filter(|l| !l.trim().is_empty())
        .map(split_table_row)
        .collect();
    if rows.is_empty() {
        return block.to_string();
    }
    let ncols = rows.iter().map(Vec::len).max().unwrap_or(0);

    // Which row (if any) is the delimiter row (all cells are alignment dashes).
    let delim_row = rows.iter().position(|r| r.iter().all(|c| is_delim_cell(c)));

    // Column widths from the non-delimiter rows (min 3 so `---` fits).
    let mut widths = vec![3usize; ncols];
    for (ri, row) in rows.iter().enumerate() {
        if Some(ri) == delim_row {
            continue;
        }
        for (ci, cell) in row.iter().enumerate() {
            widths[ci] = widths[ci].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    for (ri, row) in rows.iter().enumerate() {
        let mut cells: Vec<String> = Vec::with_capacity(ncols);
        for (ci, &w) in widths.iter().enumerate() {
            let raw = row.get(ci).map(String::as_str).unwrap_or("");
            if Some(ri) == delim_row {
                cells.push(format_delim_cell(raw, w));
            } else {
                let pad = w.saturating_sub(raw.chars().count());
                cells.push(format!("{raw}{}", " ".repeat(pad)));
            }
        }
        out.push_str("| ");
        out.push_str(&cells.join(" | "));
        out.push_str(" |");
        out.push('\n');
    }
    out.pop(); // drop the final newline; caller controls trailing newline
    out
}

/// Split a markdown table row into trimmed cells, dropping the leading/trailing
/// empty cells produced by the outer pipes.
fn split_table_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

/// True when a cell is a delimiter cell (only `-` and optional leading/trailing
/// `:` alignment markers).
fn is_delim_cell(cell: &str) -> bool {
    let c = cell.trim();
    !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':') && c.contains('-')
}

/// Re-dash a delimiter cell to `width`, preserving its `:` alignment markers.
fn format_delim_cell(cell: &str, width: usize) -> String {
    let c = cell.trim();
    let left = c.starts_with(':');
    let right = c.ends_with(':');
    let w = width.max(3);
    match (left, right) {
        (true, true) => format!(":{}:", "-".repeat(w.saturating_sub(2).max(1))),
        (true, false) => format!(":{}", "-".repeat(w.saturating_sub(1).max(1))),
        (false, true) => format!("{}:", "-".repeat(w.saturating_sub(1).max(1))),
        (false, false) => "-".repeat(w),
    }
}

// ---------------------------------------------------------------------------
// P2-2 — Auto-pair decision (pure)
// ---------------------------------------------------------------------------

/// What an auto-pair-enabled editor should do when `typed` (a single opening or
/// closing char) is entered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoPairAction {
    /// Wrap the current selection: insert `open`..selection..`close`.
    Wrap { open: char, close: char },
    /// Insert the pair `open``close` and place the caret between them.
    InsertPair { open: char, close: char },
    /// The typed char equals the closing char immediately right of the caret —
    /// "type over" it (advance the caret, insert nothing).
    TypeOver,
    /// Do nothing special; let the editor insert the char normally.
    Passthrough,
}

/// The closing partner of an auto-pair opening char, if any.
pub fn auto_pair_close(open: char) -> Option<char> {
    Some(match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    })
}

/// Decide the auto-pair action for a typed char.
///
/// * Opening char + non-empty selection → `Wrap`.
/// * Opening char + empty selection → `InsertPair`.
/// * A closing char equal to the char right of the caret → `TypeOver`.
/// * Otherwise → `Passthrough`.
pub fn auto_pair_action(
    typed: char,
    has_selection: bool,
    char_after_caret: Option<char>,
) -> AutoPairAction {
    if let Some(close) = auto_pair_close(typed) {
        if has_selection {
            return AutoPairAction::Wrap { open: typed, close };
        }
        // Type-over: an opening char that is ALSO its own close (quotes/backtick)
        // sitting right of the caret should type over, not double up.
        if close == typed && char_after_caret == Some(typed) {
            return AutoPairAction::TypeOver;
        }
        return AutoPairAction::InsertPair { open: typed, close };
    }
    // A closing char typed right before its match → type over.
    if matches!(typed, ')' | ']' | '}') && char_after_caret == Some(typed) {
        return AutoPairAction::TypeOver;
    }
    AutoPairAction::Passthrough
}

// ---------------------------------------------------------------------------
// P2-4 — Heading-section fold regions
// ---------------------------------------------------------------------------

/// A foldable heading section: `(header_line, last_body_line)` 0-based, where
/// the section runs from the heading line to the line before the next
/// same-or-higher-level heading (or end of document).
pub fn heading_fold_regions(text: &str) -> Vec<(usize, usize)> {
    let headings = heading_outline(text);
    let total_lines = text.split('\n').count();
    let mut out = Vec::new();
    for (i, h) in headings.iter().enumerate() {
        // Find the next heading at the same or higher level (lower/equal number).
        let mut end = total_lines.saturating_sub(1);
        for next in &headings[i + 1..] {
            if next.level <= h.level {
                end = next.line.saturating_sub(1);
                break;
            }
        }
        if end > h.line {
            out.push((h.line, end));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Shared line helpers (byte-exact, trailing-newline aware)
// ---------------------------------------------------------------------------

/// Split into owned lines WITHOUT the trailing-newline empty element. Preserves
/// each line's bytes verbatim (including an embedded `\r`).
fn split_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Re-join lines with `\n`, re-appending the trailing newline when the source
/// had one.
fn join_lines(lines: Vec<String>, had_final_nl: bool) -> String {
    let mut out = lines.join("\n");
    if had_final_nl {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_list_marker ----

    #[test]
    fn parses_bullet_and_ordered_markers() {
        let m = parse_list_marker("- hello").unwrap();
        assert_eq!(m.kind, MarkerKind::Bullet('-'));
        assert_eq!(m.content, "hello");
        let m = parse_list_marker("  * nested").unwrap();
        assert_eq!(m.indent, "  ");
        assert_eq!(m.kind, MarkerKind::Bullet('*'));
        let m = parse_list_marker("12. twelfth").unwrap();
        assert_eq!(
            m.kind,
            MarkerKind::Ordered {
                num: 12,
                delim: '.'
            }
        );
        assert_eq!(m.content, "twelfth");
        let m = parse_list_marker("3) paren").unwrap();
        assert_eq!(m.kind, MarkerKind::Ordered { num: 3, delim: ')' });
    }

    #[test]
    fn rejects_non_list_lines() {
        assert!(parse_list_marker("plain text").is_none());
        assert!(parse_list_marker("-no space").is_none());
        assert!(parse_list_marker("1.no space").is_none());
        assert!(parse_list_marker("").is_none());
        assert!(parse_list_marker("3.14 is pi").is_none()); // delim not . or )
    }

    // ---- P0-1 task toggle ----

    #[test]
    fn task_toggle_flips_box() {
        assert_eq!(
            toggle_task_on_lines("- [ ] buy milk\n", 0, 0).unwrap(),
            "- [x] buy milk\n"
        );
        assert_eq!(
            toggle_task_on_lines("- [x] buy milk\n", 0, 0).unwrap(),
            "- [ ] buy milk\n"
        );
        // Uppercase X also unchecks.
        assert_eq!(
            toggle_task_on_lines("* [X] done\n", 0, 0).unwrap(),
            "* [ ] done\n"
        );
    }

    #[test]
    fn task_toggle_adds_box_to_plain_item() {
        assert_eq!(
            toggle_task_on_lines("- buy milk\n", 0, 0).unwrap(),
            "- [ ] buy milk\n"
        );
        // Preserves indentation.
        assert_eq!(
            toggle_task_on_lines("    - nested\n", 0, 0).unwrap(),
            "    - [ ] nested\n"
        );
        // Empty bullet gets an empty box.
        assert_eq!(toggle_task_on_lines("-", 0, 0).unwrap(), "- [ ]");
    }

    #[test]
    fn task_toggle_range_and_no_change() {
        let src = "- [ ] a\n- [ ] b\nnot a list\n";
        let out = toggle_task_on_lines(src, 0, 1).unwrap();
        assert_eq!(out, "- [x] a\n- [x] b\nnot a list\n");
        // No list item in range → None.
        assert!(toggle_task_on_lines("plain\ntext\n", 0, 1).is_none());
    }

    #[test]
    fn tasks_progress_counts() {
        let src = "- [x] a\n- [ ] b\n- [x] c\n- plain\n# heading\n";
        assert_eq!(tasks_progress(src), (2, 3));
        assert_eq!(tasks_progress("no tasks here"), (0, 0));
    }

    // ---- P0-2 list continuation ----

    #[test]
    fn continue_bullet_and_ordered() {
        let c = continue_list_marker("- item");
        assert_eq!(c.marker_to_insert.as_deref(), Some("- "));
        assert!(!c.clear_current_line);
        let c = continue_list_marker("  * nested item");
        assert_eq!(c.marker_to_insert.as_deref(), Some("  * "));
        let c = continue_list_marker("3. third");
        assert_eq!(c.marker_to_insert.as_deref(), Some("4. "));
        let c = continue_list_marker("2) second");
        assert_eq!(c.marker_to_insert.as_deref(), Some("3) "));
    }

    #[test]
    fn continue_task_inserts_fresh_unchecked_box() {
        let c = continue_list_marker("- [x] done item");
        assert_eq!(c.marker_to_insert.as_deref(), Some("- [ ] "));
        let c = continue_list_marker("- [ ] todo item");
        assert_eq!(c.marker_to_insert.as_deref(), Some("- [ ] "));
    }

    #[test]
    fn continue_empty_item_terminates_list() {
        let c = continue_list_marker("- ");
        assert!(c.clear_current_line);
        assert!(c.marker_to_insert.is_none());
        let c = continue_list_marker("3. ");
        assert!(c.clear_current_line);
        // Empty task box also terminates.
        let c = continue_list_marker("- [ ] ");
        assert!(c.clear_current_line);
    }

    #[test]
    fn continue_non_list_is_noop() {
        let c = continue_list_marker("just prose");
        assert!(c.marker_to_insert.is_none());
        assert!(!c.clear_current_line);
    }

    #[test]
    fn continue_ordered_marker_saturates_at_u64_max() {
        // A ordinal at u64::MAX must not overflow to `0.` (which wrapped in
        // release, where overflow-checks are off, and panicked in debug).
        let line = format!("{}. last", u64::MAX);
        let c = continue_list_marker(&line);
        assert_eq!(
            c.marker_to_insert.as_deref(),
            Some(format!("{}. ", u64::MAX).as_str()),
            "the next ordinal must saturate at u64::MAX, not wrap to 0"
        );
    }

    #[test]
    fn toggle_wrap_normalises_reversed_range() {
        // A reversed (hi < lo) selection must wrap the same inner text as the
        // forward range, not collapse to an empty insertion. Regression: `hi`
        // was derived from the already-minimised `lo`, yielding (min,min).
        let (fwd, _, _) = toggle_wrap("hello", 1, 4, "*");
        let (rev, _, _) = toggle_wrap("hello", 4, 1, "*");
        assert_eq!(fwd, "h*ell*o");
        assert_eq!(rev, fwd, "reversed range must wrap identically to forward");
    }

    // ---- P0-3 indent / outdent + renumber ----

    #[test]
    fn indent_adds_one_level_to_list_lines_only() {
        let src = "- a\nplain\n- b\n";
        let out = indent_list_lines(src, 0, 2, 2, 1).unwrap();
        assert_eq!(out, "  - a\nplain\n  - b\n");
    }

    #[test]
    fn outdent_removes_one_level() {
        let src = "    - a\n";
        let out = indent_list_lines(src, 0, 0, 2, -1).unwrap();
        assert_eq!(out, "  - a\n");
        // Outdent at column 0 → no change.
        assert!(indent_list_lines("- a\n", 0, 0, 2, -1).is_none());
    }

    #[test]
    fn ordered_renumber_after_indent() {
        // Indenting the middle item one level renumbers the remaining top run.
        let src = "1. a\n2. b\n3. c\n";
        let out = indent_list_lines(src, 1, 1, 3, 1).unwrap();
        // Top-level run becomes 1, 2 (a, c); the indented b restarts at its own.
        assert_eq!(out, "1. a\n   2. b\n2. c\n");
    }

    #[test]
    fn renumber_fixes_a_misnumbered_run() {
        let mut lines: Vec<String> = vec!["1. a".into(), "5. b".into(), "9. c".into()];
        renumber_ordered_lists(&mut lines);
        assert_eq!(lines, vec!["1. a", "2. b", "3. c"]);
    }

    // ---- P0-4 emphasis wrap ----

    #[test]
    fn wrap_selection_with_marker() {
        let (out, lo, hi) = toggle_wrap("hello world", 0, 5, "**");
        assert_eq!(out, "**hello** world");
        assert_eq!((lo, hi), (2, 7));
    }

    #[test]
    fn unwrap_when_already_wrapped_inside() {
        let (out, _, _) = toggle_wrap("**hello** world", 0, 9, "**");
        assert_eq!(out, "hello world");
    }

    #[test]
    fn unwrap_when_markers_outside_selection() {
        // `**` outside the [2,7) selection of "hello".
        let (out, lo, hi) = toggle_wrap("**hello** world", 2, 7, "**");
        assert_eq!(out, "hello world");
        assert_eq!((lo, hi), (0, 5));
    }

    #[test]
    fn wrap_empty_selection_inserts_pair() {
        let (out, lo, hi) = toggle_wrap("ab", 1, 1, "`");
        assert_eq!(out, "a``b");
        assert_eq!((lo, hi), (2, 2)); // caret between the backticks
    }

    #[test]
    fn wrap_strikethrough_and_italic() {
        let (out, _, _) = toggle_wrap("x", 0, 1, "~~");
        assert_eq!(out, "~~x~~");
        let (out, _, _) = toggle_wrap("x", 0, 1, "*");
        assert_eq!(out, "*x*");
    }

    // ---- P1-1 heading outline ----

    #[test]
    fn outline_extracts_headings_with_levels() {
        let src = "# Title\n\nintro\n\n## Section\ntext\n### Sub\n";
        let o = heading_outline(src);
        assert_eq!(o.len(), 3);
        assert_eq!(
            o[0],
            HeadingItem {
                line: 0,
                level: 1,
                title: "Title".into()
            }
        );
        assert_eq!(
            o[1],
            HeadingItem {
                line: 4,
                level: 2,
                title: "Section".into()
            }
        );
        assert_eq!(
            o[2],
            HeadingItem {
                line: 6,
                level: 3,
                title: "Sub".into()
            }
        );
    }

    #[test]
    fn outline_ignores_headings_in_code_fences() {
        let src = "# Real\n\n```\n# not a heading\n```\n\n## Also Real\n";
        let o = heading_outline(src);
        assert_eq!(o.len(), 2);
        assert_eq!(o[0].title, "Real");
        assert_eq!(o[1].title, "Also Real");
    }

    #[test]
    fn outline_strips_trailing_hashes_and_rejects_seven() {
        let o = heading_outline("## Closed ##\n####### too deep\n");
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].title, "Closed");
    }

    // ---- P1-3 reading time ----

    #[test]
    fn reading_time_rounds_up_and_floors_at_one() {
        assert_eq!(reading_time_minutes(0), 0);
        assert_eq!(reading_time_minutes(1), 1);
        assert_eq!(reading_time_minutes(225), 1);
        assert_eq!(reading_time_minutes(226), 2);
        assert_eq!(reading_time_minutes(1000), 5);
    }

    // ---- P1-4 title case ----

    #[test]
    fn title_case_capitalises_words() {
        assert_eq!(to_title_case("the quick brown FOX"), "The Quick Brown Fox");
        // Whitespace runs (incl newlines) preserved.
        assert_eq!(to_title_case("a\nb  c"), "A\nB  C");
        assert_eq!(to_title_case(""), "");
    }

    // ---- P1-2 smart paste ----

    #[test]
    fn url_detection() {
        assert!(looks_like_url("https://example.com"));
        assert!(looks_like_url("http://a.b/c?d=1"));
        assert!(looks_like_url("mailto:x@y.com"));
        assert!(!looks_like_url("not a url"));
        assert!(!looks_like_url("https://a b.com")); // whitespace
        assert!(!looks_like_url("ftp://host"));
        assert!(!looks_like_url("https://")); // scheme only
        assert!(!looks_like_url(""));
    }

    #[test]
    fn markdown_link_build() {
        assert_eq!(
            make_markdown_link("the site", "https://x.com"),
            "[the site](https://x.com)"
        );
    }

    // ---- P2-1 table formatter ----

    #[test]
    fn table_bounds_finds_contiguous_block() {
        let src = "para\n| a | b |\n| - | - |\n| 1 | 2 |\nafter\n";
        assert_eq!(table_block_bounds(src, 2), Some((1, 3)));
        assert_eq!(table_block_bounds(src, 0), None);
    }

    #[test]
    fn table_format_aligns_columns() {
        let src = "| name | qty |\n|---|---|\n| apple | 3 |\n| fig | 10 |";
        let out = format_markdown_table(src);
        let expected = "\
| name  | qty |
| ----- | --- |
| apple | 3   |
| fig   | 10  |";
        assert_eq!(out, expected);
    }

    #[test]
    fn table_format_preserves_alignment_colons() {
        let src = "| a | b |\n|:--|--:|\n| 1 | 2 |";
        let out = format_markdown_table(src);
        // Left-aligned col keeps a leading colon, right-aligned a trailing one.
        assert!(out.lines().nth(1).unwrap().contains(":--"));
        assert!(out.lines().nth(1).unwrap().contains("--:"));
    }

    // ---- P2-2 auto-pair ----

    #[test]
    fn auto_pair_wraps_selection() {
        assert_eq!(
            auto_pair_action('(', true, None),
            AutoPairAction::Wrap {
                open: '(',
                close: ')'
            }
        );
    }

    #[test]
    fn auto_pair_inserts_pair_on_empty() {
        assert_eq!(
            auto_pair_action('[', false, None),
            AutoPairAction::InsertPair {
                open: '[',
                close: ']'
            }
        );
    }

    #[test]
    fn auto_pair_types_over_closing() {
        assert_eq!(
            auto_pair_action(')', false, Some(')')),
            AutoPairAction::TypeOver
        );
        // Quote right of caret → type over instead of doubling.
        assert_eq!(
            auto_pair_action('"', false, Some('"')),
            AutoPairAction::TypeOver
        );
    }

    #[test]
    fn auto_pair_passthrough_for_plain_char() {
        assert_eq!(
            auto_pair_action('a', false, None),
            AutoPairAction::Passthrough
        );
        assert_eq!(
            auto_pair_action(')', false, Some('x')),
            AutoPairAction::Passthrough
        );
    }

    // ---- P2-4 heading folding ----

    #[test]
    fn heading_fold_regions_span_sections() {
        let src = "# A\nl1\nl2\n## B\nl3\n# C\nl4\n";
        let regions = heading_fold_regions(src);
        // # A spans to the line before # C (line 5) → (0, 4).
        // ## B spans to the line before # C (same/higher) → (3, 4).
        // # C spans to EOF → (5, 7) (trailing empty line included).
        assert!(regions.contains(&(0, 4)));
        assert!(regions.contains(&(3, 4)));
        assert!(regions.iter().any(|(s, _)| *s == 5));
    }
}
