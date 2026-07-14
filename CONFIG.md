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

Most settings are grouped into twelve tables: `[editor]`, `[appearance]`, `[fonts]`, `[window]`, `[updates]`, `[spellcheck]`, `[plugins]`, `[toolbar]`, `[motion]`, `[scroll]`, `[reporting]`, `[integration]`. Three keys live at the top level, before any table — see below.

---

## Top-level keys

These sit at the top of the file, above the first `[table]` header.

| Key | Type | Default | Description |
|---|---|---|---|
| `schema_version` | integer | `4` | Config-format version, used for one-time migrations. Written automatically; you never need to set it by hand. A file written before versioning existed reads as `0` and is migrated forward exactly once on load. **Do not hand-edit this downward** — it does not downgrade your file, it just re-runs migrations. Current value: `CURRENT_SCHEMA_VERSION = 4`. |
| `ui_scale` | float | `1.0` | Whole-app accessibility zoom, applied to every panel and the chrome (distinct from `fonts.editor_size`, which scales only the editor text). Clamped to `0.5`–`3.0`; a nonsensical value falls back to `1.0` rather than blanking the window. |

### `[keybindings]` — rebindable shortcuts

Every action below can be rebound to any key combo. Only the keys you list are overridden; the rest keep their defaults.

A combo is `+`-separated modifiers followed by one key, e.g. `mod+shift+f`:

- `mod` — the platform command modifier: **Ctrl** on Windows/Linux, **Cmd** on macOS. (`ctrl`, `control`, `cmd`, and `command` are accepted as aliases.)
- `shift`, `alt` (`option` is an alias for `alt`).
- The key: a letter (`f`), a digit (`num0`), a function key (`f11`), an arrow (`arrowup`), or a named key (`tab`, `period`, `slash`, `backslash`, `openbracket`, `closebracket`, `equals`, `minus`). Case does not matter.

Modifiers must match **exactly**: `mod+o` (open file) does not fire when Shift is held, which is what keeps it distinct from `mod+shift+o` (go to symbol).

Two combos that mean the same chord — `mod+shift+f` and `ctrl+shift+f` — collide, and a combo that names no key (`mod`) or two keys (`a+b`) can never fire. Settings surfaces both as warnings rather than letting the shortcut silently do nothing.

`F1` (cheatsheet), `Esc` (close overlay), `F3` / `Shift+F3` (find next/previous) and `Ctrl+scroll` (font zoom) are fixed and not rebindable.

| Action | Default | Description |
|---|---|---|
| `new_file` | `mod+n` | New file / tab. |
| `open_file` | `mod+o` | Open a file. |
| `save` | `mod+s` | Save the active file. |
| `find` | `mod+f` | Open the in-buffer find bar. |
| `find_in_files` | `mod+shift+f` | Open project-wide find. |
| `replace` | `mod+h` | Open find-and-replace. |
| `command_palette` | `mod+shift+p` | Open the command palette. |
| `fuzzy_finder` | `mod+p` | Open the fuzzy file finder. |
| `goto_line` | `mod+g` | Go to line. |
| `goto_symbol` | `mod+shift+o` | Go to symbol in the active buffer. |
| `recent_files` | `mod+r` | Open the recent-files list. |
| `close_tab` | `mod+w` | Close the active tab. |
| `next_tab` | `mod+tab` | Cycle to the next tab. |
| `prev_tab` | `mod+shift+tab` | Cycle to the previous tab. |
| `reopen_tab` | `mod+shift+r` | Reopen the most recently closed tab. |
| `toggle_grid` | `mod+backslash` | Toggle the multi-note grid. |
| `toggle_comment` | `mod+slash` | Toggle line comments on the selection. |
| `jump_bracket` | `mod+m` | Jump to the matching bracket. |
| `toggle_fullscreen` | `f11` | Toggle OS fullscreen. |
| `toggle_zen` | `mod+period` | Toggle zen / distraction-free mode. |
| `cycle_theme` | `mod+shift+t` | Cycle to the next theme. |
| `toggle_minimap` | `mod+shift+m` | Toggle the minimap. |
| `toggle_md_preview` | `mod+shift+v` | Toggle the markdown live-preview panel. |
| `fold_all` | `mod+shift+openbracket` | Fold every region in the active buffer. |
| `expand_all` | `mod+shift+closebracket` | Expand every folded region. |
| `increase_font` | `mod+equals` | Increase the editor font size. Also fires on `mod++`. |
| `decrease_font` | `mod+minus` | Decrease the editor font size. |
| `reset_font` | `mod+num0` | Reset the editor font size. |
| `move_line_up` | `alt+arrowup` | Move the current line up. |
| `move_line_down` | `alt+arrowdown` | Move the current line down. |
| `duplicate_line` | `mod+shift+d` | Duplicate the current line. |
| `join_lines` | `mod+j` | Join the next line onto the current one. |
| `toggle_bookmark` | `mod+f2` | Toggle a bookmark on the cursor line. |
| `next_bookmark` | `f2` | Jump to the next bookmark. |
| `prev_bookmark` | `shift+f2` | Jump to the previous bookmark. |

```toml
[keybindings]
save = "mod+e"          # rebind Save to Ctrl+E
toggle_zen = "f10"      # zen mode on F10
```

---

## `[editor]` — editing behavior

| Key | Type | Default | Description |
|---|---|---|---|
| `tab_width` | integer | `4` | Width of a tab stop, in columns. |
| `insert_spaces` | boolean | `true` | Insert spaces instead of a tab character when pressing Tab. |
| `show_line_numbers` | boolean | `true` | Show the line-number gutter. |
| `show_change_bar` | boolean | `true` | Notepad++-style change bar in the gutter: amber marks lines edited but unsaved, green marks edited-then-saved lines, untouched lines have none. |
| `show_minimap` | boolean | `true` | Show the minimap. |
| `word_wrap` | boolean | `true` | Soft-wrap long lines to the viewport width. |
| `highlight_selection_occurrences` | boolean | `true` | When text is selected, faintly box every other matching run in the viewport. |
| `highlight_trailing_whitespace` | boolean | `false` | Tint trailing spaces/tabs on each line (distinct from rendering all whitespace). |
| `rulers` | array of integers | `[]` | Vertical guide rulers at these 1-based columns, e.g. `[80, 100]`. Empty = none. |
| `auto_save` | boolean | `false` | Automatically save dirty buffers. |
| `restore_session` | boolean | `true` | Reopen the previous session's tabs on launch. |
| `note_theme` | string | `"base16-eighties.dark"` | Colour scheme for the note text / syntax highlighting, separate from the app chrome theme. One of the bundled note themes; an unknown value falls back to the default. See [THEMING.md](THEMING.md) § Note (editor text) colour themes. |
| `syntax_from_theme` | boolean | `false` | Drive in-editor token colours from the active chrome theme's `[syntax]` map instead of `note_theme`. Off by default — `note_theme` stays authoritative unless you opt in. See [THEMING.md](THEMING.md). |
| `detect_links` | boolean | `true` | Detect bare `http(s)://` URLs in the text and render them as a coloured, underlined, Ctrl/Cmd-click-to-open link (scheme allow-listed to http/https). |
| `md_rich_coloring` | boolean | `true` | Master switch for the extra markdown token-colouring passes (dividers, `#tags`, `~~strikethrough~~`, task boxes, table pipes). Off disables all of them; the per-token keys below tune individual passes while the master is on. |
| `md_color_dividers` | boolean | `true` | Colour decorative divider lines (`----`, `====//====//`, `* * *`, setext underlines, box-drawing rules). Only active when `md_rich_coloring` is on. |
| `md_color_tags` | boolean | `true` | Colour `#tag` tokens. Only active when `md_rich_coloring` is on. |
| `md_color_strikethrough` | boolean | `true` | Colour `~~strikethrough~~` spans. Only active when `md_rich_coloring` is on. |
| `md_color_task_boxes` | boolean | `true` | Colour task checkboxes `[ ]`/`[x]`. Only active when `md_rich_coloring` is on. |
| `md_color_table_pipes` | boolean | `true` | Colour table `\|` cell separators. Only active when `md_rich_coloring` is on. |

## `[appearance]` — theme and window

| Key | Type | Default | Description |
|---|---|---|---|
| `theme` | string | `"itasha-corp"` | Theme name: a built-in scheme or the file stem of a user theme in your themes directory. A broken/unknown theme falls back to `wired-noir`. See [THEMING.md](THEMING.md). |
| `follow_os_theme` | boolean | `true` | Follow the OS dark/light preference. |
| `frameless` | boolean | `true` | Use a frameless window with the custom brand titlebar (no OS title bar). Set `false` for standard OS window decorations. |
| `toolbar_icons` | boolean | `false` | Render the quick-access toolbar as Phosphor (Thin) icon glyphs instead of text labels. |
| `jp_glyph_labels` | boolean | `false` | Append a small, dim, English-redundant kanji to each toolbar action whose canonical Japanese term is verified (e.g. New → 新, Save → 保, Find → 検). Actions whose canonical kanji is uncertain (open-folder, palette, CRT, LSP) stay English-only — Folklore-Consultant gate (DECISION-2026-005). |

## `[window]` — window translucency and chrome

Translucency is **off by default** — a normal opaque window. When `transparency_enabled` is on, the frameless surface reveals the desktop through the translucent panels; there is no OS glass/mica/vibrancy backdrop mode (the DWM materials re-added native caption buttons over the custom titlebar and were collapsed to a single toggle).

| Key | Type | Default | Description |
|---|---|---|---|
| `transparency_enabled` | boolean | `false` | Master toggle for window translucency — the single predicate every render path consults. |
| `mode` | string | `"opaque"` | Legacy field retained for config back-compat only. It no longer selects a surface — `transparency_enabled` is the single predicate. Accepted values (`opaque`/`transparent`/`glass`/`mica`/`vibrancy`) parse but have no visual effect. |
| `opacity` | float | `1.0` | Window opacity when translucent (0.0–1.0; the 0.0 floor lets the window go fully transparent). Fresh-configs-only: an existing config keeps whatever value it already stored — only a brand-new config picks up the `1.0` default. |
| `tint_enabled` | boolean | `true` | Master on/off switch for the window colour tint. When off, no tint is applied regardless of `tint`/`tint_strength` (so you can toggle the effect without losing your chosen colour + strength). The tint only shows once `tint_strength` is above 0. |
| `tint` | string | `"#08060d"` | Hex `#rrggbb` colour tint overlaid on the translucent window. |
| `tint_strength` | float | `0.0` | Strength of the tint overlay (0.0 = none). |
| `always_on_top` | boolean | `false` | Keep the window above other windows. |

## `[fonts]` — typography

IBM Plex Mono is **bundled** with the binary and used as the default editor and UI face. egui renders via ab_glyph which does not perform OpenType shaping, so there is **no `ligatures` option** — ligature substitution requires a shaping engine egui does not have, so the flag could never do anything.

| Key | Type | Default | Description |
|---|---|---|---|
| `editor_family` | string | `"IBM Plex Mono"` | Monospace "font theme" for the editing surface. One of the bundled (OFL-licensed) family display names; an unknown value falls back to the default. |
| `ui_family` | string | `"IBM Plex Mono"` | UI-chrome font family (toolbar, settings, status bar, menus). A bundled family name, or `"System default"` to keep egui's built-in UI font. |
| `editor_size` | float | `14.0` | Editor font size in points. |
| `line_height` | float | `1.2` | Line height as a multiple of the font size. |

## `[updates]` — telemetry-free auto-update

The update check contacts **only** the public GitHub Releases API and sends **zero PII** — no analytics, no custom server, no shipped token. Downloads are cryptographically verified before any swap (see [SECURITY.md](SECURITY.md)).

| Key | Type | Default | Description |
|---|---|---|---|
| `mode` | string | `"notify"` | One of: `off` (never check), `notify` (check and tell you), `manual` (check only when you ask), `auto` (download + verify + apply automatically). |
| `check_interval_hours` | integer | `24` | Hours between background version checks. |

## `[spellcheck]` — privacy-respecting offline spellcheck

Spellcheck is **on by default** and **fully offline** — no network, no cloud. It is code-aware: it checks comments and strings while leaving code identifiers alone.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | boolean | `true` | Master toggle for spellcheck. |
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
| `require_signed` | boolean | `false` | Strict mode: when on, a plugin must additionally carry a valid minisign signature over its entry script (the exact bytes that execute) from a pinned author key. Off by default so existing unsigned local script plugins keep working under the trust-on-first-use (TOFU) gate. |
| `trusted` | table | `{}` | Trust-on-first-use approvals (`plugin id` → approved entry-script SHA-256). TOFU-managed state — populated when you approve a discovered plugin; not normally hand-edited. |

## `[toolbar]` — customizable quick-access toolbar

`items` is an ordered list of action ids (the id `"sep"` renders a divider). Reorder/add/remove entries from Settings → Toolbar; the layout persists here.

| Key | Type | Default | Description |
|---|---|---|---|
| `items` | array of strings | `["new","open","save","sep","find","palette","sep","split","minimap","wrap","sep","spellcheck"]` | Ordered toolbar action ids; `"sep"` is a divider. |
| `menu` | array of strings | `[]` | Action ids parked in a single "⋯" overflow dropdown instead of taking a slot. |
| `show_dropdown` | boolean | `true` | Show the "⋯" overflow dropdown (when `menu` is non-empty). |
| `button_size_px` | float | `24.0` | Minimum button height in logical pixels (clamped 16–64). |
| `button_spacing_px` | float | `6.0` | Horizontal spacing between items in logical pixels (clamped 0–24). |
| `icon_size_px` | float | `14.0` | Icon glyph size in logical pixels (clamped 10–32; only used when `appearance.toolbar_icons` is on). |

## `[motion]` — animation and CRT ambience

Subtle motion is **on by default**; the CRT/VHS effects are individually **off by default**. Turn `enabled` off for a fully static surface.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | boolean | `true` | Master toggle for animations/motion. |
| `intensity` | float | `0.6` | UI transition speed — scales egui's chrome-transition time (hover fades, panel/collapsible expand-collapse, value-change lerps). Does NOT control the retro effects. Shown in Settings as **UI transition speed**. |
| `cursor_blink` | boolean | `true` | Blink the text caret. |
| `crt_scanlines` | boolean | `false` | CRT scanline overlay. |
| `scanline_darkness` | float | _(tuned)_ | Strength of the scanline overlay. |
| `flicker` | boolean | `false` | CRT flicker. |
| `flicker_strength` | float | _(tuned)_ | Strength of the flicker. |
| `flicker_speed` | float | `1.0` | Flicker cadence multiplier (0.25–3.0). `1.0` reproduces the shipped rate. |
| `vhs_tracking` | boolean | `false` | VHS tracking-line artifact. |
| `vhs_speed` | float | `1.0` | VHS tracking-band drift multiplier (0.25–3.0). `1.0` reproduces the shipped drift. |
| `wired_ambient` | boolean | `false` | Ambient "wired" mesh background. |
| `mesh_density` | float | _(tuned)_ | Density of the ambient mesh. |
| `mesh_drift_speed` | float | `1.0` | Wired-mesh node-drift multiplier (0.25–3.0). `1.0` reproduces the shipped drift. |
| `caret_trail` | boolean | `false` | Trailing afterglow on the caret. |
| `boot_glitch` | boolean | `false` | One-shot glitch on launch. |

## `[scroll]` — scrolling behavior

| Key | Type | Default | Description |
|---|---|---|---|
| `speed` | float | `75.0` | Mouse-wheel scroll speed (clamped to the settings band). |
| `animate_jumps` | boolean | `true` | Animate large scroll jumps. |
| `autoscroll` | boolean | `true` | Middle-button autoscroll. |
| `autoscroll_sensitivity` | float | `6.0` | Autoscroll speed sensitivity. |
| `autoscroll_dead_zone` | float | `12.0` | Pixels of dead-zone around the autoscroll origin. |

## `[reporting]` — opt-in W1TN3SS crash/error reporting

Both streams default **`off`** — SCR1B3 stays telemetry-free unless you opt in. See [PRIVACY.md](PRIVACY.md) § opt-in reporting. The two streams are never bundled under one toggle.

| Key | Type | Default | Description |
|---|---|---|---|
| `crash_reports` | string | `"off"` | Consent posture for the crash-report stream. One of `off`, `ask_each_time`, `always`. |
| `manual_issues` | string | `"off"` | Consent posture for the user-initiated "Report an issue" stream. One of `off`, `ask_each_time`, `always`. |
| `issue_intake.repo` | string | `"46b-ETYKiAL/Itasha.Corp_S4F3-W1TN3SS"` | `owner/repo` the prefilled GitHub Issue-Form deep link targets (operator-editable). |
| `issue_intake.mailto_alias` | string | _(support alias)_ | `mailto:` support alias for the "Email feedback instead" fallback. Empty disables it. |

## `[integration]` — OS default-app / file associations

Defaults **off** — SCR1B3 never registers as a file handler without an explicit action in **Settings → Default app**. Claim groups are `plain_text`, `markdown`, `json`, and `source_code`; registration is per-OS (Windows ProgID / macOS UTI / Linux MIME).

| Key | Type | Default | Description |
|---|---|---|---|
| `register_file_types` | boolean | `false` | Whether SCR1B3 is registered as a default file-type handler. No registration is performed until you turn this on in Settings. |
| `claimed_types` | array of strings | `[]` | Persisted claim-group keys (`plain_text`, `markdown`, `json`, `source_code`). Empty while registered means "the default set" (all four). |
| `last_registration_unix` | integer | _(unset)_ | Unix seconds of the last successful registration (status only). |

---

## Full example `scr1b3.toml`

This is a representative default configuration — see each table above for the full key list. Copy it as a starting point and trim it to only the keys you want to change.

```toml
[editor]
tab_width = 4
insert_spaces = true
show_line_numbers = true
show_change_bar = true
show_minimap = true
word_wrap = true
auto_save = false
restore_session = true
note_theme = "base16-eighties.dark"
syntax_from_theme = false
detect_links = true
md_rich_coloring = true
md_color_dividers = true
md_color_tags = true
md_color_strikethrough = true
md_color_task_boxes = true
md_color_table_pipes = true

[appearance]
theme = "itasha-corp"
follow_os_theme = true
frameless = true

[window]
transparency_enabled = false
mode = "opaque"
opacity = 1.0
tint_enabled = true
tint = "#08060d"
tint_strength = 0.0
always_on_top = false

[fonts]
editor_family = "IBM Plex Mono"
ui_family = "IBM Plex Mono"
editor_size = 14.0
line_height = 1.2

[updates]
mode = "notify"
check_interval_hours = 24

[spellcheck]
enabled = true
language = "en_US"
check_comments = true
check_strings = true
check_identifiers = false
# custom_dict_path = "/path/to/personal.dic"

[plugins]
enabled = true
disabled = []
require_signed = false

[toolbar]
items = ["new", "open", "save", "sep", "find", "palette", "sep", "split", "minimap", "wrap", "sep", "spellcheck"]
menu = []
show_dropdown = true
button_size_px = 24.0
button_spacing_px = 6.0
icon_size_px = 14.0

[motion]
enabled = true
intensity = 0.6
cursor_blink = true
crt_scanlines = false
flicker = false
vhs_tracking = false
wired_ambient = false
caret_trail = false
boot_glitch = false

[scroll]
speed = 75.0
animate_jumps = true
autoscroll = true
autoscroll_sensitivity = 6.0
autoscroll_dead_zone = 12.0

[reporting]
crash_reports = "off"
manual_issues = "off"

[integration]
register_file_types = false
claimed_types = []
```

### Minimal example

A real config rarely needs more than a few lines:

```toml
[appearance]
theme = "itasha-corp"

[editor]
tab_width = 2
```
