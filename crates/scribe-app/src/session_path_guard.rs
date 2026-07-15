//! R6 / S-04 (CWE-59 link-following, CWE-610 untrusted-resource) —
//! session-restore path validation.
//!
//! On restore the app re-opens whatever path it finds in `session.json`, with
//! NO user interaction. This module answers exactly one question about such a
//! path: **can opening it reach off this machine?**
//!
//! ## What this guards, and what it deliberately does not
//!
//! A UNC path (`\\attacker\share\…`) is the whole threat. Opening one makes
//! Windows authenticate to that host and send a NetNTLMv2 response — the
//! user's credentials, to a server of the attacker's choosing. That is a real
//! 2026 vector, not a legacy one: Windows 11 24H2 made SMB signing mandatory
//! by default, but signing defeats RELAY, not CAPTURE; the setting that stops
//! capture (SMB NTLM blocking) is opt-in. 2025 alone ran CVE-2025-24054 →
//! CVE-2025-50154 → CVE-2025-59214 — a patch, then two bypasses, with
//! in-the-wild exploitation eight days after the first fix.
//!
//! This module USED to also fence restores inside a "prior working set" of
//! allowed roots. That fence is gone, for two reasons:
//!
//! 1. **It was circular.** The roots were the parent directories of the
//!    manifest's OWN declared paths — derived from the very artifact they were
//!    meant to police. A manifest naming `C:\Users\victim\.ssh\id_rsa` made
//!    `C:\Users\victim\.ssh` an allowed root, so the path authorised itself
//!    and was ACCEPTED. It never stopped the attack it was written for, while
//!    its docstring claimed "a tampered/attacker-chosen path is NEVER
//!    auto-opened".
//! 2. **It defended a non-harm.** Auto-opening a local file shows the user a
//!    file they can already read, on their own screen; the attacker learns
//!    nothing. Contrast the UNC case, which converts a local write into REMOTE
//!    credential exfiltration — an escape from the local trust domain. That
//!    asymmetry is why one control survives and the other does not.
//!
//! There is also no honest way to repair the fence rather than remove it: a
//! text editor legitimately opens files anywhere, so any root set is either
//! derived from other user-writable state (circular again — rooting it in
//! `recent_files` just moves the problem one file along) or narrow enough to
//! break the product.
//!
//! ## Scope, stated honestly
//!
//! Per Chromium's and Microsoft's published threat models, an attacker who can
//! write this user's config dir is INSIDE the user boundary and is not a
//! serviceable boundary at all — Chromium's FAQ names "change configuration
//! files" explicitly. So this is **defense-in-depth and correctness, not a
//! security boundary**, in the same category as MOTW or UAC. It is worth the
//! ~30 lines anyway because the blast radius (credentials leaving the machine)
//! is categorically worse than the local reads such an attacker already has,
//! and because Notepad++ shipped CVE-2026-52886 for this exact shape — a
//! `session.xml` auto-opening attacker-chosen paths on restore — rather than
//! dismissing it as out of scope.
//!
//! Paths that fail are skipped and logged at debug by the caller.

use std::path::{Component, Path};

/// Is `raw` a UNC network-share path or a Windows device-namespace path —
/// i.e. a "remote / non-ordinary-file" path that must NEVER be auto-opened
/// on restore? Covers:
///   * verbatim-UNC (`\\?\UNC\server\share`),
///   * plain UNC (`\\server\share`, `//server/share`) — the SMB/NTLM
///     credential-leak vector, and
///   * device-namespace (`\\.\PhysicalDrive0`, `\\?\C:` style raw devices) —
///     not an ordinary file.
///
/// IMPORTANT: a Windows *verbatim-disk* path (`\\?\C:\Data\…\notes.md`, the
/// form `std::fs::canonicalize` returns) is an ORDINARY LOCAL FILE and MUST
/// NOT be flagged — else every canonicalized restore path would be falsely
/// rejected. We distinguish verbatim-disk (allowed) from verbatim-UNC /
/// device (rejected) precisely.
///
/// Pure + cheap; defends the highest-value vector (SMB/NTLM credential leak)
/// independently of whether the target exists. The name is kept as
/// `is_unc_path` for call-site brevity; the doc above is the precise scope.
pub(crate) fn is_unc_path(raw: &Path) -> bool {
    // First, ask the path API directly. On Windows this is authoritative and
    // cleanly separates network/device prefixes from local disk ones.
    if let Some(Component::Prefix(p)) = raw.components().next() {
        use std::path::Prefix::*;
        return match p.kind() {
            // Network shares — the credential-leak vector.
            UNC(..) | VerbatimUNC(..) => true,
            // Raw device namespace (`\\.\PhysicalDrive0`) — not an ordinary file.
            DeviceNS(..) => true,
            // Ordinary local disk (incl. the `\\?\C:` verbatim-disk that
            // `canonicalize` returns) — allowed.
            Verbatim(..) | VerbatimDisk(..) | Disk(..) => false,
        };
    }

    // Fallback for non-Windows hosts (our tests run there too): a
    // `std::path::Component` does NOT classify a `\\server\share` string as a
    // Prefix on Unix, so pattern-match the raw bytes. A leading double
    // separator (`\\` or `//`) denotes a UNC share or a device path.
    let s = raw.to_string_lossy();
    let chars: Vec<char> = s.chars().collect();
    let starts_double_sep = chars.len() >= 2
        && (chars[0] == '\\' || chars[0] == '/')
        && (chars[1] == '\\' || chars[1] == '/');
    if !starts_double_sep {
        return false;
    }

    // `\\?\…` (verbatim) or `\\.\…` (device) prefix.
    let third = chars.get(2).copied();
    match third {
        // `\\.\…` device namespace → always rejected (raw device).
        Some('.') => true,
        // `\\?\…` verbatim → local (allowed) UNLESS the explicit
        // `\\?\UNC\…` network spelling.
        Some('?') => {
            let head: String = s.chars().take(8).collect::<String>().to_ascii_uppercase();
            head.replace('/', "\\").starts_with("\\\\?\\UNC")
        }
        // Plain `\\server\share` / `//server/share` → UNC.
        _ => true,
    }
}

/// How many symlink hops we will follow before giving up. A cycle or an
/// absurd chain resolves to "unsafe" rather than spinning.
const MAX_LINK_HOPS: u32 = 8;

/// Does `path` resolve to somewhere on LOCAL storage?
///
/// This is the whole guard, and the ONLY property that matters is: **can
/// auto-opening this path make Windows authenticate to a remote SMB host?**
///
/// The answer is computed WITHOUT EVER TOUCHING A REMOTE LOCATION, which is
/// the entire subtlety. Touching a UNC path *is* the attack — `CreateFile`,
/// `canonicalize`, even a bare metadata call opens an SMB session and hands
/// over a NetNTLMv2 response before it can return an answer to inspect.
/// (This is the Horizon3 class: `os.path.isdir()` on an attacker-influenced
/// path leaked credentials in jupyter_server / Gradio / Streamlit —
/// CVE-2024-35178. The CHECK was the trigger.)
///
/// So we resolve links by hand:
///   * `is_unc_path` is a pure STRING test — no syscall,
///   * `symlink_metadata` does not follow the FINAL component,
///   * `read_link` reads the reparse data and does NOT follow it.
///
/// A previous version called `std::fs::canonicalize` here and then tested the
/// result for UNC, under a comment claiming "defense in depth against a
/// symlink→UNC pivot". It had it backwards: `canonicalize` FOLLOWS the link,
/// so a symlink to `\\attacker\share\x` leaked the hash on the way to
/// computing the value we would then reject. The check performed the attack.
///
/// Known and accepted limit: a symlinked ANCESTOR (`C:\notes\evil\x.md` where
/// `evil` → `\\attacker\share`) is still resolved by the OS during the
/// `symlink_metadata` below, because lstat only declines to follow the final
/// component. Reaching that requires the manifest to already NAME such a path
/// — i.e. either the user opened it themselves in a prior session (they
/// touched it live already), or the attacker can write the manifest, in which
/// case they would simply declare the UNC path directly and be caught by the
/// string test. It is not reachable in a way the string test does not already
/// cover, so we do not walk every ancestor to close it.
fn resolves_locally(path: &Path) -> bool {
    let mut cur = path.to_path_buf();
    for _ in 0..MAX_LINK_HOPS {
        // STRING test first, every hop, before any syscall touches `cur`.
        if is_unc_path(&cur) {
            return false;
        }
        // Also the existence gate: a vanished target is skipped.
        let Ok(md) = std::fs::symlink_metadata(&cur) else {
            return false;
        };
        if !md.file_type().is_symlink() {
            return true;
        }
        let Ok(target) = std::fs::read_link(&cur) else {
            return false;
        };
        cur = if target.is_absolute() {
            target
        } else {
            match cur.parent() {
                Some(parent) => parent.join(target),
                None => return false,
            }
        };
    }
    // Chain too long / cyclic → fail closed.
    false
}

/// The pure decision: is `path` safe to AUTO-open on restore? Fails CLOSED.
///
/// "Safe" means exactly one thing — opening it will not make the OS reach out
/// to a remote host. It deliberately does NOT try to keep the restore inside
/// any "prior working set". See the module header for why that fence was
/// removed rather than repaired.
pub(crate) fn is_safe_restore_path(path: &Path) -> bool {
    resolves_locally(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn unc_paths_are_detected() {
        assert!(is_unc_path(Path::new(r"\\attacker\share\evil.txt")));
        assert!(is_unc_path(Path::new(r"\\?\UNC\server\share\x")));
        assert!(is_unc_path(Path::new(r"\\.\PhysicalDrive0")));
        assert!(is_unc_path(Path::new("//server/share/x")));
        // A normal local path is NOT a UNC path.
        assert!(!is_unc_path(Path::new("/srv/me/notes.md")));
        assert!(!is_unc_path(Path::new(r"C:\Data\me\notes.md")));
        assert!(!is_unc_path(Path::new("relative/notes.md")));
        // CRITICAL: a Windows *verbatim-disk* path (`\\?\C:\…`, the form
        // `canonicalize` returns) is LOCAL — it must NOT be flagged, or every
        // canonicalized restore path would be falsely rejected.
        assert!(!is_unc_path(Path::new(r"\\?\C:\Data\me\notes.md")));
        // …but the explicit verbatim-UNC spelling IS rejected, and a `\\.\`
        // device-namespace path is rejected too (it is a raw device, not an
        // ordinary file we should auto-open).
        assert!(is_unc_path(Path::new(r"\\?\UNC\server\share\x")));
        assert!(is_unc_path(Path::new(r"\\.\C:\Data\me\notes.md")));
    }

    /// Create a symlink at `link` pointing to `target`, or panic explaining
    /// why. NEVER skips: a silently-skipped test is not a passing test, and
    /// these are the only tests that exercise the link-following logic at all.
    /// On Windows this needs Developer Mode (or SeCreateSymbolicLink).
    fn symlink_or_explain(target: &Path, link: &Path) {
        #[cfg(unix)]
        let r = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let r = std::os::windows::fs::symlink_file(target, link);
        r.unwrap_or_else(|e| {
            panic!(
                "could not create the symlink this test needs ({} -> {}): {e}. \
                 On Windows, enable Developer Mode.",
                link.display(),
                target.display()
            )
        });
    }

    #[test]
    fn a_unc_path_is_rejected_on_the_string_alone() {
        // Unconditional, and — critically — decided WITHOUT any filesystem
        // call. Touching a UNC path is itself the credential leak, so the
        // reject cannot depend on the target existing.
        assert!(!is_safe_restore_path(Path::new(
            r"\\attacker\share\notes.md"
        )));
        assert!(!is_safe_restore_path(Path::new(
            r"\\?\UNC\attacker\share\x"
        )));
        assert!(!is_safe_restore_path(Path::new(r"//attacker/share/x")));
    }

    // WHY THE ORDERING TEST BELOW IS UNIX-ONLY -- read before "fixing" it.
    //
    // Windows is where the leak is real, so a Windows ordering test is what
    // you want. It cannot be written honestly here, and pretending otherwise
    // is worse than not having it.
    //
    // Proving the string test fires FIRST needs a fixture that is
    // UNC-classified AND reachable -- otherwise the metadata call fails, the
    // guard returns `false` for the wrong reason, and the test passes with the
    // guard DELETED. That is not hypothetical: mutating the string check away
    // and running this module's tests on Windows left ALL of them green.
    // `\.\NUL` was the obvious candidate (device-namespace + always openable)
    // and it fails too -- `symlink_metadata` errors on it. Every remaining
    // candidate needs a real SMB server or admin shares, neither of which
    // belongs in a unit test.
    //
    // So the property is established compositionally instead:
    //   * `unc_paths_are_detected` pins the CLASSIFICATION on Windows, for
    //     every prefix kind that matters;
    //   * the unix test below pins the ORDERING -- and `resolves_locally` is
    //     the same platform-independent code on both targets.
    // Classification correct on Windows + ordering correct anywhere => the
    // ordering is correct on Windows.
    //
    // Consequence to expect: a Windows-only cargo-mutants run reports the
    // string check as a SURVIVOR; the Linux CI run kills it. Do NOT "kill" it
    // locally by weakening the fixture.
    #[cfg(unix)]
    #[test]
    fn the_unc_reject_fires_on_the_string_before_any_filesystem_call() {
        // The ONLY test here that can prove the ORDERING, and it can do so
        // only because its fixture is REACHABLE.
        //
        // On Linux `//tmp/x` names the same file as `/tmp/x`, so the metadata
        // call on it SUCCEEDS. Strip the string test and the code falls
        // through, finds an ordinary file, and ACCEPTS it — so this test
        // fails. That is what makes it discriminating.
        //
        // An unreachable `\\attacker\share\x` can NOT prove this: its metadata
        // call fails, so the verdict is `false` either way and the test passes
        // with the guard deleted. Which is the whole point — on Windows that
        // metadata call IS the credential leak, and a test that cannot tell
        // the two paths apart cannot tell you the leak is fixed.
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("notes.md");
        fs::write(&file, b"x").expect("write");
        let double_slashed = PathBuf::from(format!("/{}", file.display()));

        // Both fixture properties are asserted: a fixture that is invalid in
        // either direction would test nothing.
        assert!(
            is_unc_path(&double_slashed),
            "fixture must be UNC-classified, else it proves nothing"
        );
        assert!(
            fs::symlink_metadata(&double_slashed).is_ok(),
            "fixture must be REACHABLE, else the reject could come from the \
             metadata call failing rather than from the string test"
        );

        assert!(
            !is_safe_restore_path(&double_slashed),
            "a UNC-classified path must be rejected on the string, before the \
             filesystem is ever asked about it"
        );
    }

    #[test]
    fn an_ordinary_local_file_is_restorable() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("notes.md");
        fs::write(&file, b"hello").expect("write");
        assert!(
            is_safe_restore_path(&file),
            "a real local file must restore — this is the common case, and the \
             guard exists to stop remote reads, not ordinary ones"
        );
    }

    #[test]
    fn a_vanished_target_is_skipped() {
        let dir = tempdir().expect("tempdir");
        let ghost = dir.path().join("does-not-exist.md");
        assert!(
            !is_safe_restore_path(&ghost),
            "a vanished target must be skipped, not auto-created"
        );
    }

    #[test]
    fn a_symlink_to_a_unc_target_is_rejected_without_ever_touching_it() {
        // THE regression test for the bug this module was rewritten around.
        // The old code called `canonicalize` here and then tested the RESULT
        // for UNC — but canonicalize FOLLOWS the link, so it opened
        // `\\attacker\share\x` (leaking the NetNTLMv2 hash) on the way to
        // computing the value it would then reject. The check performed the
        // attack it was checking for.
        //
        // The target deliberately does not exist and is unreachable: if this
        // test ever hangs or does DNS, the guard has started touching it.
        let dir = tempdir().expect("tempdir");
        let link = dir.path().join("innocent-looking.md");
        symlink_or_explain(Path::new(r"\\attacker\share\x"), &link);

        assert!(
            !is_safe_restore_path(&link),
            "a symlink whose target is UNC must be rejected via read_link, \
             which does not follow it"
        );
    }

    #[test]
    fn a_symlink_chain_ending_at_unc_is_rejected() {
        // One hop is not enough: link1 -> link2 -> \\attacker\share. Each hop
        // is string-tested before the next lstat, so the chain is walked
        // without the OS ever resolving the far end.
        let dir = tempdir().expect("tempdir");
        let link2 = dir.path().join("hop2.md");
        symlink_or_explain(Path::new(r"\\attacker\share\x"), &link2);
        let link1 = dir.path().join("hop1.md");
        symlink_or_explain(&link2, &link1);

        assert!(
            !is_safe_restore_path(&link1),
            "the chain must be followed to the UNC end"
        );
    }

    #[test]
    fn a_symlink_to_a_local_file_still_restores() {
        // The guard must not become "reject all symlinks" — symlinked notes
        // directories are a normal setup and breaking them is a real cost.
        let dir = tempdir().expect("tempdir");
        let real = dir.path().join("real.md");
        fs::write(&real, b"x").expect("write");
        let link = dir.path().join("link.md");
        symlink_or_explain(&real, &link);

        assert!(
            is_safe_restore_path(&link),
            "a symlink to a local file is safe to open"
        );
    }

    #[test]
    fn a_symlink_cycle_fails_closed_instead_of_spinning() {
        let dir = tempdir().expect("tempdir");
        let a = dir.path().join("a.md");
        let b = dir.path().join("b.md");
        // b -> a first (a does not exist yet; a dangling target is fine), then
        // a -> b closes the loop.
        symlink_or_explain(&a, &b);
        symlink_or_explain(&b, &a);

        assert!(
            !is_safe_restore_path(&a),
            "a cycle must exhaust the hop budget and fail closed, not hang"
        );
    }
}
