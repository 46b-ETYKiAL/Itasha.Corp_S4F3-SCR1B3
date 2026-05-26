//! Applying a verified update: keep-one-prior-binary backup, atomic install,
//! and rollback. The running-executable swap is delegated to `self-replace`
//! (handles the Windows rename-aside trick); the testable backup/install/
//! rollback logic operates on arbitrary paths.

use std::fs;
use std::io;
use std::path::Path;

/// Copy `target` to `backup` (keep-one-prior for rollback), then move `new`
/// into `target`. Caller MUST have verified `new` (checksum + signature) first.
pub fn install_with_backup(new: &Path, target: &Path, backup: &Path) -> io::Result<()> {
    if target.exists() {
        fs::copy(target, backup)?;
    }
    // Prefer atomic rename; fall back to copy across filesystems.
    match fs::rename(new, target) {
        Ok(()) => Ok(()),
        Err(_) => {
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

/// Replace the *currently running* executable with `new` (already verified).
/// Uses `self-replace` so it works while the binary is running, including the
/// Windows locked-file case. Returns the path to the backup of the old binary.
pub fn replace_running_executable(new: &Path) -> io::Result<()> {
    self_replace::self_replace(new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(path: &Path, content: &[u8]) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(content).unwrap();
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
}
