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
- Compiled extensions run in a **WASM sandbox**; the scripting "easy mode" runs in a restricted Rhai engine with seven caps wired (operations, call depth, string size, array size, map size, modules, expression depth) plus a wall-clock deadline. Both the `eval` and `import` keywords are removed from the parser, and the module resolver is a no-op — a script using them fails to **compile**, never just at runtime.
- A plugin cannot silently gain network access, exfiltrate file contents, or escalate privileges — anything beyond its consented capabilities is denied.
- You can disable any plugin via `[plugins] disabled` or turn the whole system off with `[plugins] enabled = false`.

## Configuration is data, not code

The config and theme files are **TOML** — a data format with no code-execution surface. Malformed config or themes fall back to safe defaults with a surfaced error; they cannot run arbitrary code.

## Language-server (LSP) spawn discipline

Optional language servers run as **child processes** under your user identity, never under a shell:

- The server binary path is taken directly from your config (a single executable name or absolute path).
- Arguments pass to the child via `std::process::Command::args(&[...])` — one Rust array, not a shell string. Argv is delivered to the child unmodified.
- The toolchain pin is `rust-version = "1.80"` in `Cargo.toml`, well above the **1.77.2** floor that mitigates [CVE-2024-24576 ("BatBadBut")](https://nvd.nist.gov/vuln/detail/CVE-2024-24576) — the bug where Windows `.bat` / `.cmd` arguments could be reinterpreted by `cmd.exe`'s tokeniser. Newer rustc escapes them safely; our MSRV makes that escaping unconditional.
- The child's stdin / stdout are wired as pipes; stderr is **dropped** so a chatty language server cannot pollute the editor's own log channel.
- A missing or unspawnable server **degrades gracefully** to "no LSP for this language" rather than aborting — the editor is the trust boundary, not the language server.

## Plugin tarball verification (manifest + author-key pinning)

When a plugin ships via the registry (or via a signed URL install), the tarball is verified before any code runs:

1. **SHA-256 checksum** declared in the manifest is recomputed from the on-the-wire bytes. Mismatch → `"Plugin file is corrupted or has been modified since publication."` and the install aborts.
2. **minisign signature** is verified against the manifest-declared `author_pubkey`. The verification path is the **same** `update::verify` code that gates SCR1B3's auto-updater — one cryptographic surface, not two.
3. **Trust-on-first-use (TOFU) author-key pinning.** The first install records the manifest's `author_pubkey` in `<config_dir>/plugins/pinned-keys.toml`. Subsequent installs / updates of the **same plugin id** must present a byte-identical key. A different key triggers a **"Author key changed — accept new key?"** modal; silent rotation is refused. Same discipline as SSH `known_hosts` and OpenBSD `signify`.

The signed unit is the **whole gzipped tarball** — no metadata extraction, no canonicalisation, no reproducibility wizardry. The verifier rebuilds the bytes from the file on disk and either accepts or rejects.

## Unsafe-code discipline

Every SCR1B3 crate carries a crate-root attribute that gates `unsafe` usage:

- **`scribe-render`** — `#![forbid(unsafe_code)]`. Pure-safe Rust: theme → egui `Visuals` mapping, color math, CRT parameter conversion. Forbid is unconditional; no local override is reachable.
- **`scribe-app`** — `#![forbid(unsafe_code)]`. The egui/eframe shell never needs `unsafe`.
- **`scribe-core`** — `#![deny(unsafe_code)]` with a single documented exception in `document.rs` for the read-only `memmap2::Mmap::map` on the multi-GB-open path. The exception carries an explicit `#[allow(unsafe_code)]` annotation and a `SAFETY:` comment naming the invariants (read-only handle; dropped before any edit).

`forbid` is preferred where it is reachable because it **cannot be locally overridden** — a new `unsafe` block cannot land via `#[allow]`. `deny` is used only where a documented exception exists; the `#[allow(unsafe_code)]` is then visible per call-site so the unsafe budget is explicit, not implicit.

## Supply chain

- All dependencies are pinned via `Cargo.lock`.
- `cargo-audit` (advisory database) and `cargo-deny` (license + advisory policy) gate the build in CI.
- A CycloneDX 1.6 SBOM is produced at release.
- Dependency names are checked against slopsquatting before any addition.

## Continuous security gates

These workflows run on every push and pull request against the public repository (and on a nightly cron for the supply-chain checks so an advisory that lands between PRs is caught within 24 hours):

| Gate | File | What it does |
|---|---|---|
| Supply-chain policy | [`workflows/cargo-deny.yml`](.github/workflows/cargo-deny.yml) | `cargo-deny` matrix-runs **advisories / bans / licenses / sources** against the workspace `deny.toml`. Nightly cron @ 06:30 UTC. |
| Secret scan | [`workflows/secret-scan.yml`](.github/workflows/secret-scan.yml) | `gitleaks` scans full history for committed API keys / tokens / minisign private-key material. |
| SBOM | [`workflows/sbom.yml`](.github/workflows/sbom.yml) | `cargo-cyclonedx` emits a CycloneDX 1.6 SBOM and attaches it to every tagged release as `scr1b3-sbom.cdx.json`. |
| Content-safety | [`scripts/content_safety_audit.py`](scripts/content_safety_audit.py) | The publishable-cleanliness gate — fails any change that introduces an internal path, plan token, agent-system reference, or secret-shaped string into the public-repo-bound tree. |
| Dependency bumps | [`dependabot.yml`](.github/dependabot.yml) | Weekly Cargo + GitHub-Actions update PRs (Monday 09:00 UTC, 5 PRs cap). |

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately using GitHub's **[Private Vulnerability Reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)** (the "Report a vulnerability" button under the repository's Security tab). Include:

- A description of the issue and its impact.
- Steps to reproduce (a minimal proof of concept if possible).
- Affected version(s) and OS.

We aim to acknowledge reports promptly, keep you updated on remediation, and credit reporters who wish to be named once a fix ships. Please allow reasonable time for a fix before any public disclosure.

## Supported versions

Security fixes target the latest released version. Update via your package manager or the auto-update mechanism to stay current.
