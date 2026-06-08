# Security Policy

SCR1B3 is built privacy-first and minimal-attack-surface by construction. This document describes that posture and how to report vulnerabilities. A per-trust-boundary STRIDE analysis (auto-update / plugins / LSP / opened files / local state) lives in [`docs/threat-model.md`](docs/threat-model.md).

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
- The updater **refuses** any download whose signature does not verify against the embedded trust set, or whose checksum does not match. An unsigned or tampered artifact is never installed.
- **Key-rotation-safe trust set.** Verification accepts a release signed by **any** key in an embedded set of trusted keys, so the signing key can be rotated with **zero downtime**: ship a build that trusts both the old and new keys, switch CI to sign with the new key, then retire the old key in a later release — no client is ever stranded. Trying multiple keys never upgrades a bad signature into an accepted one (a full cryptographic verify is still required).
- **Anti-downgrade (rollback-attack defense).** The updater refuses to install any version that is not **strictly newer** than the running build — enforced again at the moment of applying, not only at selection — so an attacker cannot replay an older, still-validly-signed release to force a downgrade to a known-vulnerable build (the TUF monotonic-version rule).
- **Automatic rollback.** The previous binary is snapshotted before the in-place swap and is automatically restored if the updated binary fails to relaunch — a failed update never leaves you without a working app.
- **One click, then auto-cleanup.** Choosing **Update now** downloads and installs in a single action (no separate "install" step). Once the update is applied, the downloaded archive/installer and its `.minisig`/`.sha256` sidecars are deleted, and the one kept-prior backup is removed at the next launch (once the new build has confirmed it runs) — no stale downloads are left behind.
- **Least-privilege install, explicit elevation.** For a per-user / portable install the updater swaps the binary in place with **your own user permissions** — no elevation. For an install in a protected location (e.g. `C:\Program Files`), it runs the **verified, signed** self-elevating installer, which requests administrator rights via the standard Windows **UAC** prompt. It never silently elevates and never uses setuid; the same minisign + checksum gate applies to the installer as to the archive.

To verify a release manually, use the published `minisign` public key against the release's signature file (instructions accompany each release).

## Plugin trust + sandbox model

The user plugin/mod system is opt-in and sandboxed (see [PLUGINS.md](PLUGINS.md)):

- **A plugin only runs after you approve it.** Dropping a folder into the plugins dir does **not** auto-execute it. By default the editor uses **trust-on-first-use**: it records the SHA-256 of the *exact* entry script you approved (in `[plugins] trusted`) and refuses to run a brand-new **or silently modified** script until you approve it again (Settings → Plugins → Manage plugins → **Approve & run**). A held-back plugin is shown as "not running — needs your approval".
- **Strict signed mode.** Set `[plugins] require_signed = true` and a plugin runs only when it carries a valid **minisign** signature over its entry script from a **pinned author key** (TOFU key-pinning; a changed key is refused). This authenticates the *code that runs*, not just metadata, via the same `update::verify` cryptographic surface as the auto-updater.
- **Sandbox.** The scripting "easy mode" runs in a restricted Rhai engine with seven caps wired (operations, call depth, string size, array size, map size, modules, expression depth) plus a wall-clock deadline. Both the `eval` and `import` keywords are removed from the parser and the module resolver is a no-op — a script using them fails to **compile**, never just at runtime. (Compiled WASM extensions run in a WASM sandbox.)
- **Capability surface.** The v1 host exposes only buffer-text operations to scripts — there is **no** ambient filesystem, network, or process access for a script to reach, so a plugin cannot silently exfiltrate files or open the network. Privileged host capabilities (and the per-capability consent prompt the manifest models) are not yet exposed; any future privileged capability will be gated behind explicit consent before it ships.
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

## Binary hardening (Windows)

The release binary is built with **Control Flow Guard** (`/guard:cf`) via
[`.cargo/config.toml`](.cargo/config.toml) (`-C control-flow-guard` under the
`cfg(windows)` target). CFG hardens indirect calls and returns against
ROP/JOP control-flow-hijack chains — the residual exploit class that
memory-safety does not, on its own, fully close in the presence of `unsafe` in
transitive GPU/windowing dependencies (`wgpu`, `winit`). It carries ~0.1% size
and ~3% runtime overhead and has been stable since Rust 1.47.

ASLR (`/DYNAMICBASE`), DEP (`/NXCOMPAT`) and High-Entropy VA are already MSVC
linker defaults for 64-bit Rust executables; CFG is the high-value mitigation
that is *not* on by default, so it is opted in for every Windows build.

**The mitigations are CI-verified, so they cannot silently regress.** The
`binary hardening (winchecksec)` CI job builds the release binary and runs
[winchecksec](https://github.com/trailofbits/winchecksec) (SHA-256-pinned
download) over it, failing the build unless the shipped exe actually carries
**CFG + ASLR (`/DYNAMICBASE`) + High-Entropy VA + DEP (`/NXCOMPAT`)**. A change
that ever dropped a mitigation fails here instead of shipping a weaker binary.

The release binary is also built with `--remap-path-prefix`, stripping the
build-machine absolute paths (workspace + cargo home) from the artifact for
cleaner, more deterministic output.

## Supply chain

- All dependencies are pinned via `Cargo.lock`.
- `cargo-audit` (advisory database) and `cargo-deny` (license + advisory policy) gate the build in CI.
- **The telemetry-free posture is enforced at the dependency-graph level.** [`deny.toml`](deny.toml) `[bans]` denies every alternative HTTP client / async runtime (`reqwest`, `hyper`, `isahc`, `curl`, `surf`, `tokio`) and every analytics / crash-reporting crate (`sentry`, `opentelemetry`, `posthog-rs`, `mixpanel`, …). The *only* sanctioned network stack is the synchronous, runtime-free `ureq` + `rustls` used by the opt-out update check; the `cargo-deny bans` gate (a required status check) fails the build if a second egress path or any telemetry crate ever enters the tree.
- **An unused dependency fails CI.** `cargo-machete` runs in CI and blocks any dependency that is declared but unused — keeping the dependency graph (and the attack surface) minimal.
- A CycloneDX 1.6 SBOM is produced at release, **and the shipped binary embeds its own scannable SBOM** via `cargo-auditable`: the exact released artifact can be vuln-scanned offline with `cargo audit bin scr1b3.exe`, trivy, grype, or osv-scanner — not just a detached side-file.
- **SLSA build-provenance.** Every release artifact carries a signed [build-provenance attestation](https://docs.github.com/actions/security-guides/using-artifact-attestations) (GitHub OIDC + Sigstore), verifiable with `gh attestation verify <file> --repo <repo>`. This is *additive* to the minisign signatures: minisign attests **author** identity (the embedded key); provenance attests **build** integrity (which workflow, commit, and runner produced the artifact).
- Dependency names are checked against slopsquatting before any addition.

## Continuous security gates

These workflows run on every push and pull request against the public repository (and on a nightly cron for the supply-chain checks so an advisory that lands between PRs is caught within 24 hours):

| Gate | File | What it does |
|---|---|---|
| Supply-chain policy | [`workflows/cargo-deny.yml`](.github/workflows/cargo-deny.yml) | `cargo-deny` matrix-runs **advisories / bans / licenses / sources** against the workspace `deny.toml`. Nightly cron @ 06:30 UTC. |
| Secret scan | [`workflows/secret-scan.yml`](.github/workflows/secret-scan.yml) | `gitleaks` scans full history for committed API keys / tokens / minisign private-key material. |
| SBOM | [`workflows/sbom.yml`](.github/workflows/sbom.yml) | `cargo-cyclonedx` emits a CycloneDX 1.6 SBOM and attaches it to every tagged release as `scr1b3-sbom.cdx.json`. |
| Content-safety | [`scripts/content_safety_audit.py`](scripts/content_safety_audit.py) | The publishable-cleanliness gate — fails any change that introduces an internal path, agent-system reference, or secret-shaped string into the public-repo-bound tree. |
| Dependency bumps | [`dependabot.yml`](.github/dependabot.yml) | Weekly Cargo + GitHub-Actions update PRs (Monday 09:00 UTC, 5 PRs cap). |
| Workflow security (zizmor) | [`workflows/workflow-security.yml`](.github/workflows/workflow-security.yml) | `zizmor` static analysis for template injection, dangerous triggers, unpinned actions, and excessive permissions. SARIF results land in the Security tab. Weekly cron @ Thursday 06:47 UTC. |
| CodeQL | [`workflows/codeql.yml`](.github/workflows/codeql.yml) | CodeQL with `security-extended` queries on Python (`scripts/*.py`) and GitHub Actions. Rust is not a CodeQL-supported free public-repo language — it is covered by `cargo clippy --deny warnings` and `cargo-deny`. Weekly cron @ Tuesday 14:23 UTC. |
| OpenSSF Scorecard | [`workflows/scorecard.yml`](.github/workflows/scorecard.yml) | OpenSSF Scorecard supply-chain assessment with public publication of the score. Weekly cron @ Wednesday 09:31 UTC. |

## CI/CD security posture

This section documents the load-bearing CI/CD controls on the `master` branch of this public repository. It is a baseline — the controls below are the defaults the maintainer commits to; any drift should be reconciled, not normalised.

### Branch protection contract

`master` carries a GitHub branch-protection rule with the following guarantees:

| Setting | Value | Why |
|---|---|---|
| `required_status_checks` | `build & test (ubuntu-latest)`, `build & test (windows-latest)`, `build & test (macos-latest)`, `cargo-deny advisories`, `cargo-deny bans`, `cargo-deny licenses`, `cargo-deny sources`, `public-repo content-safety audit`, `F0RG3-W1R3 install-manifest audit`, `gitleaks`, `zizmor workflow audit`, `Analyze (python)`, `Analyze (actions)`, `Scorecard analysis` | A PR cannot merge unless every gate that already exists passes. The gates are advisory if they aren't required; advisory is honour-system. |
| `required_pull_request_reviews.required_approving_review_count` | `1` | Forces every change through PR review, even from the maintainer. |
| `required_pull_request_reviews.dismiss_stale_reviews` | `true` | A new push invalidates a stale approval. |
| `required_pull_request_reviews.bypass_pull_request_allowances.users` | `[46b-ETYKiAL]` | Preserves the solo-maintainer emergency self-merge path; admin-bypass is the documented escape hatch for hotfixes when no second reviewer is available. |
| `required_conversation_resolution` | `true` | Every PR comment thread must be resolved before merge. |
| `required_linear_history` | `true` | No merge commits — rebase or squash only. |
| `required_signatures` | `true` | Every commit landing on `master` must be signature-verified. |
| `allow_force_pushes` | `false` | History on `master` is immutable. |
| `allow_deletions` | `false` | `master` cannot be deleted. |
| `enforce_admins` | `false` | Deliberately off so the solo maintainer can land emergency hotfixes when CI is broken; every admin-bypass merge is audited by GitHub itself. |

### Dependabot security updates

Dependabot is configured at [`.github/dependabot.yml`](.github/dependabot.yml) for both the Cargo ecosystem (workspace + crate manifests under `crates/`) and the `github-actions` ecosystem (the workflows under `.github/workflows/`). Updates land as PRs every Monday 09:00 UTC with a cap of 5 open PRs per ecosystem.

Dependabot security updates (the auto-PR on a newly-disclosed advisory) is the higher-priority surface — when enabled at the repo settings layer, an advisory landing on a transitive dependency opens a PR within minutes rather than waiting for the weekly tick.

### SHA-pinned GitHub Actions

Every `uses: <action>@<tag>` reference in `.github/workflows/*.yml` is pinned by full 40-char commit SHA with a trailing version comment, e.g.:

```yaml
- uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6.0.2
- uses: gitleaks/gitleaks-action@ff98106e4c7b2bc287b24eaf42907196329070c7 # v2.3.9
```

This closes the tag-mover supply-chain class documented in the March-2025 `tj-actions/changed-files` incident: a JS Action runs in-process on the runner with full secret context, so a malicious tag-move on a popular action repo would let it exfiltrate every secret in the workflow scope on its next run. SHA-pinning makes the action a content-addressed dependency rather than a mutable reference.

Dependabot rewrites both the SHA pin and the trailing version comment together — the version comment never drifts from the pinned SHA, because the same Dependabot PR updates both lines.

The companion control is `persist-credentials: false` on every `actions/checkout@…` step. Without it, the runner leaves `GITHUB_TOKEN` material in `.git/config` for downstream steps to read — SHA-pinning the checkout alone is insufficient.

### Weekly security-scan schedule

The three workflow-security feeds are staggered across the week to spread runner usage:

| Workflow | Cron | Slot |
|---|---|---|
| CodeQL | `23 14 * * 2` | Tuesday 14:23 UTC |
| OpenSSF Scorecard | `31 9 * * 3` | Wednesday 09:31 UTC |
| Workflow Security (zizmor) | `47 6 * * 4` | Thursday 06:47 UTC |

Each also runs on every push to `master` and every PR, so the weekly cron is the catch-up surface for advisories or upstream rule additions that landed between PRs.

### Gitleaks: PR-time and push-protection

`gitleaks` runs on every PR and every push to `master` via the [`workflows/secret-scan.yml`](.github/workflows/secret-scan.yml) workflow. Full history is scanned (`fetch-depth: 0`), and the action exits non-zero on any finding — a PR carrying a leak cannot merge.

The complementary control is GitHub-native **secret scanning + push protection**, enabled at the repo settings layer. This blocks a `git push` at the server before the commit lands, so even a leaked secret in a *local* commit never reaches the public history.

The two controls compose: push protection catches the leak at `git push` time; gitleaks catches anything that slips through (e.g. a credential committed before the secret scanner learned its pattern, surfaced via the full-history scan).

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately using GitHub's **[Private Vulnerability Reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)** (the "Report a vulnerability" button under the repository's Security tab). Include:

- A description of the issue and its impact.
- Steps to reproduce (a minimal proof of concept if possible).
- Affected version(s) and OS.

We aim to acknowledge reports promptly, keep you updated on remediation, and credit reporters who wish to be named once a fix ships. Please allow reasonable time for a fix before any public disclosure.

## Supported versions

Security fixes target the latest released version. Update via your package manager or the auto-update mechanism to stay current.
