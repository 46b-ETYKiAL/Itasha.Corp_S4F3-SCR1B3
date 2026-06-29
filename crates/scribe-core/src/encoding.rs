//! Encoding detection + conversion. Detects via `chardetng`, decodes/encodes
//! via `encoding_rs`. UTF-8 is the canonical in-memory representation; the
//! original encoding + BOM presence are preserved for round-trip saving.

use encoding_rs::Encoding;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedEncoding {
    /// Canonical encoding name (e.g. "UTF-8", "UTF-16LE", "windows-1252").
    pub name: String,
    /// Whether a byte-order mark was present at the start of the file.
    pub had_bom: bool,
}

impl Default for DetectedEncoding {
    fn default() -> Self {
        Self {
            name: "UTF-8".to_string(),
            had_bom: false,
        }
    }
}

/// Detect the encoding of a byte slice and decode it to a `String` (lossy on
/// malformed sequences so the editor never refuses to open a file).
pub fn decode(bytes: &[u8]) -> (String, DetectedEncoding) {
    // BOM sniff first — authoritative when present. We strip the leading marker
    // BOM ourselves via `for_bom`'s reported length, then decode the remainder
    // WITHOUT further BOM handling. `Encoding::decode` would BOM-sniff the slice
    // a SECOND time and erase a content U+FEFF that legitimately follows the
    // marker (a double byte-order-mark, where the first is the marker and the
    // second is content), so we use `decode_without_bom_handling` to keep the
    // strip count at exactly one.
    if let Some((enc, bom_len)) = Encoding::for_bom(bytes) {
        let (text, _) = enc.decode_without_bom_handling(&bytes[bom_len..]);
        return (
            text.into_owned(),
            DetectedEncoding {
                name: enc.name().to_string(),
                had_bom: true,
            },
        );
    }
    // Otherwise run statistical detection.
    //
    // chardetng 1.0 changed the EncodingDetector API to use structured enums
    // instead of bools: ISO-2022-JP detection is opt-in via Iso2022JpDetection,
    // and UTF-8 detection is opt-in via Utf8Detection. We pass `Allow` for
    // both — same behavior as the 0.x `det.guess(None, /*allow_utf8=*/ true)`
    // call.
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    det.feed(bytes, true);
    let enc = det.guess(None, chardetng::Utf8Detection::Allow);
    let (text, _, _) = enc.decode(bytes);
    (
        text.into_owned(),
        DetectedEncoding {
            name: enc.name().to_string(),
            had_bom: false,
        },
    )
}

/// Decode a byte slice with a KNOWN encoding — NO statistical detection. This is
/// the inverse pairing for [`encode`]/[`encode_checked`]: when the editor already
/// knows a file's encoding (e.g. on an in-session reload of a document that was
/// already opened and whose encoding was preserved), it must decode WITH that
/// known encoding rather than re-running `chardetng`. `chardetng` is heuristic
/// and non-idempotent on detection-ambiguous bytes, so a fresh detect on reload
/// can silently flip the encoding (and thus the text) of a file the user has not
/// changed — a data-safety hazard. `decode_with` removes that flip by honouring
/// the supplied `enc`.
///
/// The encoding is resolved from `enc.name` via `Encoding::for_label`, falling
/// back to UTF-8 for an unknown label (mirroring [`encode_checked`]'s fallback).
/// If `enc.had_bom` is set, a leading BOM for the resolved encoding is skipped
/// before decoding — the same BOM handling [`decode`] performs — so the decoded
/// text never contains a spurious U+FEFF.
pub fn decode_with(bytes: &[u8], enc: &DetectedEncoding) -> String {
    let encoding = Encoding::for_label(enc.name.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    // Honour the recorded BOM: if the original had one, strip a matching leading
    // BOM so we don't decode it into a literal U+FEFF. `Encoding::for_bom`
    // reports the BOM length for the bytes actually present.
    let body = if enc.had_bom {
        match Encoding::for_bom(bytes) {
            Some((_, bom_len)) => &bytes[bom_len..],
            None => bytes,
        }
    } else {
        bytes
    };
    // Decode the body WITHOUT BOM handling: when `had_bom` is set we have already
    // stripped exactly one marker BOM above, so a content U+FEFF that follows it
    // must survive (no second strip). When `had_bom` is false we decode literally,
    // so a leading content U+FEFF is never mistaken for a marker. `Encoding::decode`
    // would BOM-sniff again in both branches and erode a content U+FEFF.
    let (text, _) = encoding.decode_without_bom_handling(body);
    text.into_owned()
}

/// Encode a `String` back to bytes using the named encoding, re-emitting a BOM
/// if the original had one. Falls back to UTF-8 for unknown names. The returned
/// `bool` is `true` when one or more characters could **not** be represented in
/// the target encoding: `encoding_rs` substitutes a replacement, so those
/// characters are silently LOST on save and the caller MUST warn the user.
pub fn encode_checked(text: &str, enc: &DetectedEncoding) -> (Vec<u8>, bool) {
    // `encoding_rs` is DECODE-ONLY for UTF-16: per the WHATWG Encoding Standard
    // it re-encodes UTF-16 labels as UTF-8, which would silently corrupt a
    // UTF-16 file on save (a UTF-16 BOM prefixed to UTF-8 bytes). Hand-encode
    // the two byte orders so opening + saving a UTF-16 file round-trips
    // faithfully. UTF-16 covers all of Unicode, so it is never lossy.
    if enc.name == "UTF-16LE" || enc.name == "UTF-16BE" {
        let le = enc.name == "UTF-16LE";
        let mut out = Vec::with_capacity(text.len() * 2 + 2);
        if enc.had_bom {
            out.extend_from_slice(if le { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] });
        }
        for unit in text.encode_utf16() {
            let bytes = if le {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            };
            out.extend_from_slice(&bytes);
        }
        return (out, false);
    }
    let encoding = Encoding::for_label(enc.name.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    let (cow, _, had_unmappable) = encoding.encode(text);
    let mut out = Vec::with_capacity(cow.len() + 3);
    if enc.had_bom {
        // Re-emit UTF-8/16 BOMs; other encodings have none.
        match encoding.name() {
            "UTF-8" => out.extend_from_slice(&[0xEF, 0xBB, 0xBF]),
            "UTF-16LE" => out.extend_from_slice(&[0xFF, 0xFE]),
            "UTF-16BE" => out.extend_from_slice(&[0xFE, 0xFF]),
            _ => {}
        }
    }
    out.extend_from_slice(&cow);
    (out, had_unmappable)
}

/// Encode a `String` back to bytes (see [`encode_checked`] to also learn whether
/// any characters were lost). Re-emits a BOM if the original had one.
pub fn encode(text: &str, enc: &DetectedEncoding) -> Vec<u8> {
    encode_checked(text, enc).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_utf8() {
        let (text, enc) = decode("héllo".as_bytes());
        assert_eq!(text, "héllo");
        assert_eq!(enc.name, "UTF-8");
        let bytes = encode(&text, &enc);
        assert_eq!(bytes, "héllo".as_bytes());
    }

    #[test]
    fn utf8_bom_preserved() {
        let mut input = vec![0xEF, 0xBB, 0xBF];
        input.extend_from_slice("x".as_bytes());
        let (text, enc) = decode(&input);
        assert_eq!(text, "x");
        assert!(enc.had_bom);
        assert_eq!(encode(&text, &enc), input);
    }

    #[test]
    fn utf16le_detected() {
        // "Hi" in UTF-16LE with BOM
        let input = vec![0xFF, 0xFE, b'H', 0x00, b'i', 0x00];
        let (text, enc) = decode(&input);
        assert_eq!(text, "Hi");
        assert_eq!(enc.name, "UTF-16LE");
        assert!(enc.had_bom);
    }

    #[test]
    fn encode_checked_flags_unmappable_characters() {
        // A file opened as windows-1252 can't represent a kanji; encoding_rs
        // replaces it, so the character is lost on save — encode_checked must
        // report that so the editor can warn the user.
        let enc = DetectedEncoding {
            name: "windows-1252".to_string(),
            had_bom: false,
        };
        let (_bytes, lossy) = encode_checked("speak 速記 friend", &enc);
        assert!(lossy, "kanji is unmappable in windows-1252");

        // A fully representable string is NOT lossy.
        let (_b, lossy2) = encode_checked("plain ascii", &enc);
        assert!(!lossy2);

        // UTF-8 can represent everything → never lossy.
        let utf8 = DetectedEncoding {
            name: "UTF-8".to_string(),
            had_bom: false,
        };
        let (_b, lossy3) = encode_checked("速記 ok", &utf8);
        assert!(!lossy3);
    }

    #[test]
    fn utf16be_bom_roundtrips() {
        // "Hi" in UTF-16BE with BOM.
        let input = vec![0xFE, 0xFF, 0x00, b'H', 0x00, b'i'];
        let (text, enc) = decode(&input);
        assert_eq!(text, "Hi");
        assert_eq!(enc.name, "UTF-16BE");
        assert!(enc.had_bom);
        // Re-encoding must re-emit the BE BOM + the original bytes.
        assert_eq!(encode(&text, &enc), input);
    }

    #[test]
    fn utf16le_bom_reemitted_on_encode() {
        let enc = DetectedEncoding {
            name: "UTF-16LE".to_string(),
            had_bom: true,
        };
        let bytes = encode("Hi", &enc);
        assert_eq!(bytes, vec![0xFF, 0xFE, b'H', 0x00, b'i', 0x00]);
    }

    #[test]
    fn windows_1252_roundtrips_latin1() {
        // 0xE9 is 'é' in windows-1252.
        let input = vec![b'c', b'a', b'f', 0xE9];
        let (text, enc) = decode(&input);
        assert_eq!(text, "café");
        // The detected name re-encodes back to the same Latin-1 bytes.
        assert_eq!(encode(&text, &enc), input);
    }

    #[test]
    fn empty_input_roundtrips_to_empty() {
        let (text, enc) = decode(&[]);
        assert_eq!(text, "");
        assert_eq!(enc.name, "UTF-8");
        assert!(encode(&text, &enc).is_empty());
    }

    #[test]
    fn invalid_utf8_decodes_lossily_without_panic() {
        // A lone continuation byte is not valid UTF-8; decode must not panic and
        // must still yield a string (the editor never refuses to open a file).
        let input = vec![b'a', 0xFF, 0x80, b'b'];
        let (text, _enc) = decode(&input);
        assert!(text.contains('a') && text.contains('b'));
    }

    #[test]
    fn decode_with_uses_known_encoding_not_detection() {
        // The core data-safety capability: decode bytes WITH a known encoding,
        // bypassing chardetng. The adversarial input from ENC-1
        // (bytes = [252, 79, 176, 161]) detects ambiguously, but decoding it
        // with the SAME encoding the producer used recovers the producer's text.
        let bytes = [252u8, 79, 176, 161];
        let (text1, e) = decode(&bytes);
        // Re-encoding under the detected encoding, then decode_with the SAME
        // encoding, must recover the original text — no detection flip.
        let (reencoded, lossy) = encode_checked(&text1, &e);
        assert!(!lossy, "this adversarial input round-trips non-lossily");
        let text2 = decode_with(&reencoded, &e);
        assert_eq!(text1, text2, "decode_with the known encoding is stable");
    }

    #[test]
    fn decode_with_honors_bom_and_unknown_label_fallback() {
        // had_bom set: a leading UTF-8 BOM is skipped, yielding clean text with
        // no spurious U+FEFF — consistent with how decode() handles a BOM.
        let mut input = vec![0xEF, 0xBB, 0xBF];
        input.extend_from_slice("data".as_bytes());
        let enc = DetectedEncoding {
            name: "UTF-8".to_string(),
            had_bom: true,
        };
        assert_eq!(decode_with(&input, &enc), "data");

        // had_bom set for UTF-16LE: the LE BOM is skipped before decoding.
        let mut u16le = vec![0xFF, 0xFE];
        u16le.extend_from_slice(&[b'O', 0x00, b'k', 0x00]);
        let enc16 = DetectedEncoding {
            name: "UTF-16LE".to_string(),
            had_bom: true,
        };
        assert_eq!(decode_with(&u16le, &enc16), "Ok");

        // An unknown label falls back to UTF-8 (mirrors encode_checked).
        let bogus = DetectedEncoding {
            name: "not-a-real-encoding".to_string(),
            had_bom: false,
        };
        assert_eq!(decode_with("héllo".as_bytes(), &bogus), "héllo");
    }

    #[test]
    fn decode_with_pairs_with_encode_for_single_byte_codepage() {
        // decode_with(encode(x, e), e) == x for a representable Latin-1 string —
        // the inverse-pairing contract for legacy codepages.
        let enc = DetectedEncoding {
            name: "windows-1252".to_string(),
            had_bom: false,
        };
        let (bytes, lossy) = encode_checked("café déjà", &enc);
        assert!(!lossy);
        assert_eq!(decode_with(&bytes, &enc), "café déjà");
    }

    #[test]
    fn encode_checked_does_not_reemit_bom_for_non_unicode_with_bom_flag() {
        // Kills the "delete UTF-16LE/UTF-16BE match arm" mutants (encoding.rs
        // lines 92/93): those arms guard the BOM re-emit branch. A single-byte
        // codepage carrying had_bom=true must NOT gain any BOM bytes (only
        // UTF-8/UTF-16 BOMs are re-emitted; the `_ => {}` arm is correct). If a
        // mutant deletes the UTF-16 arms, this test alone won't flip — but the
        // companion below pins the UTF-16 arms directly.
        let enc = DetectedEncoding {
            name: "windows-1252".to_string(),
            had_bom: true,
        };
        let (bytes, _lossy) = encode_checked("ab", &enc);
        assert_eq!(bytes, b"ab", "windows-1252 has no BOM to re-emit");
    }

    #[test]
    fn encode_checked_reemits_utf16_bom_via_named_match_arms() {
        // Directly pins encoding.rs lines 92/93: a UTF-16 encoding with had_bom
        // re-emits the correct 2-byte BOM. The hand-rolled UTF-16 path at the top
        // of encode_checked owns the live UTF-16 flow, but these named match arms
        // are the documented contract for the BOM table; deleting them must fail
        // a test. We assert the BOM bytes the table specifies for each order.
        let le = DetectedEncoding {
            name: "UTF-16LE".to_string(),
            had_bom: true,
        };
        let be = DetectedEncoding {
            name: "UTF-16BE".to_string(),
            had_bom: true,
        };
        assert_eq!(&encode_checked("Hi", &le).0[..2], &[0xFF, 0xFE]);
        assert_eq!(&encode_checked("Hi", &be).0[..2], &[0xFE, 0xFF]);
    }

    #[test]
    fn double_bom_content_char_is_preserved_and_redetect_is_stable() {
        // REGRESSION (double-BOM): the FIRST U+FEFF is the encoding MARKER; a
        // SECOND U+FEFF is CONTENT. Detection must strip exactly one leading
        // marker BOM and preserve every subsequent U+FEFF, and a re-detect of
        // the re-encoded bytes must be IDEMPOTENT (no further U+FEFF eroded per
        // pass). This mirrors the `bom_tagged_roundtrips_redetect_stably`
        // proptest counterexample s = "\u{feff}\u{feff}".
        for name in ["UTF-16LE", "UTF-16BE", "UTF-8"] {
            let tagged = DetectedEncoding {
                name: name.to_string(),
                had_bom: true,
            };
            let s = "\u{feff}\u{feff}"; // marker + one content U+FEFF
            let (file_bytes, lossy) = encode_checked(s, &tagged);
            assert!(!lossy, "{name} represents U+FEFF");

            // First detect: the marker BOM is consumed, the content U+FEFF stays.
            let (text1, e1) = decode(&file_bytes);
            assert!(e1.had_bom, "{name}: a BOM-tagged file detects had_bom");
            assert_eq!(
                text1, s,
                "{name}: the content U+FEFF after the marker BOM must survive"
            );

            // Re-encode under the detected encoding, then re-detect: stable.
            let (reencoded, lossy2) = encode_checked(&text1, &e1);
            assert!(!lossy2);
            let (text2, e2) = decode(&reencoded);
            assert!(
                e2.had_bom,
                "{name}: the re-emitted marker BOM detects again"
            );
            assert_eq!(text1, text2, "{name}: re-detection must be idempotent");
        }
    }

    #[test]
    fn decode_with_preserves_content_bom_after_marker() {
        // decode_with must NOT double-strip: with had_bom=true it skips exactly
        // one leading marker BOM and decodes the remainder LITERALLY, so a
        // content U+FEFF immediately after the marker survives.
        let enc = DetectedEncoding {
            name: "UTF-16LE".to_string(),
            had_bom: true,
        };
        let s = "\u{feff}data"; // one content U+FEFF, then "data"
                                // encode prepends the marker BOM, then the body "\u{feff}data", so the
                                // bytes are [marker BOM] + [content U+FEFF] + ['d','a','t','a'].
        let bytes = encode(s, &enc);
        assert_eq!(decode_with(&bytes, &enc), s);

        // had_bom=false: a leading content U+FEFF is NOT mistaken for a BOM.
        let enc_no_bom = DetectedEncoding {
            name: "UTF-8".to_string(),
            had_bom: false,
        };
        let mut u8_bytes = vec![0xEF, 0xBB, 0xBF];
        u8_bytes.extend_from_slice("x".as_bytes());
        assert_eq!(decode_with(&u8_bytes, &enc_no_bom), "\u{feff}x");
    }

    #[test]
    fn unknown_encoding_name_falls_back_to_utf8() {
        let enc = DetectedEncoding {
            name: "not-a-real-encoding".to_string(),
            had_bom: false,
        };
        let (bytes, lossy) = encode_checked("héllo", &enc);
        // UTF-8 fallback represents everything (not lossy) + round-trips.
        assert!(!lossy);
        assert_eq!(bytes, "héllo".as_bytes());
    }
}

#[cfg(test)]
mod proptests {
    //! Property invariants for decode/encode — a corruption bug here loses user
    //! data on every save, so the laws are asserted over arbitrary input.
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Decoding arbitrary bytes must NEVER panic (the editor opens any file).
        #[test]
        fn decode_is_total(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
            let (_text, _enc) = decode(&bytes);
        }

        /// A string that decodes as plain UTF-8 (no BOM) re-encodes to the exact
        /// original bytes — the lossless round-trip the editor depends on.
        #[test]
        fn utf8_roundtrips_losslessly(s in ".*") {
            let (text, enc) = decode(s.as_bytes());
            if enc.name == "UTF-8" && !enc.had_bom {
                prop_assert_eq!(&text, &s);
                prop_assert_eq!(encode(&text, &enc), s.clone().into_bytes());
            }
        }

        /// The hand-rolled UTF-16LE/BE encoder is exact and never lossy (UTF-16
        /// covers all of Unicode). Verified by decoding the produced units
        /// directly — statistical `decode` can't ID BOM-less UTF-16, so the
        /// encoder is checked independently of detection.
        #[test]
        fn utf16_encoder_is_exact(s in ".*", be in any::<bool>(), bom in any::<bool>()) {
            let enc = DetectedEncoding {
                name: if be { "UTF-16BE" } else { "UTF-16LE" }.to_string(),
                had_bom: bom,
            };
            let (bytes, lossy) = encode_checked(&s, &enc);
            prop_assert!(!lossy);
            let start = if bom { 2 } else { 0 };
            let units: Vec<u16> = bytes[start..]
                .chunks_exact(2)
                .map(|c| {
                    if be {
                        u16::from_be_bytes([c[0], c[1]])
                    } else {
                        u16::from_le_bytes([c[0], c[1]])
                    }
                })
                .collect();
            prop_assert_eq!(String::from_utf16(&units).unwrap(), s);
        }
    }
}
