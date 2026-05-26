# Theming

SCR1B3 themes are plain TOML files using a **Helix-style three-namespace schema**. They are UI-toolkit-agnostic: colors are RGBA values the renderer maps onto the editor surface and chrome. A theme never needs to define every key — unspecified UI keys fall back to sane defaults, and a broken user theme falls back to the compiled-in default so the editor never blanks.

## Selecting a theme

Set the theme name in your config (see [CONFIG.md](CONFIG.md)):

```toml
[appearance]
theme = "itasha-void"
```

The value is either a **built-in scheme name** or the **file stem** of a theme file you drop in your themes directory.

## Color values

Every color is written one of two ways:

1. **A `#`-hex literal**: `#RGB`, `#RRGGBB`, or `#RRGGBBAA`.
   - `#08060d` → opaque void black
   - `#00fffe` → opaque signal cyan
   - `#00fffe33` → cyan at ~20% alpha (used for selection)
   - `#fff` → shorthand white
2. **A palette name** defined in the `[palette]` table — so you set a color once and reference it everywhere.

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

Define your base colors once. Each value is a `#`-hex literal. Names are arbitrary; the brand default uses `void`, `bezel`, `panel`, `text`, `muted`, `cyan`, `green`, `magenta`, `yellow`, `orange`, `red`.

```toml
[palette]
void    = "#08060d"
cyan    = "#00fffe"
green   = "#01fe36"
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
| `selection` | Selection highlight (typically uses an alpha value, e.g. `#00fffe33`). |
| `accent` | Primary accent (links, focus strokes). |
| `ok` | Success / OK status. |
| `error` | Error status. |
| `warning` | Warning status. |

### `[syntax]` — token-scope colors

Syntax keys color code tokens. Lookups use **longest-matching-scope-wins** fallback: a token scoped `function.builtin.static` resolves to `function.builtin`, then `function`, then the default. So you can define broad scopes (`function`, `keyword`) and optionally refine specific ones. Common scopes:

`keyword` · `function` · `string` · `comment` · `type` · `constant` · `number` · `variable`

## The brand default: `itasha-void`

`itasha-void` is the compiled-in default and the fallback for any broken theme. It is the Itasha.Corp CRT palette: **void black, signal cyan, status green**. Written as a TOML theme it looks like this:

```toml
name = "itasha-void"
appearance = "dark"

[palette]
void    = "#08060d"
bezel   = "#111118"
panel   = "#0d0b14"
text    = "#d6e2f0"
muted   = "#5a5869"
cyan    = "#00fffe"
green   = "#01fe36"
magenta = "#d946ef"
yellow  = "#fbbf24"
orange  = "#fb923c"
red     = "#ff0040"

[ui]
background         = "void"
foreground         = "text"
panel              = "panel"
bezel              = "bezel"
gutter             = "void"
line_number        = "muted"
line_number_active = "cyan"
cursor             = "cyan"
selection          = "#00fffe33"
accent             = "cyan"
ok                 = "green"
error              = "red"
warning            = "yellow"

[syntax]
keyword  = "magenta"
function = "cyan"
string   = "green"
comment  = "muted"
type     = "yellow"
constant = "orange"
number   = "orange"
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

## CRT effects

The retro CRT look is **not** part of the theme — it is a separate GPU post-process pass configured in `[effects]` (see [CONFIG.md](CONFIG.md)). It is **off by default**. Enable and tune it independently of your color theme:

```toml
[effects]
crt_enabled = true
scanline = 0.30
phosphor_glow = 0.20
bloom = 0.15
vignette = 0.25
curvature = 0.0
chromatic_aberration = 0.05
respect_reduced_motion = true   # auto-disable animation under OS reduced-motion
```

Because effects and themes are orthogonal, you can run the CRT shader over any theme — or run any theme flat with effects off.
