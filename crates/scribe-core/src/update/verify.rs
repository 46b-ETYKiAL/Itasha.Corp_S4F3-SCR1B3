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
pub const EMBEDDED_PUBLIC_KEY: &str = "untrusted comment: minisign public key: EAF9AC0C656E5A63\nRWRjWm5lDKz56qYOp/YzNsKqIO699Q77292KSPBkJ2KQQZKk7ynAI2bE";

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

/// Full gate: an artifact is acceptable iff its checksum matches AND its
/// signature verifies against the embedded key.
pub fn verify_artifact(
    bytes: &[u8],
    expected_sha256: &str,
    sig_str: &str,
    public_key_box: &str,
) -> Result<(), String> {
    if !verify_checksum(bytes, expected_sha256) {
        return Err("checksum mismatch".to_string());
    }
    verify_signature(bytes, sig_str, public_key_box)
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
}
