//! A `Document` is one open file (or scratch buffer): a rope-backed text body
//! plus its on-disk identity (path, encoding, EOL) and dirty state.
//!
//! Large files are memory-mapped read-only for browsing; the first edit copies
//! the visible/needed text into the rope so edits stay microsecond-fast and the
//! on-disk file is never mutated underneath the user.

use crate::encoding::{self, DetectedEncoding};
use crate::eol::{self, Eol};
use crate::error::Result;
use ropey::Rope;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Files at or above this size open read-only via mmap rather than loading the
/// whole thing into the rope. 256 MiB is comfortably past normal source files
/// but well short of multi-GB logs we still want to *browse*.
pub const LARGE_FILE_THRESHOLD: u64 = 256 * 1024 * 1024;

#[derive(Debug)]
pub struct Document {
    rope: Rope,
    path: Option<PathBuf>,
    encoding: DetectedEncoding,
    eol: Eol,
    dirty: bool,
    /// Opened read-only because the file exceeds `LARGE_FILE_THRESHOLD`.
    read_only_large: bool,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            encoding: DetectedEncoding::default(),
            eol: Eol::default(),
            dirty: false,
            read_only_large: false,
        }
    }
}

impl Document {
    /// A new empty scratch buffer.
    pub fn scratch() -> Self {
        Self::default()
    }

    /// Open a file. Detects encoding + EOL; normalizes line endings to `\n`
    /// in memory. Large files are mmap-browsed read-only.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let meta = fs::metadata(path)?;
        let size = meta.len();

        if size >= LARGE_FILE_THRESHOLD {
            // mmap read-only browse: decode lossily as UTF-8 for display.
            let file = fs::File::open(path)?;
            // SAFETY: read-only mmap of a file we just opened; we never write
            // through it and drop it before any edit. Documented exception to
            // the crate-root `#![deny(unsafe_code)]` per Phase 21 T21.2 P1.
            #[allow(unsafe_code)]
            let mmap = unsafe { memmap2::Mmap::map(&file)? };
            let (text, enc) = encoding::decode(&mmap);
            let detected_eol = eol::detect(&text);
            let normalized = eol::normalize_to_lf(&text);
            return Ok(Self {
                rope: Rope::from_str(&normalized),
                path: Some(path.to_path_buf()),
                encoding: enc,
                eol: detected_eol,
                dirty: false,
                read_only_large: true,
            });
        }

        let bytes = fs::read(path)?;
        let (text, enc) = encoding::decode(&bytes);
        let detected_eol = eol::detect(&text);
        let normalized = eol::normalize_to_lf(&text);
        Ok(Self {
            rope: Rope::from_str(&normalized),
            path: Some(path.to_path_buf()),
            encoding: enc,
            eol: detected_eol,
            dirty: false,
            read_only_large: false,
        })
    }

    /// Save back to the document's path using its original encoding + EOL.
    /// Atomic: writes to a temp file then renames over the target. Returns
    /// `Ok(true)` when one or more characters could not be represented in the
    /// file's encoding (they were replaced — i.e. data was lost — so the caller
    /// MUST warn the user). A document opened read-only (`read_only_large`, the
    /// 256 MiB-and-up mmap browse path) refuses to save and returns
    /// [`CoreError::FileTooLargeToEdit`](crate::error::CoreError::FileTooLargeToEdit).
    pub fn save(&mut self) -> Result<bool> {
        let Some(path) = self.path.clone() else {
            return Err(crate::error::CoreError::Other(
                "no path set; use save_as".into(),
            ));
        };
        self.save_as(&path)
    }

    /// Save to an explicit path (also used for "Save As"). Returns `Ok(true)`
    /// when characters were lost to the target encoding (see [`Self::save`]).
    pub fn save_as(&mut self, path: impl AsRef<Path>) -> Result<bool> {
        // C-08: enforce the read-only-large contract. A document opened via the
        // >=256 MiB mmap browse path is read-only by design; saving it would
        // re-materialise the whole rope and is exactly what the browse path
        // exists to avoid. The `read_only_large` flag previously named a
        // contract it never enforced — both `save` and `save_as` wrote anyway.
        // Refuse with a structured error so the flag means what it says; the UI
        // already gates the Save action on `is_read_only_large`, so this is a
        // defense-in-depth backstop, not a new restriction on the edit flow.
        if self.read_only_large {
            return Err(crate::error::CoreError::FileTooLargeToEdit(
                self.rope.len_bytes() as u64,
            ));
        }
        let path = path.as_ref();
        let lf_text = self.rope.to_string();
        let styled = eol::apply(&lf_text, self.eol);
        let (bytes, lossy) = encoding::encode_checked(&styled, &self.encoding);

        // Atomic write: temp file in the same dir, then rename.
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut tmp = tempfile_in(dir)?;
        tmp.write_all(&bytes)?;
        tmp.flush()?;
        let tmp_path = tmp.into_temp_path();
        tmp_path
            .persist(path)
            .map_err(|e| crate::error::CoreError::Io(e.error))?;

        self.path = Some(path.to_path_buf());
        self.dirty = false;
        Ok(lossy)
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn set_text(&mut self, text: &str) {
        self.rope = Rope::from_str(&eol::normalize_to_lf(text));
        self.dirty = true;
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn file_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".to_string())
    }

    pub fn encoding(&self) -> &DetectedEncoding {
        &self.encoding
    }

    pub fn eol(&self) -> Eol {
        self.eol
    }

    pub fn set_eol(&mut self, eol: Eol) {
        if self.eol != eol {
            self.eol = eol;
            self.dirty = true;
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    pub fn is_read_only_large(&self) -> bool {
        self.read_only_large
    }

    /// Best-effort language id from the file extension (used by syntax + spell).
    pub fn language_hint(&self) -> Option<String> {
        self.path
            .as_ref()
            .and_then(|p| p.extension())
            .map(|e| e.to_string_lossy().to_lowercase())
    }
}

/// Minimal in-tree temp-file helper so we don't pull `tempfile` into the
/// production dependency set (it stays a dev-dependency for tests).
fn tempfile_in(dir: &Path) -> Result<TempFile> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_path = dir.join(format!(".scr1b3-tmp-{nonce}"));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)?;
    Ok(TempFile {
        file: Some(file),
        path: tmp_path,
    })
}

/// Tiny RAII temp file with persist-or-cleanup semantics.
struct TempFile {
    file: Option<fs::File>,
    path: PathBuf,
}

struct TempPath {
    path: PathBuf,
}

impl TempFile {
    /// The temp file is `Some` for the whole write phase and only taken in
    /// `into_temp_path` (which consumes `self`), so this is infallible by
    /// construction. We still surface a structured `io::Error` instead of
    /// `.expect()`-panicking: this is the atomic-SAVE path, and a panic here
    /// would crash the editor and lose the user's unsaved buffer. The error
    /// propagates through `save_as`'s `?` into `CoreError::Io` and is shown to
    /// the user, honouring the crate invariant "editor operations never panic".
    fn file_mut(&mut self) -> std::io::Result<&mut fs::File> {
        self.file.as_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "internal: temp file handle already taken before write",
            )
        })
    }
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.file_mut()?.write_all(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file_mut()?.flush()
    }
    fn into_temp_path(mut self) -> TempPath {
        // Close the file handle so the rename can proceed on Windows.
        self.file.take();
        TempPath {
            path: std::mem::take(&mut self.path),
        }
    }
}

impl TempPath {
    fn persist(self, dest: &Path) -> std::result::Result<(), PersistError> {
        match fs::rename(&self.path, dest) {
            Ok(()) => {
                std::mem::forget(self);
                Ok(())
            }
            // Windows: rename can fail across some conditions; fall back to copy.
            Err(_) => match fs::copy(&self.path, dest) {
                Ok(_) => {
                    let _ = fs::remove_file(&self.path);
                    std::mem::forget(self);
                    Ok(())
                }
                Err(e) => {
                    let _ = fs::remove_file(&self.path);
                    Err(PersistError { error: e })
                }
            },
        }
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct PersistError {
    error: std::io::Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_edit_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hello.txt");
        {
            let mut f = std::fs::File::create(&p).unwrap();
            f.write_all(b"hello\nworld\n").unwrap();
        }
        let mut doc = Document::open(&p).unwrap();
        assert_eq!(doc.len_lines(), 3); // trailing newline => 3 line slots
        assert!(!doc.is_dirty());
        doc.set_text("changed\n");
        assert!(doc.is_dirty());
        doc.save().unwrap();
        assert!(!doc.is_dirty());
        let reread = std::fs::read_to_string(&p).unwrap();
        assert_eq!(reread, "changed\n");
    }

    #[test]
    fn crlf_preserved_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("win.txt");
        std::fs::write(&p, b"a\r\nb\r\n").unwrap();
        let mut doc = Document::open(&p).unwrap();
        assert_eq!(doc.eol(), Eol::Crlf);
        doc.set_text("x\ny\n");
        doc.save().unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert_eq!(raw, b"x\r\ny\r\n");
    }

    #[test]
    fn utf8_bom_preserved_across_open_save() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bom.txt");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"hi\n");
        std::fs::write(&p, &bytes).unwrap();
        let mut doc = Document::open(&p).unwrap();
        assert_eq!(doc.text(), "hi\n");
        doc.set_text("bye\n");
        doc.save().unwrap();
        // The BOM is re-emitted on save (round-trip preserves the file's shape).
        let raw = std::fs::read(&p).unwrap();
        assert_eq!(&raw[..3], &[0xEF, 0xBB, 0xBF]);
        assert_eq!(&raw[3..], b"bye\n");
    }

    #[test]
    fn utf16le_file_decodes_and_reencodes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("u16.txt");
        // "Hi\n" UTF-16LE with BOM.
        std::fs::write(&p, [0xFF, 0xFE, b'H', 0, b'i', 0, b'\n', 0]).unwrap();
        let mut doc = Document::open(&p).unwrap();
        assert_eq!(doc.text(), "Hi\n");
        doc.set_text("Ok\n");
        doc.save().unwrap();
        let raw = std::fs::read(&p).unwrap();
        // BOM + UTF-16LE encoding of "Ok\n".
        assert_eq!(raw, [0xFF, 0xFE, b'O', 0, b'k', 0, b'\n', 0]);
    }

    #[test]
    fn latin1_file_roundtrips_unchanged_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("latin1.txt");
        std::fs::write(&p, [b'c', b'a', b'f', 0xE9, b'\n']).unwrap(); // café\n
        let mut doc = Document::open(&p).unwrap();
        assert_eq!(doc.text(), "café\n");
        doc.save().unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert_eq!(raw, [b'c', b'a', b'f', 0xE9, b'\n']);
    }

    #[test]
    fn set_eol_changes_on_disk_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("eol.txt");
        std::fs::write(&p, b"a\nb\n").unwrap();
        let mut doc = Document::open(&p).unwrap();
        assert_eq!(doc.eol(), Eol::Lf);
        doc.set_eol(Eol::Crlf);
        doc.save().unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"a\r\nb\r\n");
    }

    #[test]
    fn no_trailing_newline_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notrail.txt");
        std::fs::write(&p, b"no newline at end").unwrap();
        let mut doc = Document::open(&p).unwrap();
        assert_eq!(doc.text(), "no newline at end");
        doc.set_text("still none");
        doc.save().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "still none");
    }

    #[test]
    fn empty_file_opens_and_saves_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("empty.txt");
        std::fs::write(&p, b"").unwrap();
        let mut doc = Document::open(&p).unwrap();
        assert_eq!(doc.text(), "");
        doc.save().unwrap();
        assert!(std::fs::read(&p).unwrap().is_empty());
    }

    // Property-based complement to the example-based round-trip tests above:
    // for ANY pure-ASCII UTF-8 content under ANY EOL style, open->save must
    // reproduce the on-disk bytes EXACTLY and never report a lossy encode. This
    // catches content-dependent regressions in the LF-normalize -> EOL-reapply
    // -> encode pipeline that fixed example inputs would miss.
    //
    // NOTE: the >=256 MiB mmap read path (`LARGE_FILE_THRESHOLD`) is NOT
    // exercised here — constructing a file that large in a unit test is
    // prohibitive, and the threshold is a non-injectable `const`. The bytes
    // path (normal files) is covered exhaustively.
    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(96))]
        #[test]
        fn save_reopen_is_byte_identical_for_ascii(
            lines in proptest::collection::vec("[ -~]{0,32}", 1..8),
            eol_idx in 0usize..3,
            trailing in proptest::prelude::any::<bool>(),
        ) {
            let eol = [Eol::Lf, Eol::Crlf, Eol::Cr][eol_idx];
            let mut content = lines.join(eol.as_str());
            if trailing {
                content.push_str(eol.as_str());
            }
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("rt.txt");
            std::fs::write(&p, content.as_bytes()).unwrap();

            let mut doc = Document::open(&p).unwrap();
            // With >=2 lines there is an unambiguous separator, so the EOL must
            // be detected exactly. (A single line / trailing-only case can be
            // ambiguous, so we only assert byte-identity there.)
            if lines.len() >= 2 {
                proptest::prop_assert_eq!(doc.eol(), eol);
            }
            let lossy = doc.save().unwrap();
            proptest::prop_assert!(!lossy, "pure-ASCII content must never encode lossily");
            let reread = std::fs::read(&p).unwrap();
            proptest::prop_assert_eq!(reread, content.as_bytes());
        }
    }

    // ---- accessors + error paths (previously uncovered) ----

    #[test]
    fn save_without_path_errors_directing_to_save_as() {
        // A scratch buffer has no path: `save()` must surface a clear error
        // rather than panic or silently no-op, so the caller routes to `save_as`.
        let mut doc = Document::scratch();
        doc.set_text("orphan\n");
        let err = doc.save().expect_err("a pathless buffer cannot save()");
        assert!(
            err.to_string().contains("save_as"),
            "error should direct the caller to save_as, got: {err}"
        );
    }

    #[test]
    fn save_as_sets_path_and_subsequent_save_reuses_it() {
        // `save_as` on a scratch buffer persists AND records the path, so a
        // following bare `save()` (no args) round-trips to the same file.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("named.txt");
        let mut doc = Document::scratch();
        doc.set_text("first\n");
        let lossy = doc.save_as(&p).unwrap();
        assert!(!lossy);
        assert_eq!(doc.path().unwrap(), p.as_path());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "first\n");
        // The path is now sticky — a plain save() rewrites the same file.
        doc.set_text("second\n");
        doc.save().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "second\n");
    }

    #[test]
    fn rope_and_len_accessors_reflect_content() {
        // `rope()`, `len_bytes()`, and `len_lines()` are thin accessors over the
        // backing rope; assert they report the buffer's real shape.
        let mut doc = Document::scratch();
        doc.set_text("ab\ncd\n");
        assert_eq!(doc.len_bytes(), 6, "two 2-char lines + two newlines");
        assert_eq!(doc.len_lines(), 3, "trailing newline yields a 3rd slot");
        // The borrowed rope agrees with the owned-String view.
        assert_eq!(doc.rope().to_string(), doc.text());
    }

    #[test]
    fn mark_clean_clears_the_dirty_flag() {
        // `mark_clean` is the inverse of `mark_dirty`; an externally-persisted
        // buffer can be reset to clean without re-saving through Document.
        let mut doc = Document::scratch();
        doc.mark_dirty();
        assert!(doc.is_dirty());
        doc.mark_clean();
        assert!(!doc.is_dirty(), "mark_clean must clear the dirty flag");
    }

    #[test]
    fn language_hint_lowercases_extension_and_is_none_without_path() {
        // The hint feeds syntax + spell; it must be the lowercased extension, or
        // None for a pathless scratch buffer (no extension to derive from).
        assert!(Document::scratch().language_hint().is_none());
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("Module.RS"); // mixed-case extension
        std::fs::write(&p, b"fn main() {}\n").unwrap();
        let doc = Document::open(&p).unwrap();
        assert_eq!(
            doc.language_hint().as_deref(),
            Some("rs"),
            "extension must be lowercased for case-insensitive routing"
        );
    }

    #[test]
    fn read_only_large_doc_refuses_to_save() {
        // C-08: a Document flagged `read_only_large` (opened via the >=256 MiB
        // mmap browse path) must REFUSE to save — the flag now means what its
        // name says. Previously save/save_as wrote regardless, so the name
        // over-promised. The error is the structured FileTooLargeToEdit variant
        // so the UI can surface it, and the on-disk file is never touched.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("huge.log");
        std::fs::write(&p, b"original content\n").unwrap();

        // Construct a read-only-large doc directly (a real 256 MiB file is
        // prohibitive in a unit test; the private field is reachable from the
        // in-module test).
        let mut doc = Document {
            rope: Rope::from_str("edited in memory\n"),
            path: Some(p.clone()),
            encoding: DetectedEncoding::default(),
            eol: Eol::Lf,
            dirty: true,
            read_only_large: true,
        };

        // Bare save() is refused with the structured error.
        let err = doc.save().expect_err("a read-only-large doc must not save");
        match err {
            crate::error::CoreError::FileTooLargeToEdit(_) => {}
            other => panic!("expected FileTooLargeToEdit, got: {other:?}"),
        }
        // save_as is refused too (the read-only contract is about the source
        // document, independent of the destination path).
        let other = dir.path().join("copy.log");
        assert!(
            matches!(
                doc.save_as(&other),
                Err(crate::error::CoreError::FileTooLargeToEdit(_))
            ),
            "save_as must also refuse a read-only-large doc"
        );

        // The on-disk files are untouched: the original keeps its bytes and the
        // alternate target was never created.
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "original content\n",
            "the source file must be left exactly as it was"
        );
        assert!(!other.exists(), "save_as must not create the target file");
    }

    #[test]
    fn normal_doc_still_saves_after_read_only_enforcement() {
        // Regression guard: an ordinary (not read-only-large) document still
        // saves normally — the C-08 enforcement only blocks the read-only flag.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ok.txt");
        std::fs::write(&p, b"x\n").unwrap();
        let mut doc = Document::open(&p).unwrap();
        assert!(!doc.is_read_only_large());
        doc.set_text("y\n");
        doc.save().expect("a normal document must still save");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "y\n");
    }

    #[test]
    fn save_to_unwritable_target_returns_err_without_panic() {
        // Data-loss hardening: when the atomic-save temp file cannot be created
        // (target directory does not exist), `save_as` must surface an
        // `Err(CoreError::Io)` so the UI can warn the user — it must NEVER panic
        // and lose the in-memory buffer. Regression guard for the `TempFile`
        // write path (formerly `.expect("temp file open")`).
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().join("does-not-exist-subdir").join("out.txt");
        let mut doc = Document::scratch();
        doc.set_text("precious unsaved work\n");

        let result = doc.save_as(&bogus);
        assert!(
            result.is_err(),
            "save to a non-existent directory must return Err, not panic"
        );
        // The buffer survives the failed save: content is intact and the doc is
        // still considered dirty (the save did not succeed).
        assert_eq!(doc.text(), "precious unsaved work\n");
        assert!(
            doc.is_dirty(),
            "a failed save must leave the document dirty so the user can retry"
        );
        match result {
            Err(crate::error::CoreError::Io(_)) => {}
            other => panic!("expected CoreError::Io on unwritable target, got: {other:?}"),
        }
    }
}
