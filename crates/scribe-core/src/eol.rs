//! End-of-line handling. Detects the dominant line ending, normalizes the
//! in-memory text to `\n`, and re-applies the chosen style on save so files
//! round-trip with their original (or a user-chosen) EOL.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Eol {
    /// `\n` — Unix/macOS.
    #[default]
    Lf,
    /// `\r\n` — Windows.
    Crlf,
    /// `\r` — classic Mac.
    Cr,
}

impl Eol {
    pub fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::Crlf => "\r\n",
            Eol::Cr => "\r",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::Crlf => "CRLF",
            Eol::Cr => "CR",
        }
    }
}

/// Detect the dominant EOL of a string by counting occurrences.
pub fn detect(text: &str) -> Eol {
    let crlf = text.matches("\r\n").count();
    // Lone CRs = total CR minus the ones that were part of CRLF.
    let total_cr = text.matches('\r').count();
    let lone_cr = total_cr.saturating_sub(crlf);
    // Lone LFs = total LF minus the ones that were part of CRLF.
    let total_lf = text.matches('\n').count();
    let lone_lf = total_lf.saturating_sub(crlf);

    if crlf >= lone_lf && crlf >= lone_cr && crlf > 0 {
        Eol::Crlf
    } else if lone_cr > lone_lf && lone_cr > 0 {
        Eol::Cr
    } else {
        Eol::Lf
    }
}

/// Normalize any mix of CRLF/CR/LF to a single `\n` for in-memory editing.
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Apply an EOL style to LF-normalized text on the way out to disk.
pub fn apply(text: &str, eol: Eol) -> String {
    match eol {
        Eol::Lf => text.to_string(),
        Eol::Crlf => text.replace('\n', "\r\n"),
        Eol::Cr => text.replace('\n', "\r"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_crlf() {
        assert_eq!(detect("a\r\nb\r\nc"), Eol::Crlf);
    }

    #[test]
    fn detect_lf() {
        assert_eq!(detect("a\nb\nc"), Eol::Lf);
    }

    #[test]
    fn detect_cr() {
        assert_eq!(detect("a\rb\rc"), Eol::Cr);
    }

    #[test]
    fn roundtrip_crlf() {
        let original = "a\r\nb\r\n";
        let eol = detect(original);
        let norm = normalize_to_lf(original);
        assert_eq!(norm, "a\nb\n");
        assert_eq!(apply(&norm, eol), original);
    }

    #[test]
    fn roundtrip_cr_and_lf() {
        for original in ["x\ry\rz", "x\ny\nz", "only-one-line"] {
            let eol = detect(original);
            let norm = normalize_to_lf(original);
            assert_eq!(apply(&norm, eol), original, "round-trip for {original:?}");
        }
    }

    #[test]
    fn normalize_collapses_mixed_endings() {
        assert_eq!(normalize_to_lf("a\r\nb\rc\nd"), "a\nb\nc\nd");
    }

    #[test]
    fn apply_is_idempotent_on_lf_normalized_text() {
        // Applying LF to already-LF text is a no-op; applying then re-normalizing
        // recovers the LF form for every style.
        let norm = "a\nb\nc\n";
        for eol in [Eol::Lf, Eol::Crlf, Eol::Cr] {
            let applied = apply(norm, eol);
            assert_eq!(normalize_to_lf(&applied), norm, "re-normalize {eol:?}");
        }
    }

    #[test]
    fn detect_defaults_to_lf_without_line_endings() {
        assert_eq!(detect("no newlines here"), Eol::Lf);
        assert_eq!(detect(""), Eol::Lf);
    }

    #[test]
    fn detect_majority_wins_on_mixed() {
        // Two CRLF vs one lone LF → CRLF dominates.
        assert_eq!(detect("a\r\nb\r\nc\nd"), Eol::Crlf);
    }

    #[test]
    fn as_str_matches_apply_separator() {
        for eol in [Eol::Lf, Eol::Crlf, Eol::Cr] {
            assert_eq!(apply("a\nb", eol), format!("a{}b", eol.as_str()));
        }
    }
}
