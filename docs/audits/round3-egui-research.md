# Round-3 egui 0.34 Research — SCR1B3

Best-in-class, egui-0.34-correct implementation guidance for 8 UI/testing items.
Grounded in the live SCR1B3 source (`crates/scribe-app/src/app.rs`,
`settings.rs`, `main.rs`, `scribe-core/src/config.rs`, `theme.rs`) and the pinned
crate sources in the local cargo registry (egui-phosphor 0.12.0 thin variant,
epaint TextShape). Pins: `eframe 0.34`, `egui 0.34`, `egui_tiles 0.15`,
`egui-phosphor 0.12 (thin)`.

> Convention: SCR1B3 already centralizes panel fills through
> `fn panel_fill(theme, window)` and the transparent surface through
> `App::clear_color`. New work below extends these existing seams rather than
> introducing parallel ones.

---

## 1. Vertical (rotated 90°) tab text — interactive, with close/pin buttons

**egui has no built-in vertical text.** The established idiom is to lay out a
normal horizontal `Galley`, then paint it with `epaint::TextShape` carrying an
`angle` (radians, clockwise, **pivot = `pos`, the galley's upper-left corner**).
SCR1B3 already paints the minimap this exact way (`app.rs:2623`), so this is a
known-good path in the codebase.

Confirmed API (epaint `TextShape`, stable across 0.29→0.34):

```rust
pub struct TextShape { pub pos: Pos2, pub galley: Arc<Galley>, pub angle: f32, /* … */ }
impl TextShape {
    pub fn new(pos: Pos2, galley: Arc<Galley>, fallback_color: Color32) -> Self;
    pub fn with_angle(self, angle: f32) -> Self;            // radians, clockwise
    pub fn with_override_text_color(self, c: Color32) -> Self;
}
```
`angle` doc: *"Rotate text by this many radians clockwise. The pivot is `pos`."*

**Cleanest interactive approach** — keep the *click/drag/context-menu hit-test*
on a normal allocated rect; only the **label painting** is rotated. Do NOT try
to make the rotated mesh itself the interactive surface (its bounds are
unrotated → wrong hit area). Instead allocate a tall, narrow column cell and
paint into it.

Pivot math: for **bottom-to-top** (the usual "reads upward" vertical tab),
rotate `-PI/2` and anchor the galley's *pos* at the **bottom-left** of the cell
so the text climbs up the column. For **top-to-bottom**, rotate `+PI/2` and
anchor at **top-right**.

```rust
use egui::{epaint::TextShape, FontId, Sense, vec2, Color32};

/// One vertical tab in a single-column stack. Returns the click/secondary/drag response.
fn vertical_tab(
    ui: &mut egui::Ui,
    label: &str,
    is_pinned: bool,
    accent: Color32,
    fg: Color32,
) -> egui::Response {
    // 1) Lay out the (still horizontal) galley once.
    let galley = ui.fonts_mut(|f| {
        f.layout_no_wrap(label.to_owned(), FontId::monospace(13.0), fg)
    });
    let text_len = galley.size().x;     // becomes the cell HEIGHT after rotation
    let text_thk = galley.size().y;     // becomes the cell WIDTH after rotation

    // 2) Allocate the tall, narrow cell. Pad for the close/pin row at the top.
    let pad = 6.0;
    let icon_row = 18.0; // room for ✕ / pin at the cell's TOP
    let cell = vec2(text_thk + pad * 2.0, text_len + pad * 2.0 + icon_row);
    let (rect, resp) =
        ui.allocate_exact_size(cell, Sense::click_and_drag());

    // 3) Hover/active background — makes the column read as discrete tabs.
    if resp.hovered() || resp.dragged() {
        ui.painter().rect_filled(rect, 3.0, accent.gamma_multiply(0.12));
    }

    // 4) Paint the rotated label (bottom-to-top). Pivot = pos = bottom-left.
    let baseline = egui::pos2(
        rect.center().x - text_thk * 0.5,
        rect.bottom() - pad,           // start at the bottom…
    );
    ui.painter().add(
        TextShape::new(baseline, galley, fg)
            .with_angle(-std::f32::consts::FRAC_PI_2) // -90° → reads upward
            .with_override_text_color(if resp.hovered() { accent } else { fg }),
    );

    // 5) Close + pin buttons live in the (unrotated) top icon row, so they stay
    //    trivially clickable. put_button paints into a sub-rect of `rect`.
    let pin_rect = egui::Rect::from_min_size(rect.left_top() + vec2(2.0, 2.0), vec2(14.0, 14.0));
    let close_rect = egui::Rect::from_min_size(rect.right_top() + vec2(-16.0, 2.0), vec2(14.0, 14.0));
    let pin_glyph = if is_pinned { egui_phosphor::thin::PUSH_PIN }
                    else        { egui_phosphor::thin::PUSH_PIN_SLASH };
    // small, theme-tinted icon buttons:
    ui.put(pin_rect, egui::Button::new(pin_glyph).frame(false));
    ui.put(close_rect, egui::Button::new(egui_phosphor::thin::X).frame(false));

    // 6) Context menu stays on the whole-cell response.
    resp.context_menu(|ui| { /* pin / close others / … */ });
    resp
}
```

Stack them in a single column with `ui.vertical(|ui| { for tab … })`. Because
the *hit rect* is axis-aligned, click + drag-reorder (use the existing
`tab_index_after_move` helper, `app.rs`) + `context_menu` all work unmodified;
only the glyphs are rotated.

Caveats from the egui issue tracker: rotation pivot is `pos` (not the anchor) —
`TextShape::angle` doc and issue #428 / #7051 both confirm; that is why we
compute `baseline` explicitly instead of relying on an anchor. Rotated text is
*not* snapped to the pixel grid (issue #5164), so expect very slightly softer
glyphs — acceptable for a tab spine.

Sources:
[TextShape (egui Painter docs)](https://docs.rs/egui/latest/egui/struct.Painter.html) ·
[Text rotation #428](https://github.com/emilk/egui/issues/428) ·
[Rotation for UI elements #2054](https://github.com/emilk/egui/issues/2054) ·
[rotation anchor #7051](https://github.com/emilk/egui/issues/7051) ·
[pixel-grid #5164](https://github.com/emilk/egui/issues/5164) ·
local epaint `TextShape` source (cargo registry).

---

## 2. Window transparency / vibrancy on the OS app window (not an egui child Window)

SCR1B3 **already implements this** — `apply_window_effect` in `app.rs:170`
calls `window_vibrancy::apply_acrylic` (Win), `apply_mica` (Win11),
`apply_vibrancy` (macOS); `main.rs:88` sets `with_transparent(true)`;
`clear_color` (`app.rs:3625`) returns fully transparent. The research task is
the **architecture that keeps `egui::Window("settings")` OPAQUE while the root
frame is translucent** — and confirming the alpha interplay.

### The three layers and how alpha composes

1. **OS surface / clear color.** `eframe::App::clear_color` must return
   `[0,0,0,0]` (already done) so the wgpu surface is cleared to transparent and
   the OS blur (acrylic/mica/vibrancy) shows through. `ViewportBuilder
   ::with_transparent(true)` (done) is the prerequisite — egui-wgpu then picks a
   premultiplied composite-alpha mode.
2. **Root panel fill alpha.** Everything visible is painted by panels. The
   translucency is produced by giving the *root* `CentralPanel`/`SidePanel`
   frames a fill whose **alpha < 255**. SCR1B3's `panel_fill()` already does
   exactly this: it multiplies `window.opacity (0.30..=1.0)` into the panel
   color's alpha *only when* `effective_translucent()` (`app.rs:3350`,
   `config.rs:127`). So the alpha source of truth is one function.
3. **`window-vibrancy` effect.** Applied once at startup against the raw OS
   handle from `CreationContext`. It tints/blurs whatever is *behind* the
   transparent pixels. On Linux there is no OS API → the portable transparent
   surface + a tint overlay carries the look (already the fallback).

`Frame::fill` alpha + `clear_color` interaction: the clear color is the *bottom*
layer (fully transparent → OS blur). Each panel `Frame` paints on top with its
own alpha; a panel at `alpha=235` lets ~8% of the blurred desktop through. So
**lowering panel alpha = more glass**; `clear_color` itself stays `[0,0,0,0]`
permanently.

### Keeping `egui::Window("settings")` opaque

An `egui::Window` is an *interior* egui container, painted by the same renderer
on top of the translucent panels. To make it opaque, override its `frame` with a
**fully-opaque fill** (alpha 255) regardless of `window.opacity`:

```rust
// Reuse the theme panel color but FORCE alpha = 255 for child windows.
fn opaque_window_frame(ui: &egui::Ui, theme: &Theme) -> egui::Frame {
    let base = ui_color(theme, "panel", Rgba::new(0x0d, 0x0b, 0x14, 255));
    egui::Frame::window(ui.style())
        .fill(egui::Color32::from_rgb(base.r(), base.g(), base.b())) // a = 255
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
}

egui::Window::new("settings")
    .frame(opaque_window_frame(ui, &self.theme)) // ← opaque chrome
    .open(&mut keep_open)
    .show(ctx, |ui| { /* … */ });
```

Because the settings window's frame is opaque, the translucent root behind it is
fully occluded under the window's rect — exactly "transparency affects ONLY the
app background." Do the same `.frame(...)` override on the command-palette,
go-to-line, and open-file windows if you want them readable over glass. The
single seam: `panel_fill()` keeps alpha for *panels*; child `Window`s opt into
opacity via an explicit `.frame()` with `a=255`.

> Known egui/eframe pitfall: with the **wgpu** backend, transparency only
> composites if the painted content is itself non-opaque AND the platform's
> composite-alpha mode supports it — Windows historically needed
> `with_transparent(true)` + a transparent clear color (both present here).
> Issues #2680 / #4451 track the wgpu-specific quirks; SCR1B3's existing combo
> is the working configuration.

Sources:
[eframe acrylic/mica/blur #3050](https://github.com/emilk/egui/issues/3050) ·
[window-vibrancy crate](https://crates.io/crates/window-vibrancy) ·
[window-vibrancy docs](https://docs.rs/window-vibrancy) ·
[Transparent background discussion #4228](https://github.com/emilk/egui/discussions/4228) ·
[wgpu transparency #2680](https://github.com/emilk/egui/issues/2680) ·
[Win transparency #4451](https://github.com/emilk/egui/issues/4451) ·
SCR1B3 `app.rs:170` / `app.rs:3350` / `main.rs:88`.

---

## 3. Font themes (family + size + line-height + weight presets)

egui's font model: `FontDefinitions { font_data: Map<String, FontData>,
families: Map<FontFamily, Vec<String>> }`, installed via `ctx.set_fonts(defs)`.
A *font theme* is a named preset that (a) reorders the `Monospace` /
`Proportional` family priority lists to put the chosen face first, and (b)
carries size + line-height knobs applied through `Style::text_styles` /
`Style::spacing`. egui's `ab_glyph` rasterizer does **not** do OT shaping, so
"weight" must be a **separate bundled TTF** (a `…-Bold.ttf`), not a synthetic
weight — SCR1B3 already documents this for ligatures (`app.rs:625` block).

### Architecture (parallel to color themes)

```rust
pub struct FontTheme {
    pub id: &'static str,            // "jetbrains", "iosevka", …
    pub mono_first: &'static str,    // font_data key to push to front of Monospace
    pub prop_first: Option<&'static str>,
    pub base_size: f32,              // editor monospace size
    pub line_height: f32,           // multiplier → Style spacing
}
```

**Register all faces once at startup** (you cannot create new
`FontFamily::Name` slots at runtime — issue #7068 — but you *can* re-order the
existing `Monospace`/`Proportional` vectors and call `set_fonts` again, which is
cheap and restart-free):

```rust
fn install_fonts(ctx: &egui::Context, faces: &[(&str, &'static [u8])]) {
    let mut defs = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut defs, egui_phosphor::Variant::Thin);
    for (key, bytes) in faces {
        defs.font_data.insert((*key).to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(bytes)));
    }
    // CJK + Hack fallbacks already present in SCR1B3 stay at the tail.
    ctx.set_fonts(defs);
}

/// Runtime switch — reorder the Monospace family so the theme's face wins, then
/// re-set. No restart.
fn apply_font_theme(ctx: &egui::Context, all_keys: &[&str], theme: &FontTheme) {
    let mut defs = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut defs, egui_phosphor::Variant::Thin);
    // (re-insert all font_data — keep an Arc-cached copy to avoid re-reading bytes)
    let mono = defs.families.entry(egui::FontFamily::Monospace).or_default();
    mono.retain(|k| k != theme.mono_first);
    mono.insert(0, theme.mono_first.to_owned());
    ctx.set_fonts(defs);

    // size + line height live in Style, not FontDefinitions:
    ctx.style_mut(|s| {
        for (_, fid) in s.text_styles.iter_mut() {
            if fid.family == egui::FontFamily::Monospace { fid.size = theme.base_size; }
        }
        s.spacing.interact_size.y = theme.base_size * theme.line_height;
    });
}
```

Line-height: egui has no per-galley line-height knob; approximate via
`Style::spacing` and the editor's row pitch. Size is per-`TextStyle` via
`Style::text_styles`.

### Recommended ~6–10 OFL coding faces (JetBrains Mono already bundled)

All SIL OFL-1.1, ship `*-Regular.ttf` (+ optional `-Bold.ttf`):

| Face | Why include |
|---|---|
| **JetBrains Mono** (bundled) | Tall x-height, default. |
| **IBM Plex Mono** | Humanist, distinct from JBM; pairs with Plex Sans. |
| **Iosevka** (Term/Fixed) | Narrow — most lines-per-screen; ship the *non-ligature* `Fixed` build. |
| **Cascadia Code / Mono** | MS default; ship **Cascadia Mono** (no ligatures, since ab_glyph won't shape them anyway). |
| **Source Code Pro** | Adobe classic, 7 weights → easy bold. |
| **Fira Mono** | Fira Code without the (unusable) ligatures. |
| **Hack** | Already egui's fallback; square, neutral. |
| **Space Mono** | Display/retro — fits SCR1B3's retro-terminal brand. |
| **Geist Mono** (optional) | Vercel, very clean, OFL. |
| **Noto Sans Mono** (optional) | Widest glyph coverage fallback. |

**Subset/size management:** the binary already embeds fonts via
`include_bytes!`. Full TTFs are 200KB–1MB+ each; for a "not bloated" editor,
either (a) ship Regular-only per face and synth-bold *off* (accept no bold), or
(b) pre-subset to Latin + common symbols with `pyftsubset`/`fonttools`
(`--layout-features='' --unicodes=U+0000-024F,U+2000-206F,U+2190-21FF`),
trimming Iosevka/Plex to ~40–80KB. Lazy alternative: load non-default faces from
`<config_dir>/fonts/*.ttf` with `FontData::from_owned` so they're not compiled
in at all (matches SCR1B3's "drop a wordlist to change dictionary" pattern).

Sources:
[FontDefinitions docs](https://docs.rs/egui/latest/egui/struct.FontDefinitions.html) ·
[custom_font example](https://github.com/emilk/egui/blob/main/examples/custom_font/src/main.rs) ·
[runtime font families #7068](https://github.com/emilk/egui/issues/7068) ·
[JetBrains Mono](https://www.jetbrains.com/lp/mono/) ·
[open-source coding fonts overview](https://uxdesign.cc/which-open-source-monospaced-font-is-best-for-coding-6bafd8d4b43c) ·
SCR1B3 `app.rs:625`.

---

## 4. Independent background-color override that resets on theme change

**State model:** config holds `Option<bg_override>`. On *theme change*, write the
new theme's panel/background color *into* the override (so it visually "resets"
to the theme but remains user-editable). Effective bg = `override.unwrap_or(theme bg)`.

```rust
// scribe-core/src/config.rs (extends WindowConfig / AppearanceConfig)
#[serde(default)]
pub background_override: Option<String>, // "#RRGGBB", None = follow theme

// On theme switch (app.rs, where self.theme is reassigned):
fn set_theme(&mut self, theme: Theme) {
    let bg = ui_color(&theme, "background", Rgba::new(0x07,0x0a,0x0c,255));
    // "reset to theme bg" == overwrite the override with the new theme's bg.
    self.config.appearance.background_override = Some(bg.to_hex()); // or None to mean "follow"
    self.theme = theme;
}
```

Two valid semantics — pick one and be consistent:
- **"Follow then snapshot"** (above): theme change copies theme bg into the
  override; a "Reset" button sets it back to `None`.
- **"Pure follow until edited"**: keep `None` while following; only set `Some`
  when the user picks a custom color; theme change sets it back to `None`.
  Simpler — recommended.

**Apply it through `Visuals`,** which is where egui reads background colors:

```rust
fn apply_visuals(ctx: &egui::Context, theme: &Theme, bg_override: Option<Color32>) {
    let mut v = if theme.is_light() { egui::Visuals::light() } else { egui::Visuals::dark() };
    let bg = bg_override.unwrap_or_else(|| ui_color(theme, "background", /*…*/).into());
    v.panel_fill   = bg;                 // SidePanel / TopBottomPanel / CentralPanel default fill
    v.window_fill  = ui_color(theme, "panel", /*…*/).into(); // egui::Window chrome
    v.extreme_bg_color = darker(bg);     // TextEdit / ScrollArea troughs — keep distinct from panel
    ctx.set_visuals(v);
}
```

Interplay: `panel_fill` = the app background you're overriding;
`window_fill` = child `egui::Window` chrome (keep on theme so item #2's opaque
settings window matches); `extreme_bg_color` = the editor text-area well — keep
it *darker than* the override so the typing surface stays readable even if the
user picks a pale custom bg. SCR1B3's per-panel `panel_fill()` helper should read
the same override so the alpha-translucency path (item #2) and the bg-override
path (item #4) compose: `effective_bg = override.unwrap_or(theme) → then apply
opacity alpha`.

Sources:
[Visuals docs](https://docs.rs/egui/latest/egui/style/struct.Visuals.html) ·
[Style/Visuals theming](https://docs.rs/egui/latest/egui/struct.Style.html) ·
SCR1B3 `theme.rs:93+`, `app.rs:3350`.

---

## 5. Grabbable drag chips in a wrapped grid

SCR1B3 **already ships** the chip idiom in `settings.rs:1337` /
`settings.rs:1441` using `ui.dnd_drag_source(id, payload, |ui| …)` +
`ui.dnd_drop_zone::<T,_>(...)` + `egui::DragAndDrop::payload::<T>(ctx)`. The
"make it LOOK draggable" work is purely visual on top of that:

```rust
fn drag_chip(ui: &mut egui::Ui, id: egui::Id, payload: ChipPayload, label: &str) -> egui::Response {
    let resp = ui.dnd_drag_source(id, payload, |ui| {
        let frame = egui::Frame::default()
            .inner_margin(egui::Margin::symmetric(6, 3))
            .corner_radius(egui::CornerRadius::same(4))
            .fill(ui.visuals().widgets.inactive.weak_bg_fill)
            .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color));
        frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                // grip handle — phosphor thin DOTS_SIX_VERTICAL (confirmed present).
                ui.add(egui::Label::new(
                    egui::RichText::new(egui_phosphor::thin::DOTS_SIX_VERTICAL).weak()
                ).selectable(false));
                ui.label(label);
            });
        });
    }).response;
    // Grab cursor on hover (the "this is draggable" affordance).
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    if resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    resp
}
```

**2–3 row wrapped grid:** `ui.horizontal_wrapped(|ui| { for chip … })` (already
used at `settings.rs:1437`) auto-wraps when the row fills; cap the panel/parent
width to force ~2–3 rows. For a *strict* N-column grid, use
`egui::Grid::new(id).num_columns(n)` and `ui.end_row()` every n items. The grip
glyph "⠿" currently used is a Braille fallback; prefer the phosphor
`DOTS_SIX_VERTICAL` (thin) for visual parity with the rest of the icon set.

egui paints the drag-source body at the cursor automatically while dragging
(free live preview) — that is the canonical `dnd_drag_source` behavior. Watch
issue #5822: a drag-source zone takes pointer priority over *interior* input
widgets, so don't nest a `TextEdit`/`Slider` inside a chip.

Sources:
[drag-and-drop discussion #3869](https://github.com/emilk/egui/discussions/3869) ·
[egui-dnd minimal example](https://techoverflow.net/2025/09/25/rust-egui-minimal-drag-n-drop-example-using-egui-dnd/) ·
[egui_dnd docs](https://docs.rs/egui_dnd/latest/egui_dnd/) ·
[dnd source priority #5822](https://github.com/emilk/egui/issues/5822) ·
SCR1B3 `settings.rs:1337`.

---

## 6. Resizable side panels + minimap min width

`SidePanel::resizable(true)` needs `min_width` / `max_width` (or
`width_range(lo..=hi)`) and a `default_width`. Resizing only works if the panel
content can actually shrink/grow (wrapping text, a `ScrollArea`, a `Separator`,
or a `TextEdit`) — otherwise the drag handle does nothing.

Practical minimums (egui clamps to its own internal margins; a SidePanel can
shrink to roughly the resize-handle width, ~**8–12 px**, but anything below
~24 px is unusable for content):

```rust
egui::SidePanel::left("filetree")
    .resizable(true)
    .default_width(220.0)
    .width_range(120.0..=420.0)   // good filetree range
    .show(ctx, |ui| { /* ScrollArea so it can resize */ });

// Minimap: SCR1B3 currently pins it with .exact_width(110.0).resizable(false)
// (app.rs:2588). To make it resizable-to-small:
egui::SidePanel::right("minimap")
    .resizable(true)
    .default_width(110.0)
    .width_range(36.0..=160.0)    // 36 px = legible micro-map; lower renders but is mush
    .show(ctx, |ui| { /* paint TextShape minimap scaled to ui.available_width() */ });
```

Recommended values: filetree `default 220, range 120..=420`; minimap
`default 96, range 36..=160`. Below ~36 px the 3px-font minimap galley
(`app.rs:2615`) collapses to noise, so 36 is the practical floor. For a
collapsible panel, gate the whole `SidePanel::show` behind a bool with
`show_animated` for the slide.

Sources:
[SidePanel docs](https://docs.rs/egui/latest/egui/containers/struct.SidePanel.html) ·
[resizing SidePanel #1522](https://github.com/emilk/egui/discussions/1522) ·
[width ratio #5308](https://github.com/emilk/egui/discussions/5308) ·
SCR1B3 `app.rs:2588`.

---

## 7. egui-phosphor (0.12, Thin) glyphs for drag/close/pin — tofu check

**Verified directly against the pinned crate source**
(`egui-phosphor-0.12.0/src/variants/thin.rs`). All present in the **thin**
variant (so they render, not tofu, given `Variant::Thin` is registered — which
SCR1B3 does at `app.rs:626`):

| Purpose | Constant | Codepoint | Line |
|---|---|---|---|
| Drag handle (preferred, vertical) | `egui_phosphor::thin::DOTS_SIX_VERTICAL` | `U+EAE2` | 507 |
| Drag handle (square 2×3) | `egui_phosphor::thin::DOTS_SIX` | `U+E794` | 506 |
| Drag handle (3-dot vertical, lighter) | `egui_phosphor::thin::DOTS_THREE_VERTICAL` | `U+E208` | 513 |
| Drag handle (9-dot waffle) | `egui_phosphor::thin::DOTS_NINE` | `U+E1FC` | 505 |
| Close | `egui_phosphor::thin::X` | `U+E4F6` | 1525 |
| Pin (pinned state) | `egui_phosphor::thin::PUSH_PIN` | `U+E3E2` | 1126 |
| Unpin / not-pinned | `egui_phosphor::thin::PUSH_PIN_SLASH` | `U+E3E4` | 1129 |
| List / overflow menu | `egui_phosphor::thin::LIST` | `U+E2F0` | 863 |

`PUSH_PIN` is already used at `app.rs:44` / `app.rs:6575`, so the thin font is
known-good in this build. **Recommendation:** `DOTS_SIX_VERTICAL` for the tab/
chip drag handle (reads as a vertical grip), `X` for close, `PUSH_PIN` ⇄
`PUSH_PIN_SLASH` to toggle pin state. No tofu risk — all four are in the thin
variant's `ALL` table.

Sources:
[egui-phosphor crate](https://crates.io/crates/egui-phosphor) ·
[egui-phosphor docs 0.12](https://docs.rs/crate/egui-phosphor/latest) ·
[Phosphor icon gallery](https://phosphoricons.com/) ·
local `egui-phosphor-0.12.0/src/variants/thin.rs` (lines cited above).

---

## 8. Rust test coverage + e2e / perf / security for an egui app

### Coverage — `cargo-llvm-cov` (recommended over tarpaulin)

`cargo-llvm-cov` uses LLVM source-based instrumentation (region-level, accurate),
works on **Windows/macOS/Linux** (tarpaulin is Linux-x86_64 ptrace-only — a
non-starter for SCR1B3's Windows-first build). For the 80% gate:

```bash
cargo install cargo-llvm-cov
cargo llvm-cov --workspace --fail-under-lines 80 --lcov --output-path lcov.info
# HTML for humans:
cargo llvm-cov --workspace --html
# nextest integration (faster, matches the monorepo cargo_test gate):
cargo llvm-cov nextest --workspace --fail-under-lines 80
```

CI: `cargo-llvm-cov` emits lcov/cobertura for Codecov, and `--fail-under-lines
80` is the hard gate. Exclude `fuzz/` (already a separate workspace) and the
`main.rs` thin shell with `#[cfg(not(coverage))]` or `--ignore-filename-regex`.

### E2E click-through — `egui_kittest`

Built on `kittest` + AccessKit; drives the real UI headless and queries the
AccessKit tree by label/role. Pairs lockstep with egui — use the `0.34`-matching
`egui_kittest`.

```rust
use egui_kittest::Harness;
use egui_kittest::kittest::Queries; // get_by_label, get_by_role…

#[test]
fn open_settings_via_toolbar() {
    let mut h = Harness::new_ui(|ui| { my_app.ui(ui); });
    h.get_by_label("settings").click();   // AccessKit label query
    h.run();                               // pump events one frame
    assert!(h.query_by_label("Appearance").is_some());
}
```
Patterns: `Harness::new_ui` for a widget, `Harness::new` for a full `App`;
`get_by_label` / `get_by_role` / `query_by_*` (non-panicking) for AccessKit
queries; `.click()` / `.type_text()` to drive; `h.run()` to advance a frame.
With the `wgpu` + `snapshot` features, `h.snapshot("name")` does
pixel-regression into `tests/snapshots/` — ideal for the brand-chrome / vertical-
tab rendering from item #1. Ensure widgets are AccessKit-labeled (eframe enables
`accesskit` — already in SCR1B3's eframe features) so queries resolve.

### Perf on large files — criterion (or simple timing)

`criterion` for statistically-sound benches (rope insert/delete, syntect
highlight of a 10MB file, minimap galley layout). For a quick CI smoke-gate, a
plain `std::time::Instant` assert (`assert!(elapsed < Duration::from_millis(50))`)
guards regressions cheaply.

```rust
// benches/large_file.rs
use criterion::{criterion_group, criterion_main, Criterion, black_box};
fn bench_open_10mb(c: &mut Criterion) {
    let text = "x\n".repeat(5_000_000);
    c.bench_function("rope_from_10mb", |b| b.iter(|| {
        black_box(ropey::Rope::from_str(black_box(&text)))
    }));
}
criterion_group!(g, bench_open_10mb); criterion_main!(g);
```
SCR1B3 already memmaps large files (`memmap2`) and memoizes the minimap galley —
bench *those* paths specifically. Avoid layout-of-whole-file egui benches; bench
the core (ropey/syntect) which is deterministic and headless.

### Security test ideas

1. **Plugin minisign verify** (`plugin_manager.rs` already wires minisign):
   feed a tampered plugin + a valid-but-wrong-key signature; assert load is
   *refused*. Test the negative path (bad sig, truncated sig, swapped key) — the
   positive path alone is insufficient.
2. **Path traversal on open:** assert that opening `../../etc/passwd`-style or
   symlink-escape paths from the filetree / CLI is normalized/sandboxed (canonical-
   ize and reject escapes outside the opened root if a root is set). Fuzz the path
   parser (the `fuzz/` workspace already exists — add a path-normalization target).
2b. **Encoding / decompression bombs & untrusted content:** open a file with
   adversarial bytes (invalid UTF-8, BOM games, 2GB of NULs, a 1-line 500MB
   file) and assert no panic / bounded memory — `chardetng`/`encoding_rs` paths.
   This is a natural cargo-fuzz target alongside the existing fuzz harness.
3. **No-network invariant** (brand promise): a test that asserts the binary
   links no HTTP client and that `RELEASES_URL` opening is the *only* outbound
   action — e.g. grep the dependency tree in CI (`cargo deny`/`cargo tree`) for
   `reqwest`/`hyper` and fail if present. `deny.toml` already exists; add a
   `bans` entry.

Sources:
[cargo-llvm-cov vs tarpaulin #1195](https://github.com/rusqlite/rusqlite/issues/1195) ·
[Rust coverage primer](https://rustprojectprimer.com/measure/coverage.html) ·
[how to do coverage in Rust](https://blog.rng0.io/how-to-do-code-coverage-in-rust/) ·
[kittest (rerun-io)](https://github.com/rerun-io/kittest) ·
[egui_kittest crate](https://crates.io/crates/egui_kittest) ·
[egui 0.30 kittest release notes](https://github.com/emilk/egui/releases/tag/0.30.0) ·
[criterion.rs](https://github.com/bheisler/criterion.rs) ·
SCR1B3 `plugin_manager.rs`, `deny.toml`, `fuzz/`.

---

## Cross-cutting notes

- **One alpha source of truth.** Items #2 + #4 both flow through `panel_fill()` /
  `Visuals.panel_fill`; compose as `effective_bg = override.unwrap_or(theme) →
  apply opacity alpha when effective_translucent()`. Don't add a second alpha path.
- **Child `egui::Window`s opt OUT of glass** via an explicit opaque `.frame()`
  (item #2) — this is the mechanism that scopes translucency to the app background.
- **ab_glyph = no shaping** → ligatures off, "weight" = a separate bundled TTF
  (item #3); this is already documented structurally in `app.rs:625`.
- **Rotation pivot is `pos`** (item #1) — compute the baseline explicitly; don't
  rely on galley anchor for rotated text.
