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
  `crates/scribe-core/src/update/verify.rs` → `EMBEDDED_PUBLIC_KEY`.

## Signing in CI (release.yml)

For each release artifact `scr1b3-<target>.tar.gz`:

```sh
echo "$MINISIGN_SECRET_KEY" > scr1b3.key
minisign -S -s scr1b3.key -m scr1b3-<target>.tar.gz       # -> .minisig
sha256sum scr1b3-<target>.tar.gz > scr1b3-<target>.sha256
```

Upload `.tar.gz`, `.minisig`, and `.sha256` to the GitHub Release. The updater
downloads all three, verifies checksum + signature (`update::verify::verify_artifact`),
and only then applies (`update::apply`).

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
