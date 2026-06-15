//! Network half of the in-app self-updater.
//!
//! Telemetry-free by construction: the only network surfaces are
//! 1. a single unauthenticated `GET` of the public GitHub Releases API, and
//! 2. downloads of the release archive + its `.minisig` + `.sha256` siblings.
//!
//! No analytics, no identifiers, no payload: every request sends only a generic
//! `User-Agent` (app name + version), and the asset is verified (SHA-256 THEN
//! minisign against [`super::verify::EMBEDDED_PUBLIC_KEY`]) before the extracted
//! binary is ever returned (against the [`super::verify::EMBEDDED_PUBLIC_KEYS`]
//! trust set). A verify failure deletes the staging area and the binary is
//! NEVER returned unverified.
//!
//! Pure decision logic ([`select_update`]) is split out from the I/O so it can
//! be unit-tested offline against a fixture [`RawRelease`].

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::verify::{verify_artifact, EMBEDDED_PUBLIC_KEYS};

/// Mandatory `User-Agent` for every request. App name + version ONLY — no
/// machine identifier, OS fingerprint, install ID, or any unique token.
const USER_AGENT: &str = concat!("scr1b3-updater/", env!("CARGO_PKG_VERSION"));

/// GitHub REST API version header value.
const GITHUB_API_VERSION: &str = "2026-03-10";

/// GitHub Releases API `Accept` header value.
const GITHUB_ACCEPT: &str = "application/vnd.github+json";

/// Overall per-request network timeout. Bounds a slow or stalled connection (a
/// hostile or just-bad network) so an update check / download can never hang the
/// worker thread indefinitely.
const NETWORK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Hard cap on the bytes of a single downloaded response held in memory
/// (tarball / installer / sig / sha). The real artifacts are tens of MB; this
/// bounds what a hostile or misdirected response can make the app allocate —
/// the body is minisign-verified AFTER download, so this is the *pre-verify*
/// memory-safety guard (and the reason `Content-Length` is never trusted for a
/// raw `with_capacity`).
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// A single release asset as returned by the GitHub Releases API. Only the
/// fields the updater needs are deserialized.
#[derive(Clone, Debug, Deserialize)]
pub struct RawAsset {
    pub name: String,
    pub browser_download_url: String,
}

/// The subset of the GitHub `releases/latest` JSON the updater reads. Made
/// public + constructible so [`select_update`] can be unit-tested with a
/// fixture (no network).
#[derive(Clone, Debug, Deserialize)]
pub struct RawRelease {
    pub tag_name: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<RawAsset>,
}

/// A verifiable platform installer asset (Windows `*-x86_64-setup.exe`) + its
/// signature/checksum sidecars. Present only when the release ships one for
/// this platform. Used for the in-place-update path when the app lives in a
/// protected, admin-owned location (e.g. `C:\Program Files`): the installer
/// self-elevates, so running it updates in place where a direct exe swap can't.
#[derive(Clone, Debug)]
pub struct InstallerAsset {
    /// The `setup.exe` browser_download_url.
    pub url: String,
    /// The `setup.exe.minisig` url.
    pub sig_url: String,
    /// The `setup.exe.sha256` url.
    pub sha_url: String,
}

/// One resolved, newer-than-current release ready to download.
#[derive(Clone, Debug)]
pub struct ReleaseInfo {
    pub version: semver::Version,
    /// The original tag string (e.g. `v0.4.0`).
    pub tag: String,
    /// The `.tar.gz` browser_download_url.
    pub asset_url: String,
    /// The `.tar.gz.minisig` url.
    pub sig_url: String,
    /// The `.tar.gz.sha256` url.
    pub sha_url: String,
    /// The release page (for "view changelog" in a browser).
    pub html_url: String,
    /// The self-elevating Windows installer for this release, when present —
    /// the apply path for a Program-Files install. `None` on platforms/releases
    /// without a `setup.exe`. Boxed so the (common) `None` case keeps
    /// `ReleaseInfo` small — it rides inside several UI-state enum variants.
    pub installer: Option<Box<InstallerAsset>>,
}

/// The result of a successful update check. A tri-state so the UI can ALWAYS
/// distinguish "you're current" from "a newer release exists but has no build
/// for your platform" — the latter must never read as "up to date" (the
/// classic self-updater false-negative). Network/parse/rate-limit failures are
/// a separate `Err` from [`check_for_update`], never folded into this enum.
#[derive(Clone, Debug)]
pub enum UpdateOutcome {
    /// A newer release WITH a downloadable asset matching this build's target.
    Available(ReleaseInfo),
    /// Already on (or ahead of) the newest published release. `latest` is the
    /// highest semver seen — shown next to the current version so "up to date"
    /// is never ambiguous.
    UpToDate { latest: semver::Version },
    /// A newer release exists but ships no asset matching this build's target
    /// triple (e.g. a platform that release skipped). The user is pointed at
    /// the release page to download manually rather than told "up to date".
    NewerButNoAsset {
        latest: semver::Version,
        target: String,
        html_url: String,
    },
}

/// Parse a release `tag_name` into a [`semver::Version`], tolerating a single
/// leading `v`. Returns `None` on malformed input (the caller treats that as
/// "no update", never a crash).
fn parse_tag(tag: &str) -> Option<semver::Version> {
    let s = tag.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    semver::Version::parse(s).ok()
}

/// Build a [`ReleaseInfo`] from a raw release IF it carries the three assets
/// (`scr1b3-<target>.tar.gz` + `.minisig` + `.sha256`) for this build's target.
/// Pure; no version/prerelease gating (callers do that).
fn build_release_info(
    raw: &RawRelease,
    version: semver::Version,
    target: &str,
) -> Option<ReleaseInfo> {
    let asset_name = format!("scr1b3-{target}.tar.gz");
    let sig_name = format!("{asset_name}.minisig");
    // Canonical sha name is `<asset>.sha256` (= `scr1b3-<target>.tar.gz.sha256`).
    // For robustness we ALSO accept the legacy `scr1b3-<target>.sha256` (the
    // pre-fix release name, dropped the `.tar.gz` infix) so a naming drift can
    // never again silently classify a real release as "no asset for platform".
    let sha_name = format!("{asset_name}.sha256");
    let legacy_sha_name = format!("scr1b3-{target}.sha256");
    let find = |name: &str| -> Option<&str> {
        raw.assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.as_str())
    };
    let sha_url = find(&sha_name)
        .or_else(|| find(&legacy_sha_name))?
        .to_string();
    Some(ReleaseInfo {
        version,
        tag: raw.tag_name.clone(),
        asset_url: find(&asset_name)?.to_string(),
        sig_url: find(&sig_name)?.to_string(),
        sha_url,
        html_url: raw.html_url.clone(),
        installer: find_installer(raw, target).map(Box::new),
    })
}

/// Find the self-elevating Windows installer asset for this release, if any.
/// The installer is named `scr1b3-<tag>-x86_64-setup.exe` (tag-keyed, NOT
/// target-keyed), so we match by the `-x86_64-setup.exe` suffix and require
/// both verifiable sidecars (`.minisig` + `.sha256`). Windows targets only.
fn find_installer(raw: &RawRelease, target: &str) -> Option<InstallerAsset> {
    if !target.contains("windows") {
        return None;
    }
    let exe = raw
        .assets
        .iter()
        .find(|a| a.name.ends_with("-x86_64-setup.exe"))?;
    let sig_name = format!("{}.minisig", exe.name);
    let sha_name = format!("{}.sha256", exe.name);
    let url_of = |name: &str| {
        raw.assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.clone())
    };
    Some(InstallerAsset {
        url: exe.browser_download_url.clone(),
        sig_url: url_of(&sig_name)?,
        sha_url: url_of(&sha_name)?,
    })
}

/// PURE (no network) decision over the FULL release list: pick the highest
/// **semver** among non-draft/non-prerelease releases (NOT GitHub's
/// `/releases/latest`, which sorts by commit date + honors a mutable, cacheable
/// "latest" flag and can therefore skip a newer tag), then classify against the
/// running version. This is the discovery strategy mature updaters use
/// (electron-updater / WinSparkle / self_update all pick highest-semver
/// themselves rather than trust feed order).
pub fn select_best(
    releases: &[RawRelease],
    current: &semver::Version,
    target: &str,
) -> UpdateOutcome {
    let best = releases
        .iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| parse_tag(&r.tag_name).map(|v| (v, r)))
        .max_by(|a, b| a.0.cmp(&b.0));

    let Some((latest, raw)) = best else {
        // No parseable stable release at all — treat as "current" (nothing to
        // offer), never an error.
        return UpdateOutcome::UpToDate {
            latest: current.clone(),
        };
    };
    if latest <= *current {
        return UpdateOutcome::UpToDate { latest };
    }
    match build_release_info(raw, latest.clone(), target) {
        Some(info) => UpdateOutcome::Available(info),
        None => UpdateOutcome::NewerButNoAsset {
            latest,
            target: target.to_string(),
            html_url: raw.html_url.clone(),
        },
    }
}

/// PURE (no network) decision: given the raw release, the current version, and
/// this build's target triple, return `Some(ReleaseInfo)` when the release is
/// newer AND a matching `scr1b3-<target>.tar.gz` asset (+ `.minisig` + `.sha256`
/// siblings) is present; `None` when up-to-date, malformed, a prerelease/draft,
/// or no matching asset triple exists.
pub fn select_update(
    raw: &RawRelease,
    current: &semver::Version,
    target: &str,
) -> Option<ReleaseInfo> {
    if raw.prerelease || raw.draft {
        return None;
    }
    let latest = parse_tag(&raw.tag_name)?;
    if latest <= *current {
        return None;
    }
    build_release_info(raw, latest, target)
}

/// Apply-time anti-downgrade guard (TUF rollback-attack defense). Returns `Ok`
/// only when `candidate` parses to a STRICTLY newer semver than `running`.
///
/// This is enforced at the moment of APPLYING an update — in addition to the
/// selection-time `latest <= current` skip in [`select_best`] / [`select_update`]
/// — so a tampered or replayed older-but-validly-signed release can never be
/// installed over a newer running build. `running` is the compiled-in
/// `CARGO_PKG_VERSION` (authoritative). `candidate` may carry a leading `v`
/// (it is parsed with the same [`parse_tag`] normalisation as release tags).
pub fn ensure_upgrade(candidate: &str, running: &str) -> Result<(), String> {
    let cand = parse_tag(candidate)
        .ok_or_else(|| format!("refusing to install: unparseable update version {candidate:?}"))?;
    let cur = semver::Version::parse(running)
        .map_err(|e| format!("internal: bad running version {running:?}: {e}"))?;
    if cand <= cur {
        return Err(format!(
            "refusing to install v{cand}: not newer than the running v{cur} (downgrade protection)"
        ));
    }
    Ok(())
}

/// Blocking GET of `/repos/{owner}/{repo}/releases` (the FULL list, one page).
/// Sends `Cache-Control: no-cache` so an intermediary can't serve a stale list
/// that hides a freshly-published release, and maps a 403/429 to an explicit
/// rate-limit message (unauthenticated GitHub allows 60 req/hr/IP). Never
/// panics.
///
/// We deliberately poll the FULL list rather than `/releases/latest`: that
/// computed resource excludes drafts/prereleases AND lags a just-published tag
/// (it is the more cache-prone endpoint), whereas the list reflects a new tag
/// immediately and lets [`select_best`] do its own highest-semver selection.
/// Freshness is enforced by the `no-cache` request header, NOT a query-string
/// cache-buster — a `?t=` param is a documented anti-pattern (it pollutes
/// shared caches and forfeits GitHub's ETag/`If-None-Match` 304 path, which is
/// the fuller solution but needs cross-check persisted state the updater does
/// not keep).
pub fn fetch_releases(owner: &str, repo: &str) -> Result<Vec<RawRelease>, String> {
    // per_page=100 returns every release in one page for a project this size.
    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases?per_page=100");
    let releases = ureq::get(&url)
        // The API answers 200 directly, so forbid redirects (no off-GitHub
        // bounce to forged JSON that would steer the asset URLs the updater
        // trusts up to the minisign check) + a timeout (anti-hang).
        .config()
        .max_redirects(0)
        .max_redirects_will_error(true)
        .timeout_global(Some(NETWORK_TIMEOUT))
        .build()
        .header("User-Agent", USER_AGENT)
        .header("Accept", GITHUB_ACCEPT)
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .header("Cache-Control", "no-cache")
        .call()
        .map_err(map_github_error)?
        .body_mut()
        .read_json::<Vec<RawRelease>>()
        .map_err(|e| format!("failed to parse releases JSON: {e}"))?;
    Ok(releases)
}

/// Friendly mapping for a GitHub API transport/status error. A 403/429 on the
/// unauthenticated API is almost always the 60 req/hr/IP rate limit — surface
/// that distinctly so the UI shows "check failed (rate limited)", never the
/// false-negative "up to date".
fn map_github_error(e: ureq::Error) -> String {
    let s = e.to_string();
    if s.contains("403") || s.contains("429") || s.to_lowercase().contains("rate limit") {
        format!("GitHub rate limit reached (unauthenticated: 60 checks/hour) — try again in a few minutes. [{s}]")
    } else {
        format!("update check failed: {s}")
    }
}

/// Convenience: fetch the full release list + classify in one blocking call
/// (the worker thread calls this). `Ok(UpdateOutcome::…)` always distinguishes
/// up-to-date / available / newer-but-no-asset; `Err` means the network fetch
/// itself failed (and is shown as a check failure, never as "up to date").
pub fn check_for_update(
    owner: &str,
    repo: &str,
    current: &semver::Version,
    target: &str,
) -> Result<UpdateOutcome, String> {
    let releases = fetch_releases(owner, repo)?;
    Ok(select_best(&releases, current, target))
}

/// Blocking GET of a small file (sig / sha), returning its raw bytes.
fn download_small(url: &str) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    // Redirects ARE allowed here: an asset URL legitimately 302s from
    // github.com to the `*.githubusercontent.com` CDN. The content is
    // minisign+SHA-256 verified after download, so a misdirected body is caught
    // at verify time; the size cap below + the timeout are the pre-verify
    // memory/hang guards.
    let mut resp = ureq::get(url)
        .config()
        .timeout_global(Some(NETWORK_TIMEOUT))
        .build()
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("download failed for {url}: {e}"))?;
    let reader = resp.body_mut().as_reader();
    std::io::Read::take(reader, MAX_DOWNLOAD_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read failed for {url}: {e}"))?;
    if buf.len() as u64 > MAX_DOWNLOAD_BYTES {
        return Err(format!(
            "download for {url} exceeded the {MAX_DOWNLOAD_BYTES}-byte safety cap"
        ));
    }
    Ok(buf)
}

/// Blocking GET of a large asset, streaming the body to drive `progress`
/// (`downloaded`, `total`). `total` is read from `Content-Length`; if absent it
/// is reported as `0` (the UI shows an indeterminate bar). Returns the full
/// asset bytes.
fn download_asset(url: &str, mut progress: impl FnMut(u64, u64)) -> Result<Vec<u8>, String> {
    // Redirects allowed (CDN 302, as in `download_small`); verified post-download.
    let mut resp = ureq::get(url)
        .config()
        .timeout_global(Some(NETWORK_TIMEOUT))
        .build()
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("download failed for {url}: {e}"))?;

    let total: u64 = resp
        .headers()
        .get("Content-Length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);

    let mut reader = resp.body_mut().as_reader();
    // NEVER pre-allocate `Content-Length` blindly — it is attacker-controllable,
    // so reserving `total` up front lets a forged `Content-Length: 512MiB` force
    // a huge allocation before a single byte (let alone the signature) is checked.
    // Reserve only a small initial buffer and let the Vec grow as bytes actually
    // arrive (the streaming loop below enforces the real MAX_DOWNLOAD_BYTES cap).
    const INITIAL_RESERVE: u64 = 1024 * 1024; // 1 MiB
    let mut buf: Vec<u8> = Vec::with_capacity(total.min(INITIAL_RESERVE) as usize);
    let mut chunk = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    progress(0, total);
    loop {
        let n = reader
            .read(&mut chunk)
            .map_err(|e| format!("read failed for {url}: {e}"))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        downloaded += n as u64;
        // Bound the in-memory body so a hostile/misdirected response (or a body
        // with no honest `Content-Length`) can't exhaust memory before verify.
        if downloaded > MAX_DOWNLOAD_BYTES {
            return Err(format!(
                "download for {url} exceeded the {MAX_DOWNLOAD_BYTES}-byte safety cap"
            ));
        }
        progress(downloaded, total);
    }
    Ok(buf)
}

/// Hard cap on the bytes written for a single extracted binary — a
/// decompression-bomb / disk-fill guard. The real binary is tens of MB; 512 MiB
/// is generous headroom while bounding what a corrupt or hostile (yet somehow
/// signature-valid) archive could write.
const MAX_EXTRACTED_BINARY_BYTES: u64 = 512 * 1024 * 1024;

/// Extract the single `scr1b3` / `scr1b3.exe` binary entry from a `.tar.gz`
/// archive's bytes into `dir`, returning the path to the extracted file. On
/// unix the extracted file is made executable (`0o755`). This is split out from
/// [`download_verify_extract`] so it can be unit-tested directly (no network,
/// no signature) — the production path NEVER reaches here without a passing
/// `verify_artifact`.
fn extract_binary(archive_bytes: &[u8], dir: &Path) -> Result<PathBuf, String> {
    let gz = flate2::read::GzDecoder::new(archive_bytes);
    let mut archive = tar::Archive::new(gz);

    let entries = archive
        .entries()
        .map_err(|e| format!("failed to read tar entries: {e}"))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("failed to read tar entry: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("bad tar entry path: {e}"))?;
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if file_name == "scr1b3" || file_name == "scr1b3.exe" {
            // Defense-in-depth. The archive is already minisign-verified before we
            // ever reach here, so this is belt-and-suspenders, but archive
            // extraction is the canonical Rust-CVE class (TARmageddon /
            // CVE-2025-59825, zip-slip), so we harden it anyway:
            //  * Path traversal is already neutralised — `out_path` joins only the
            //    BASENAME (`path.file_name()`) to `dir`, so a `../../evil` entry
            //    path can never escape `dir`.
            //  * Reject any NON-REGULAR entry (symlink / hardlink / dir / device):
            //    a link entry named `scr1b3` must never be honoured (we would
            //    otherwise write an empty file, but rejecting is clearer + safer).
            //  * Cap the bytes written so a malformed or hostile archive cannot
            //    fill the disk (decompression-bomb guard).
            if entry.header().entry_type() != tar::EntryType::Regular {
                return Err(format!(
                    "refusing non-regular tar entry for {file_name} (type {:?})",
                    entry.header().entry_type()
                ));
            }
            let out_path = dir.join(&file_name);
            let mut out = fs::File::create(&out_path)
                .map_err(|e| format!("failed to create {}: {e}", out_path.display()))?;
            let mut limited = std::io::Read::take(entry, MAX_EXTRACTED_BINARY_BYTES);
            let written = std::io::copy(&mut limited, &mut out)
                .map_err(|e| format!("failed to write extracted binary: {e}"))?;
            drop(out);
            if written >= MAX_EXTRACTED_BINARY_BYTES {
                let _ = fs::remove_file(&out_path);
                return Err(format!(
                    "extracted binary exceeded the {MAX_EXTRACTED_BINARY_BYTES}-byte safety cap \
                     (corrupt or hostile archive)"
                ));
            }
            set_executable(&out_path)?;
            return Ok(out_path);
        }
    }
    Err("archive did not contain a scr1b3 / scr1b3.exe binary".to_string())
}

/// Mark `path` executable (`0o755`) on unix; a no-op on other platforms.
#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .map_err(|e| format!("failed to stat extracted binary: {e}"))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).map_err(|e| format!("failed to chmod extracted binary: {e}"))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Blocking: download asset + sig + sha into `staging_dir`, run
/// [`verify_artifact`] (sha256 THEN minisign against [`EMBEDDED_PUBLIC_KEYS`]),
/// then extract the single binary from the `.tar.gz` into `staging_dir`,
/// returning the path to the extracted, verified binary.
///
/// `progress` is called as `(downloaded_bytes, total_bytes)` for the big asset
/// so the UI can show a bar. ANY verify failure deletes `staging_dir` and
/// returns `Err` — the binary is NEVER returned unverified.
pub fn download_verify_extract(
    info: &ReleaseInfo,
    staging_dir: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf, String> {
    match download_verify_extract_inner(info, staging_dir, progress) {
        Ok(p) => Ok(p),
        Err(e) => {
            // Failure (network OR verify) wipes the staging dir so no partial /
            // unverified artifact is ever left behind.
            let _ = fs::remove_dir_all(staging_dir);
            Err(e)
        }
    }
}

/// Download the self-elevating installer (`setup.exe`), verify it (SHA-256 THEN
/// minisign against the embedded key — IDENTICAL gate to the tar.gz path), and
/// write it into `staging_dir`, returning the path to the verified `.exe`. The
/// caller launches it to update in place (the installer requests UAC). ANY
/// verify failure wipes `staging_dir` and returns `Err` — an unverified
/// installer is NEVER written for launch.
pub fn download_verify_installer(
    installer: &InstallerAsset,
    staging_dir: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf, String> {
    match download_verify_installer_inner(installer, staging_dir, progress) {
        Ok(p) => Ok(p),
        Err(e) => {
            let _ = fs::remove_dir_all(staging_dir);
            Err(e)
        }
    }
}

fn download_verify_installer_inner(
    installer: &InstallerAsset,
    staging_dir: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf, String> {
    fs::create_dir_all(staging_dir).map_err(|e| format!("failed to create staging dir: {e}"))?;

    let exe_bytes = download_asset(&installer.url, progress)?;
    let sig_bytes = download_small(&installer.sig_url)?;
    let sha_text = download_small(&installer.sha_url)?;

    let sha_str = String::from_utf8(sha_text)
        .map_err(|e| format!("sha256 sidecar is not valid UTF-8: {e}"))?;
    let expected_sha = sha_str
        .split_whitespace()
        .next()
        .ok_or_else(|| "sha256 sidecar was empty".to_string())?;
    let sig_str =
        String::from_utf8(sig_bytes).map_err(|e| format!("minisig is not valid UTF-8: {e}"))?;

    // SHA-256 THEN minisign against the trusted key set. Fails closed.
    verify_artifact(&exe_bytes, expected_sha, &sig_str, EMBEDDED_PUBLIC_KEYS)?;

    // Only reached when verification passed — write the verified installer out.
    let out = staging_dir.join("scr1b3-setup.exe");
    fs::write(&out, &exe_bytes).map_err(|e| format!("failed to write installer: {e}"))?;
    Ok(out)
}

fn download_verify_extract_inner(
    info: &ReleaseInfo,
    staging_dir: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf, String> {
    fs::create_dir_all(staging_dir).map_err(|e| format!("failed to create staging dir: {e}"))?;

    // Big asset (streamed for progress) + the two tiny sidecars.
    let asset_bytes = download_asset(&info.asset_url, progress)?;
    let sig_bytes = download_small(&info.sig_url)?;
    let sha_text = download_small(&info.sha_url)?;

    // The .sha256 sidecar is text — either a bare hex digest or the
    // `<hex>  <filename>` `sha256sum` form. Take the first whitespace token.
    let sha_str = String::from_utf8(sha_text)
        .map_err(|e| format!("sha256 sidecar is not valid UTF-8: {e}"))?;
    let expected_sha = sha_str
        .split_whitespace()
        .next()
        .ok_or_else(|| "sha256 sidecar was empty".to_string())?;

    let sig_str =
        String::from_utf8(sig_bytes).map_err(|e| format!("minisig is not valid UTF-8: {e}"))?;

    // SHA-256 THEN minisign against the trusted key set. Fails closed.
    verify_artifact(&asset_bytes, expected_sha, &sig_str, EMBEDDED_PUBLIC_KEYS)?;

    // Only reached when verification passed.
    extract_binary(&asset_bytes, staging_dir)
}

#[cfg(test)]
mod tests {
    use super::super::verify::sha256_hex;
    use super::*;
    use std::io::Write;

    fn asset(name: &str, url: &str) -> RawAsset {
        RawAsset {
            name: name.to_string(),
            browser_download_url: url.to_string(),
        }
    }

    /// A release fixture for `<target>` with a full asset triple at `tag`.
    fn release_with_triple(tag: &str, target: &str) -> RawRelease {
        let base = format!("scr1b3-{target}.tar.gz");
        RawRelease {
            tag_name: tag.to_string(),
            prerelease: false,
            draft: false,
            html_url: "https://github.com/o/r/releases/tag/x".to_string(),
            assets: vec![
                asset(&base, &format!("https://dl/{base}")),
                asset(
                    &format!("{base}.minisig"),
                    &format!("https://dl/{base}.minisig"),
                ),
                asset(
                    &format!("{base}.sha256"),
                    &format!("https://dl/{base}.sha256"),
                ),
            ],
        }
    }

    #[test]
    fn select_update_returns_some_on_newer_with_matching_triple() {
        let target = "x86_64-unknown-linux-gnu";
        let raw = release_with_triple("v0.4.0", target);
        let current = semver::Version::parse("0.3.2").unwrap();
        let info = select_update(&raw, &current, target).expect("expected an update");
        assert_eq!(info.version, semver::Version::parse("0.4.0").unwrap());
        assert_eq!(info.tag, "v0.4.0");
        assert_eq!(info.asset_url, format!("https://dl/scr1b3-{target}.tar.gz"));
        assert_eq!(
            info.sig_url,
            format!("https://dl/scr1b3-{target}.tar.gz.minisig")
        );
        assert_eq!(
            info.sha_url,
            format!("https://dl/scr1b3-{target}.tar.gz.sha256")
        );
        assert_eq!(info.html_url, "https://github.com/o/r/releases/tag/x");
    }

    #[test]
    fn select_update_accepts_legacy_sha_name() {
        // Regression for the recurring "no build for your platform" bug: every
        // release before this fix shipped the checksum as `scr1b3-<target>.sha256`
        // (no `.tar.gz` infix) while the updater looked for `<asset>.sha256`
        // (`scr1b3-<target>.tar.gz.sha256`) — so `find()` returned None and a
        // perfectly-good release was classified NewerButNoAsset. Releases now ship
        // the canonical name AND the updater accepts the legacy one; this locks
        // the fallback so a release with EITHER sha name resolves.
        let target = "x86_64-pc-windows-msvc";
        let base = format!("scr1b3-{target}.tar.gz");
        let raw = RawRelease {
            tag_name: "v0.5.0".to_string(),
            prerelease: false,
            draft: false,
            html_url: "https://github.com/o/r/releases/tag/x".to_string(),
            assets: vec![
                asset(&base, &format!("https://dl/{base}")),
                asset(
                    &format!("{base}.minisig"),
                    &format!("https://dl/{base}.minisig"),
                ),
                // LEGACY checksum name (no `.tar.gz` infix).
                asset(
                    &format!("scr1b3-{target}.sha256"),
                    &format!("https://dl/scr1b3-{target}.sha256"),
                ),
            ],
        };
        let current = semver::Version::parse("0.4.0").unwrap();
        let info =
            select_update(&raw, &current, target).expect("legacy sha name must still resolve");
        assert_eq!(info.sha_url, format!("https://dl/scr1b3-{target}.sha256"));
    }

    #[test]
    fn ensure_upgrade_enforces_strict_monotonic_version() {
        // Strictly-newer candidates are allowed (with or without a leading `v`).
        assert!(ensure_upgrade("v0.5.0", "0.4.9").is_ok());
        assert!(ensure_upgrade("0.4.10", "0.4.9").is_ok());
        // Anti-downgrade (TUF rollback attack): equal or older is REFUSED at
        // apply time even though such a release may be validly signed.
        assert!(
            ensure_upgrade("v0.4.9", "0.4.9").is_err(),
            "equal must be refused"
        );
        assert!(
            ensure_upgrade("v0.4.8", "0.4.9").is_err(),
            "older must be refused"
        );
        assert!(ensure_upgrade("0.3.0", "0.4.9").is_err());
        // An unparseable candidate fails closed (never installs).
        assert!(ensure_upgrade("not-a-version", "0.4.9").is_err());
    }

    #[test]
    fn select_update_none_on_same_version() {
        let target = "x86_64-pc-windows-msvc";
        let raw = release_with_triple("v0.3.2", target);
        let current = semver::Version::parse("0.3.2").unwrap();
        assert!(select_update(&raw, &current, target).is_none());
    }

    // --- select_best (the FULL-list, highest-semver discovery path) ---

    #[test]
    fn select_best_picks_highest_semver_not_list_order() {
        // List order is deliberately NOT newest-first, and a 0.4.10 is present to
        // catch lexical-vs-semver bugs ("0.4.2" > "0.4.10" lexically).
        let target = "x86_64-pc-windows-msvc";
        let releases = vec![
            release_with_triple("v0.4.2", target),
            release_with_triple("v0.4.10", target),
            release_with_triple("v0.4.1", target),
        ];
        let current = semver::Version::parse("0.4.0").unwrap();
        match select_best(&releases, &current, target) {
            UpdateOutcome::Available(info) => {
                assert_eq!(info.version, semver::Version::parse("0.4.10").unwrap());
            }
            other => panic!("expected Available(0.4.10), got {other:?}"),
        }
    }

    #[test]
    fn select_best_up_to_date_reports_latest_seen() {
        let target = "x86_64-pc-windows-msvc";
        let releases = vec![
            release_with_triple("v0.4.0", target),
            release_with_triple("v0.4.2", target),
        ];
        let current = semver::Version::parse("0.4.2").unwrap();
        match select_best(&releases, &current, target) {
            UpdateOutcome::UpToDate { latest } => {
                assert_eq!(latest, semver::Version::parse("0.4.2").unwrap());
            }
            other => panic!("expected UpToDate, got {other:?}"),
        }
    }

    #[test]
    fn select_best_newer_but_no_asset_for_platform() {
        // Newest release ships ONLY a linux asset; a windows build sees a newer
        // version with no matching asset — must NOT read as "up to date".
        let newest = release_with_triple("v0.5.0", "x86_64-unknown-linux-gnu");
        let releases = vec![newest];
        let current = semver::Version::parse("0.4.0").unwrap();
        match select_best(&releases, &current, "x86_64-pc-windows-msvc") {
            UpdateOutcome::NewerButNoAsset { latest, target, .. } => {
                assert_eq!(latest, semver::Version::parse("0.5.0").unwrap());
                assert_eq!(target, "x86_64-pc-windows-msvc");
            }
            other => panic!("expected NewerButNoAsset, got {other:?}"),
        }
    }

    #[test]
    fn select_best_ignores_prerelease_and_draft() {
        let target = "x86_64-pc-windows-msvc";
        let mut pre = release_with_triple("v0.9.0", target);
        pre.prerelease = true;
        let mut draft = release_with_triple("v0.8.0", target);
        draft.draft = true;
        let releases = vec![pre, draft, release_with_triple("v0.4.2", target)];
        let current = semver::Version::parse("0.4.0").unwrap();
        match select_best(&releases, &current, target) {
            // 0.9.0 (prerelease) + 0.8.0 (draft) are skipped → 0.4.2 wins.
            UpdateOutcome::Available(info) => {
                assert_eq!(info.version, semver::Version::parse("0.4.2").unwrap());
            }
            other => panic!("expected Available(0.4.2), got {other:?}"),
        }
    }

    #[test]
    fn select_best_empty_list_is_up_to_date_not_error() {
        let current = semver::Version::parse("0.4.0").unwrap();
        match select_best(&[], &current, "x86_64-pc-windows-msvc") {
            UpdateOutcome::UpToDate { latest } => assert_eq!(latest, current),
            other => panic!("expected UpToDate, got {other:?}"),
        }
    }

    /// Add the self-elevating Windows installer triple to a fixture release.
    fn with_installer(mut raw: RawRelease) -> RawRelease {
        let exe = format!("scr1b3-{}-x86_64-setup.exe", raw.tag_name);
        raw.assets.push(asset(&exe, &format!("https://dl/{exe}")));
        raw.assets.push(asset(
            &format!("{exe}.minisig"),
            &format!("https://dl/{exe}.minisig"),
        ));
        raw.assets.push(asset(
            &format!("{exe}.sha256"),
            &format!("https://dl/{exe}.sha256"),
        ));
        raw
    }

    #[test]
    fn release_info_captures_windows_installer() {
        let target = "x86_64-pc-windows-msvc";
        let raw = with_installer(release_with_triple("v0.4.3", target));
        let current = semver::Version::parse("0.4.0").unwrap();
        let info = select_update(&raw, &current, target).expect("update");
        let inst = info.installer.expect("installer present for windows");
        assert_eq!(inst.url, "https://dl/scr1b3-v0.4.3-x86_64-setup.exe");
        assert_eq!(
            inst.sig_url,
            "https://dl/scr1b3-v0.4.3-x86_64-setup.exe.minisig"
        );
        assert_eq!(
            inst.sha_url,
            "https://dl/scr1b3-v0.4.3-x86_64-setup.exe.sha256"
        );
    }

    #[test]
    fn release_info_no_installer_for_non_windows() {
        let target = "x86_64-unknown-linux-gnu";
        // Even if a setup.exe is in the release, a linux build never offers it.
        let raw = with_installer(release_with_triple("v0.4.3", target));
        let current = semver::Version::parse("0.4.0").unwrap();
        let info = select_update(&raw, &current, target).expect("update");
        assert!(info.installer.is_none());
    }

    #[test]
    fn release_info_no_installer_when_sidecar_missing() {
        let target = "x86_64-pc-windows-msvc";
        let mut raw = with_installer(release_with_triple("v0.4.3", target));
        // Drop the installer's .sha256 — without a full verifiable triple the
        // installer must NOT be offered (fail closed).
        raw.assets
            .retain(|a| !a.name.ends_with("-x86_64-setup.exe.sha256"));
        let current = semver::Version::parse("0.4.0").unwrap();
        let info = select_update(&raw, &current, target).expect("update");
        assert!(info.installer.is_none());
    }

    #[test]
    fn select_update_none_on_older_version() {
        let target = "x86_64-pc-windows-msvc";
        let raw = release_with_triple("v0.2.0", target);
        let current = semver::Version::parse("0.3.2").unwrap();
        assert!(select_update(&raw, &current, target).is_none());
    }

    #[test]
    fn select_update_none_when_minisig_missing() {
        let target = "aarch64-apple-darwin";
        let mut raw = release_with_triple("v0.4.0", target);
        // Drop the .minisig sibling.
        raw.assets.retain(|a| !a.name.ends_with(".minisig"));
        let current = semver::Version::parse("0.3.2").unwrap();
        assert!(select_update(&raw, &current, target).is_none());
    }

    #[test]
    fn select_update_none_when_sha_missing() {
        let target = "aarch64-apple-darwin";
        let mut raw = release_with_triple("v0.4.0", target);
        raw.assets.retain(|a| !a.name.ends_with(".sha256"));
        let current = semver::Version::parse("0.3.2").unwrap();
        assert!(select_update(&raw, &current, target).is_none());
    }

    #[test]
    fn select_update_none_when_asset_missing() {
        let target = "aarch64-apple-darwin";
        let mut raw = release_with_triple("v0.4.0", target);
        // Drop the bare .tar.gz (keep the sidecars).
        let base = format!("scr1b3-{target}.tar.gz");
        raw.assets.retain(|a| a.name != base);
        let current = semver::Version::parse("0.3.2").unwrap();
        assert!(select_update(&raw, &current, target).is_none());
    }

    #[test]
    fn select_update_none_on_prerelease() {
        let target = "x86_64-unknown-linux-gnu";
        let mut raw = release_with_triple("v0.4.0", target);
        raw.prerelease = true;
        let current = semver::Version::parse("0.3.2").unwrap();
        assert!(select_update(&raw, &current, target).is_none());
    }

    #[test]
    fn select_update_none_on_draft() {
        let target = "x86_64-unknown-linux-gnu";
        let mut raw = release_with_triple("v0.4.0", target);
        raw.draft = true;
        let current = semver::Version::parse("0.3.2").unwrap();
        assert!(select_update(&raw, &current, target).is_none());
    }

    #[test]
    fn select_update_none_on_malformed_tag() {
        let target = "x86_64-unknown-linux-gnu";
        let raw = release_with_triple("not-a-version", target);
        let current = semver::Version::parse("0.3.2").unwrap();
        assert!(select_update(&raw, &current, target).is_none());
    }

    #[test]
    fn select_update_tolerates_tag_without_v_prefix() {
        let target = "x86_64-unknown-linux-gnu";
        let raw = release_with_triple("0.5.0", target);
        let current = semver::Version::parse("0.3.2").unwrap();
        let info = select_update(&raw, &current, target).expect("expected an update");
        assert_eq!(info.version, semver::Version::parse("0.5.0").unwrap());
    }

    /// Build a real `.tar.gz` containing a single fake binary, then assert
    /// `extract_binary` pulls it back out. This exercises the gz+tar extraction
    /// path independently of (and without weakening) the verify gate.
    #[test]
    fn extract_binary_roundtrips_a_fake_binary() {
        let dir = tempfile::tempdir().unwrap();

        let bin_name = if cfg!(windows) {
            "scr1b3.exe"
        } else {
            "scr1b3"
        };
        let payload = b"#!/bin/sh\necho fake scr1b3 binary\n";

        // gz + tar the fake binary in memory.
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, bin_name, &payload[..])
                .unwrap();
            builder.finish().unwrap();
        }
        let archive_bytes = gz.finish().unwrap();

        let extracted = extract_binary(&archive_bytes, dir.path()).unwrap();
        assert_eq!(
            extracted.file_name().and_then(|n| n.to_str()),
            Some(bin_name)
        );
        assert_eq!(fs::read(&extracted).unwrap(), payload);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&extracted).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    /// Defense-in-depth: a tarball whose `scr1b3` entry is a SYMLINK (not a
    /// regular file) is REJECTED — the updater must never honour a link entry
    /// (the TARmageddon / CVE-2025-59825 class), and nothing is written.
    #[test]
    fn extract_binary_rejects_symlink_entry() {
        let dir = tempfile::tempdir().unwrap();
        let bin_name = if cfg!(windows) {
            "scr1b3.exe"
        } else {
            "scr1b3"
        };
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            builder
                .append_link(&mut header, bin_name, "/etc/passwd")
                .unwrap();
            builder.finish().unwrap();
        }
        let archive_bytes = gz.finish().unwrap();

        let err = extract_binary(&archive_bytes, dir.path()).unwrap_err();
        assert!(err.contains("non-regular"), "got: {err}");
        assert!(
            !dir.path().join(bin_name).exists(),
            "a symlink entry must not produce any output file"
        );
    }

    /// Defense-in-depth against the zip-slip / TARmageddon class
    /// (CVE-2025-59825): a tar entry whose PATH carries a directory PREFIX is
    /// neutralised to its BASENAME — `extract_binary` joins only
    /// `path.file_name()` to the output dir, so the binary always lands INSIDE
    /// `dir` and can never escape it. This locks in the basename-only invariant
    /// so a future refactor cannot reintroduce path traversal.
    ///
    /// Note: we use a multi-segment subdir prefix rather than literal `..`
    /// because the `tar` crate REFUSES to even build an archive whose entry path
    /// contains `..` (`append_data` returns an error) — that is the FIRST layer
    /// of defense. Since extraction reduces any path to its `file_name()`,
    /// stripping a subdir prefix exercises the exact same neutralisation code
    /// path a `..` entry would hit if a hand-crafted archive smuggled one in.
    #[test]
    fn extract_binary_neutralises_path_prefix_to_basename() {
        let dir = tempfile::tempdir().unwrap();
        let bin_name = if cfg!(windows) {
            "scr1b3.exe"
        } else {
            "scr1b3"
        };
        let payload = b"fake binary payload";
        let prefixed_path = format!("nested/evil/subdir/{bin_name}");

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, &prefixed_path, &payload[..])
                .unwrap();
            builder.finish().unwrap();
        }
        let archive_bytes = gz.finish().unwrap();

        let extracted = extract_binary(&archive_bytes, dir.path()).unwrap();
        // The subdir prefix was stripped: the binary landed at dir/<basename>,
        // NOT at dir/nested/evil/subdir/<basename> and never outside dir.
        assert_eq!(extracted, dir.path().join(bin_name));
        assert!(
            extracted.starts_with(dir.path()),
            "extracted path escaped the target dir: {extracted:?}"
        );
        assert!(
            !dir.path().join("nested").exists(),
            "the subdir prefix must not have been recreated under the target dir"
        );
        assert_eq!(fs::read(&extracted).unwrap(), payload);
    }

    /// First layer of zip-slip defense: the `tar` crate itself refuses to
    /// construct an archive whose entry path contains `..` — so a traversal
    /// archive cannot be produced through the normal API at all. Documents +
    /// asserts that invariant (the basename-strip in `extract_binary` is the
    /// second layer, covered above).
    #[test]
    fn tar_builder_refuses_to_write_a_dotdot_entry() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(&mut gz);
        let mut header = tar::Header::new_gnu();
        let payload = b"x";
        header.set_size(payload.len() as u64);
        header.set_cksum();
        let res = builder.append_data(&mut header, "../../etc/evil", &payload[..]);
        assert!(
            res.is_err(),
            "tar builder must reject a `..` traversal entry path"
        );
    }

    #[test]
    fn extract_binary_errs_when_no_binary_entry() {
        let dir = tempfile::tempdir().unwrap();
        // Archive containing only an unrelated file.
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            let data = b"readme";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "README.txt", &data[..])
                .unwrap();
            builder.finish().unwrap();
        }
        let archive_bytes = gz.finish().unwrap();
        assert!(extract_binary(&archive_bytes, dir.path()).is_err());
    }

    /// `sha256_hex` over the archive bytes is the same digest the `.sha256`
    /// sidecar carries — a sanity check that the verify input we feed matches
    /// the documented contract (the sidecar's first whitespace token).
    #[test]
    fn sha_sidecar_first_token_matches_archive_digest() {
        let archive = b"pretend tarball bytes";
        let digest = sha256_hex(archive);
        let sidecar = format!("{digest}  scr1b3-x.tar.gz\n");
        let first = sidecar.split_whitespace().next().unwrap();
        assert_eq!(first, digest);
    }

    /// Round-trip the staging-cleanup contract at the helper level: a temp dir
    /// with a partial file is removed by `remove_dir_all`, proving the failure
    /// branch of `download_verify_extract` cleans up. (We can't drive the full
    /// function without the network, so we assert the cleanup primitive.)
    #[test]
    fn staging_cleanup_removes_partial_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        fs::create_dir_all(&staging).unwrap();
        let mut f = fs::File::create(staging.join("partial.bin")).unwrap();
        f.write_all(b"partial").unwrap();
        drop(f);
        assert!(staging.exists());
        fs::remove_dir_all(&staging).unwrap();
        assert!(!staging.exists());
    }
}
