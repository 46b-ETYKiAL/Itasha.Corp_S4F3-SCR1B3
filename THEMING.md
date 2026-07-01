# Theming

SCR1B3 themes are plain TOML files using a **Helix-style three-namespace schema**. They are UI-toolkit-agnostic: colors are RGBA values the renderer maps onto the editor surface and chrome. A theme never needs to define every key — unspecified UI keys fall back to sane defaults, and a broken user theme falls back to the compiled-in default so the editor never blanks.

## Selecting a theme

The fastest path: **Settings → Appearance → Theme** picks one of the built-ins from a dropdown. The same value can be set in your config (see [CONFIG.md](CONFIG.md)):

```toml
[appearance]
theme = "itasha-corp"
```

The value is either a **built-in name** (see below) or the **file stem** of a theme file you drop in your themes directory. If the file exists it overrides the built-in of the same name; otherwise the built-in with that name renders; otherwise the editor falls back to `wired-noir` so a misnamed entry never blanks the UI.

## Built-in themes

SCR1B3 ships **32 themes** (all compiled into the binary — no asset path needed). The catalogue is organised in four lines:

### Calm-canon line (DECISION-2026-005 brand canon)
| Name | Appearance | Voice | Notes |
|---|---|---|---|
| `itasha-corp` | dark | teal `#34E0D0` | **Default**. The shared Itasha.Corp house-brand palette: cool near-black layers, off-white text, one teal accent, Akira-red reserved for alarms. |
| `wired-noir` | dark | teal `#34E0D0` | The compiled-in **fallback** for any broken theme. Cool near-black, off-white text, one teal accent, Akira-red reserved for alarms. |
| `phosphor-amber` | dark | amber `#FFC04A` | BBS / hairline-terminal heritage. |
| `lain-mauve` | dark | mauve `#C89AE8` | Wired-era violet melancholy (pastel-leaning). |
| `ghost-paper` | light | teal (darker) | Warm paper background, ink-grey text. WCAG AA. |
| `a11y-high-contrast` | dark | teal `#00FFE0` | WCAG AAA-target. Pure white on black, saturated complements — for low-vision users. |

### Brand-signature line — **`itasha-neon` family** (DECISION-2026-009 brand-LINE)
| Name | Appearance | Voice | Notes |
|---|---|---|---|
| `itasha-neon` | dark | cyan `#00FFFF` | Main. Reconciles the user-seed 13 colours with Itten complementary-pair discipline (cyan = system voice; hot-pink/fuchsia/deep-purple demoted to syntax-token roles ≤5% glyph-area). |
| `itasha-neon-pastel` | dark | cyan `#9EE5E5` | ~30% chroma. 8-hour-session comfort. |
| `itasha-neon-soft` | dark | cyan `#5CFFE5` | ~60% chroma. Between full-neon and pastel. |
| `itasha-neon-night` | dark | cyan `#00FFFF` | Pure black, max-saturation tokens. For dark-room use. |
| `itasha-neon-dawn` | **light** | cyan `#0A90A0` | Light-appearance port of the neon line. Daytime partner. WCAG AAA body. |
| `itasha-neon-aurora` | dark | cyan `#5CFFE5` | Cyan-violet axis only. Wired-net aurora mood. |

### Heritage-alt line (DECISION-2026-009 §5 — brand-influence palettes)
| Name | Appearance | Voice | Anchor | Notes |
|---|---|---|---|---|
| `geocities-bbs` | dark | hyperlink `#5050FF` | Web 1.0 16-colour cohort | **Camp slot — construction-yellow IS body text; sticker required, not for long sessions.** |
| `lain-wired` | dark | mauve `#8C6CD0` | Serial Experiments Lain (the Wired) | Copper-circuit warning, deep violet-black. |
| `kusanagi-dive` | dark | cyan `#34DCE0` | Ghost in the Shell (1995) | Cyan-on-deep-marine dive-sequence palette. |
| `akira-redshift` | dark | red `#FF2030` | Akira (1988) | **Opt-in only.** Red-as-voice documented exception (Akira IS red). Alarm degrades to higher-saturation, not colour swap. |
| `atompunk-sodium` | dark | sodium `#FFA030` | Eames-era Atompunk | **Opt-in only.** Sodium-orange-as-voice documented exception. |
| `terminal-lock` | dark | phosphor `#33FF66` | Tektronix 4014 / Hercules | Pure terminal-green-on-black heritage. |
| `mecha-armour` | dark | chrome `#A8B0C0` | Gundam RX-78-2 colour-spec | Federation white/blue/red/yellow on graphite-black. |
| `shutoko-night` | dark | Bayside `#0C2D6A` | 80s–2000s JDM (Itasha brand root) | Documented period paint codes (Honda NH-547 / Nissan BT2 / Mazda Soul-Red-Crystal precedent). |

### Wave-4 line (brand-fonts-themes spec — influence palettes)
| Name | Notes |
|---|---|
| `dialup-glow` | Early-web dial-up CRT warmth. |
| `present-day` | "Present day, present time" — Lain-adjacent neutral. |
| `thermoptic` | Ghost-in-the-Shell thermoptic-camouflage shimmer. |
| `capsule-mono` | Capsule-corp monochrome. |
| `jet-age` | Mid-century jet-age instrument panel. |
| `packet-trace` | Network packet-trace greens. |
| `cockpit-amber` | Amber cockpit instrumentation. |
| `nerv-magi` | NERV / MAGI command-console reds and blacks. |
| `colony-drift` | Space-colony drift palette. |
| `kanjo-loop` | Kanjozoku loop-racer night. |
| `yaksha-ink` | Ink-wash yaksha tones. |
| `datamosh-haze` | Datamosh compression-artifact haze. |

All 32 themes keep the **one-accent-equals-system-voice** principle: a single accent colour carries every interactive signal (cursor, hover, selection, active line-number, OK status). Akira-red is **alarm-only** across the family — never decorative — with the two documented exceptions above (`akira-redshift`, `atompunk-sodium`) cabined to opt-in-only themes.

## Color values

Every color is written one of two ways:

1. **A `#`-hex literal**: `#RGB`, `#RRGGBB`, or `#RRGGBBAA`.
   - `#070A0C` → opaque wired-noir void
   - `#34E0D0` → opaque wired-noir teal (the system voice)
   - `#34E0D033` → teal at ~20% alpha (used for selection)
   - `#fff` → shorthand white
2. **A palette name** defined in the `[palette]` table — so you set a color once and reference it everywhere.

## Customising a built-in

Use **Settings → Appearance → Export to user theme** to write the current built-in's full palette to `<config_dir>/themes/<name>.toml`. SCR1B3 sets the active theme to your new name automatically; open the file, edit the colours, save — the live-reload watcher applies your changes immediately. The original built-in stays intact, so you can always switch back from the picker.

### Live color editor (in-app)

Once a user theme exists on disk, **Settings → Appearance → Edit colors live** surfaces a color picker for every `[palette]` / `[ui]` / `[syntax]` entry. Changes write back to the TOML on every drag; the watcher reloads and applies them live. Sections collapse independently so you can focus on `[ui]` or `[syntax]` without scrolling past the rest. Switch theme to revert; built-ins stay immutable.

## Schema

A theme has an optional header and three color tables.

```toml
name = "my-theme"          # optional; defaults to the file stem / "custom"
appearance = "dark"        # "dark" (default) or "light"

[palette]
# named base colors — reference these in [ui] and [syntax]

[ui]
# editor chrome colors

[syntax]
# token-scope colors
```

### `[palette]` — named base colors

Define your base colors once. Each value is a `#`-hex literal. Names are arbitrary; the wired-noir default uses `void`, `bezel`, `panel`, `text`, `muted`, `dim`, `teal`, `red`, `amber`, `green`, `slate`, `sage`, `steel`, `sand`.

```toml
[palette]
void  = "#070A0C"
teal  = "#34E0D0"
amber = "#F2B33D"
```

### `[ui]` — chrome colors

UI keys color the editor frame and gutter. Each value is a palette name **or** a hex literal. Recognized keys (all optional — missing keys use defaults):

| Key | Purpose |
|---|---|
| `background` | Editor background. |
| `foreground` | Default text color. |
| `panel` | Side panels / popovers. |
| `bezel` | Frameless-window bezel / titlebar. |
| `gutter` | Line-number gutter background. |
| `line_number` | Inactive line numbers. |
| `line_number_active` | Current line number. |
| `cursor` | Text caret. |
| `selection` | Selection highlight (typically uses an alpha value, e.g. `#34E0D033`). |
| `accent` | Primary accent (links, focus strokes). |
| `ok` | Success / OK status. |
| `error` | Error status. |
| `warning` | Warning status. |

### `[syntax]` — token-scope colors

Syntax keys color code tokens. Lookups use **longest-matching-scope-wins** fallback: a token scoped `function.builtin.static` resolves to `function.builtin`, then `function`, then the default. So you can define broad scopes (`function`, `keyword`) and optionally refine specific ones. Common code scopes:

`keyword` · `function` · `string` · `comment` · `type` · `constant` · `number` · `variable`

#### Markdown markup scopes

Markdown source is colored like Notepad++ under every theme. These `markup.*` keys
color the structural tokens; each falls back to a matching code token when omitted,
so markdown is colored even in a theme that only sets code colors:

`markup.heading` · `markup.bold` · `markup.italic` · `markup.quote` · `markup.list` · `markup.raw` · `markup.link` · `markup.separator`

`markup.bold` and `markup.italic` also render with real weight/slant, not just color.
Per-level heading colors are supported via `markup.heading.1` … `markup.heading.6`
(longest-match fallback to `markup.heading`).

#### Autolinked URLs

`url` colors `http(s)://` links detected in editor text (they are also underlined and
open in your browser on Ctrl/Cmd-click). Falls back to the `accent` UI color when omitted.

#### Making `[syntax]` drive the editor

By default the in-editor colors come from the separate `editor.note_theme` syntect
preset. Set **`editor.syntax_from_theme = true`** to make the active theme's `[syntax]`
map (including all the keys above) the source of in-editor token colors for every
language. Example:

```toml
[syntax]
keyword        = "#79A0B0"
markup.heading = "#A9C2CC"
markup.bold    = "#79A0B0"
markup.italic  = "#8DA88C"
markup.quote   = "#4F5E66"
markup.list    = "#34E0D0"
markup.raw     = "#C9A86A"
markup.link    = "#34E0D0"
markup.separator = "#5A6B73"
url            = "#34E0D0"
```

## The compiled-in fallback: `wired-noir`

`wired-noir` is the compiled-in fallback for any broken or missing theme (the user-facing default theme is `itasha-corp`). It is the lore-council-approved (DECISION-2026-005) palette: **cool near-black layers, off-white text, one teal accent (the system voice), Akira-red reserved for alarms, restrained amber for warnings**. Written as a TOML theme it looks like this:

```toml
name = "wired-noir"
appearance = "dark"

[palette]
void  = "#070A0C"
panel = "#0E1417"
bezel = "#1A242B"
text  = "#C8D6DC"
muted = "#5A6B73"
dim   = "#4F5E66"
teal  = "#34E0D0"   # the system voice
red   = "#FF3B30"   # alarms only
amber = "#F2B33D"   # warnings
green = "#6FB89A"   # muted ok
slate = "#79A0B0"   # keyword
sage  = "#8DA88C"   # string
steel = "#A9C2CC"   # type
sand  = "#C9A86A"   # constant / number

[ui]
background         = "void"
foreground         = "text"
panel              = "panel"
bezel              = "bezel"
gutter             = "void"
line_number        = "muted"
line_number_active = "teal"
cursor             = "teal"
selection          = "#34E0D033"
accent             = "teal"
ok                 = "green"
error              = "red"
warning            = "amber"

[syntax]
keyword  = "slate"
function = "teal"
string   = "sage"
comment  = "dim"
type     = "steel"
constant = "sand"
number   = "sand"
variable = "text"
```

## Writing a custom theme

1. Create `my-theme.toml` in your themes directory (alongside the built-in schemes).
2. Define a `[palette]`, then map `[ui]` and `[syntax]` keys to palette names or hex literals.
3. Point your config at it:
   ```toml
   [appearance]
   theme = "my-theme"
   ```
4. Save — the theme applies live. If the file has a bad color or malformed TOML, SCR1B3 surfaces the error and keeps the previous/default theme rather than blanking.

A light theme is as simple as setting `appearance = "light"` and choosing lighter `background` / darker `foreground` values.

## CRT effects (not implemented)

A CRT / retro post-process pass (scanline, phosphor glow, bloom, vignette, curvature, chromatic aberration) was scaffolded early but **not shipped** — there is no GPU shader behind it, so it carries no `[effects]` config table. The aesthetic is expressed through the color themes themselves (e.g. `phosphor-amber`, `terminal-lock`) rather than a screen-space filter. If a post-process pass lands later it will be a separate, opt-in feature documented here and in [CONFIG.md](CONFIG.md).
