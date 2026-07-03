# Privacy

SCR1B3 is **telemetry-free by default**. It ships **no** background analytics and
**no** usage counters. The two network surfaces that exist — the optional update
version-check, and **opt-in, default-OFF** crash/error reporting — are both under
your explicit control and previewable before anything is sent. This document is
the full transparency record: exactly which network calls the app can make,
exactly what each sends, what it never sends, how to disable them, where state is
stored, and how to clear it.

## Summary

| Question | Answer |
|---|---|
| Does SCR1B3 ship usage analytics? | **No.** No analytics, no usage counters, no background reporting of any kind. Usage telemetry is out of scope. |
| Does SCR1B3 report crashes? | **Only if you opt in.** Crash/error reporting is **off by default**, per-stream, and nothing is captured for transmission until you turn it on **and** consent to a specific report. See [Opt-in reporting](#opt-in-crash--error-reporting). |
| Does SCR1B3 have an account system? | **No.** There is no account, no sign-in, no profile. |
| Does SCR1B3 phone home on its own? | **Only a single update version-check.** The update check defaults to `notify` — a single telemetry-free GitHub-Releases version check at startup (no PII, no auto-download) — and reporting is default-OFF. Set `[updates] mode = "off"` for **zero** startup network calls. |
| Does SCR1B3 collect a unique identifier? | **No** install-id, no fingerprint, no per-session ID — for either the update check or a report. |
| Where is my data stored? | **Locally only.** Reports are spooled on your machine and leave it **only** on your explicit consent. |
| Can I run fully offline? | **Yes.** With `[updates] mode = "off"` and reporting left off (the default), SCR1B3 makes zero outbound connections. |

## Network surface 1 — the optional update version-check

The update version-check:

- Asks the GitHub Releases API for the latest release tag of this repo.
- Sends **zero PII** — an unauthenticated HTTPS GET asking "what is the latest
  release?" and nothing else. No identifiers, no shipped token.
- Receives a JSON document describing the latest release.
- Is **fully user-controllable** via `[updates] mode` in your config:

  | Mode | Behavior |
  |---|---|
  | `off` | No update check ever. Zero network calls. |
  | `notify` | **Default.** Check once at startup; if a newer version exists, show a passive notification. Never auto-download. |
  | `manual` | No automatic check; SCR1B3 checks only when you click *Check for updates* in Settings. |
  | `auto` | Check once at startup; if newer, ask yes/no before downloading + verifying + installing. |

  Change in [`CONFIG.md`](CONFIG.md); the change takes effect on next start (or
  instantly via live-reload).

- When a signed update is fetched (in `auto` mode, or when you click *Update
  now*), it is **cryptographically verified** via minisign / ed25519 against the
  pinned signing key embedded in the binary before any file is touched on disk. A
  failed verification aborts the update; the staging area is wiped and no partial
  state is left.

## Opt-in crash / error reporting

SCR1B3 can help us fix the bugs that crash it — but **only with your explicit,
per-stream consent**. Reporting is provided by the in-house
[W1TN3SS](https://github.com/46b-ETYKiAL/Itasha.Corp_S4F3-W1TN3SS) SDK
(`itasha-report-core`, pinned by exact git tag — no third-party crash-reporting
SaaS, no vendor SDK). SCR1B3 implements no reporting behavior itself; it calls the
audited SDK at a few thin seams.

**The posture, accurate to what SCR1B3 ships today:**

- **Off by default, per stream.** Crash/error reporting and manual issue
  reporting are two separate consented streams, each defaulting to off
  (`ReportingMode::Off`). A config written before this feature upgrades with
  reporting fully off and nothing overwritten. There is no on-by-default path and
  no opt-out default.
- **Local-first, consent-gated.** When reporting is on, a fault is captured into a
  sanitized text report and written to a **local spool**. It transmits
  **nothing** on its own — transmission requires a consent token that exists only
  after you explicitly agree (enforced at the type level by the SDK, so a
  consent-free send cannot even compile).
- **Previewable + redactable.** The consent dialog shows you the **literal,
  editable** report text so you can review and redact it before anything leaves
  the machine.
- **Sanitized.** Backtraces are stripped by the SDK's allowlist sanitizer: home
  directory normalized to `<HOME>`, username and hostname dropped, environment
  values scrubbed, fields size-capped. The panic path captures only the
  `&'static str` message and our own `file:line` site — a `String` payload that
  could embed buffer text or a path is deliberately suppressed at capture, so
  **your note content is never read into a report**.
- **No persistent identifier.** A report carries no install-id, no fingerprint,
  no per-session ID — only an ephemeral per-report nonce that is never stored.
- **No hardcoded endpoint.** SCR1B3 ships **no** report endpoint and no default
  URL. A build with none configured can spool locally but can **never** transmit;
  a consented send with no endpoint stays spooled and returns a structured
  `no-endpoint` outcome — never a silent drop, never a fake success.
- **Pseudonymous, not anonymous.** A stack trace can carry indirect identifiers,
  so we label report data honestly as **pseudonymous** under GDPR — never
  marketed as "anonymous." The full legal classification is in the W1TN3SS
  [privacy policy](https://github.com/46b-ETYKiAL/Itasha.Corp_S4F3-W1TN3SS/blob/main/docs/privacy-policy.md).

For the canonical, fleet-wide detail — the GDPR privacy policy, the concrete
"what we collect / what we never collect" page, and the end-to-end-encryption
(developer-key) model — see the W1TN3SS docs:

- [W1TN3SS privacy policy](https://github.com/46b-ETYKiAL/Itasha.Corp_S4F3-W1TN3SS/blob/main/docs/privacy-policy.md)
- [What we collect / never collect](https://github.com/46b-ETYKiAL/Itasha.Corp_S4F3-W1TN3SS/blob/main/docs/what-we-collect.md)

## Manual issue reporting

SCR1B3's in-app **Report an issue** dialog is always user-initiated and
per-submission. It deep-links into the fleet's shared GitHub Issue-Form templates
in your browser (with a clipboard fallback when the prefilled URL would be too
long). Nothing is submitted in the background — you fill out and submit the form
yourself.

## What is NOT sent

Across every surface above, SCR1B3 never sends:

- ❌ No usage analytics (usage counters, command-frequency, error "pings")
- ❌ No machine identifier (install-id, hardware ID, MAC, fingerprint)
- ❌ No persistent or per-session ID (a report carries only an ephemeral nonce)
- ❌ No retained client IP (the W1TN3SS ingest service drops it at the edge)
- ❌ No raw note / buffer / file content
- ❌ No PII (name, email, account — there is no account)
- ❌ No referrer / locale / timezone beyond what the OS HTTPS stack itself signals
  (User-Agent is `scr1b3-updater/<version>` for the update check, or
  `itasha-report-core/<version>` for a consented report)

## Local state

SCR1B3 stores state in standard per-OS directories (resolved via the
`directories` crate):

| Class | Windows | macOS | Linux |
|---|---|---|---|
| Config (TOML) | `%APPDATA%\ItashaCorp\scr1b3\config\scr1b3.toml` | `~/Library/Application Support/com.ItashaCorp.scr1b3/scr1b3.toml` | `~/.config/scr1b3/scr1b3.toml` |
| Themes | `%APPDATA%\ItashaCorp\scr1b3\config\themes\` | `~/Library/Application Support/com.ItashaCorp.scr1b3/themes/` | `~/.config/scr1b3/themes/` |
| Plugin pinned keys (TOFU) | `%APPDATA%\ItashaCorp\scr1b3\config\plugins\pinned-keys.toml` | `~/Library/Application Support/com.ItashaCorp.scr1b3/plugins/pinned-keys.toml` | `~/.config/scr1b3/plugins/pinned-keys.toml` |
| Crash/error report spool | `%APPDATA%\ItashaCorp\scr1b3\config\reports\` | `~/Library/Application Support/com.ItashaCorp.scr1b3/reports/` | `~/.config/scr1b3/reports/` |
| Session manifest + unsaved-buffer backups | `%APPDATA%\ItashaCorp\scr1b3\config\session.json` (+ `…\config\backup\`) | `~/Library/Application Support/com.ItashaCorp.scr1b3/session.json` (+ `…/backup/`) | `~/.config/scr1b3/session.json` (+ `~/.config/scr1b3/backup/`) |

The recent-files list is not a separate file — it is stored inside
`scr1b3.toml` (`[editor] recent_files`). Structured logs are emitted to
**stderr** (RUST_LOG-controlled; default `warn`) and are **not written to
disk** — the app writes no cache directory.

A spooled report stays in `reports/` until you consent to send it or you clear
local state. No data is written outside these directories.

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

To erase everything SCR1B3 ever wrote on disk (including any spooled reports):

```bash
# Windows (PowerShell)
Remove-Item -Recurse "$env:APPDATA\ItashaCorp\scr1b3"

# macOS
rm -rf ~/Library/Application\ Support/com.ItashaCorp.scr1b3

# Linux
rm -rf ~/.config/scr1b3
```

The editor will start fresh on next launch.

## Verifying these claims

You don't have to take this document at its word:

- Read the source. The update-check call lives in
  [`crates/scribe-core/src/update/`](crates/scribe-core/src/update/); the
  reporting host glue is in
  [`crates/scribe-app/src/reporting.rs`](crates/scribe-app/src/reporting.rs); the
  sanitizer, spool, preview, and consent gate live in the public W1TN3SS SDK and
  are auditable there.
- Read [`SECURITY.md`](SECURITY.md) for the threat model and signed-update
  integrity story.
- Audit the binary with your favorite traffic monitor. With `[updates] mode =
  "off"` and reporting left off (the default), SCR1B3 makes zero outbound
  connections.

## Changes to this document

Any change to a network surface, the storage layout, the reporting posture, or
the plugin-sandbox boundary is a change to this privacy posture and will be
called out in the [`CHANGELOG`](https://github.com/46b-ETYKiAL/Itasha.Corp_S4F3-SCR1B3/releases)
under a "Privacy-relevant change" heading.

## Reporting concerns

If you find a discrepancy between this document and SCR1B3's actual behavior —
including the network surface, what data it stores, where it stores it, or what a
plugin can reach — please follow the disclosure process in
[`SECURITY.md`](SECURITY.md). Privacy issues are treated as security-severity.
