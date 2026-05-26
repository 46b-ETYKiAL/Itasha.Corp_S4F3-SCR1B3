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
    let mut det = chardetng::EncodingDetector::new();
    det.feed(bytes, true);
    let enc = det.guess(None, true);
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
/// if the original had one. Falls back to UTF-8 for unknown names.
pub fn encode(text: &str, enc: &DetectedEncoding) -> Vec<u8> {
    let encoding = Encoding::for_label(enc.name.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    let (cow, _, _) = encoding.encode(text);
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
    out
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
}
