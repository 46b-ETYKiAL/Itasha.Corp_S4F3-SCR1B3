# ADR 0003 — Configuration Format

**Status:** Accepted

## Context

SCR1B3's "deep customization without bloat" pillar requires a configuration format that is human-friendly, has excellent defaults, supports partial overrides, and — critically — is **not a programming language**, so config can never become a code-execution attack surface. It must also support live reload so changes apply without a restart.

## Decision

Configuration uses **TOML** with **live reload**.

- **TOML**, parsed via `serde` + the `toml` crate. It is readable, well-specified, and widely understood.
- **`#[serde(default)]` everywhere.** A partial user file merges onto the full default config rather than failing. You only write the keys you want to change.
- **Never panics.** A malformed file returns a parse error; the editor falls back to defaults and surfaces the error in-app. A missing file silently uses defaults. The editor always starts.
- **Live reload** via a filesystem watcher (`notify` + a debouncer), so saving the config applies changes immediately.
- **Per-OS location** resolved via the `directories` crate: `%APPDATA%\ItashaCorp\scr1b3\config\scr1b3.toml` (Windows), `~/.config/scr1b3/scr1b3.toml` (Linux), `~/Library/Application Support/com.ItashaCorp.scr1b3/scr1b3.toml` (macOS).

Config is partitioned into seven tables — `[editor]`, `[appearance]`, `[fonts]`, `[effects]`, `[updates]`, `[spellcheck]`, `[plugins]` — each with serde defaults. Themes use a separate TOML schema (see ADR 0005).

## Consequences

- Config is **data, not code** — no execution surface, satisfying the security posture.
- Robust against partial and malformed input; the editor is never bricked by a bad config.
- Customization is discoverable and documented key-by-key (see `CONFIG.md`).
- A schema is versioned from v1 with a documented stability contract; new keys are additive with defaults so old config files keep working.
