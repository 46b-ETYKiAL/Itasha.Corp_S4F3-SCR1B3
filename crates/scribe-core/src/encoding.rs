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
    // BOM sniff first — authoritative when present.
    if let Some((enc, bom_len)) = Encoding::for_bom(bytes) {
        let (text, _, _) = enc.decode(&bytes[bom_len..]);
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
