# Security Policy

SCR1B3 is built privacy-first and minimal-attack-surface by construction. This document describes that posture and how to report vulnerabilities.

## Telemetry-free posture

SCR1B3 collects **nothing** about you and transmits **no** file contents, ever.

- **No account, no login, no cloud.** The editor works fully offline.
- **No analytics, no usage counters, no crash beacons.** There is no phone-home.
- **File contents never leave your device.** Editing, search, syntax highlighting, and spellcheck are entirely local.
- **Local logs only.** Structured logs are written locally with off-by-default verbosity (controlled by the `RUST_LOG` environment variable) and are never transmitted.

### The single network surface

The **only** outbound network call SCR1B3 ever makes is the optional update version-check. It:

- Contacts **only** the public GitHub Releases API.
- Sends **zero PII** — it is an unauthenticated request that asks "what is the latest release?" and nothing else. No identifiers, no telemetry, no shipped token.
- Is **fully user-controllable** via `[updates] mode` in your config: `off`, `notify`, `manual`, or `auto`. Set it to `off` to disable all network activity. (See [CONFIG.md](CONFIG.md).)

## Signed-update verification

When an update is downloaded, SCR1B3 **cryptographically verifies it before applying it**:

- Release artifacts are signed (minisign / ed25519) and checksummed.
- The updater **refuses** any download whose signature does not verify or whose checksum does not match. An unsigned or tampered artifact is never installed.
- The previous binary is retained so an update can be rolled back in one step.
- The updater runs with your user permissions and writes only to the install directory — no privilege escalation, no setuid.

To verify a release manually, use the published `minisign` public key against the release's signature file (instructions accompany each release).

## Plugin capability-consent model

The user plugin/mod system is opt-in and sandboxed (see [PLUGINS.md](PLUGINS.md)):

- Plugins **declare the capabilities they need** (for example, reading a file, watching the buffer). You **approve** those capabilities before the plugin runs.
- Compiled extensions run in a **WASM sandbox**; the scripting "easy mode" runs in a restricted Lua environment.
- A plugin cannot silently gain network access, exfiltrate file contents, or escalate privileges — anything beyond its consented capabilities is denied.
- You can disable any plugin via `[plugins] disabled` or turn the whole system off with `[plugins] enabled = false`.

## Configuration is data, not code

The config and theme files are **TOML** — a data format with no code-execution surface. Malformed config or themes fall back to safe defaults with a surfaced error; they cannot run arbitrary code.

## Supply chain

- All dependencies are pinned via `Cargo.lock`.
- `cargo-audit` (advisory database) and `cargo-deny` (license + advisory policy) gate the build in CI.
- A CycloneDX SBOM is produced at release.
- Dependency names are checked against slopsquatting before any addition.

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately using GitHub's **[Private Vulnerability Reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)** (the "Report a vulnerability" button under the repository's Security tab). Include:

- A description of the issue and its impact.
- Steps to reproduce (a minimal proof of concept if possible).
- Affected version(s) and OS.

We aim to acknowledge reports promptly, keep you updated on remediation, and credit reporters who wish to be named once a fix ships. Please allow reasonable time for a fix before any public disclosure.

## Supported versions

Security fixes target the latest released version. Update via your package manager or the auto-update mechanism to stay current.
