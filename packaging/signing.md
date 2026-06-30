# Release Signing (minisign / ed25519)

SCR1B3 release artifacts are signed with [minisign](https://jedisct1.github.io/minisign/)
(ed25519). The in-app updater verifies the signature against a public key
**embedded in the binary** before swapping — so an update is applied only if it
came from the holder of the secret key. SHA-256 checksums provide a second,
independent integrity layer.

## One-time key generation (maintainer, offline)

```sh
minisign -G -p scr1b3.pub -s scr1b3.key
```

- `scr1b3.key` — the **secret key**. NEVER commit it. Store it in the CI secret
  `MINISIGN_SECRET_KEY` (and a local password manager). It is git-ignored.
- `scr1b3.pub` — the public key. Copy its base64 line into
  `crates/scribe-core/src/update/verify.rs` → `EMBEDDED_PUBLIC_KEY`. The same
  public key is also published in-repo at [`minisign.pub`](minisign.pub) (it
  matches `verify.rs`'s `EMBEDDED_PUBLIC_KEY`) for out-of-band verification.

## Signing in CI (release.yml)

The release job signs **every** published asset with the secret key and — before
that sign loop runs — generates a Tier-1 **update manifest** (`latest.json`),
which is the document the in-app updater verifies first.

For each release artifact (`scr1b3-<target>.tar.gz`, the Windows
`scr1b3-<tag>-x86_64-setup.exe`, and `latest.json` itself):

```sh
echo "$MINISIGN_SECRET_KEY" > scr1b3.key
minisign -S -s scr1b3.key -m <asset>        # -> <asset>.minisig
sha256sum <asset> > <asset>.sha256
```

The job emits `dist/latest.json` ahead of the loop — a deterministic,
key-sorted manifest listing, per platform, each asset's `{asset_name, url,
size, sha256}` plus the release `{version, release_index, minimum_version,
valid_until_utc}`. Because it sits at the top of `dist/`, it is signed like any
other asset, producing `latest.json.minisig`.

### How the updater verifies (Tier-1 signed manifest)

1. The updater fetches `latest.json` + `latest.json.minisig` and verifies the
   signature over the **raw manifest bytes** against the embedded key set
   (`update::manifest::parse_and_verify`). An **absent or unverifiable manifest
   is a hard refusal** — there is no fallback to a per-asset selector (the legacy
   `select_best` / `select_update` / `build_release_info` flow was **removed** so
   an attacker who strips the manifest cannot downgrade to a weaker path).
2. The resolved archive download is **pinned to the manifest's SIGNED `sha256`**.
   The standalone `.sha256` sidecar is kept only as defense-in-depth and **must
   AGREE** with the signed digest (a disagreement fails closed).
3. The archive is then checksum- + minisign-verified
   (`update::verify::verify_artifact` against `EMBEDDED_PUBLIC_KEYS`) and only
   then applied. The manifest additionally enforces the freeze beacon
   (`valid_until_utc`), the `minimum_version` floor, and the monotonic
   `release_index` anti-rollback ordinal.

So artifact identity is a **signed hash inside a signed manifest**, not a
free-standing `.sha256` an attacker could recompute over a swapped payload.

### Now wired in CI (`.github/workflows/release.yml`)

The signing flow above is **automated**, gated on the secrets/vars being set:

| Secret / var | Kind | Purpose |
|---|---|---|
| `MINISIGN_SECRET_KEY` | secret | The **passwordless** ed25519 secret key (generate with `rsign generate -W`, or `minisign -G -W`). Present → the release job installs rsign2 and signs every asset (`*.minisig`). Absent → the job logs a `::warning::` and ships checksummed-but-**unsigned** artifacts (the in-app updater then rejects them — fail-closed). The CI signs with rsign2, the same tool used to generate the key; the app verifies with the `minisign-verify` crate (interoperable). |
| `SCR1B3_MINISIGN_PUBLIC_KEY` | var (optional) | The PUBLIC key box-form. When set, the build job swaps it into `EMBEDDED_PUBLIC_KEY` at build time — an alternative to committing the key into `verify.rs` by hand. When unset, whatever is committed in `verify.rs` is used. |

The real public key is committed in `crates/scribe-core/src/update/verify.rs` (`EMBEDDED_PUBLIC_KEY` — a public value). A maintainer activates signed auto-updates by adding the `MINISIGN_SECRET_KEY` secret; the matching public key is already embedded. Until the secret is set, releases are unsigned and the updater is inert by design — never insecure. (Use a passwordless key so the non-interactive CI sign needs no password secret.)

## Windows code signing (separate concern)

Authenticode-sign the `.exe`/`.msi` so SmartScreen/AV trust the self-replace
swap. This is independent of the minisign update-integrity chain (Authenticode =
OS/AV trust; minisign = our update authenticity). Use the `WINDOWS_CERT` secret.

## Threat model

- Secret key compromise → attacker can sign malicious updates. Mitigation: key
  stored only in CI secret + offline backup; rotate by shipping a new embedded
  public key in a normally-signed release before retiring the old key.
- The updater refuses unsigned, wrong-signed, or checksum-mismatched artifacts
  and keeps the prior binary for rollback.
