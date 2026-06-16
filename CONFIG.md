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

All settings are grouped into six tables: `[editor]`, `[appearance]`, `[fonts]`, `[updates]`, `[spellcheck]`, `[plugins]`.

---

## `[editor]` — editing behavior

| Key | Type | Default | Description |
|---|---|---|---|
| `tab_width` | integer | `4` | Width of a tab stop, in columns. |
| `insert_spaces` | boolean | `true` | Insert spaces instead of a tab character when pressing Tab. |
| `show_line_numbers` | boolean | `true` | Show the line-number gutter. |
| `show_change_bar` | boolean | `true` | Notepad++-style change bar in the gutter: amber marks lines edited but unsaved, green marks edited-then-saved lines, untouched lines have none. |
| `show_minimap` | boolean | `true` | Show the minimap. |
| `word_wrap` | boolean | `false` | Soft-wrap long lines to the viewport width. |
| `highlight_selection_occurrences` | boolean | `true` | When text is selected, faintly box every other matching run in the viewport. |
| `highlight_trailing_whitespace` | boolean | `false` | Tint trailing spaces/tabs on each line (distinct from rendering all whitespace). |
| `rulers` | array of integers | `[]` | Vertical guide rulers at these 1-based columns, e.g. `[80, 100]`. Empty = none. |
| `auto_save` | boolean | `false` | Automatically save dirty buffers. |
| `restore_session` | boolean | `true` | Reopen the previous session's tabs on launch. |

## `[appearance]` — theme and window

| Key | Type | Default | Description |
|---|---|---|---|
| `theme` | string | `"wired-noir"` | Theme name: a built-in scheme or the file stem of a user theme in your themes directory. See [THEMING.md](THEMING.md). |
| `follow_os_theme` | boolean | `true` | Follow the OS dark/light preference. |
| `frameless` | boolean | `true` | Use a frameless window with the custom brand titlebar (no OS title bar). Set `false` for standard OS window decorations. |
| `toolbar_icons` | boolean | `false` | Render the quick-access toolbar as Phosphor (Thin) icon glyphs instead of text labels. |
| `jp_glyph_labels` | boolean | `false` | Append a small, dim, English-redundant kanji to each toolbar action whose canonical Japanese term is verified (e.g. New → 新, Save → 保, Find → 検). Actions whose canonical kanji is uncertain (open-folder, palette, CRT, LSP) stay English-only — Folklore-Consultant gate (DECISION-2026-005). |

## `[fonts]` — typography

JetBrains Mono Regular is **bundled** with the binary (OFL-1.1, see `assets/fonts/JetBrainsMono/OFL.txt`) and registered as the primary Monospace family. egui renders via ab_glyph which does not perform OpenType shaping, so JetBrains Mono ligatures are inherently OFF (no config knob; the shaper does not support enabling them).

| Key | Type | Default | Description |
|---|---|---|---|
| `editor_family` | array of strings | `["JetBrains Mono", "Cascadia Code", "Consolas"]` | Ordered fallback list of monospace families for the editing surface. JetBrains Mono ships bundled; other entries fall through to the system font registry. |
| `ui_family` | array of strings | `["Inter"]` | Ordered fallback list of families for UI chrome (tabs, status bar, menus). |
| `editor_size` | float | `14.0` | Editor font size in points. |
| `line_height` | float | `1.4` | Line height as a multiple of the font size. |
| `ligatures` | boolean | `true` | Enable programming ligatures where the font supports them. |

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
show_change_bar = true
show_minimap = true
word_wrap = false
auto_save = false
restore_session = true

[appearance]
theme = "wired-noir"
follow_os_theme = true
frameless = true

[fonts]
editor_family = ["JetBrains Mono", "Cascadia Code", "Consolas"]
ui_family = ["Inter"]
editor_size = 14.0
line_height = 1.4
ligatures = true

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
theme = "wired-noir"

[editor]
tab_width = 2
```
