//! Telemetry-free update logic.
//!
//! Privacy by construction: the only network surface is a version check against
//! the public GitHub Releases API (no PII, no analytics, no custom server, no
//! shipped token). This module owns the *decision* logic (version compare +
//! mode handling) and is fully testable offline via an injectable fetcher.
//! The actual HTTP fetch + signature verification + binary swap live behind
//! the `net` feature so the core stays dependency-light and tests never touch
//! the network.

pub mod apply;
pub mod verify;

pub use crate::config::UpdateMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// Tag like `v0.2.0` or `0.2.0`.
    pub version: String,
    /// Public asset URL for this platform.
    pub asset_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// Checking is disabled (`mode = off`).
    Disabled,
    /// Up to date.
    UpToDate,
    /// A newer release is available.
    Available(ReleaseInfo),
    /// Network unavailable / check failed — never an error, just skipped.
    Offline,
}

/// Parse a semver-ish `x.y.z` (tolerating a leading `v`) into a comparable tuple.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let mut it = s.split(['.', '-', '+']);
    let major = it.next()?.parse().ok()?;
    let minor = it.next().unwrap_or("0").parse().ok()?;
    let patch = it.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// True if `candidate` is strictly newer than `current`.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (parse_version(current), parse_version(candidate)) {
        (Some(c), Some(n)) => n > c,
        _ => false,
    }
}

/// Decide update status given the current version, the configured mode, and a
/// fetcher closure that returns the latest release (or `None` when offline).
/// This is the testable core — no network, no I/O.
pub fn evaluate<F>(current: &str, mode: UpdateMode, fetch_latest: F) -> UpdateStatus
where
    F: FnOnce() -> Option<ReleaseInfo>,
{
    if mode == UpdateMode::Off {
        return UpdateStatus::Disabled;
    }
    match fetch_latest() {
        None => UpdateStatus::Offline,
        Some(rel) if is_newer(current, &rel.version) => UpdateStatus::Available(rel),
        Some(_) => UpdateStatus::UpToDate,
    }
}

/// The single permitted version-check endpoint. Documented here so the
/// telemetry-free guarantee is auditable: GitHub Releases, unauthenticated,
/// zero PII. (Used by the `net` feature implementation.)
pub const RELEASES_ENDPOINT: &str =
    "https://api.github.com/repos/itasha-corp/scr1b3/releases/latest";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("v1.0.0", "v1.0.1"));
        assert!(!is_newer("1.2.3", "1.2.3"));
        assert!(!is_newer("2.0.0", "1.9.9"));
    }

    #[test]
    fn disabled_mode_never_checks() {
        let status = evaluate("0.1.0", UpdateMode::Off, || {
            panic!("must not fetch when disabled");
        });
        assert_eq!(status, UpdateStatus::Disabled);
    }

    #[test]
    fn offline_is_graceful() {
        let status = evaluate("0.1.0", UpdateMode::Notify, || None);
        assert_eq!(status, UpdateStatus::Offline);
    }

    #[test]
    fn detects_available() {
        let rel = ReleaseInfo {
            version: "0.2.0".into(),
            asset_url: "https://x/y".into(),
        };
        let status = evaluate("0.1.0", UpdateMode::Notify, || Some(rel.clone()));
        assert_eq!(status, UpdateStatus::Available(rel));
    }

    #[test]
    fn up_to_date() {
        let rel = ReleaseInfo {
            version: "0.1.0".into(),
            asset_url: "https://x/y".into(),
        };
        assert_eq!(
            evaluate("0.1.0", UpdateMode::Notify, || Some(rel)),
            UpdateStatus::UpToDate
        );
    }
}
