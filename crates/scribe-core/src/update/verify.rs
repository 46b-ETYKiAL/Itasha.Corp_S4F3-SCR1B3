//! Update artifact verification: SHA-256 checksum + minisign (ed25519)
//! signature. Defense in depth — the checksum catches corruption, the
//! signature catches tampering. An update is applied ONLY if both pass.
//!
//! The public key is embedded in the binary at build time; the secret key
//! lives outside the repo (CI secret). See packaging/signing.md.

use sha2::{Digest, Sha256};

/// The embedded minisign public key (full box form). This is the REAL SCR1B3
/// release signing key — a PUBLIC value, safe to commit. The in-app updater
/// verifies every downloaded artifact against it; only the holder of the
/// matching secret key (a GitHub Actions secret, never committed) can produce
/// an accepted signature.
pub const EMBEDDED_PUBLIC_KEY: &str = "untrusted comment: minisign public key: BD4ADF9145E13B17\nRWQXO+FFkd9Kvdw2hUrWtt5Eoebj41ckYRPGs7tTH+zym1moqwXT5D7N";

/// The full set of trusted release-signing public keys. An artifact is accepted
/// when ANY key in this set fully verifies its signature — the mechanism that
/// makes signing-key ROTATION safe: during a rotation window BOTH the outgoing
/// and incoming keys are listed here, so clients built before AND after the
/// rotation accept releases signed by either key. Order is irrelevant (every
/// key is tried); the slice is authoritative and embedded at build time.
///
/// Rotation procedure (zero-downtime, no client left stranded):
/// 1. Generate the new keypair; add its PUBLIC key to this slice and ship a
///    release. Clients on that build now trust BOTH keys.
/// 2. Once that release is widely adopted, switch CI to sign with the NEW
///    secret key. Old clients still accept it (old key was never removed yet);
///    new clients accept it via the new key.
/// 3. After the old key is fully retired, drop it from this slice in a later
///    release.
pub const EMBEDDED_PUBLIC_KEYS: &[&str] = &[EMBEDDED_PUBLIC_KEY];

/// Hex-encoded SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-ish checksum comparison (case-insensitive hex).
pub fn verify_checksum(bytes: &[u8], expected_hex: &str) -> bool {
    sha256_hex(bytes).eq_ignore_ascii_case(expected_hex.trim())
}

/// Verify a minisign signature (`sig_str` = the `.minisig` file contents)
/// against `bytes` using the given public-key box string.
pub fn verify_signature(bytes: &[u8], sig_str: &str, public_key_box: &str) -> Result<(), String> {
    let pk = minisign_verify::PublicKey::decode(public_key_box)
        .map_err(|e| format!("bad public key: {e}"))?;
    let sig =
        minisign_verify::Signature::decode(sig_str).map_err(|e| format!("bad signature: {e}"))?;
    pk.verify(bytes, &sig, false)
        .map_err(|e| format!("signature verification failed: {e}"))
}

/// Verify a minisign signature against a SET of candidate public keys, accepting
/// if ANY key verifies. This is the key-rotation-safe form of
/// [`verify_signature`] (see [`EMBEDDED_PUBLIC_KEYS`]).
///
/// SECURITY: acceptance requires a FULL cryptographic verification against at
/// least one key. minisign embeds an 8-byte key id in both the public key and
/// the signature; that id is only a routing HINT and is attacker-controllable,
/// so it is never trusted on its own — `minisign_verify` rejects a key-id
/// mismatch before the Ed25519 check, and a key-id match still requires the
/// signature itself to verify. Trying multiple keys cannot upgrade a bad
/// signature into an accepted one.
pub fn verify_any_signature(
    bytes: &[u8],
    sig_str: &str,
    public_key_boxes: &[&str],
) -> Result<(), String> {
    if public_key_boxes.is_empty() {
        return Err("no trusted public keys configured".to_string());
    }
    let mut last_err = None;
    for pk in public_key_boxes {
        match verify_signature(bytes, sig_str, pk) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| "signature did not verify against any trusted key".to_string()))
}

/// Full gate: an artifact is acceptable iff its checksum matches AND its
/// signature verifies against AT LEAST ONE of the trusted `public_keys` (pass
/// [`EMBEDDED_PUBLIC_KEYS`] in production). The multi-key form is what makes
/// signing-key rotation safe; verification still fails closed when no key
/// verifies.
pub fn verify_artifact(
    bytes: &[u8],
    expected_sha256: &str,
    sig_str: &str,
    public_keys: &[&str],
) -> Result<(), String> {
    if !verify_checksum(bytes, expected_sha256) {
        return Err("checksum mismatch".to_string());
    }
    verify_any_signature(bytes, sig_str, public_keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        // SHA-256("abc")
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn checksum_match_and_mismatch() {
        let data = b"scr1b3 release artifact";
        let good = sha256_hex(data);
        assert!(verify_checksum(data, &good));
        assert!(verify_checksum(data, &good.to_uppercase())); // case-insensitive
        assert!(!verify_checksum(data, "deadbeef"));
    }

    #[test]
    fn signature_roundtrip_accepts_valid_rejects_tampered() {
        // Sign with the dev-only `minisign` crate, verify with the production
        // `minisign-verify` path. Proves the verify path accepts a real sig and
        // rejects tampered data.
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"the new scr1b3 binary bytes";
        let sig_box = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            Some("scr1b3 v9.9.9"),
            Some("comment"),
        )
        .unwrap();
        let sig_str = sig_box.to_string();

        // Valid signature over the exact bytes -> accepted.
        assert!(verify_signature(data, &sig_str, &pk_box).is_ok());

        // Tampered bytes -> rejected.
        let tampered = b"the new scr1b3 binary bytez";
        assert!(verify_signature(tampered, &sig_str, &pk_box).is_err());
    }

    #[test]
    fn embedded_key_rejects_bogus_signatures() {
        // The embedded release key must reject a malformed / forged signature
        // (it only accepts artifacts signed by the matching secret key).
        assert!(
            verify_signature(b"x", "untrusted comment: x\nbogus", EMBEDDED_PUBLIC_KEY).is_err()
        );
    }

    #[test]
    fn embedded_key_decodes() {
        // The committed embedded key must be a well-formed minisign public key
        // (so a real signature CAN verify against it).
        assert!(minisign_verify::PublicKey::decode(EMBEDDED_PUBLIC_KEY).is_ok());
    }

    #[test]
    fn embedded_key_set_is_nonempty_and_all_decode() {
        // The production trust set must be non-empty and every entry must be a
        // valid minisign public key — a malformed rotation entry would silently
        // never match.
        assert!(!EMBEDDED_PUBLIC_KEYS.is_empty());
        for pk in EMBEDDED_PUBLIC_KEYS {
            assert!(
                minisign_verify::PublicKey::decode(pk).is_ok(),
                "embedded key did not decode: {pk}"
            );
        }
    }

    #[test]
    fn multi_key_accepts_when_any_key_in_the_set_matches() {
        // Key-rotation contract: a release signed by a key that is NOT the first
        // entry must still be accepted as long as it is somewhere in the set.
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"rotated-key release bytes";
        let sig = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            Some("scr1b3 v9.9.9"),
            Some("comment"),
        )
        .unwrap()
        .to_string();

        // Set = [unrelated production key, the key that actually signed].
        let keys = [EMBEDDED_PUBLIC_KEY, pk_box.as_str()];
        assert!(verify_any_signature(data, &sig, &keys).is_ok());

        // verify_artifact composes checksum + the multi-key signature check.
        let sha = sha256_hex(data);
        assert!(verify_artifact(data, &sha, &sig, &keys).is_ok());
        // ...and still fails closed on a checksum mismatch even with a good sig.
        assert!(verify_artifact(data, "deadbeef", &sig, &keys).is_err());
    }

    #[test]
    fn multi_key_rejects_when_no_key_in_the_set_matches() {
        // A signature from a key OUTSIDE the trust set must be rejected — trying
        // multiple keys never upgrades an untrusted signature into an accepted
        // one.
        let signer = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let other_pk = other.pk.to_box().unwrap().to_string();
        let data = b"bytes signed by a key not in the trust set";
        let sig = minisign::sign(
            Some(&signer.pk),
            &signer.sk,
            std::io::Cursor::new(&data[..]),
            Some("scr1b3 v9.9.9"),
            Some("comment"),
        )
        .unwrap()
        .to_string();

        let keys = [EMBEDDED_PUBLIC_KEY, other_pk.as_str()];
        assert!(verify_any_signature(data, &sig, &keys).is_err());
    }

    #[test]
    fn multi_key_empty_set_rejects() {
        // An empty trust set must never accept anything (fail-closed).
        assert!(verify_any_signature(b"x", "untrusted comment: x\nbogus", &[]).is_err());
    }

    #[test]
    fn verify_artifact_rejects_good_checksum_but_bad_signature() {
        // The critical supply-chain attack: an attacker who can recompute the
        // SHA-256 sidecar (trivial — it's just a hash of their payload) but
        // CANNOT forge the minisign signature. A correct checksum must NEVER be
        // enough; the signature is the only thing the checksum cannot substitute
        // for. We sign with one key, then verify against a DIFFERENT trusted key
        // set so the (well-formed) signature does not verify — checksum passes,
        // signature fails, artifact REJECTED.
        let attacker = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let trusted = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let trusted_pk = trusted.pk.to_box().unwrap().to_string();

        let payload = b"malicious 'update' payload with a valid checksum";
        let good_sha = sha256_hex(payload); // attacker can always produce this
        let attacker_sig = minisign::sign(
            Some(&attacker.pk),
            &attacker.sk,
            std::io::Cursor::new(&payload[..]),
            Some("forged"),
            Some("forged"),
        )
        .unwrap()
        .to_string();

        // Checksum matches the payload, but the signature is from an untrusted
        // key -> the composite gate must reject.
        let keys = [trusted_pk.as_str()];
        let res = verify_artifact(payload, &good_sha, &attacker_sig, &keys);
        assert!(
            res.is_err(),
            "a correct checksum must NOT rescue an untrusted signature, got {res:?}"
        );

        // Control: the SAME payload+checksum WITH a signature from the trusted
        // key is accepted — proving the rejection above was the signature, not
        // an unrelated failure.
        let good_sig = minisign::sign(
            Some(&trusted.pk),
            &trusted.sk,
            std::io::Cursor::new(&payload[..]),
            Some("real"),
            Some("real"),
        )
        .unwrap()
        .to_string();
        assert!(
            verify_artifact(payload, &good_sha, &good_sig, &keys).is_ok(),
            "the trusted-key signature over the same bytes must verify"
        );
    }

    #[test]
    fn verify_artifact_rejects_truncated_or_garbage_signature_text() {
        // A structurally-malformed `.minisig` (not a real signature at all) must
        // be rejected at decode time, never silently treated as "no signature ->
        // ok". Defends the fail-closed contract against a corrupt sidecar.
        let payload = b"bytes";
        let good_sha = sha256_hex(payload);
        for bad_sig in [
            "",                           // empty sidecar
            "not a minisign file",        // garbage
            "untrusted comment: x",       // header only, no sig line
            "untrusted comment: x\nQUJD", // header + junk base64
        ] {
            let res = verify_artifact(payload, &good_sha, bad_sig, EMBEDDED_PUBLIC_KEYS);
            assert!(
                res.is_err(),
                "malformed signature {bad_sig:?} must be rejected, got {res:?}"
            );
        }
    }
}
