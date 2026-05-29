//! Phase 20 T20.2 — plugin tarball integrity verification.
//!
//! Re-uses the existing [`crate::update::verify`] surface so the plugin
//! and release-binary verification share one code path. The bytes-to-
//! verify change from "release binary" to "plugin tarball" but the
//! algorithm (SHA-256 + minisign over the same byte stream) stays
//! identical. This keeps the cryptographic surface area small and means
//! any future verify-path hardening lands once for both consumers.
//!
//! ## Canonical signed unit
//!
//! The plugin tarball is the **whole** gzipped-tar produced from the
//! plugin directory (manifest + entry script(s) + any bundled assets).
//! Authors generate it once, sign the gz bytes with minisign, publish
//! both the `.tar.gz` and the sibling `.minisig`. The verifier rebuilds
//! the bytes-to-verify by reading the tarball into memory — no
//! reproducibility wizardry, no metadata extraction; if the on-the-wire
//! byte stream matches the sig, we accept.
//!
//! ## Failure-mode taxonomy (user-facing strings)
//!
//! | Failure        | Message |
//! |----------------|---------|
//! | Checksum bad   | "Plugin file is corrupted or has been modified since publication." |
//! | Signature bad  | "Plugin signature is invalid. Refusing to install." |
//!
//! The strings are kept short and security-honest: we do NOT name the
//! bytes mismatched or the key shape; the install UI can elaborate.

use crate::update::verify::{verify_checksum, verify_signature};

/// Verify a plugin tarball against the manifest-declared SHA-256 +
/// minisign signature + public key. Returns `Ok(())` only when BOTH the
/// checksum matches AND the minisign signature verifies.
///
/// The checksum check runs FIRST so a corrupted download is rejected
/// with the friendlier "file corrupted" message rather than the
/// scarier "signature invalid" string.
pub fn verify_plugin_tarball(
    tarball_bytes: &[u8],
    expected_sha256: &str,
    signature: &str,
    pubkey: &str,
) -> Result<(), String> {
    if !verify_checksum(tarball_bytes, expected_sha256) {
        return Err("Plugin file is corrupted or has been modified since publication.".to_string());
    }
    verify_signature(tarball_bytes, signature, pubkey)
        .map_err(|_| "Plugin signature is invalid. Refusing to install.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::verify::sha256_hex;

    /// The happy path: a real keypair signs a known byte string, the
    /// manifest declares the matching SHA-256 + signature + pubkey, and
    /// `verify_plugin_tarball` returns `Ok(())`.
    #[test]
    fn happy_path_accepts_signed_tarball() {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"the synthetic plugin tarball bytes";
        let sig_box = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            None,
            None,
        )
        .unwrap();
        let sig_str = sig_box.to_string();
        let sha = sha256_hex(data);

        verify_plugin_tarball(data, &sha, &sig_str, &pk_box)
            .expect("happy path must accept signed tarball");
    }

    /// Checksum mismatch is the friendlier failure — surface the
    /// "corrupted" message and never even attempt the signature verify.
    #[test]
    fn rejects_checksum_mismatch_with_friendly_message() {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"the synthetic plugin tarball bytes";
        let sig_box = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            None,
            None,
        )
        .unwrap();
        let sig_str = sig_box.to_string();
        let bogus_sha = sha256_hex(b"a different payload");

        let err = verify_plugin_tarball(data, &bogus_sha, &sig_str, &pk_box)
            .expect_err("checksum mismatch must reject");
        assert!(
            err.contains("corrupted"),
            "want friendly corrupted message, got {err:?}"
        );
    }

    /// Signature mismatch (right checksum, wrong sig) lands the
    /// security-honest "signature invalid" message.
    #[test]
    fn rejects_signature_mismatch_with_security_honest_message() {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"the synthetic plugin tarball bytes";
        let sha = sha256_hex(data);
        // Sign a DIFFERENT payload, then ship the wrong sig with the
        // right checksum — this is the "attacker tampers but lies about
        // size" failure mode.
        let other = b"wholly different bytes";
        let sig_box = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&other[..]),
            None,
            None,
        )
        .unwrap();
        let sig_str = sig_box.to_string();

        let err = verify_plugin_tarball(data, &sha, &sig_str, &pk_box)
            .expect_err("signature mismatch must reject");
        assert!(
            err.contains("signature"),
            "want signature-invalid message, got {err:?}"
        );
    }

    /// A second keypair's signature against the same bytes must reject
    /// — the attacker who signs with their own key cannot impersonate
    /// the legitimate author.
    #[test]
    fn rejects_wrong_key_signature() {
        let kp_legit = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let kp_attacker = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let legit_pk_box = kp_legit.pk.to_box().unwrap().to_string();
        let data = b"the synthetic plugin tarball bytes";
        let sha = sha256_hex(data);
        // The attacker signs the SAME bytes (so the checksum matches)
        // with the WRONG key. The manifest pins the legit key, so the
        // attacker's signature must reject.
        let attacker_sig = minisign::sign(
            Some(&kp_attacker.pk),
            &kp_attacker.sk,
            std::io::Cursor::new(&data[..]),
            None,
            None,
        )
        .unwrap()
        .to_string();

        let err = verify_plugin_tarball(data, &sha, &attacker_sig, &legit_pk_box)
            .expect_err("wrong-key signature must reject");
        assert!(
            err.contains("signature"),
            "want signature-invalid message, got {err:?}"
        );
    }
}
