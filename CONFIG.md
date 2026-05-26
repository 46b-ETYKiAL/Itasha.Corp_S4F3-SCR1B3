# Configuration

SCR1B3 is configured by a single, live-reloading **TOML** file. Great defaults out of the box; everything overridable.

## File location

| OS | Path |
|---|---|
| Windows | `%APPDATA%\ItashaCorp\scr1b3\config\scr1b3.toml` |
| Linux | `~/.config/scr1b3/scr1b3.toml` |
| macOS | `~/Library/Application Support/com.ItashaCorp.scr1b3/scr1b3.toml` |

## Behavior

- **Partial files merge onto defaults.** You only need to write the keys you want to change; every unspecified field keeps its default. A file containing just `[editor]\ntab_width = 2` changes only the tab width.
- **Malformed config never breaks the editor.** A parse error falls back to the full default config and surfaces the error message in-app.
- **A missing file is fine.** Absent config silently uses defaults.
- **Live reload.** Saving the file applies changes without a restart.

All settings are grouped into seven tables: `[editor]`, `[appearance]`, `[fonts]`, `[effects]`, `[updates]`, `[spellcheck]`, `[plugins]`.

---

## `[editor]` — editing behavior

| Key | Type | Default | Description |
|---|---|---|---|
| `tab_width` | integer | `4` | Width of a tab stop, in columns. |
| `insert_spaces` | boolean | `true` | Insert spaces instead of a tab character when pressing Tab. |
| `show_line_numbers` | boolean | `true` | Show the line-number gutter. |
| `show_minimap` | boolean | `true` | Show the minimap. |
| `word_wrap` | boolean | `false` | Soft-wrap long lines to the viewport width. |
| `auto_save` | boolean | `false` | Automatically save dirty buffers. |
| `restore_session` | boolean | `true` | Reopen the previous session's tabs on launch. |

## `[appearance]` — theme and window

| Key | Type | Default | Description |
|---|---|---|---|
| `theme` | string | `"itasha-void"` | Theme name: a built-in scheme or the file stem of a user theme in your themes directory. See [THEMING.md](THEMING.md). |
| `follow_os_theme` | boolean | `true` | Follow the OS dark/light preference. |
| `frameless` | boolean | `true` | Use a frameless window with the custom brand titlebar (no OS title bar). Set `false` for standard OS window decorations. |

## `[fonts]` — typography

| Key | Type | Default | Description |
|---|---|---|---|
| `editor_family` | array of strings | `["JetBrains Mono", "Cascadia Code", "Consolas"]` | Ordered fallback list of monospace families for the editing surface. |
| `ui_family` | array of strings | `["Inter"]` | Ordered fallback list of families for UI chrome (tabs, status bar, menus). |
| `editor_size` | float | `14.0` | Editor font size in points. |
| `line_height` | float | `1.4` | Line height as a multiple of the font size. |
| `ligatures` | boolean | `true` | Enable programming ligatures where the font supports them. |

## `[effects]` — CRT / retro post-process

The CRT post-process pass is **disabled by default** (zero cost when off). When enabled, each value is an intensity in the `0.0`–`1.0` range. Effects are suppressed automatically when the OS requests reduced motion (if `respect_reduced_motion` is on).

| Key | Type | Default | Description |
|---|---|---|---|
| `crt_enabled` | boolean | `false` | Master toggle for the CRT/retro post-process shader. |
| `scanline` | float | `0.30` | Scanline intensity. |
| `phosphor_glow` | float | `0.20` | Phosphor-glow intensity. |
| `bloom` | float | `0.15` | Bloom intensity on bright pixels. |
| `vignette` | float | `0.25` | Edge-darkening vignette intensity. |
| `curvature` | float | `0.0` | Screen-curvature amount (`0.0` = flat). |
| `chromatic_aberration` | float | `0.05` | RGB-fringe chromatic aberration intensity. |
| `respect_reduced_motion` | boolean | `true` | Disable animated CRT effects when the OS requests reduced motion. |

## `[updates]` — telemetry-free auto-update

The update check contacts **only** the public GitHub Releases API and sends **zero PII** — no analytics, no custom server, no shipped token. Downloads are cryptographically verified before any swap (see [SECURITY.md](SECURITY.md)).

| Key | Type | Default | Description |
|---|---|---|---|
| `mode` | string | `"notify"` | One of: `off` (never check), `notify` (check and tell you), `manual` (check only when you ask), `auto` (download + verify + apply automatically). |
| `check_interval_hours` | integer | `24` | Hours between background version checks. |

## `[spellcheck]` — privacy-respecting offline spellcheck

Spellcheck is **off by default** and **fully offline** when enabled — no network, no cloud. It is code-aware: it can check comments and strings while leaving code identifiers alone.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | boolean | `false` | Master toggle for spellcheck. |
| `language` | string | `"en_US"` | Dictionary language code. |
| `check_comments` | boolean | `true` | Spellcheck inside comments. |
| `check_strings` | boolean | `true` | Spellcheck inside string literals. |
| `check_identifiers` | boolean | `false` | Spellcheck code identifiers (usually noisy; off by default). |
| `custom_dict_path` | string (path) | _(unset)_ | Optional path to a personal dictionary file for "add to dictionary". |

## `[plugins]` — user plugin/mod system

See [PLUGINS.md](PLUGINS.md) for the capability-consent model.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | boolean | `true` | Master toggle for the plugin system. |
| `disabled` | array of strings | `[]` | Plugin ids you have explicitly disabled. |

---

## Full example `scr1b3.toml`

This is the complete default configuration written out explicitly. Copy it as a starting point and trim it to only the keys you want to change.

```toml
[editor]
tab_width = 4
insert_spaces = true
show_line_numbers = true
show_minimap = true
word_wrap = false
auto_save = false
restore_session = true

[appearance]
theme = "itasha-void"
follow_os_theme = true
frameless = true

[fonts]
editor_family = ["JetBrains Mono", "Cascadia Code", "Consolas"]
ui_family = ["Inter"]
editor_size = 14.0
line_height = 1.4
ligatures = true

[effects]
crt_enabled = false
scanline = 0.30
phosphor_glow = 0.20
bloom = 0.15
vignette = 0.25
curvature = 0.0
chromatic_aberration = 0.05
respect_reduced_motion = true

[updates]
mode = "notify"
check_interval_hours = 24

[spellcheck]
enabled = false
language = "en_US"
check_comments = true
check_strings = true
check_identifiers = false
# custom_dict_path = "/path/to/personal.dic"

[plugins]
enabled = true
disabled = []
```

### Minimal example

A real config rarely needs more than a few lines:

```toml
[appearance]
theme = "itasha-void"

[effects]
crt_enabled = true
scanline = 0.45

[editor]
tab_width = 2
```
