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
