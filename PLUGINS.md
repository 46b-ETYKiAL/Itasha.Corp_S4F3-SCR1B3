# Writing SCR1B3 Mods & Plugins

SCR1B3 is extensible by ordinary users — **no build step, no toolchain.** Drop a
folder with a `plugin.toml` and a `.rhai` script into your plugins directory and
SCR1B3 loads it on the next launch.

> Plugins directory: `<config>/plugins/` (a `plugins/` folder inside SCR1B3's
> config directory) — e.g.
> `%APPDATA%\ItashaCorp\scr1b3\config\plugins\` (Windows),
> `~/.config/scr1b3/plugins/` (Linux),
> `~/Library/Application Support/com.ItashaCorp.scr1b3/plugins/` (macOS).

## Two tracks

| Track | Language | Build step | Use for |
|-------|----------|-----------|---------|
| **Easy mode** (this guide) | [Rhai](https://rhai.rs) script | **none** | commands, on-save/open hooks, buffer transforms |
| **Power track** | any → WASM component (wasmtime/WIT) | yes (compile to `wasm32-wasip2`) | language grammars, LSP wiring, heavier extensions (reserved — not yet shipped; only the Rhai easy-mode runs today) |

The easy mode is sandboxed by construction: Rhai scripts have **no filesystem,
network, or process access**. They can only transform buffer text and surface
commands/notifications. Privileged capabilities (filesystem, network, spawning
a language server) are declared in `plugin.toml` and require explicit user
consent — none are granted to scripts in v1.

## Anatomy of a mod

```
my-mod/
├── plugin.toml      # manifest
└── main.rhai        # script
```

### `plugin.toml`

```toml
id = "my-mod"            # unique id
name = "My Mod"          # shown in the plugin manager
version = "0.1.0"
api_version = 1          # SCR1B3 plugin API version (current: 1)
kind = "script"          # "script" (Rhai) or "wasm"
entry = "main.rhai"      # script file, relative to this folder
capabilities = ["buffer"]  # what the mod may do
description = "What it does."
```

### `main.rhai`

A script declares its contributions at load time and defines handler functions:

```rhai
// A command that appears in the command palette.
fn cmd_shout() {
    set_buffer_text(buffer_text().to_upper());
    notify("done");
}
register_command("shout", "Shout (Uppercase)", "cmd_shout");

// A hook that runs on save / open / change.
fn on_save() { notify("saved " + buffer_text().len() + " chars"); }
on_event("save", "on_save");
```

## Host API (easy mode)

| Function | Description |
|----------|-------------|
| `buffer_text() -> string` | The current buffer contents. |
| `set_buffer_text(s)` | Replace the buffer contents. |
| `notify(s)` | Show a transient notification to the user. |
| `log(s)` | Write to the local debug log (never transmitted). |
| `register_command(id, label, fn_name)` | Add a palette command bound to a script function. |
| `on_event(event, fn_name)` | Run a function on a lifecycle event (`"open"`, `"save"`, `"change"`). |

Everything else is the [Rhai standard library](https://rhai.rs/book/) (strings,
arrays, maps, math) — all pure, all sandboxed.

## Examples

See [`examples/plugins/`](examples/plugins/): `uppercase` (a command) and
`wordcount` (a command + on-save hook). Copy one into your plugins directory to
try it.

## Capabilities & consent

`capabilities = ["buffer"]` is the only capability scripts use today. The
`filesystem_read`, `filesystem_write`, `network`, and `process` capabilities are
reserved for the WASM power track and will prompt for explicit user consent
before first use — SCR1B3 never grants ambient access, and **signatures alone
are not treated as safety**: consent is the gate.

## Versioning

`api_version` is checked against the SCR1B3 build. A mod requiring a newer API
than your editor supports is skipped (with a notice) rather than loaded — so an
out-of-date editor never runs a mod it can't host safely.
