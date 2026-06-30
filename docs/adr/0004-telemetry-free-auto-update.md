# ADR 0004 — Telemetry-Free Auto-Update

**Status:** Accepted

## Context

Users need an easy way to stay current, but the editor's core promise is privacy: no account, no analytics, no phone-home. A typical auto-update mechanism is also a telemetry vector (it can carry identifiers) and a supply-chain risk (it installs code). SCR1B3 must deliver updates without compromising either privacy or integrity, and the user must remain in control.

## Decision

Auto-update is **telemetry-free and cryptographically verified**, with the user in full control.

- **Single network surface.** The only outbound request is a version check against the **public GitHub Releases API** (`https://api.github.com/repos/46b-ETYKiAL/Itasha.Corp_S4F3-SCR1B3/releases?per_page=100`). It fetches the full release list and selects the **highest stable semver** tag — deliberately *not* GitHub's mutable `/releases/latest` (which sorts by commit date and honors a cacheable "latest" flag, and can therefore skip a newer tag). It is unauthenticated, sends **zero PII**, and uses no custom server and no shipped token.
- **User-controlled modes** via `[updates] mode`: `off` (never check — no network at all), `notify` (check and inform; the default), `manual` (check only on request), `auto` (download, verify, and apply automatically). `check_interval_hours` bounds background checks.
- **Decision logic is offline-testable.** The version-compare and mode handling live in pure functions with an injectable fetcher, so tests never touch the network; the actual HTTP fetch, signature verification, and binary swap live behind a feature flag and keep the core dependency-light.
- **Offline is graceful.** A failed or unavailable check is reported as "offline" and never an error — it never blocks editing.
- **Verify-before-swap.** Downloaded artifacts are signed (minisign / ed25519) and checksummed; the updater refuses anything that fails verification. The prior binary is retained for one-step rollback. The updater writes only to the install directory with user permissions.
- **Signed-manifest required (no fallback).** A newer release is installed **only** through a signed `latest.json` manifest (+ `latest.json.minisig`) verified against the embedded key; each download's SHA-256 is pinned to the manifest's *signed* digest. An absent or unverifiable manifest is a hard refusal — there is no non-manifest install path. The manifest also carries the freeze-beacon (`valid_until_utc`), the `minimum_version` floor, and the monotonic `release_index` anti-rollback ordinal.

## Consequences

- Users get effortless updates without surrendering privacy; the privacy claim is auditable in source (the single endpoint is a documented constant).
- The update path cannot install tampered or unsigned binaries.
- Updating is reversible and never escalates privileges.
- Turning updates fully `off` removes the editor's last network surface, leaving it 100% offline.
