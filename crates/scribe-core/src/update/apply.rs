//! Applying a verified update: keep-one-prior-binary backup, atomic install,
//! and rollback. The running-executable swap is delegated to `self-replace`
//! (handles the Windows rename-aside trick); the testable backup/install/
//! rollback logic operates on arbitrary paths.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Copy `target` to `backup` (keep-one-prior for rollback), then move `new`
/// into `target`. Caller MUST have verified `new` (checksum + signature) first.
pub fn install_with_backup(new: &Path, target: &Path, backup: &Path) -> io::Result<()> {
    if target.exists() {
        fs::copy(target, backup)?;
    }
    // Prefer atomic rename; fall back to copy across filesystems.
    match fs::rename(new, target) {
        Ok(()) => Ok(()),
        Err(e) => {
            // The atomic swap was not possible (typically a cross-volume move);
            // the non-atomic copy fallback below succeeds, but the degrade was
            // previously invisible. Record the error KIND only (no paths).
            tracing::warn!(
                target: "scribe::update",
                error_kind = ?e.kind(),
                "atomic install rename failed — falling back to a non-atomic copy"
            );
            fs::copy(new, target)?;
            let _ = fs::remove_file(new);
            Ok(())
        }
    }
}

/// Restore the prior binary from `backup` over `target`.
pub fn rollback(backup: &Path, target: &Path) -> io::Result<()> {
    if !backup.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no backup to roll back to",
        ));
    }
    fs::copy(backup, target)?;
    Ok(())
}

/// The sibling backup path for a binary at `exe` (`scr1b3.exe` → `scr1b3.bak`,
/// `scr1b3` → `scr1b3.bak`). Same directory ⇒ same volume, so restoring it is an
/// atomic rename and the keep-one-prior copy never crosses a filesystem
/// boundary (the property `self-replace` relies on).
pub fn backup_path_for(exe: &Path) -> PathBuf {
    exe.with_extension("bak")
}

/// Replace the *currently running* executable with `new` (already verified),
/// FIRST snapshotting the current binary to a sibling `.bak` so a failed
/// post-swap relaunch can [`rollback_running_executable`] to a known-good prior
/// version. Uses `self-replace` so the swap works while the binary is running
/// (the Windows locked-file case). Returns the backup path on success.
///
/// Before this kept-one-prior backup, an update obliterated the old binary with
/// no recovery path; the backup is what makes the relaunch-failure rollback in
/// the UI possible.
///
/// Fails fast — WITHOUT touching the running binary — when `new` is not a
/// readable file. Callers verify checksum + signature at DOWNLOAD time, but the
/// staged file can still be gone by APPLY time (a temp sweeper or AV quarantine
/// between the two). `self_replace` renames the running executable aside before
/// it discovers the source is unusable, and does not put it back, so without this
/// guard that window ends with the user having NO binary at all — the exact
/// "obliterated with no recovery path" outcome the backup exists to prevent.
/// See `replace_running_executable_with_missing_source_is_a_noop`.
pub fn replace_running_executable(new: &Path) -> io::Result<PathBuf> {
    // Probe the source by OPENING it: `exists()` would still race, and an
    // unreadable-but-present file must not reach `self_replace` either.
    fs::File::open(new).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("staged update binary is not readable, running binary left untouched: {e}"),
        )
    })?;
    let exe = std::env::current_exe()?;
    let backup = backup_path_for(&exe);
    fs::copy(&exe, &backup)?;
    self_replace::self_replace(new)?;
    Ok(backup)
}

/// Roll the *currently running* executable back to a `backup` produced by
/// [`replace_running_executable`] — used when a just-installed update fails to
/// relaunch, so the on-disk binary returns to the known-good prior version.
/// Uses `self-replace` (the same atomic, locked-file-safe swap).
pub fn rollback_running_executable(backup: &Path) -> io::Result<()> {
    if !backup.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no backup to roll back to",
        ));
    }
    self_replace::self_replace(backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // --- Documented surviving-mutant dispositions (cargo-mutants) -------------
    //
    // UNTESTABLE WITHOUT A PRODUCTION SEAM (not equivalent — real gaps):
    //   apply.rs:57 (`replace_running_executable -> Ok(Default::default())`) and
    //   apply.rs:69 (`delete !` in `rollback_running_executable`).
    //
    // Both functions operate on the CURRENTLY-RUNNING executable: they call
    // `std::env::current_exe()` and `self_replace::self_replace(...)`, which
    // OVERWRITES the running test-runner binary itself. Exercising either to the
    // point where the mutation becomes observable (the backup `fs::copy` in 57,
    // or the existing-backup → self_replace path in 69) would require actually
    // swapping the test process's own binary mid-run — destructive and
    // non-deterministic, and unsafe to do in CI. Distinguishing the mutants
    // without that side effect would require injecting the exe path + the swap
    // function (dependency injection in production code), which is out of scope
    // for this test-only change. The pure, side-effect-free halves ARE covered:
    // `backup_path_for` (the sibling-path math) and the missing-backup fail-closed
    // guard (`rollback_running_without_backup_errors`) are both tested below.

    fn write(path: &Path, content: &[u8]) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(content).unwrap();
    }

    /// A missing staged binary must abort BEFORE the running executable is
    /// touched — no `.bak` written, and the running binary still on disk.
    ///
    /// This is the one `replace_running_executable` case that is safe to run for
    /// real, precisely BECAUSE it must not reach `self_replace`. It is also how
    /// the bug it guards was found: `updater::tests::downloaded_ok_chains_into_
    /// apply_and_surfaces_an_install_failure` feeds a nonexistent staged path to
    /// this function, and `self_replace` renamed the TEST RUNNER's own binary
    /// aside and left it there. Under `cargo test` that is invisible (one
    /// process, binary already loaded, 1785 green). Under `cargo nextest` —
    /// process-per-test, which is what the coverage job runs — the binary was
    /// gone and the next 24 updater tests died with "error spawning child
    /// process: The system cannot find the file specified".
    ///
    /// Asserting on `current_exe` is deliberate: a tempdir stand-in would not
    /// reproduce it, since the destructive call is hard-wired to the RUNNING exe.
    #[test]
    fn replace_running_executable_with_missing_source_is_a_noop() {
        let exe = std::env::current_exe().expect("the test runner has a path");
        let backup = backup_path_for(&exe);
        // Do not let a previous run's leftovers decide the result.
        let _ = fs::remove_file(&backup);

        let missing = exe.with_file_name("definitely-not-a-staged-binary-9f3a");
        assert!(!missing.exists(), "fixture must not exist");

        let err = replace_running_executable(&missing)
            .expect_err("a missing staged binary must not be installed");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(
            err.to_string().contains("running binary left untouched"),
            "the error must say the binary was spared, got: {err}"
        );
        assert!(
            exe.exists(),
            "THE RUNNING TEST BINARY WAS DESTROYED — every later nextest process \
             would fail to spawn"
        );
        assert!(
            !backup.exists(),
            "a failed apply must not leave a .bak behind: nothing was replaced"
        );
    }

    #[test]
    fn install_creates_backup_and_swaps() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("scr1b3.bin");
        let new = dir.path().join("scr1b3.new");
        let backup = dir.path().join("scr1b3.bak");
        write(&target, b"v1");
        write(&new, b"v2");

        install_with_backup(&new, &target, &backup).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"v2");
        assert_eq!(fs::read(&backup).unwrap(), b"v1");
    }

    #[test]
    fn rollback_restores_prior() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("scr1b3.bin");
        let new = dir.path().join("scr1b3.new");
        let backup = dir.path().join("scr1b3.bak");
        write(&target, b"v1");
        write(&new, b"v2-broken");

        install_with_backup(&new, &target, &backup).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"v2-broken");
        // Self-test failed -> roll back.
        rollback(&backup, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"v1");
    }

    #[test]
    fn rollback_without_backup_errors() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("t");
        let backup = dir.path().join("nope.bak");
        write(&target, b"x");
        assert!(rollback(&backup, &target).is_err());
    }

    #[test]
    fn backup_path_is_sibling_with_bak_extension() {
        // Windows .exe and bare-name (Unix) both map to a sibling `.bak`.
        assert_eq!(
            backup_path_for(Path::new("/opt/app/scr1b3.exe")),
            Path::new("/opt/app/scr1b3.bak")
        );
        assert_eq!(
            backup_path_for(Path::new("/opt/app/scr1b3")),
            Path::new("/opt/app/scr1b3.bak")
        );
    }

    #[test]
    fn rollback_running_without_backup_errors() {
        // The running-exe rollback must fail closed when the backup is missing,
        // rather than handing a non-existent file to self-replace.
        let dir = tempfile::tempdir().unwrap();
        assert!(rollback_running_executable(&dir.path().join("nope.bak")).is_err());
    }
}
