//! Persisted monotonic `release_index` high-water mark for the Tier-1 updater.
//!
//! ## Why a separate ordinal from the version floor
//!
//! [`super::net::ensure_upgrade`] already keeps a strictly-monotonic VERSION
//! floor (apply-time anti-downgrade). The signed manifest carries a parallel
//! `release_index` (`major*1_000_000 + minor*1_000 + patch`) — a single integer
//! total-order over releases. The Tier-1 check refuses any manifest whose
//! `release_index` is not STRICTLY greater than the highest index ever applied,
//! closing the signed-but-older replay window at check time the same way
//! `ensure_upgrade` closes it at apply time. The two are deliberately redundant
//! defenses; this one is the manifest-native ordinal.
//!
//! ## Persistence — sibling-of-exe
//!
//! The high-water index is a single integer line in
//! `<exe-dir>/.scr1b3-release-index`, written next to the running executable.
//! This keeps the updater's persisted state co-located with the binary it
//! guards, needs NO new dependency (plain `std::fs` text I/O), and inherits the
//! install dir's permissions.
//!
//! ## Fail-safe reads, best-effort writes
//!
//! A read error (missing/unreadable/corrupt record, or no resolvable exe) yields
//! `0` — the lowest possible floor, so a record problem can never BLOCK an
//! otherwise-valid update (it just does not raise the floor). A write error is
//! logged-and-ignored (best-effort): persisting the new index must never fail an
//! already-applied update, because the next launch re-derives the floor from the
//! record (and, redundantly, from the version floor) regardless. Writes are
//! MONOTONIC — the record is only ever advanced upward.

use std::path::{Path, PathBuf};

/// File name of the persisted "highest release_index ever applied" record,
/// stored next to the running executable.
const RELEASE_INDEX_FILE: &str = ".scr1b3-release-index";

/// Path to the release-index record next to `exe`.
fn record_path_for(exe: &Path) -> PathBuf {
    exe.with_file_name(RELEASE_INDEX_FILE)
}

/// Read the persisted high-water `release_index` from the record next to `exe`.
/// Fail-safe: a missing/unreadable/empty/corrupt record yields `0` (the lowest
/// floor), NEVER an error and never a spuriously-high value that could block a
/// genuine update.
pub fn read_applied_index(exe: &Path) -> u64 {
    match std::fs::read_to_string(record_path_for(exe)) {
        Ok(text) => text.trim().parse::<u64>().unwrap_or(0),
        Err(_) => 0,
    }
}

/// Advance the high-water record next to `exe` to `index` IFF it is higher than
/// the current record (MONOTONIC — never lowers the floor). Best-effort: a write
/// failure is returned to the caller but must never block an applied update.
/// Called AFTER a successful apply.
pub fn record_applied_index(exe: &Path, index: u64) -> std::io::Result<()> {
    // Only advance — never regress the high-water mark (a tampered-low or stale
    // value cannot weaken the floor).
    if read_applied_index(exe) >= index {
        return Ok(());
    }
    std::fs::write(record_path_for(exe), format!("{index}\n"))
}

/// The persisted high-water `release_index` for the RUNNING installation,
/// reading the record next to the current executable. Falls back to `0` when
/// `current_exe()` is unavailable or no record exists (fail-safe — never blocks
/// an update on a missing record).
pub fn applied_index() -> u64 {
    std::env::current_exe()
        .ok()
        .map(|exe| read_applied_index(&exe))
        .unwrap_or(0)
}

/// Persist `index` as the new high-water mark next to the current executable,
/// best-effort. A failure to resolve the exe or write the record is ignored —
/// the next launch re-derives the floor from the record and the version floor,
/// so a lost write never corrupts the anti-rollback guarantee. Called AFTER a
/// successful in-place apply.
pub fn record_applied_index_for_current_exe(index: u64) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = record_applied_index(&exe, index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_is_zero_when_no_record() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("scr1b3.exe");
        assert_eq!(read_applied_index(&exe), 0);
    }

    #[test]
    fn record_round_trips_and_is_monotonic() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("scr1b3.exe");

        record_applied_index(&exe, 4040).unwrap();
        assert_eq!(read_applied_index(&exe), 4040);

        // A higher index advances the mark.
        record_applied_index(&exe, 5000).unwrap();
        assert_eq!(read_applied_index(&exe), 5000);

        // A LOWER or EQUAL "apply" must NOT lower the high-water mark.
        record_applied_index(&exe, 4100).unwrap();
        assert_eq!(
            read_applied_index(&exe),
            5000,
            "record must be monotonic — never regress"
        );
        record_applied_index(&exe, 5000).unwrap();
        assert_eq!(read_applied_index(&exe), 5000);
    }

    #[test]
    fn read_tolerates_corrupt_or_empty_record() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("scr1b3");
        std::fs::write(record_path_for(&exe), "not-a-number\n").unwrap();
        assert_eq!(read_applied_index(&exe), 0, "corrupt record reads as 0");
        std::fs::write(record_path_for(&exe), "").unwrap();
        assert_eq!(read_applied_index(&exe), 0, "empty record reads as 0");
        std::fs::write(record_path_for(&exe), "   \n ").unwrap();
        assert_eq!(read_applied_index(&exe), 0, "whitespace record reads as 0");
    }

    #[test]
    fn record_path_is_a_sibling_of_the_exe() {
        let exe = Path::new("/opt/app/scr1b3.exe");
        let rec = record_path_for(exe);
        assert_eq!(rec.parent(), exe.parent(), "record is a sibling of the exe");
        assert_eq!(
            rec.file_name().and_then(|n| n.to_str()),
            Some(RELEASE_INDEX_FILE)
        );
    }

    #[test]
    fn applied_index_for_current_exe_never_panics() {
        // The production entry points read/write next to the running test exe
        // (whose dir may be read-only); they must never panic.
        let _ = applied_index();
        record_applied_index_for_current_exe(1);
    }
}
