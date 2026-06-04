# Privacy

SCR1B3 is **telemetry-free by construction**. This document is the full
transparency record: exactly which network calls the app makes, exactly
what it sends, what it doesn't, how to disable the one optional call,
where state is stored, and how to clear it.

## Summary

| Question | Answer |
|---|---|
| Does SCR1B3 ship telemetry? | **No.** Not analytics, not usage counters, not crash beacons. |
| Does SCR1B3 have an account system? | **No.** There is no account, no sign-in, no profile. |
| Does SCR1B3 phone home? | **One optional network call only** — the update version-check (described below). The default mode makes **no** call at startup. |
| Does SCR1B3 collect a unique identifier? | **No** install-id, no fingerprint, no per-session ID. |
| Where is my data stored? | **Locally only.** On your machine. Nothing leaves the device. |
| Can I run fully offline? | **Yes.** The default mode (`manual`) makes no network call until you click *Check for updates*; `[updates] mode = "off"` removes the check entirely. |

## The single network surface

The **only** outbound network call SCR1B3 ever makes is the optional
update version-check. It:

- Asks the GitHub Releases API for the latest release tag of this repo.
- Sends **zero PII** — it is an unauthenticated HTTPS GET that asks
  "what is the latest release?" and nothing else. No identifiers, no
  telemetry, no shipped token.
- Receives a JSON document describing the latest release.
- Is **fully user-controllable** via `[updates] mode` in your config:

  | Mode | Behavior |
  |---|---|
  | `off` | No update check ever. Zero network calls. |
  | `manual` | **Default.** No automatic check; SCR1B3 checks only when you click *Check for updates* in Settings. |
  | `notify` | Check once at startup; if a newer version exists, show a passive notification. Never auto-download. |
  | `auto` | Check once at startup; if newer, ask yes/no before downloading + verifying + installing. |

  Default: `manual` — no network at startup. Change in
  [`CONFIG.md`](CONFIG.md); the change takes effect on next start (or
  instantly via live-reload).

- When a signed update is fetched (in `auto` mode, or when you click
  *Update now*), it is **cryptographically verified** via minisign /
  ed25519 against the pinned signing key embedded in the binary before
  any file is touched on disk. A failed verification aborts the update;
  the staging area is wiped and no partial state is left.

That's it. There is no other outbound surface.

## What is NOT sent

- ❌ No PII (name, email, account)
- ❌ No machine identifier (install-id, hardware ID, MAC)
- ❌ No telemetry payload (usage counters, command-frequency, error pings)
- ❌ No filesystem path or file-content
- ❌ No referrer / locale / timezone (beyond what the OS HTTPS stack
  itself signals — User-Agent is set to `scr1b3-updater/<version>` only)
- ❌ No crash reports

## Local state

SCR1B3 stores state in standard per-OS directories (resolved via the
`directories` crate):

| Class | Windows | macOS | Linux |
|---|---|---|---|
| Config (TOML) | `%APPDATA%\scr1b3\config.toml` | `~/Library/Application Support/scr1b3/config.toml` | `~/.config/scr1b3/config.toml` |
| Themes | `…\scr1b3\themes\` | `…/scr1b3/themes/` | `~/.config/scr1b3/themes/` |
| Plugin pinned keys (TOFU) | `…\scr1b3\plugin-keys\` | `…/scr1b3/plugin-keys/` | `~/.config/scr1b3/plugin-keys/` |
| Recent files / session | `%LOCALAPPDATA%\scr1b3\session.toml` | `~/Library/Caches/scr1b3/session.toml` | `~/.cache/scr1b3/session.toml` |
| Logs | `%LOCALAPPDATA%\scr1b3\logs\` | `~/Library/Logs/scr1b3/` | `~/.cache/scr1b3/logs/` |

No data is written outside these directories.

## Plugins

Plugins run inside an embedded Rhai interpreter that is **sandboxed by
construction**:

- No ambient filesystem access — the v1 host exposes only buffer-text
  operations to scripts, so there is nothing for a script to read or
  write on disk.
- No ambient network access.
- Bounded by an operation count + wall-clock deadline so a runaway
  script cannot hang the editor.

A plugin only runs after you approve its exact entry script
(trust-on-first-use by SHA-256), or — with `[plugins] require_signed`
— after a minisign signature over the script verifies under a pinned
author key. A new or silently-changed script is held back until you
approve it again. See [`PLUGINS.md`](PLUGINS.md) for the full sandbox
boundary.

## Clearing local state

To erase everything SCR1B3 ever wrote on disk:

```bash
# Windows (PowerShell)
Remove-Item -Recurse "$env:APPDATA\scr1b3", "$env:LOCALAPPDATA\scr1b3"

# macOS
rm -rf ~/Library/Application\ Support/scr1b3 \
       ~/Library/Caches/scr1b3 \
       ~/Library/Logs/scr1b3

# Linux
rm -rf ~/.config/scr1b3 ~/.cache/scr1b3
```

The editor will start fresh on next launch.

## Verifying these claims

You don't have to take this document at its word:

- Read the source. The single update-check call lives in
  [`crates/scribe-core/src/update/`](crates/scribe-core/src/update/);
  the network-shape is one HTTPS GET, no body, no payload.
- Read [`SECURITY.md`](SECURITY.md) for the threat model and signed-
  update integrity story.
- Audit the binary with your favorite traffic monitor. With
  `[updates] mode = "off"`, SCR1B3 makes zero outbound connections.

## Changes to this document

Any change to the network surface, the storage layout, or the
plugin-sandbox boundary is a breaking change to this privacy posture
and will be called out in the [`CHANGELOG`](https://github.com/46b-ETYKiAL/Itasha.Corp_S4F3-SCR1B3/releases)
under a "Privacy-relevant change" heading.

## Reporting concerns

If you find a discrepancy between this document and SCR1B3's actual
behavior — including the network surface, what data it stores, where
it stores it, or what a plugin can reach — please follow the
disclosure process in [`SECURITY.md`](SECURITY.md). Privacy issues
are treated as security-severity.
