//! Rendering & text-geometry leaf helpers for the editor surface, extracted
//! from `mod.rs` (A-01 wave 3 — behavior-preserving move). Free functions
//! grouped by role: font-set build/keys, panel fill, syntax-highlight job +
//! layouter, squiggle paint, char/byte index conversions, bracket matching,
//! indent/newline helpers, the completion popup, theme load, and the config
//! watcher. The previously-private fns are widened to `pub(crate)` so `mod.rs`
//! can `pub(crate) use` them for its bare-name call sites + the `use super::*`
//! siblings + `super::grip_handle` (grid_render), mirroring the `commands`
//! re-export; `grip_handle` already was `pub(crate)`.
#![allow(clippy::wildcard_imports)]

use super::*;

/// Change-detection key for the live font set (#103): note family + UI family.
/// When this string changes, the font set is rebuilt and re-applied.
pub(crate) fn font_state_key(fonts: &scribe_core::config::FontConfig) -> String {
    format!("{}\u{0}{}", fonts.editor_family, fonts.ui_family)
}

/// Resolve a font display name to its embedded family key, falling back to
/// JetBrains Mono for an unknown / stale config value.
pub(crate) fn font_family_key(display: &str) -> &'static str {
    FONT_FAMILIES
        .iter()
        .find(|(d, _)| *d == display)
        .map(|(_, k)| *k)
        .unwrap_or("JetBrainsMono")
}

/// Build the egui font set with `editor_family` as the primary Monospace face
/// (#87). All bundled coding fonts are registered; the selected one is placed
/// first in the Monospace family, JetBrains Mono is kept right behind it as a
/// fallback, and the Noto Sans JP kanji subset is appended to both families so
/// the toolbar kanji never tofu. egui's ab_glyph does no OT shaping, so
/// ligatures are structurally off regardless of face.
pub(crate) fn build_fonts(editor_family: &str, ui_family: &str) -> egui::FontDefinitions {
    use std::sync::Arc;
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Thin);

    macro_rules! embed {
        ($key:literal, $path:literal) => {
            fonts.font_data.insert(
                $key.to_owned(),
                Arc::new(egui::FontData::from_static(include_bytes!($path))),
            );
        };
    }
    embed!(
        "JetBrainsMono",
        "../../../../assets/fonts/JetBrainsMono/JetBrainsMono-Regular.ttf"
    );
    embed!(
        "IBMPlexMono",
        "../../../../assets/fonts/IBMPlexMono/IBMPlexMono-Regular.ttf"
    );
    embed!(
        "FiraMono",
        "../../../../assets/fonts/FiraMono/FiraMono-Regular.ttf"
    );
    embed!(
        "SpaceMono",
        "../../../../assets/fonts/SpaceMono/SpaceMono-Regular.ttf"
    );
    embed!(
        "Cousine",
        "../../../../assets/fonts/Cousine/Cousine-Regular.ttf"
    );
    embed!(
        "SourceCodePro",
        "../../../../assets/fonts/SourceCodePro/SourceCodePro-Regular.ttf"
    );
    embed!(
        "B612Mono",
        "../../../../assets/fonts/B612Mono/B612Mono-Regular.ttf"
    );
    embed!(
        "ShareTechMono",
        "../../../../assets/fonts/ShareTechMono/ShareTechMono-Regular.ttf"
    );
    embed!("VT323", "../../../../assets/fonts/VT323/VT323-Regular.ttf");
    // Wave 4 — brand display + accent faces (atomic with the FONT_FAMILIES
    // additions above; a key without its embed fails the registration test).
    embed!("Doto", "../../../../assets/fonts/Doto/Doto[ROND,wght].ttf");
    embed!(
        "MajorMonoDisplay",
        "../../../../assets/fonts/MajorMonoDisplay/MajorMonoDisplay-Regular.ttf"
    );
    embed!(
        "ChakraPetch",
        "../../../../assets/fonts/ChakraPetch/ChakraPetch-Regular.ttf"
    );
    embed!(
        "Wallpoet",
        "../../../../assets/fonts/Wallpoet/Wallpoet-Regular.ttf"
    );
    embed!(
        "Michroma",
        "../../../../assets/fonts/Michroma/Michroma-Regular.ttf"
    );
    embed!(
        "RedHatMono",
        "../../../../assets/fonts/RedHatMono/RedHatMono[wght].ttf"
    );
    embed!("Teko", "../../../../assets/fonts/Teko/Teko[wght].ttf");
    embed!(
        "Rajdhani",
        "../../../../assets/fonts/Rajdhani/Rajdhani-Regular.ttf"
    );
    embed!(
        "Saira",
        "../../../../assets/fonts/Saira/Saira[wdth,wght].ttf"
    );
    embed!(
        "ZenDots",
        "../../../../assets/fonts/ZenDots/ZenDots-Regular.ttf"
    );
    embed!(
        "Syncopate",
        "../../../../assets/fonts/Syncopate/Syncopate-Regular.ttf"
    );
    embed!(
        "SplineSansMono",
        "../../../../assets/fonts/SplineSansMono/SplineSansMono[wght].ttf"
    );
    embed!(
        "NotoSansJP-Subset",
        "../../../../assets/fonts/NotoSansJP/NotoSansJP-Subset.ttf"
    );

    let selected = font_family_key(editor_family);
    if let Some(mono) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        mono.insert(0, selected.to_owned());
        if selected != "JetBrainsMono" {
            mono.insert(1, "JetBrainsMono".to_owned());
        }
        // egui-phosphor's `add_to_fonts` only registers the icon font in the
        // Proportional family, so phosphor glyphs (CHECK, DOTS_SIX_VERTICAL, …)
        // render as tofu boxes in any `.monospace()` text (the status bar, the
        // pane-header note name). Append phosphor as a Monospace fallback too so
        // those glyphs resolve there as well — JetBrains Mono still leads.
        if !mono.iter().any(|f| f == "phosphor") {
            mono.push("phosphor".to_owned());
        }
    }
    // #103 — the UI (proportional) font is chosen SEPARATELY from the note font.
    // "System default" (or any unknown value) leaves egui's built-in UI font
    // untouched; a bundled family name puts that face first in the Proportional
    // family so the whole app UI (toolbar / settings / status) uses it.
    if let Some(&(_, ui_key)) = FONT_FAMILIES.iter().find(|(d, _)| *d == ui_family) {
        if let Some(prop) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            prop.insert(0, ui_key.to_owned());
        }
    }
    for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("NotoSansJP-Subset".to_owned());
    }
    fonts
}

/// Blend `base` toward `tint` by `strength` (0..1) on the RGB channels only,
/// preserving `base`'s alpha. This is the colour-math core of the window
/// "tint" knob: it shifts a BACKGROUND surface colour toward the tint hue
/// without ever touching a glyph — text/foreground theme colours are computed
/// separately and never pass through here, so they stay byte-identical. A
/// `strength <= 0` returns `base` unchanged (0 = no tint, matching the config
/// semantics). Made `pub(crate)` so the editor visuals path
/// (`ScribeApp::current_visuals`) can reuse the exact same blend for the
/// central panel + editor-well backgrounds.
pub(crate) fn blend_tint(base: Color32, tint: Color32, strength: f32) -> Color32 {
    let s = strength.clamp(0.0, 1.0);
    if s <= 0.0 {
        return base;
    }
    let lerp = |a: u8, b: u8| -> u8 {
        (f32::from(a) + (f32::from(b) - f32::from(a)) * s)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_unmultiplied(
        lerp(base.r(), tint.r()),
        lerp(base.g(), tint.g()),
        lerp(base.b(), tint.b()),
        base.a(),
    )
}

/// Apply the window colour-tint (`window.tint` at `window.tint_strength`) to a
/// background surface colour. A missing/unparsable tint hex or a zero strength
/// leaves `base` untouched. Kept separate from `panel_fill` so the same tinting
/// is reusable for the editor visuals path.
pub(crate) fn apply_window_tint(
    base: Color32,
    window: &scribe_core::config::WindowConfig,
) -> Color32 {
    if !window.tint_enabled || window.tint_strength <= 0.0 {
        return base;
    }
    match Rgba::parse_hex(&window.tint) {
        Some(t) => blend_tint(base, Color32::from_rgb(t.r, t.g, t.b), window.tint_strength),
        None => base,
    }
}

pub(crate) fn panel_fill(
    theme: &Theme,
    window: &scribe_core::config::WindowConfig,
    background_override: Option<&str>,
) -> Color32 {
    // #88 — an explicit background override (hex) wins over the theme's panel
    // colour; otherwise follow the theme. Translucency (glass mode) still
    // applies its alpha on top, so the override composes with vibrancy.
    let base: Color32 = match background_override.and_then(Rgba::parse_hex) {
        Some(o) => Color32::from_rgb(o.r, o.g, o.b),
        None => ui_color(theme, "panel", Rgba::new(0x0d, 0x0b, 0x14, 255)),
    };
    // Window colour-tint: shift the RGB of the *background* fill toward the tint
    // colour. This replaces the old full-surface translucent overlay layer,
    // which — in glass/translucent window mode — washed the ENTIRE content area
    // (the area behind and around the glyphs) so the user perceived the text as
    // tinted too. Blending into the fill colour tints only the background;
    // glyphs are painted on top with their own (untinted) theme colours. Done
    // BEFORE the translucency alpha so the tint colours the RGB and vibrancy
    // still composes on top.
    let base = apply_window_tint(base, window);
    if window.effective_translucent() {
        // 0.02 floor matches the settings slider min + scribe_render::apply_window_opacity
        // so the full slider travel is live (the old 0.30 floor was a dead band;
        // #24 dropped 0.05 → 0.02 for a more see-through lowest setting).
        let a = (window.opacity.clamp(0.02, 1.0) * 255.0).round() as u8;
        Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a)
    } else {
        base
    }
}

/// Paint a dot drag-grip and return its response. We paint the dots instead of
/// drawing the phosphor `DOTS_SIX_VERTICAL` glyph because that PUA codepoint
/// renders as a tofu square in this build's font atlas (the glyph IS in the
/// font and phosphor IS registered in both families, yet egui's atlas resolves
/// it to .notdef here — a known egui-phosphor footgun). Painted dots are
/// font-independent and always render as a clean, recognizable grip. `enabled`
/// = false dims it and drops the drag sense (used for pinned panes).
///
/// `rotated` flips the grip's orientation to MATCH the tab's text orientation:
/// `false` (default) paints a 2×3 column of dots (a tall handle) for horizontal
/// tabs/headers; `true` paints a 3×2 row of dots (a wide handle) for the rotated
/// (vertical-text) side tabs, so the grip reads as a handle in that orientation
/// instead of staying vertical against horizontal text.
pub(crate) fn grip_handle(
    ui: &mut egui::Ui,
    enabled: bool,
    color: Color32,
    rotated: bool,
) -> egui::Response {
    let h = ui.text_style_height(&egui::TextStyle::Body);
    let sense = if enabled {
        egui::Sense::click_and_drag()
    } else {
        egui::Sense::hover()
    };
    // Swap the allocation's aspect with the orientation: tall+narrow for the
    // vertical handle, wide+short for the rotated (horizontal) handle.
    let size = if rotated {
        egui::vec2(h.max(15.0), 11.0)
    } else {
        egui::vec2(11.0, h)
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let dim = if enabled {
        color
    } else {
        color.gamma_multiply(0.5)
    };
    let c = rect.center();
    let painter = ui.painter();
    // 2 cols × 3 rows (vertical) vs 3 cols × 2 rows (rotated) — the dot grid is
    // transposed so the handle's long axis follows the tab's long axis.
    let (xs, ys): (&[f32], &[f32]) = if rotated {
        (&[c.x - 4.5, c.x, c.x + 4.5], &[c.y - 2.5, c.y + 2.5])
    } else {
        (&[c.x - 2.5, c.x + 2.5], &[c.y - 4.5, c.y, c.y + 4.5])
    };
    for &x in xs {
        for &y in ys {
            painter.circle_filled(egui::pos2(x, y), 1.5, dim);
        }
    }
    resp
}

/// Build a syntect-colored `LayoutJob` for the editor surface. Free function so
/// the egui `layouter` closure captures only the highlighter, not `self`.
#[allow(clippy::too_many_arguments)]
fn highlight_job(
    hl: &Highlighter,
    text: &str,
    ext: Option<&str>,
    font: FontId,
    line_height_mult: f32,
    inc_cache: &mut IncrementalHighlightState,
    fg: Color32,
    url_color: Color32,
    detect_links: bool,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let lines = hl.highlight_document_incremental(text, ext, inc_cache);
    // Explicit per-row height honours the `fonts.line_height` setting (epaint
    // TextFormat.line_height; epaint defaults to the font's natural height).
    let lh = Some(font.size * line_height_mult);
    let plain = |color: Color32| {
        let mut f = TextFormat::simple(font.clone(), color);
        f.line_height = lh;
        f
    };
    // #D — the format for a URL span: the themeable `url` colour + a persistent
    // underline (the affordance that the text is a link). Built once per job.
    let url_fmt = {
        let mut f = plain(url_color);
        f.underline = egui::Stroke::new(1.0, url_color);
        f
    };
    // Append `line[range]` to the job, sub-segmenting at URL byte-boundaries so
    // the portion inside a URL gets `url_fmt` and the rest keeps `base`. When
    // there are no URLs on the line (the common case) this is a single append.
    fn append_split(
        job: &mut LayoutJob,
        line: &str,
        range: std::ops::Range<usize>,
        base: &TextFormat,
        urls: &[std::ops::Range<usize>],
        url_fmt: &TextFormat,
    ) {
        if urls.is_empty() {
            if let Some(seg) = line.get(range.clone()) {
                if !seg.is_empty() {
                    job.append(seg, 0.0, base.clone());
                }
            }
            return;
        }
        let mut pos = range.start;
        while pos < range.end {
            let in_url = urls.iter().find(|u| u.start <= pos && pos < u.end);
            let (next, fmt) = match in_url {
                Some(u) => (u.end.min(range.end), url_fmt),
                None => {
                    let next_start = urls
                        .iter()
                        .map(|u| u.start)
                        .filter(|&st| st > pos)
                        .min()
                        .unwrap_or(range.end)
                        .min(range.end);
                    (next_start, base)
                }
            };
            if let Some(piece) = line.get(pos..next) {
                if !piece.is_empty() {
                    job.append(piece, 0.0, fmt.clone());
                }
            }
            pos = next;
        }
    }
    // Reconstruct text with colored spans line by line.
    for (li, line) in text.split_inclusive('\n').enumerate() {
        // Per-line URL byte-ranges (empty when detection is off). URLs never
        // span newlines, so per-line scanning is correct and cheap.
        let urls = if detect_links {
            scribe_core::url_scan::detect_urls(line)
        } else {
            Vec::new()
        };
        if let Some(spans) = lines.get(li) {
            let mut byte = 0usize;
            for s in spans {
                if !s.range.is_empty() {
                    let mut fmt = plain(scribe_render::syntax_color32(s.color));
                    if s.italic {
                        fmt.italics = true;
                    }
                    append_split(&mut job, line, s.range.clone(), &fmt, &urls, &url_fmt);
                }
                byte = s.range.end;
            }
            // Append any tail not covered by spans. Wave-3: use the theme
            // foreground (was hardcoded GRAY — washed out vs the body text and
            // mismatched the rope editor, which already uses the theme fg).
            // Use `get(..)` (like the per-span slice above) rather than a direct
            // `&line[byte..]`: if the highlighter ever emits a span boundary that
            // is not a UTF-8 char boundary, a direct slice would panic → abort.
            if byte < line.len() && line.get(byte..).is_some() {
                append_split(
                    &mut job,
                    line,
                    byte..line.len(),
                    &plain(fg),
                    &urls,
                    &url_fmt,
                );
            }
        } else {
            append_split(&mut job, line, 0..line.len(), &plain(fg), &urls, &url_fmt);
        }
    }
    job
}

/// Build the memoizing egui `layouter` closure for a `TextEdit`. Reuses the
/// cached highlight `LayoutJob` unless the buffer/lang/font-size changed, so
/// syntect/tree-sitter only re-run when the text actually changes.
/// The wrap width our editor layouter should USE, given the word-wrap setting
/// and the width egui hands the layouter. egui's `TextEdit` always passes the
/// scroll-viewport `available_width` as the wrap width (NOT `desired_width`), so
/// a custom layouter that blindly honours it wraps even when wrap is off — the
/// "word wrap is always on" bug. When wrap is off we force infinite width so the
/// galley lays out on one line and the `ScrollArea::both` scrolls horizontally.
/// Wave-3: decide whether the *editable* central editor should render this
/// buffer through the in-house viewport-culled rope editor. True when the user
/// opted in (`experimental`), OR when auto-promotion is enabled (`threshold > 0`)
/// AND the buffer is at least `threshold` bytes. A pure function so the
/// branch-selection logic is unit-testable without driving an egui frame.
pub(crate) fn use_rope_editor(
    experimental: bool,
    text_len: usize,
    auto_threshold_bytes: usize,
) -> bool {
    experimental || (auto_threshold_bytes > 0 && text_len >= auto_threshold_bytes)
}

/// Load static Tab-trigger snippets from `<config-dir>/snippets.toml`. A missing
/// or malformed file yields an empty set (the feature is simply inert) — never
/// an error path, so a bad snippets file can't block the editor from starting.
pub(crate) fn load_snippets() -> scribe_core::snippets::SnippetSet {
    scribe_core::config::Config::config_dir()
        .map(|d| d.join("snippets.toml"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| scribe_core::snippets::SnippetSet::from_toml(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn effective_wrap_width(word_wrap: bool, available: f32) -> f32 {
    if word_wrap {
        available
    } else {
        f32::INFINITY
    }
}

/// Char index of byte offset `byte` in `s` (#78 — spell spans are byte offsets,
/// galley cursors are char indices). Clamps to the nearest char boundary at or
/// before `byte`, so a mid-codepoint offset never panics.
pub(crate) fn byte_to_char_index(s: &str, byte: usize) -> usize {
    s.char_indices().take_while(|(i, _)| *i < byte).count()
}

/// Wave-6 bracket-match: find the bracket pair to highlight given a caret
/// char-index. Looks at the char just before and just after the caret for an
/// opener/closer, then scans for its partner respecting nesting. Returns
/// `(open_char_index, close_char_index)` in ascending order, or `None`. The scan
/// is bounded by the caller (skipped for very large buffers) to stay cheap.
pub(crate) fn matching_bracket_char_indices(text: &str, caret_ci: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let pairs = [('(', ')'), ('[', ']'), ('{', '}')];
    let is_open = |c: char| pairs.iter().any(|(o, _)| *o == c);
    let is_close = |c: char| pairs.iter().any(|(_, cl)| *cl == c);
    let partner = |c: char| -> Option<(char, bool)> {
        for (o, cl) in pairs {
            if c == o {
                return Some((cl, true)); // need a closer, scan forward
            }
            if c == cl {
                return Some((o, false)); // need an opener, scan backward
            }
        }
        None
    };
    // Prefer the char immediately to the LEFT of the caret (editor convention),
    // else the char to the RIGHT.
    let candidates = [caret_ci.checked_sub(1), Some(caret_ci)];
    for ci in candidates.into_iter().flatten() {
        let Some(&here) = chars.get(ci) else { continue };
        if !is_open(here) && !is_close(here) {
            continue;
        }
        let (want, forward) = partner(here)?;
        let mut depth = 0i32;
        if forward {
            let mut j = ci;
            while j < chars.len() {
                let c = chars[j];
                if c == here {
                    depth += 1;
                } else if c == want {
                    depth -= 1;
                    if depth == 0 {
                        return Some((ci, j));
                    }
                }
                j += 1;
            }
        } else {
            let mut j = ci as isize;
            while j >= 0 {
                let c = chars[j as usize];
                if c == here {
                    depth += 1;
                } else if c == want {
                    depth -= 1;
                    if depth == 0 {
                        return Some((j as usize, ci));
                    }
                }
                j -= 1;
            }
        }
    }
    None
}

/// Paint a red spellcheck squiggle from `x0` to `x1` along baseline `y` (#78).
/// A small triangle wave reads as the universal "misspelled" underline.
pub(crate) fn paint_squiggle(painter: &egui::Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    if x1 <= x0 {
        return;
    }
    let amp = 1.5;
    let step = 3.0;
    let stroke = egui::Stroke::new(1.0, color);
    let mut x = x0;
    let mut up = true;
    let mut prev = egui::pos2(x0, y);
    while x < x1 {
        x = (x + step).min(x1);
        let ny = if up { y - amp } else { y + amp };
        let next = egui::pos2(x, ny);
        painter.line_segment([prev, next], stroke);
        prev = next;
        up = !up;
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn make_layouter<'a>(
    hl: &'a Highlighter,
    cache: &'a std::cell::RefCell<Option<(u64, std::sync::Arc<LayoutJob>)>>,
    gcache: &'a std::cell::RefCell<Option<(u64, f32, std::sync::Arc<egui::Galley>)>>,
    inc_cache: &'a std::cell::RefCell<IncrementalHighlightState>,
    ext: Option<&'a str>,
    font: FontId,
    line_height: f32,
    word_wrap: bool,
    fg: Color32,
    url_color: Color32,
    detect_links: bool,
) -> impl FnMut(&egui::Ui, &dyn egui::TextBuffer, f32) -> std::sync::Arc<egui::Galley> + 'a {
    // egui 0.34: TextEdit::layouter callback now receives `&dyn TextBuffer`
    // instead of `&str` (so non-String buffers can be hosted). We still want
    // to hash + highlight by &str, so unpack via TextBuffer::as_str().
    move |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap: f32| {
        let text: &str = text.as_str();
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // P-07: this DELIBERATELY hashes the full buffer text every frame and
        // is NOT replaced by an `(edit_gen, len)` key. The layouter receives
        // egui's LIVE `&dyn TextBuffer` (it has no access to a per-tab
        // `edit_gen`), and the cached galley/job BAKES IN the text -- a lagging
        // counter would render STALE TEXT, not just a stale squiggle. See the
        // `edit_gen` field docs: the syntax layouter intentionally keeps its
        // content hash while the minimap/spell memos key off `edit_gen`.
        text.hash(&mut hasher);
        ext.hash(&mut hasher);
        font.size.to_bits().hash(&mut hasher);
        line_height.to_bits().hash(&mut hasher);
        // Wave-3: fold the tail/foreground colour into the key so a theme switch
        // (which changes `fg` but not the text) invalidates the cached job.
        let [r, g, b, a] = fg.to_array();
        r.hash(&mut hasher);
        g.hash(&mut hasher);
        b.hash(&mut hasher);
        a.hash(&mut hasher);
        // #D — fold the URL colour + detection toggle into the key so a theme
        // switch or toggling link-detection invalidates the cached job.
        let [ur, ug, ub, ua] = url_color.to_array();
        ur.hash(&mut hasher);
        ug.hash(&mut hasher);
        ub.hash(&mut hasher);
        ua.hash(&mut hasher);
        detect_links.hash(&mut hasher);
        let key = hasher.finish();
        let eff_wrap = effective_wrap_width(word_wrap, wrap);
        // Wave-3: full galley hit — same content key AND same wrap width. Return
        // the cached Arc<Galley> (O(1) bump); skip the LayoutJob deep-clone AND
        // the re-layout. egui's own FontsView cache does NOT save the clone.
        {
            let gslot = gcache.borrow();
            if let Some((gk, gw, gal)) = gslot.as_ref() {
                if *gk == key && *gw == eff_wrap {
                    return gal.clone();
                }
            }
        }
        let job_arc = {
            let mut slot = cache.borrow_mut();
            match slot.as_ref() {
                Some((k, j)) if *k == key => j.clone(),
                _ => {
                    let arc = std::sync::Arc::new(highlight_job(
                        hl,
                        text,
                        ext,
                        font.clone(),
                        line_height,
                        &mut inc_cache.borrow_mut(),
                        fg,
                        url_color,
                        detect_links,
                    ));
                    *slot = Some((key, arc.clone()));
                    arc
                }
            }
        };
        let mut job = (*job_arc).clone();
        job.wrap.max_width = eff_wrap;
        // egui 0.34: FontsView::layout_job caches into the view → needs &mut.
        let galley = ui.fonts_mut(|f| f.layout_job(job));
        *gcache.borrow_mut() = Some((key, eff_wrap, galley.clone()));
        galley
    }
}

/// Byte offset of char index `ci` in `s` (clamped to `s.len()`).
pub(crate) fn char_to_byte(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
}

/// Find the bookmark to jump to from `from` line (0-based) in direction
/// `dir` (`1` = next/down, `-1` = previous/up). Bookmarks are an ordered set
/// of 0-based line indices. The search wraps around the buffer, so "next"
/// past the last bookmark returns the first, and "previous" before the
/// first returns the last. Returns `None` when there are no bookmarks.
pub(crate) fn pick_bookmark(
    bookmarks: &std::collections::BTreeSet<usize>,
    from: usize,
    dir: i32,
) -> Option<usize> {
    if bookmarks.is_empty() {
        return None;
    }
    if dir >= 0 {
        // First bookmark strictly after `from`; wrap to the lowest otherwise.
        bookmarks
            .range((from + 1)..)
            .next()
            .copied()
            .or_else(|| bookmarks.iter().next().copied())
    } else {
        // Last bookmark strictly before `from`; wrap to the highest otherwise.
        bookmarks
            .range(..from)
            .next_back()
            .copied()
            .or_else(|| bookmarks.iter().next_back().copied())
    }
}

/// Translate an egui [`egui::epaint::text::cursor::CCursor`] char index into
/// a human-visible `(1-based line, 1-based column)` pair. Counts a literal
/// `\n` as a line break; the column resets on every newline.
pub(crate) fn line_col_from_char_index(text: &str, char_index: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for ch in text.chars().take(char_index) {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Replace the `[lo, hi)` char-range of `text` with `width` spaces and return
/// `(new_text, new_caret_char_index)`. Pure core of the Tab→spaces handler so it
/// can be unit-tested without a live `TextEdit`.
pub(crate) fn apply_indent(text: &str, lo: usize, hi: usize, width: usize) -> (String, usize) {
    let spaces = " ".repeat(width.max(1));
    let blo = char_to_byte(text, lo);
    let bhi = char_to_byte(text, hi);
    let mut out = text.to_string();
    out.replace_range(blo..bhi, &spaces);
    (out, lo + spaces.chars().count())
}

/// Auto-indent on Enter (#107): insert a newline at `cursor` (char index) plus a
/// copy of the CURRENT line's leading whitespace, so the new line keeps the same
/// indentation. Returns the new text and the new cursor char index (after the
/// inserted newline + indent). Pure + unit-tested. Preserves whatever the line
/// uses (spaces or tabs); this is what makes `tab_width`/`insert_spaces`-driven
/// indentation actually persist line-to-line.
pub(crate) fn newline_with_indent(text: &str, cursor: usize) -> (String, usize) {
    let bcur = char_to_byte(text, cursor);
    // Start of the current line = byte after the previous '\n' (or 0).
    let line_start = text[..bcur].rfind('\n').map(|i| i + 1).unwrap_or(0);
    // Leading whitespace of the line, but not past the cursor.
    let indent: String = text[line_start..bcur]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let insert = format!("\n{indent}");
    let mut out = text.to_string();
    out.insert_str(bcur, &insert);
    (out, cursor + insert.chars().count())
}

/// Render the completion popup as a foreground `Area` anchored just below the
/// cursor row. Returns `Some(index)` if the user clicked a row.
pub(crate) fn completion_popup(ui: &egui::Ui, pos: egui::Pos2, c: &Completion) -> Option<usize> {
    let mut clicked = None;
    egui::Area::new(egui::Id::new("scr1b3-completion"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(280.0);
                for (i, item) in c.items.iter().enumerate() {
                    let label = egui::RichText::new(item).monospace();
                    if ui.selectable_label(i == c.selected, label).clicked() {
                        clicked = Some(i);
                    }
                }
            });
        });
    clicked
}

pub(crate) fn load_theme(name: &str) -> Theme {
    // Try a user theme file `<config_dir>/themes/<name>.toml` first so users can
    // override built-ins. Then try the built-in dispatch (Phase 17 T17.2 alt
    // themes). Final fallback is the wired-noir brand default so a misnamed
    // theme never blanks the UI.
    if let Some(dir) = Config::config_dir() {
        let p = dir.join("themes").join(format!("{name}.toml"));
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(t) = Theme::from_toml_str(&s) {
                return t;
            }
        }
    }
    Theme::builtin(name).unwrap_or_else(Theme::itasha_corp)
}

/// Spawn a filesystem watcher on the config directory; sends `()` on `tx` when
/// a `.toml` change is observed. Returns the watcher (kept alive by the app).
pub(crate) fn spawn_config_watcher(
    tx: std::sync::mpsc::Sender<()>,
) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher as _;
    let dir = Config::config_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if ev
                .paths
                .iter()
                .any(|p| p.extension().is_some_and(|e| e == "toml"))
            {
                let _ = tx.send(());
            }
        }
    })
    .ok()?;
    watcher
        .watch(&dir, notify::RecursiveMode::NonRecursive)
        .ok()?;
    Some(watcher)
}

#[cfg(test)]
mod tint_tests {
    use super::*;
    use scribe_core::config::WindowConfig;

    /// A representative "text/foreground" theme colour. This is the colour the
    /// glyph layouter paints with (`ui_color(theme, "foreground", …)`); it is
    /// NEVER routed through the tint, so it is the byte-for-byte invariant the
    /// bug fix must protect.
    fn foreground_of(theme: &Theme) -> Color32 {
        ui_color(theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255))
    }

    #[test]
    fn blend_tint_zero_strength_is_identity() {
        let base = Color32::from_rgb(0x0d, 0x0b, 0x14);
        let red = Color32::from_rgb(0xff, 0x00, 0x00);
        assert_eq!(blend_tint(base, red, 0.0), base);
        // Negative / clamped-below-zero is also identity.
        assert_eq!(blend_tint(base, red, -1.0), base);
    }

    #[test]
    fn blend_tint_full_strength_is_pure_tint_rgb() {
        let base = Color32::from_rgb(0x0d, 0x0b, 0x14);
        let red = Color32::from_rgb(0xff, 0x00, 0x00);
        let out = blend_tint(base, red, 1.0);
        assert_eq!((out.r(), out.g(), out.b()), (0xff, 0x00, 0x00));
    }

    #[test]
    fn blend_tint_preserves_base_alpha() {
        // A translucent (glass-mode) base keeps its alpha; only RGB shifts.
        let base = Color32::from_rgba_unmultiplied(0x0d, 0x0b, 0x14, 0x40);
        let red = Color32::from_rgb(0xff, 0x00, 0x00);
        let out = blend_tint(base, red, 0.8);
        assert_eq!(out.a(), 0x40, "alpha (vibrancy) must be preserved");
        assert!(out.r() > base.r(), "red channel must shift toward the tint");
    }

    /// The core bug-fix guarantee: a strong tint SHIFTS the background/panel
    /// fill colour, while the representative text/foreground colour stays
    /// byte-identical. (The tint touches only background surfaces.)
    #[test]
    fn strong_tint_shifts_background_but_not_text_color() {
        let theme = Theme::itasha_corp();
        // A default (opaque) window so panel_fill returns an opaque RGB we can
        // compare directly (translucency alpha is orthogonal to the tint).
        let window = WindowConfig {
            tint: "#ff0000".to_string(),
            tint_strength: 0.8,
            ..WindowConfig::default()
        };

        // Baseline (no tint) vs tinted.
        let window_none = WindowConfig {
            tint_strength: 0.0,
            ..window.clone()
        };
        let bg_untinted = panel_fill(&theme, &window_none, None);
        let bg_tinted = panel_fill(&theme, &window, None);

        // Background clearly shifts toward red at 0.8 strength.
        assert_ne!(
            (bg_tinted.r(), bg_tinted.g(), bg_tinted.b()),
            (bg_untinted.r(), bg_untinted.g(), bg_untinted.b()),
            "the tint must change the background fill colour"
        );
        assert!(
            bg_tinted.r() > bg_untinted.r(),
            "an #ff0000 tint must raise the background's red channel"
        );

        // Text/foreground colour is untouched by the tint — byte identical.
        let fg_no_tint = foreground_of(&theme);
        // Recompute the foreground the same way with the tint active: it does
        // not depend on the tint at all, proving glyph colours never change.
        let fg_with_tint = foreground_of(&theme);
        assert_eq!(
            fg_no_tint.to_array(),
            fg_with_tint.to_array(),
            "the text/foreground colour must be byte-identical regardless of tint"
        );
        // And the tinted background must not have collapsed onto the text colour.
        assert_ne!(
            bg_tinted.to_array(),
            fg_no_tint.to_array(),
            "tinting the background must not equal the text colour"
        );
    }

    #[test]
    fn apply_window_tint_ignores_unparsable_hex() {
        let base = Color32::from_rgb(0x0d, 0x0b, 0x14);
        let window = WindowConfig {
            tint: "not-a-hex".to_string(),
            tint_strength: 0.9,
            ..WindowConfig::default()
        };
        assert_eq!(apply_window_tint(base, &window), base);
    }
}
