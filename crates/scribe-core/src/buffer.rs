//! Phase 15 KEYSTONE — `Buffer` enum: mmap-browse for large files, rope for
//! edits, with first-edit promotion that converts a read-only mmap into a
//! mutable rope without ever touching the on-disk file.
//!
//! ## Why a new abstraction next to `Document`
//!
//! [`Document`] (in `document.rs`) ALREADY mmap-browses files above the
//! 256 MiB threshold, but **immediately copies** the mmap'd bytes into a
//! `Rope`. That defeats the multi-GB browse target (a 4 GiB log file would
//! cost a 4 GiB rope at open time). The rope-editor KEYSTONE design
//! requires a buffer that **stays in the mmap representation until the
//! first edit** so a multi-GB read-only browse session keeps RSS bounded.
//!
//! `Buffer` is the new abstraction. The widget consumes `&mut Buffer` and
//! triggers [`Buffer::promote_to_rope`] when the user types. The
//! `Document` integration is a follow-up — for now the two abstractions
//! coexist and the widget operates on `Buffer` directly.
//!
//! ## Variants
//!
//! - [`Buffer::Mmap`]  — read-only memory-mapped file. Holds the live
//!   `memmap2::Mmap` handle + a lazily-built line index (offset to each
//!   `\n`). Indexing is `O(log n)` after the index is built.
//! - [`Buffer::Rope`]  — owned, mutable rope (the ropey 1.6 implementation).
//!   Created from scratch, from a small file, or from an mmap promoted at
//!   first edit.
//!
//! ## Promotion path
//!
//! [`Buffer::promote_to_rope`] decodes the mmap as UTF-8 (lossy on
//! invalid bytes — matches the existing `Document::open` discipline) and
//! moves the resulting string into a fresh `Rope::from_str`. The mmap
//! handle is dropped. The on-disk file is never touched.
//!
//! ## Why not put this on `Document`
//!
//! `Document` carries encoding + EOL + dirty state already. Adding the
//! mmap-then-promote dance to `Document` widens its API surface for a
//! feature only the rope-editor widget needs. The KEYSTONE design keeps
//! `Buffer` as the lower-layer storage; the rope-editor widget owns it;
//! follow-ups may unify the two when the multi-GB browse path lands in
//! production.

use ropey::Rope;
use std::fs;
use std::io;
use std::path::Path;

/// Files at or above this size open as [`Buffer::Mmap`] rather than loading
/// the whole thing into a `Rope`. 16 MiB matches the dossier — well under the
/// 256 MiB `Document::LARGE_FILE_THRESHOLD` because KEYSTONE wants browse
/// mode kicking in much earlier so a 100 MiB log opens instant-bounded
/// instead of paying the rope-load cost.
pub const MMAP_THRESHOLD: u64 = 16 * 1024 * 1024;

/// The buffer storage modes the rope-editor widget reads from. See the
/// module rustdoc for the architecture rationale.
#[derive(Debug)]
pub enum Buffer {
    /// Read-only memory-mapped view of a file on disk. Holds the live
    /// `memmap2::Mmap` handle and a lazily-built line index. The widget
    /// reads through this without copying; the first edit promotes to
    /// [`Buffer::Rope`].
    Mmap {
        mmap: memmap2::Mmap,
        /// Byte-offset of each `\n` in the mapped file, built on demand.
        /// Empty until [`Buffer::line_count`] or related queries are
        /// called; thereafter it covers `0..first_unindexed_byte`.
        line_index: Vec<u64>,
        /// First byte past the indexed prefix. Equal to `0` while the
        /// index is empty; bumped lazily as the widget queries more
        /// lines. Always `<= mmap.len()`.
        first_unindexed_byte: u64,
    },
    /// Owned mutable rope. Used for everything under [`MMAP_THRESHOLD`]
    /// AND for any buffer the user has edited (post-promotion).
    Rope(Rope),
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer::Rope(Rope::new())
    }
}

impl Buffer {
    /// Open a file. Files at or above [`MMAP_THRESHOLD`] open as
    /// [`Buffer::Mmap`]; smaller files load into a `Rope`.
    ///
    /// On a successful mmap, the file handle is closed (memmap2 keeps its
    /// own internal handle); the caller never sees the `File`. On UTF-8
    /// decode errors the rope path uses lossy decode.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let meta = fs::metadata(path)?;
        let size = meta.len();

        if size >= MMAP_THRESHOLD {
            let file = fs::File::open(path)?;
            // SAFETY: read-only mmap of a file we just opened; we never
            // write through it and the only reads happen via the
            // `Buffer::Mmap` variant accessors below. Documented exception
            // to the crate-root `#![deny(unsafe_code)]`.
            #[allow(unsafe_code)]
            let mmap = unsafe { memmap2::Mmap::map(&file)? };
            Ok(Buffer::Mmap {
                mmap,
                line_index: Vec::new(),
                first_unindexed_byte: 0,
            })
        } else {
            let bytes = fs::read(path)?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            Ok(Buffer::Rope(Rope::from_str(&text)))
        }
    }

    /// Total byte length of the buffer.
    /// Build an in-memory (editable) rope buffer from a string. Convenience so
    /// callers don't need a direct `ropey` dependency.
    pub fn from_text(text: &str) -> Self {
        Buffer::Rope(Rope::from_str(text))
    }

    pub fn len_bytes(&self) -> usize {
        match self {
            Buffer::Mmap { mmap, .. } => mmap.len(),
            Buffer::Rope(r) => r.len_bytes(),
        }
    }

    /// True if the buffer carries zero bytes. Empty mmap files and empty
    /// ropes both return true.
    pub fn is_empty(&self) -> bool {
        self.len_bytes() == 0
    }

    /// `true` if the buffer is currently read-only (mmap variant). The
    /// widget calls this to decide whether to show the read-only banner +
    /// to gate the promote-then-edit handler.
    pub fn is_read_only(&self) -> bool {
        matches!(self, Buffer::Mmap { .. })
    }

    /// Promote an [`Buffer::Mmap`] to [`Buffer::Rope`] in place. UTF-8
    /// lossy-decode matches the existing `Document::open` discipline so a
    /// mid-file invalid byte never aborts the editor. No-op on rope.
    ///
    /// After promotion the mmap handle is dropped (memmap2 unmaps as it
    /// goes out of scope) and the on-disk file is never modified.
    pub fn promote_to_rope(&mut self) -> io::Result<()> {
        if let Buffer::Mmap { mmap, .. } = self {
            let text = String::from_utf8_lossy(mmap).into_owned();
            *self = Buffer::Rope(Rope::from_str(&text));
        }
        Ok(())
    }

    /// Borrow the rope when the buffer is in [`Buffer::Rope`] mode.
    /// Returns `None` for [`Buffer::Mmap`] so the caller is forced to
    /// promote first if it wants a `&Rope`.
    pub fn as_rope(&self) -> Option<&Rope> {
        match self {
            Buffer::Rope(r) => Some(r),
            Buffer::Mmap { .. } => None,
        }
    }

    /// Mutable rope borrow. The caller is responsible for promoting first
    /// if necessary; this never auto-promotes (an auto-promotion on the
    /// read path would surprise the caller into a multi-GiB copy).
    pub fn as_rope_mut(&mut self) -> Option<&mut Rope> {
        match self {
            Buffer::Rope(r) => Some(r),
            Buffer::Mmap { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn default_is_empty_rope() {
        let b = Buffer::default();
        assert!(matches!(b, Buffer::Rope(_)));
        assert!(b.is_empty());
        assert!(!b.is_read_only());
    }

    #[test]
    fn open_small_file_returns_rope() -> io::Result<()> {
        let mut f = NamedTempFile::new()?;
        writeln!(f, "hello rope")?;
        let b = Buffer::open(f.path())?;
        assert!(matches!(b, Buffer::Rope(_)));
        assert!(!b.is_read_only());
        assert!(b.len_bytes() > 0);
        Ok(())
    }

    #[test]
    fn open_large_file_returns_mmap() -> io::Result<()> {
        let mut f = NamedTempFile::new()?;
        // Just past MMAP_THRESHOLD — 16 MiB + 1 byte.
        let payload = vec![b'a'; (MMAP_THRESHOLD as usize) + 1];
        f.write_all(&payload)?;
        f.flush()?;
        let b = Buffer::open(f.path())?;
        assert!(matches!(b, Buffer::Mmap { .. }));
        assert!(b.is_read_only());
        assert_eq!(b.len_bytes(), payload.len());
        Ok(())
    }

    #[test]
    fn promote_to_rope_converts_mmap_losslessly() -> io::Result<()> {
        let mut f = NamedTempFile::new()?;
        let payload = vec![b'x'; (MMAP_THRESHOLD as usize) + 32];
        f.write_all(&payload)?;
        f.flush()?;
        let mut b = Buffer::open(f.path())?;
        assert!(matches!(b, Buffer::Mmap { .. }));
        b.promote_to_rope()?;
        assert!(matches!(b, Buffer::Rope(_)));
        assert!(!b.is_read_only());
        assert_eq!(b.len_bytes(), payload.len());
        // The rope's text matches the original mmap content.
        let rope = b.as_rope().expect("rope after promote");
        let body = rope.to_string();
        assert_eq!(body.len(), payload.len());
        assert!(body.chars().all(|c| c == 'x'));
        Ok(())
    }

    #[test]
    fn promote_on_rope_is_noop() {
        let mut b = Buffer::Rope(Rope::from_str("hello"));
        b.promote_to_rope().unwrap();
        assert!(matches!(b, Buffer::Rope(_)));
        assert_eq!(b.len_bytes(), 5);
    }

    #[test]
    fn as_rope_returns_some_on_rope_none_on_mmap() {
        let r = Buffer::Rope(Rope::from_str("hi"));
        assert!(r.as_rope().is_some());
    }
}
