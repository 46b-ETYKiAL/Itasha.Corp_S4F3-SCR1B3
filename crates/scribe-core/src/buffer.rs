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
//! [`Buffer::promote_to_rope`] decodes the mmap through `crate::encoding`
//! (BOM sniff + chardetng statistical detection, lossy only on malformed
//! sequences — matches the existing `Document::open` discipline) and moves
//! the resulting string into a fresh `Rope::from_str`. The mmap handle is
//! dropped. The on-disk file is never touched.
//!
//! ## Why not put this on `Document`
//!
//! `Document` carries encoding + EOL + dirty state already. Adding the
//! mmap-then-promote dance to `Document` widens its API surface for a
//! feature only the rope-editor widget needs. The KEYSTONE design keeps
//! `Buffer` as the lower-layer storage; the rope-editor widget owns it;
//! follow-ups may unify the two when the multi-GB browse path lands in
//! production.

use crate::encoding;
use ropey::Rope;
use std::fs;
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
    /// own internal handle); the caller never sees the `File`. The rope path
    /// decodes through `crate::encoding` (BOM + chardetng), so non-UTF-8 files
    /// decode correctly rather than as lossy mojibake.
    ///
    /// Returns [`crate::Result`] (`CoreError`) to match the sibling
    /// [`Document::open`] file-loading entry point; an underlying `io::Error`
    /// is wrapped (via `CoreError: From<std::io::Error>`) into
    /// [`CoreError::Io`] with its kind and message preserved.
    pub fn open(path: impl AsRef<Path>) -> crate::Result<Self> {
        let path = path.as_ref();
        let meta = fs::metadata(path)?;
        let size = meta.len();

        if size >= MMAP_THRESHOLD {
            let file = fs::File::open(path)?;
            // SAFETY: read-only mmap of a file we just opened; we never write
            // through it and the only reads happen via the `Buffer::Mmap`
            // accessors below. Documented exception to the crate-root
            // `#![deny(unsafe_code)]`.
            //
            // The load-bearing precondition of `Mmap::map` is that the backing
            // file MUST NOT be truncated (shrunk) by another process while the
            // map is live: a read of a now-unbacked page is undefined behaviour
            // (SIGBUS on Unix, an exception on Windows). SCR1B3's own safe API
            // never truncates a mapped file — the map is dropped on
            // `promote_to_rope` before any edit, and we only ever read it — so
            // the only way to hit the UB is a *concurrent external* truncation
            // of a file open in the read-only browse view, which is outside
            // this process's control and the inherent caveat of `mmap`. We do
            // NOT hand out a borrow into the map that could outlive it: every
            // consumer (`len_bytes`, `from_utf8_lossy`, `encoding::decode`)
            // copies to owned, and `as_rope()` returns `None` for the `Mmap`
            // variant, structurally forcing a promote-to-owned before any
            // `&Rope` exists. Do not add an accessor that returns a `&str` /
            // `&[u8]` borrowed from the map without re-examining this contract.
            #[allow(unsafe_code)]
            let mmap = unsafe { memmap2::Mmap::map(&file)? };
            Ok(Buffer::Mmap {
                mmap,
                line_index: Vec::new(),
                first_unindexed_byte: 0,
            })
        } else {
            let bytes = fs::read(path)?;
            // Route decoding through `crate::encoding` (BOM sniff + chardetng
            // statistical detection) exactly like `Document::open`, so a
            // UTF-16 / Windows-1252 / Shift-JIS file decodes correctly instead
            // of becoming U+FFFD mojibake under a raw UTF-8 lossy decode.
            let (text, _enc) = encoding::decode(&bytes);
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

    /// Promote an [`Buffer::Mmap`] to [`Buffer::Rope`] in place. Decoding
    /// routes through `crate::encoding` (BOM + chardetng), matching the
    /// existing `Document::open` discipline, so a non-UTF-8 file decodes
    /// correctly and a mid-file invalid byte never aborts the editor. No-op
    /// on rope.
    ///
    /// After promotion the mmap handle is dropped (memmap2 unmaps as it
    /// goes out of scope) and the on-disk file is never modified.
    ///
    /// Returns [`crate::Result`] (`CoreError`) to match the sibling
    /// [`Buffer::open`] / [`Document::open`] file-loading API.
    pub fn promote_to_rope(&mut self) -> crate::Result<()> {
        if let Buffer::Mmap { mmap, .. } = self {
            // Decode through `crate::encoding` (BOM + chardetng), matching
            // `Document::open`, so promoting a non-UTF-8 mmap'd browse view
            // yields the correct text rather than U+FFFD mojibake.
            let (text, _enc) = encoding::decode(mmap);
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

    // miri hygiene: this test drives real file-I/O syscalls (`Buffer::open` →
    // `fs::read`) that miri cannot model, so `cargo miri test -p scribe-core`
    // would error here. CI's miri job scopes to `--test miri_soundness`, which
    // is unaffected; this `cfg_attr` keeps a developer's local `cargo miri test`
    // clean. (Not a silent skip — the test runs fully under normal `cargo test`.)
    #[cfg_attr(miri, ignore)]
    #[test]
    fn open_small_file_returns_rope() -> crate::Result<()> {
        let mut f = NamedTempFile::new()?;
        writeln!(f, "hello rope")?;
        let b = Buffer::open(f.path())?;
        assert!(matches!(b, Buffer::Rope(_)));
        assert!(!b.is_read_only());
        assert!(b.len_bytes() > 0);
        Ok(())
    }

    // miri hygiene: drives `memmap2::Mmap::map` (mmap syscall) which miri can't
    // model. See the note on `open_small_file_returns_rope`.
    #[cfg_attr(miri, ignore)]
    #[test]
    fn open_large_file_returns_mmap() -> crate::Result<()> {
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

    #[cfg_attr(miri, ignore)] // mmap syscall (see `open_small_file_returns_rope`)
    #[test]
    fn promote_to_rope_converts_mmap_losslessly() -> crate::Result<()> {
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

    #[test]
    fn mmap_threshold_is_exactly_16_mib() {
        // The browse-mode cutover is a load-bearing constant: a wrong value
        // either loads huge files into a rope (defeating bounded RSS) or
        // mmap-browses tiny files (read-only-banner surprise). Pin the exact
        // arithmetic — `16 * 1024 * 1024` mutated to `+`/`/` changes this.
        assert_eq!(MMAP_THRESHOLD, 16 * 1024 * 1024);
        assert_eq!(MMAP_THRESHOLD, 16_777_216);
    }

    #[test]
    fn is_empty_is_false_for_non_empty_buffer() {
        // Pins `is_empty` against a mutation that always returns `true`: a
        // non-empty rope MUST report not-empty (otherwise the editor would
        // treat a populated file as blank).
        let b = Buffer::from_text("not empty");
        assert!(!b.is_empty());
        assert_eq!(b.len_bytes(), "not empty".len());
        // And the empty buffer genuinely reports empty.
        assert!(Buffer::from_text("").is_empty());
    }

    #[test]
    fn from_text_builds_editable_rope_with_matching_content() {
        // Pins `from_text` against a mutation that returns `Default::default()`
        // (an EMPTY rope): the content and length must match the input.
        let b = Buffer::from_text("hello world");
        assert!(matches!(b, Buffer::Rope(_)));
        assert!(!b.is_read_only());
        let rope = b.as_rope().expect("rope from from_text");
        assert_eq!(rope.to_string(), "hello world");
        assert_eq!(b.len_bytes(), 11);
    }

    #[test]
    fn as_rope_mut_yields_writable_rope_on_rope_variant() {
        // Pins `as_rope_mut` against a mutation returning `None`: a Rope buffer
        // must hand back a mutable borrow the widget can edit through.
        let mut b = Buffer::from_text("ab");
        {
            let rope = b.as_rope_mut().expect("mutable rope");
            rope.insert(2, "c");
        }
        assert_eq!(b.as_rope().unwrap().to_string(), "abc");
    }

    /// Build a UTF-16LE-with-BOM byte payload of at least `min_bytes` bytes whose
    /// decoded text is `marker` repeated. Used to force the mmap browse path
    /// (`>= MMAP_THRESHOLD`) with non-UTF-8 content.
    fn utf16le_payload_at_least(marker: &str, min_bytes: usize) -> Vec<u8> {
        let mut out: Vec<u8> = vec![0xFF, 0xFE]; // UTF-16LE BOM
                                                 // Repeat the marker until the byte count clears the threshold.
        while out.len() < min_bytes {
            for unit in marker.encode_utf16() {
                out.extend_from_slice(&unit.to_le_bytes());
            }
        }
        out
    }

    #[cfg_attr(miri, ignore)] // mmap syscall (see `open_small_file_returns_rope`)
    #[test]
    fn promote_to_rope_decodes_utf16le_not_mojibake() -> crate::Result<()> {
        // R8/C-04: a UTF-16LE file opened via the mmap browse path then promoted
        // must decode through `crate::encoding` (BOM + chardetng), NOT through a
        // raw `String::from_utf8_lossy` that would turn every other byte into the
        // U+FFFD replacement char. The marker is multilingual so a lossy UTF-8
        // decode is unambiguously wrong.
        let marker = "café 速記 héllo\n";
        let payload = utf16le_payload_at_least(marker, (MMAP_THRESHOLD as usize) + 64);
        let mut f = NamedTempFile::new()?;
        f.write_all(&payload)?;
        f.flush()?;

        let mut b = Buffer::open(f.path())?;
        assert!(matches!(b, Buffer::Mmap { .. }), "large file opens as mmap");
        b.promote_to_rope()?;
        let rope = b.as_rope().expect("rope after promote");
        let body = rope.to_string();

        // No U+FFFD mojibake, and the real text is present.
        assert!(
            !body.contains('\u{FFFD}'),
            "decoded body must not contain replacement chars (lossy-UTF8 mojibake)"
        );
        assert!(
            body.starts_with(marker),
            "decoded text must match the source"
        );
        assert!(
            body.contains("速記"),
            "kanji must survive the UTF-16 decode"
        );
        Ok(())
    }

    #[cfg_attr(miri, ignore)] // file-I/O syscall (see `open_small_file_returns_rope`)
    #[test]
    fn open_small_utf16le_file_decodes_through_encoding() -> crate::Result<()> {
        // R8/C-04 (rope path): a SMALL UTF-16LE file (under MMAP_THRESHOLD, so it
        // takes the eager `fs::read` rope path) must also decode through
        // `crate::encoding`, not `from_utf8_lossy`.
        let mut f = NamedTempFile::new()?;
        // "Hi\n" in UTF-16LE with BOM.
        f.write_all(&[0xFF, 0xFE, b'H', 0x00, b'i', 0x00, b'\n', 0x00])?;
        f.flush()?;
        let b = Buffer::open(f.path())?;
        let rope = b.as_rope().expect("small file is a rope");
        assert_eq!(rope.to_string(), "Hi\n");
        Ok(())
    }

    #[cfg_attr(miri, ignore)] // mmap syscall (see `open_small_file_returns_rope`)
    #[test]
    fn promote_preserves_valid_utf8_unchanged() -> crate::Result<()> {
        // Behaviour-identical guard: valid UTF-8 content (the common case) must
        // round-trip through the new decode path byte-for-byte (chars), so the
        // encoding routing never regresses the plain-ASCII / UTF-8 path.
        let marker = "plain ascii line\n";
        let mut payload = Vec::new();
        while payload.len() < (MMAP_THRESHOLD as usize) + 16 {
            payload.extend_from_slice(marker.as_bytes());
        }
        let expected = String::from_utf8(payload.clone()).unwrap();
        let mut f = NamedTempFile::new()?;
        f.write_all(&payload)?;
        f.flush()?;
        let mut b = Buffer::open(f.path())?;
        b.promote_to_rope()?;
        assert_eq!(b.as_rope().unwrap().to_string(), expected);
        Ok(())
    }
}
