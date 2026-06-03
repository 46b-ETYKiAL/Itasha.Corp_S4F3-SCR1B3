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
    /// MUST warn the user).
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
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.file.as_mut().expect("temp file open").write_all(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.as_mut().expect("temp file open").flush()
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
}
