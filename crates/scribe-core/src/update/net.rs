//! Network half of the in-app self-updater.
//!
//! Telemetry-free by construction: the only network surfaces are
//! 1. a single unauthenticated `GET` of the public GitHub Releases API,
//! 2. download of the SIGNED `latest.json` manifest + its `.minisig`, and
//! 3. downloads of the release archive + its `.minisig` + `.sha256` siblings.
//!
//! No analytics, no identifiers, no payload: every request sends only a generic
//! `User-Agent` (app name + version). A Tier-1 client installs ONLY through the
//! verified signed manifest — the archive is verified (its bytes pinned to the
//! manifest's SIGNED SHA-256, then minisign against
//! [`super::verify::EMBEDDED_PUBLIC_KEYS`]) before the extracted binary is ever
//! returned. A verify failure deletes the staging area and the binary is NEVER
//! returned unverified. **There is no install path that skips the manifest.**
//!
//! ## Tier-1 REQUIRES a verified manifest — fail-CLOSED, no fallback
//!
//! When a newer release is discovered, this client REQUIRES that release to
//! carry a signed `latest.json` (+ `latest.json.minisig`). If the manifest is
//! ABSENT or fails verification, the update is REFUSED — there is deliberately
//! NO fallback to a legacy per-asset selector. A fallback would make the
//! freeze-beacon, the `minimum_version` floor, and the signed-hash binding
//! OPTIONAL: an attacker who strips `latest.json` (or its `.minisig`) could
//! force the weaker path and downgrade the protection. The legacy non-manifest
//! selectors (`select_best` / `select_update` / `build_release_info`) were
//! REMOVED for exactly this reason — they no longer exist as a code path.
//!
//! Pure decision logic ([`resolve_tier1_update`]) is split out from the I/O so
//! it can be unit-tested offline against a fixture [`RawRelease`] + manifest.
//!
//! ## Asset naming
//!
//! SCR1B3's release workflow publishes, per target, an archive named
//! `scr1b3-<target>.tar.gz` plus a `.sha256` and a `.minisig` sidecar, and
//! (Windows) a self-elevating `scr1b3-<tag>-x86_64-setup.exe` installer with its
//! own sidecars. [`manifest::Manifest::archive_for`] matches the in-place
//! archive by the **target-triple substring + `.tar.gz` extension**;
//! [`manifest::Manifest::installer_for`] matches the elevated installer.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::verify::{verify_artifact, EMBEDDED_PUBLIC_KEYS};
use super::{manifest, update_state};

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

/// Download-DoS guard for the signed `latest.json` manifest. A real manifest is
/// a few KiB (a handful of asset entries); 1 MiB is a generous ceiling that
/// still refuses an unbounded flood before the signature/serde work runs.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// A single release asset as returned by the GitHub Releases API. Only the
/// fields the updater needs are deserialized.
#[derive(Clone, Debug, Deserialize)]
pub struct RawAsset {
    pub name: String,
    pub browser_download_url: String,
}

/// The subset of the GitHub `releases/latest` JSON the updater reads. Made
/// public + constructible so the Tier-1 resolver can be unit-tested with a
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
/// signature/checksum sidecars, plus the manifest's SIGNED sha256 the download
/// is pinned to. Present only when the verified manifest enumerates an installer
/// for this platform. Used for the in-place-update path when the app lives in a
/// protected, admin-owned location (e.g. `C:\Program Files`): the installer
/// self-elevates, so running it updates in place where a direct exe swap can't.
#[derive(Clone, Debug)]
pub struct InstallerAsset {
    /// The `setup.exe` download url (the manifest's SIGNED url).
    pub url: String,
    /// The `setup.exe.minisig` url.
    pub sig_url: String,
    /// The `setup.exe.sha256` url.
    pub sha_url: String,
    /// The SIGNED SHA-256 from the verified manifest, pinned as the expected
    /// digest for the installer download (binding the bytes to the signed hash).
    pub pinned_sha256: String,
}

/// One resolved, newer-than-current release ready to download.
#[derive(Clone, Debug)]
pub struct ReleaseInfo {
    pub version: semver::Version,
    /// The original tag string (e.g. `v0.4.0`).
    pub tag: String,
    /// The `.tar.gz` download url (the manifest's SIGNED url).
    pub asset_url: String,
    /// The `.tar.gz.minisig` url.
    pub sig_url: String,
    /// The `.tar.gz.sha256` url.
    pub sha_url: String,
    /// The release page (for "view changelog" in a browser).
    pub html_url: String,
    /// The SIGNED SHA-256 from the verified manifest, pinned as the expected
    /// digest for the download (binding the bytes to the signed hash). Every
    /// `ReleaseInfo` carries a pin by construction — a Tier-1 client only ever
    /// resolves an update through the signed manifest, so there is NO
    /// unpinned/manifest-absent install path (the type makes the guarantee).
    pub pinned_sha256: String,
    /// The manifest `release_index`, persisted as the new monotonic high-water
    /// mark on a successful apply (anti-rollback). `None` only on a hand-built
    /// fixture; the production resolver always sets it.
    pub release_index: Option<u64>,
    /// The self-elevating Windows installer for this release, when the manifest
    /// enumerates one — the apply path for a Program-Files install. `None` on
    /// platforms/releases without a `setup.exe`. Boxed so the (common) `None`
    /// case keeps `ReleaseInfo` small — it rides inside several UI-state enum
    /// variants.
    pub installer: Option<Box<InstallerAsset>>,
}

/// The result of a successful update check. A tri-state so the UI can ALWAYS
/// distinguish "you're current" from "a newer release exists but has no build
/// for your platform" — the latter must never read as "up to date" (the
/// classic self-updater false-negative). Network/parse/rate-limit failures AND
/// a manifest that is absent/unverifiable/refused-by-a-gate are a separate
/// `Err` from [`check_for_update`], never folded into this enum.
#[derive(Clone, Debug)]
pub enum UpdateOutcome {
    /// A newer release WITH a verified-manifest asset matching this build's
    /// target.
    Available(ReleaseInfo),
    /// Already on (or ahead of) the newest published release. `latest` is the
    /// highest semver seen — shown next to the current version so "up to date"
    /// is never ambiguous.
    UpToDate { latest: semver::Version },
    /// A newer release exists but its verified manifest ships no archive asset
    /// matching this build's target triple (e.g. a platform that release
    /// skipped). The user is pointed at the release page to download manually
    /// rather than told "up to date".
    NewerButNoAsset {
        latest: semver::Version,
        target: String,
        html_url: String,
    },
}

/// Parse a release `tag_name` into a [`semver::Version`], tolerating a single
/// leading `v`. Returns `None` on malformed input (the caller treats that as
/// "no update", never a crash).
///
/// DISCOVERY-ONLY: this ranks releases by semver to pick the highest stable tag
/// to even consider. It NEVER builds an installable descriptor — the install
/// decision is made entirely from the SIGNED manifest in
/// [`resolve_tier1_update`]. The authoritative install version is the manifest's
/// `version`, not this tag.
fn parse_tag(tag: &str) -> Option<semver::Version> {
    let s = tag.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    semver::Version::parse(s).ok()
}

/// PURE (no network) discovery over the FULL release list: pick the highest
/// **semver** among non-draft/non-prerelease releases (NOT GitHub's
/// `/releases/latest`, which sorts by commit date + honors a mutable, cacheable
/// "latest" flag and can therefore skip a newer tag). Returns the chosen
/// `(version, release)` or `None` when there is no parseable stable release.
///
/// This is discovery ONLY — it decides WHICH release to fetch a manifest for. It
/// never produces an installable `ReleaseInfo`; that requires the verified
/// signed manifest ([`resolve_tier1_update`]).
fn pick_highest_stable(releases: &[RawRelease]) -> Option<(semver::Version, &RawRelease)> {
    releases
        .iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| parse_tag(&r.tag_name).map(|v| (v, r)))
        .max_by(|a, b| a.0.cmp(&b.0))
}

/// The archive file extension this build's release artifact carries. SCR1B3
/// ships a `.tar.gz` archive on every platform (the Windows in-place archive is
/// also a `.tar.gz`; the `setup.exe` is the separate elevated-install path).
pub const fn archive_ext() -> &'static str {
    ".tar.gz"
}

/// Apply-time anti-downgrade guard (TUF rollback-attack defense). Returns `Ok`
/// only when `candidate` parses to a STRICTLY newer semver than `running`.
///
/// This is enforced at the moment of APPLYING an update — in addition to the
/// manifest `version > current` and `release_index > persisted` gates at check
/// time — so a tampered or replayed older-but-validly-signed release can never
/// be installed over a newer running build. `running` is the compiled-in
/// `CARGO_PKG_VERSION` (authoritative). `candidate` may carry a leading `v`
/// (it is parsed with the same [`parse_tag`] normalisation as release tags).
pub fn ensure_upgrade(candidate: &str, running: &str) -> Result<(), String> {
    let cand = parse_tag(candidate)
        .ok_or_else(|| format!("refusing to install: unparseable update version {candidate:?}"))?;
    let cur = semver::Version::parse(running)
        .map_err(|e| format!("internal: bad running version {running:?}: {e}"))?;
    if cand <= cur {
        // Anti-downgrade is a TUF rollback-attack defense; a blocked downgrade
        // is a security event that must leave a durable record. Version strings
        // are NOT secrets (safe at warn+); never log signature/key material.
        tracing::warn!(
            target: "scribe::update",
            attempted = %cand,
            current = %cur,
            "blocked downgrade/rollback — refusing to install a release not newer than the running version"
        );
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
pub fn fetch_releases(owner: &str, repo: &str) -> Result<Vec<RawRelease>, String> {
    // per_page=100 returns every release in one page for a project this size.
    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases?per_page=100");
    fetch_releases_at(&url)
}

/// The URL-targetable core of [`fetch_releases`]: issue the redirect-forbidden,
/// no-cache `GET` against an explicit `url` and parse the body as a release
/// list. Split out so the request/parse path can be unit-tested against a local
/// mock server (no real network).
fn fetch_releases_at(url: &str) -> Result<Vec<RawRelease>, String> {
    let releases = ureq::get(url)
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

/// Current wall-clock as a Unix timestamp (seconds). On a clock error (a
/// before-epoch system time) returns [`i64::MAX`] so freshness checks fail
/// CLOSED — an unreadable clock must never make a stale manifest look fresh.
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(i64::MAX)
}

/// Locate the signed-manifest pair (`latest.json` + `latest.json.minisig`) among
/// the release assets. Returns `Some((json_url, sig_url))` only when BOTH are
/// present; `None` when EITHER is absent.
fn find_manifest_assets(raw: &RawRelease) -> Option<(String, String)> {
    let url_of = |name: &str| -> Option<String> {
        raw.assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.clone())
    };
    let json = url_of("latest.json")?;
    let sig = url_of("latest.json.minisig")?;
    Some((json, sig))
}

/// Require the signed-manifest pair on a release. An ABSENT manifest (or its
/// signature) is a hard refusal — a Tier-1 client never installs an update it
/// cannot verify, so a missing manifest fails CLOSED here rather than degrading
/// to a weaker per-asset path (which no longer exists).
fn require_manifest_assets(raw: &RawRelease) -> Result<(String, String), String> {
    find_manifest_assets(raw).ok_or_else(|| {
        "update could not be verified: this release carries no signed manifest \
         (latest.json + latest.json.minisig) — refusing to install"
            .to_string()
    })
}

/// Download the REQUIRED signed manifest pair for an already-fetched release.
/// `Err` when the manifest is absent (fail-closed) OR on a network/decoding
/// failure. The returned `(json_bytes, sig_str)` are UNVERIFIED — the caller
/// MUST pass them through [`manifest::parse_and_verify`] before trusting them.
fn fetch_manifest_for(raw: &RawRelease) -> Result<(Vec<u8>, String), String> {
    let (json_url, sig_url) = require_manifest_assets(raw)?;
    let json = download_small_capped(&json_url, MAX_MANIFEST_BYTES)?;
    let sig = download_small(&sig_url)?;
    let sig_str = String::from_utf8(sig)
        .map_err(|e| format!("manifest signature is not valid UTF-8: {e}"))?;
    Ok((json, sig_str))
}

/// Convenience: fetch the full release list, pick the highest stable release,
/// and — when it is newer — REQUIRE + verify its signed manifest and resolve a
/// Tier-1 update. The worker thread calls this.
///
/// Returns `Ok(UpdateOutcome::…)` (up-to-date / available / newer-but-no-asset).
/// `Err` means the network fetch failed, the release carries NO signed manifest,
/// the manifest could not be VERIFIED, or a manifest gate (identity / freshness /
/// minimum_version / anti-rollback) refused the update. There is NO fallback to
/// a non-manifest install path.
pub fn check_for_update(
    owner: &str,
    repo: &str,
    current: &semver::Version,
    target: &str,
) -> Result<UpdateOutcome, String> {
    let releases = fetch_releases(owner, repo)?;
    // Discovery: which release (if any) is even newer than us?
    let Some((latest, raw)) = pick_highest_stable(&releases) else {
        // No parseable stable release at all — silent "up to date".
        return Ok(UpdateOutcome::UpToDate {
            latest: current.clone(),
        });
    };
    if latest <= *current {
        return Ok(UpdateOutcome::UpToDate { latest });
    }
    // A NEWER release exists → Tier-1 REQUIRES a verified signed manifest on it.
    // Absent/unverifiable/gate-refused all fail CLOSED (Err) — never a fallback.
    let (json, sig_str) = fetch_manifest_for(raw)?;
    let manifest = manifest::parse_and_verify(&json, &sig_str, EMBEDDED_PUBLIC_KEYS)?;
    resolve_tier1_update(
        raw,
        &manifest,
        current,
        target,
        archive_ext(),
        now_unix_secs(),
        update_state::applied_index(),
    )
}

/// PURE (no network) Tier-1 resolver: given a VERIFIED manifest, decide the
/// update. Every gate fails CLOSED. `now_unix` and `persisted_index` are passed
/// in (not read from the clock/disk) so the whole decision is unit-testable.
///
/// Returns:
/// - `Ok(UpdateOutcome::Available(info))` — a fresh, in-policy update with the
///   SIGNED archive url + the pinned manifest SHA-256 (+ `release_index`).
/// - `Ok(UpdateOutcome::UpToDate { latest })` — the manifest version is `<=`
///   current (defensive; discovery already filtered this).
/// - `Ok(UpdateOutcome::NewerButNoAsset { .. })` — newer, but the verified
///   manifest carries no archive asset for this platform.
/// - `Err(reason)` — a gate REFUSAL (wrong product/schema, prerelease/draft,
///   stale/frozen, below the minimum floor, a rollback, an unparseable version,
///   or a malformed archive entry).
fn resolve_tier1_update(
    raw: &RawRelease,
    manifest: &manifest::Manifest,
    current: &semver::Version,
    target: &str,
    ext: &str,
    now_unix: i64,
    persisted_index: u64,
) -> Result<UpdateOutcome, String> {
    // Channel-pin (defense-in-depth): the highest-stable discovery already
    // excludes prereleases/drafts, but if one reaches here it is a different
    // release CHANNEL than the pinned stable stream — refused so the updater can
    // never jump the user stable → beta.
    if raw.prerelease || raw.draft {
        return Err("refusing a prerelease/draft release on the stable channel".to_string());
    }

    // Identity binding (the heart of Tier-1): a manifest for a DIFFERENT product
    // or an unrecognised schema family is refused — never silently honoured.
    if manifest.product != manifest::MANIFEST_PRODUCT {
        return Err(format!(
            "manifest is for a different product {:?} (expected {:?}) — refusing",
            manifest.product,
            manifest::MANIFEST_PRODUCT
        ));
    }
    if !manifest
        .schema
        .starts_with(manifest::MANIFEST_SCHEMA_PREFIX)
    {
        return Err(format!(
            "unrecognised manifest schema {:?} (expected {:?}*) — refusing",
            manifest.schema,
            manifest::MANIFEST_SCHEMA_PREFIX
        ));
    }

    // Version first: an unparseable candidate is fail-closed; an equal-or-older
    // candidate is a normal "up to date" (no scary error, no gate noise).
    let candidate = manifest.version()?;
    if candidate <= *current {
        return Ok(UpdateOutcome::UpToDate { latest: candidate });
    }

    // Freshness (freeze beacon): a stale/frozen or unreadable-deadline manifest
    // for a would-be NEWER release is refused — fail-closed.
    if !manifest.is_fresh(now_unix) {
        return Err(format!(
            "update manifest is stale/frozen (valid_until {:?} has passed) — refusing",
            manifest.valid_until_utc
        ));
    }

    // Floor sanity: refuse an in-place hop when the running install is BELOW the
    // manifest's declared minimum supported version (too old to update in place
    // — a fresh install is required). Fail-closed.
    let minimum = manifest.minimum_version()?;
    if *current < minimum {
        return Err(format!(
            "installed version {current} is below the manifest minimum_version {minimum} — \
             a fresh install is required (in-place update refused)"
        ));
    }

    // Anti-rollback on the manifest ordinal: STRICTLY greater than the highest
    // index ever applied. Equal or lower is a replay/rollback. Fail-closed.
    if manifest.release_index <= persisted_index {
        return Err(format!(
            "rollback blocked: manifest release_index {} is not newer than the last \
             applied index {persisted_index} (refusing a replayed/superseded release)",
            manifest.release_index
        ));
    }

    // Resolve the in-place ARCHIVE asset from the SIGNED manifest (skips the
    // setup .exe). No archive for this platform → "newer but no asset".
    let masset = match manifest.archive_for(target, ext) {
        Some(a) => a,
        None => {
            return Ok(UpdateOutcome::NewerButNoAsset {
                latest: candidate,
                target: target.to_string(),
                html_url: raw.html_url.clone(),
            })
        }
    };

    let info = build_tier1_release_info(
        raw,
        manifest,
        masset,
        &candidate,
        manifest.release_index,
        target,
    )?;
    Ok(UpdateOutcome::Available(info))
}

/// Resolve a manifest asset's per-asset `.minisig` + `.sha256` sidecar URLs from
/// the release asset list (the manifest does not enumerate the sidecars; they
/// are kept as defense-in-depth). A missing sidecar is a malformed release —
/// fail-closed `Err`.
fn sidecar_urls(raw: &RawRelease, asset_name: &str) -> Result<(String, String), String> {
    let url_of = |name: &str| -> Option<String> {
        raw.assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.clone())
    };
    let sig_name = format!("{asset_name}.minisig");
    let sha_name = format!("{asset_name}.sha256");
    let sig_url = url_of(&sig_name).ok_or_else(|| {
        format!("manifest asset {asset_name:?} is missing its .minisig sidecar in the release — refusing")
    })?;
    let sha_url = url_of(&sha_name).ok_or_else(|| {
        format!("manifest asset {asset_name:?} is missing its .sha256 sidecar in the release — refusing")
    })?;
    Ok((sig_url, sha_url))
}

/// Build the download plumbing for a Tier-1 update: the SIGNED archive url from
/// the manifest, the per-asset sidecar urls from the release asset list (kept as
/// defense-in-depth), the pinned manifest SHA-256 + `release_index`, and the
/// optional self-elevating installer (also manifest-pinned). A manifest archive
/// whose sidecars are ABSENT, or whose url/sha256 are empty, is a malformed
/// release — fail-closed `Err`.
fn build_tier1_release_info(
    raw: &RawRelease,
    manifest: &manifest::Manifest,
    masset: &manifest::ManifestAsset,
    candidate: &semver::Version,
    release_index: u64,
    target: &str,
) -> Result<ReleaseInfo, String> {
    if masset.sha256.trim().is_empty() {
        return Err(format!(
            "manifest archive {:?} has an empty sha256 — refusing",
            masset.asset_name
        ));
    }
    if masset.url.trim().is_empty() {
        return Err(format!(
            "manifest archive {:?} has an empty url — refusing",
            masset.asset_name
        ));
    }
    let (sig_url, sha_url) = sidecar_urls(raw, &masset.asset_name)?;
    Ok(ReleaseInfo {
        version: candidate.clone(),
        tag: raw.tag_name.clone(),
        asset_url: masset.url.clone(),
        sig_url,
        sha_url,
        html_url: raw.html_url.clone(),
        pinned_sha256: masset.sha256.clone(),
        release_index: Some(release_index),
        installer: build_tier1_installer(raw, manifest, target).map(Box::new),
    })
}

/// Build the verified-manifest, SHA-pinned self-elevating installer descriptor
/// for a Windows `target`, if the manifest enumerates one AND its sidecars are
/// present. Returns `None` (no installer offered — never a fail-OPEN install)
/// when the manifest has no installer entry, when its url/sha256 are empty, or
/// when either per-asset sidecar is missing from the release.
fn build_tier1_installer(
    raw: &RawRelease,
    manifest: &manifest::Manifest,
    target: &str,
) -> Option<InstallerAsset> {
    let masset = manifest.installer_for(target)?;
    if masset.sha256.trim().is_empty() || masset.url.trim().is_empty() {
        return None;
    }
    let (sig_url, sha_url) = sidecar_urls(raw, &masset.asset_name).ok()?;
    Some(InstallerAsset {
        url: masset.url.clone(),
        sig_url,
        sha_url,
        pinned_sha256: masset.sha256.clone(),
    })
}

/// Resolve the expected SHA-256 the downloaded artifact is verified against.
///
/// The `pinned` (signed-manifest) digest is AUTHORITATIVE; the `.sha256` sidecar
/// is kept as defense-in-depth and MUST AGREE with it — a disagreement is a
/// tampered sidecar or a manifest/asset mismatch and is refused (fail-closed).
/// Comparison is case-insensitive and whitespace-trimmed (hex digests). The
/// pinned (manifest) value is returned, so the load-bearing digest is always the
/// signed one.
fn resolve_expected_sha<'a>(pinned: &'a str, sidecar: &str) -> Result<&'a str, String> {
    if pinned.trim().eq_ignore_ascii_case(sidecar.trim()) {
        Ok(pinned.trim())
    } else {
        Err(format!(
            "manifest/sidecar sha256 disagreement: manifest {:?} != sidecar {:?} — refusing",
            pinned.trim(),
            sidecar.trim()
        ))
    }
}

/// Blocking GET of a small file (sig / sha), returning its raw bytes.
fn download_small(url: &str) -> Result<Vec<u8>, String> {
    download_small_capped(url, MAX_DOWNLOAD_BYTES)
}

/// Blocking GET of a small file with an explicit byte `cap`, returning its raw
/// bytes. Host-confined to the redirect target the CDN supplies and size-capped
/// so a hostile endpoint cannot stream an unbounded body into memory before
/// verification runs. Used for the sidecars and the signed manifest.
fn download_small_capped(url: &str, cap: u64) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    // Redirects ARE allowed here: an asset URL legitimately 302s from
    // github.com to the `*.githubusercontent.com` CDN. The content is
    // minisign+SHA-256 verified after download (and, for the manifest, minisign
    // over the raw bytes), so a misdirected body is caught at verify time; the
    // size cap below + the timeout are the pre-verify memory/hang guards.
    let mut resp = ureq::get(url)
        .config()
        .timeout_global(Some(NETWORK_TIMEOUT))
        .build()
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("download failed for {url}: {e}"))?;
    let reader = resp.body_mut().as_reader();
    std::io::Read::take(reader, cap + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read failed for {url}: {e}"))?;
    if buf.len() as u64 > cap {
        return Err(format!(
            "download for {url} exceeded the {cap}-byte safety cap"
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

/// Fuzz/test-only seam exposing the private [`extract_binary`] decompression +
/// tar-extraction path so the `fuzz/` libFuzzer harness can drive arbitrary
/// `.tar.gz` bytes through the REAL extraction (decompression-bomb + tar-slip
/// surface) without the network or signature stages. Extracts into a fresh
/// system temp directory which is removed before returning, so the fuzz target
/// asserts only "never panics" — the safety caps (`MAX_EXTRACTED_BINARY_BYTES`,
/// non-regular-entry reject, basename-only join) live in `extract_binary`
/// itself and are exercised by the dedicated unit tests above.
///
/// `#[doc(hidden)]` + the `fuzzing`-or-`test` gate keep this out of the public
/// API surface; it is NOT a production entry point.
#[doc(hidden)]
#[cfg(any(test, fuzzing))]
pub fn fuzz_extract_binary(archive_bytes: &[u8]) {
    let Ok(dir) = std::env::temp_dir()
        .join(format!("scr1b3-fuzz-extract-{}", std::process::id()))
        .canonicalize()
        .or_else(|_| {
            let d =
                std::env::temp_dir().join(format!("scr1b3-fuzz-extract-{}", std::process::id()));
            std::fs::create_dir_all(&d).map(|_| d)
        })
    else {
        return;
    };
    let _ = extract_binary(archive_bytes, &dir);
    // Best-effort cleanup; the fuzzer reuses the same dir across runs and the
    // extraction overwrites/recreates the single basename, so leftover state is
    // bounded and harmless.
    let _ = fs::remove_dir_all(&dir);
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

/// Blocking: download asset + sig + sha into `staging_dir`, pin the bytes to the
/// manifest's SIGNED sha256 (the sidecar must AGREE), run [`verify_artifact`]
/// (sha256 THEN minisign against [`EMBEDDED_PUBLIC_KEYS`]), then extract the
/// single binary from the `.tar.gz` into `staging_dir`, returning the path to
/// the extracted, verified binary.
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

/// Download the self-elevating installer (`setup.exe`), pin it to the manifest's
/// SIGNED sha256 (sidecar must AGREE), verify it (SHA-256 THEN minisign against
/// the embedded key — IDENTICAL gate to the tar.gz path), and write it into
/// `staging_dir`, returning the path to the verified `.exe`. The caller launches
/// it to update in place (the installer requests UAC). ANY verify failure wipes
/// `staging_dir` and returns `Err` — an unverified installer is NEVER written
/// for launch.
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

/// Coarse, secret-free classification of a [`verify_artifact`] error string into
/// a stable failure KIND token. The raw error may embed a `minisign-verify`
/// detail; this maps it to one of a fixed set of tokens so the durable audit log
/// records WHY verification failed without ever emitting signature/key bytes.
fn verify_failure_kind(err: &str) -> &'static str {
    let e = err.to_ascii_lowercase();
    if e.contains("checksum") {
        "checksum-mismatch"
    } else if e.contains("no trusted") {
        "no-trusted-keys"
    } else if e.contains("public key") {
        "bad-public-key"
    } else if e.contains("bad signature") {
        "malformed-signature"
    } else {
        "signature-verify-failed"
    }
}

/// Emit the durable supply-chain audit record for a FAILED artifact
/// verification. This gate is the last line before an unverified binary would be
/// installed, so a failure here MUST leave a record (it previously produced
/// none). Logs only the coarse [`verify_failure_kind`] — NEVER the signature,
/// the expected SHA, or any artifact bytes.
fn log_verify_failure(artifact: &str, err: &str) {
    tracing::error!(
        target: "scribe::update",
        artifact,
        failure_kind = verify_failure_kind(err),
        "artifact verification failed — refusing to install the downloaded artifact (supply-chain gate)"
    );
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
    let sidecar_sha = sha_str
        .split_whitespace()
        .next()
        .ok_or_else(|| "sha256 sidecar was empty".to_string())?;
    // The manifest's SIGNED digest is authoritative; the sidecar must AGREE.
    let expected_sha = resolve_expected_sha(&installer.pinned_sha256, sidecar_sha)?;
    let sig_str =
        String::from_utf8(sig_bytes).map_err(|e| format!("minisig is not valid UTF-8: {e}"))?;

    // SHA-256 THEN minisign against the trusted key set. Fails closed.
    verify_artifact(&exe_bytes, expected_sha, &sig_str, EMBEDDED_PUBLIC_KEYS)
        .inspect_err(|e| log_verify_failure("installer", e))?;

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
    let sidecar_sha = sha_str
        .split_whitespace()
        .next()
        .ok_or_else(|| "sha256 sidecar was empty".to_string())?;

    // The manifest's SIGNED digest is authoritative and the sidecar must AGREE
    // (defense-in-depth — a disagreement fails closed). Every `ReleaseInfo`
    // carries a pin, so the download is always bound to the signed hash.
    let expected_sha = resolve_expected_sha(&info.pinned_sha256, sidecar_sha)?;

    let sig_str =
        String::from_utf8(sig_bytes).map_err(|e| format!("minisig is not valid UTF-8: {e}"))?;

    // SHA-256 THEN minisign against the trusted key set. Fails closed.
    verify_artifact(&asset_bytes, expected_sha, &sig_str, EMBEDDED_PUBLIC_KEYS)
        .inspect_err(|e| log_verify_failure("release-archive", e))?;

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

    fn manifest_asset(
        platform: &str,
        kind: &str,
        name: &str,
        sha: &str,
    ) -> manifest::ManifestAsset {
        manifest::ManifestAsset {
            platform: platform.to_string(),
            kind: kind.to_string(),
            asset_name: name.to_string(),
            url: format!("https://github.com/o/r/releases/download/x/{name}"),
            size: 1234,
            sha256: sha.to_string(),
        }
    }

    /// A verified-shape manifest (as `parse_and_verify` would have produced)
    /// carrying a windows + linux `.tar.gz` archive and a windows `setup.exe`.
    fn tier1_manifest(version: &str, release_index: u64, valid_until: &str) -> manifest::Manifest {
        manifest::Manifest {
            schema: "itasha.update.manifest/v1".to_string(),
            product: "scr1b3".to_string(),
            version: version.to_string(),
            release_index,
            minimum_version: "0.4.0".to_string(),
            published_utc: "2026-06-29T14:17:42Z".to_string(),
            valid_until_utc: valid_until.to_string(),
            assets: vec![
                manifest_asset(
                    "x86_64-pc-windows-msvc",
                    "tar.gz",
                    "scr1b3-x86_64-pc-windows-msvc.tar.gz",
                    "1111aaaa",
                ),
                manifest_asset(
                    "x86_64-unknown-linux-gnu",
                    "tar.gz",
                    "scr1b3-x86_64-unknown-linux-gnu.tar.gz",
                    "2222bbbb",
                ),
                manifest_asset(
                    "x86_64-pc-windows-msvc",
                    "exe",
                    &format!("scr1b3-v{version}-x86_64-setup.exe"),
                    "3333cccc",
                ),
            ],
        }
    }

    /// A release fixture whose assets include, for `target`, the archive +
    /// `.minisig` + `.sha256` sidecars, the windows `setup.exe` triple, and the
    /// signed-manifest pair (`latest.json` + `latest.json.minisig`).
    fn raw_release(tag: &str, version: &str) -> RawRelease {
        let mut assets = Vec::new();
        for triple in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
            let base = format!("scr1b3-{triple}.tar.gz");
            assets.push(asset(&base, &format!("https://dl/{base}")));
            assets.push(asset(
                &format!("{base}.minisig"),
                &format!("https://dl/{base}.minisig"),
            ));
            assets.push(asset(
                &format!("{base}.sha256"),
                &format!("https://dl/{base}.sha256"),
            ));
        }
        let exe = format!("scr1b3-v{version}-x86_64-setup.exe");
        assets.push(asset(&exe, &format!("https://dl/{exe}")));
        assets.push(asset(
            &format!("{exe}.minisig"),
            &format!("https://dl/{exe}.minisig"),
        ));
        assets.push(asset(
            &format!("{exe}.sha256"),
            &format!("https://dl/{exe}.sha256"),
        ));
        // The signed-manifest pair.
        assets.push(asset("latest.json", "https://dl/latest.json"));
        assets.push(asset(
            "latest.json.minisig",
            "https://dl/latest.json.minisig",
        ));
        RawRelease {
            tag_name: tag.to_string(),
            prerelease: false,
            draft: false,
            html_url: "https://github.com/o/r/releases/tag/x".to_string(),
            assets,
        }
    }

    const FUTURE: &str = "2099-01-01T00:00:00Z";

    // --- pick_highest_stable (discovery only) -------------------------------

    #[test]
    fn pick_highest_stable_uses_semver_not_list_order() {
        // 0.4.10 must beat 0.4.2 (lexical vs semver) regardless of list order.
        let releases = vec![
            raw_release("v0.4.2", "0.4.2"),
            raw_release("v0.4.10", "0.4.10"),
            raw_release("v0.4.1", "0.4.1"),
        ];
        let (v, _r) = pick_highest_stable(&releases).expect("a stable release");
        assert_eq!(v, semver::Version::parse("0.4.10").unwrap());
    }

    #[test]
    fn pick_highest_stable_ignores_prerelease_and_draft() {
        let mut pre = raw_release("v0.9.0", "0.9.0");
        pre.prerelease = true;
        let mut draft = raw_release("v0.8.0", "0.8.0");
        draft.draft = true;
        let releases = vec![pre, draft, raw_release("v0.4.2", "0.4.2")];
        let (v, _r) = pick_highest_stable(&releases).unwrap();
        assert_eq!(v, semver::Version::parse("0.4.2").unwrap());
    }

    #[test]
    fn pick_highest_stable_none_on_empty_or_unparseable() {
        assert!(pick_highest_stable(&[]).is_none());
        let only_bad = vec![raw_release("not-a-version", "x")];
        assert!(pick_highest_stable(&only_bad).is_none());
    }

    // --- The fail-closed manifest requirement (THE downgrade-attack lesson) --

    #[test]
    fn manifest_absent_is_refused_fail_closed_no_install() {
        // A release that ships NO signed manifest must REFUSE the update — never
        // fall back to a non-manifest install path. This is the core Tier-1
        // invariant: there is no install path that skips the manifest.
        let mut raw = raw_release("v0.5.0", "0.5.0");
        raw.assets
            .retain(|a| a.name != "latest.json" && a.name != "latest.json.minisig");
        let err = require_manifest_assets(&raw).expect_err("absent manifest must be refused");
        assert!(
            err.contains("no signed manifest") && err.contains("refusing to install"),
            "expected a fail-closed manifest-absent refusal, got: {err}"
        );
    }

    #[test]
    fn manifest_minisig_absent_is_refused_even_when_json_present() {
        // The JSON alone is not enough — without its signature it cannot be
        // verified, so an absent `.minisig` is also a hard refusal.
        let mut raw = raw_release("v0.5.0", "0.5.0");
        raw.assets.retain(|a| a.name != "latest.json.minisig");
        assert!(require_manifest_assets(&raw).is_err());
    }

    #[test]
    fn manifest_pair_present_resolves_urls() {
        let raw = raw_release("v0.5.0", "0.5.0");
        let (json, sig) = require_manifest_assets(&raw).expect("both manifest assets present");
        assert_eq!(json, "https://dl/latest.json");
        assert_eq!(sig, "https://dl/latest.json.minisig");
    }

    // --- resolve_tier1_update gates -----------------------------------------

    #[test]
    fn resolve_available_with_pinned_sha_and_release_index() {
        let raw = raw_release("v0.5.0", "0.5.0");
        let m = tier1_manifest("0.5.0", 5000, FUTURE);
        let current = semver::Version::parse("0.4.44").unwrap();
        match resolve_tier1_update(
            &raw,
            &m,
            &current,
            "x86_64-unknown-linux-gnu",
            ".tar.gz",
            0,
            0,
        ) {
            Ok(UpdateOutcome::Available(info)) => {
                assert_eq!(info.version, semver::Version::parse("0.5.0").unwrap());
                // The download is pinned to the manifest's SIGNED linux sha.
                assert_eq!(info.pinned_sha256, "2222bbbb");
                assert_eq!(info.release_index, Some(5000));
                // The signed manifest URL is used for the archive itself.
                assert!(info
                    .asset_url
                    .ends_with("scr1b3-x86_64-unknown-linux-gnu.tar.gz"));
                // The per-asset sidecars come from the release asset list.
                assert_eq!(
                    info.sig_url,
                    "https://dl/scr1b3-x86_64-unknown-linux-gnu.tar.gz.minisig"
                );
                // Linux build offers no windows installer.
                assert!(info.installer.is_none());
            }
            other => panic!("expected Available, got {other:?}"),
        }
    }

    #[test]
    fn resolve_available_windows_pins_installer_too() {
        let raw = raw_release("v0.5.0", "0.5.0");
        let m = tier1_manifest("0.5.0", 5000, FUTURE);
        let current = semver::Version::parse("0.4.44").unwrap();
        match resolve_tier1_update(
            &raw,
            &m,
            &current,
            "x86_64-pc-windows-msvc",
            ".tar.gz",
            0,
            0,
        ) {
            Ok(UpdateOutcome::Available(info)) => {
                assert_eq!(info.pinned_sha256, "1111aaaa");
                let inst = info.installer.expect("windows installer present");
                // The installer is ALSO pinned to its signed manifest sha.
                assert_eq!(inst.pinned_sha256, "3333cccc");
                assert!(inst.url.ends_with("x86_64-setup.exe"));
                assert!(inst.sig_url.ends_with("x86_64-setup.exe.minisig"));
            }
            other => panic!("expected Available, got {other:?}"),
        }
    }

    #[test]
    fn resolve_up_to_date_when_manifest_version_not_newer() {
        let raw = raw_release("v0.4.44", "0.4.44");
        let m = tier1_manifest("0.4.44", 4044, FUTURE);
        let current = semver::Version::parse("0.4.44").unwrap();
        match resolve_tier1_update(
            &raw,
            &m,
            &current,
            "x86_64-unknown-linux-gnu",
            ".tar.gz",
            0,
            0,
        ) {
            Ok(UpdateOutcome::UpToDate { latest }) => {
                assert_eq!(latest, semver::Version::parse("0.4.44").unwrap());
            }
            other => panic!("expected UpToDate, got {other:?}"),
        }
    }

    #[test]
    fn resolve_newer_but_no_asset_for_platform() {
        // The manifest has no archive for an exotic target → NewerButNoAsset,
        // never "up to date" and never an install.
        let raw = raw_release("v0.5.0", "0.5.0");
        let m = tier1_manifest("0.5.0", 5000, FUTURE);
        let current = semver::Version::parse("0.4.44").unwrap();
        match resolve_tier1_update(&raw, &m, &current, "aarch64-apple-darwin", ".tar.gz", 0, 0) {
            Ok(UpdateOutcome::NewerButNoAsset { latest, target, .. }) => {
                assert_eq!(latest, semver::Version::parse("0.5.0").unwrap());
                assert_eq!(target, "aarch64-apple-darwin");
            }
            other => panic!("expected NewerButNoAsset, got {other:?}"),
        }
    }

    #[test]
    fn resolve_refuses_wrong_product() {
        let raw = raw_release("v0.5.0", "0.5.0");
        let mut m = tier1_manifest("0.5.0", 5000, FUTURE);
        m.product = "c0pl4nd".to_string();
        let current = semver::Version::parse("0.4.44").unwrap();
        let err = resolve_tier1_update(
            &raw,
            &m,
            &current,
            "x86_64-unknown-linux-gnu",
            ".tar.gz",
            0,
            0,
        )
        .expect_err("a foreign-product manifest must be refused");
        assert!(err.contains("different product"), "got: {err}");
    }

    #[test]
    fn resolve_refuses_unknown_schema() {
        let raw = raw_release("v0.5.0", "0.5.0");
        let mut m = tier1_manifest("0.5.0", 5000, FUTURE);
        m.schema = "evil.schema/v1".to_string();
        let current = semver::Version::parse("0.4.44").unwrap();
        let err = resolve_tier1_update(
            &raw,
            &m,
            &current,
            "x86_64-unknown-linux-gnu",
            ".tar.gz",
            0,
            0,
        )
        .expect_err("an unknown schema must be refused");
        assert!(err.contains("unrecognised manifest schema"), "got: {err}");
    }

    #[test]
    fn resolve_refuses_stale_frozen_manifest() {
        // valid_until in the past + now after it → freeze beacon tripped.
        let raw = raw_release("v0.5.0", "0.5.0");
        let m = tier1_manifest("0.5.0", 5000, "2020-01-01T00:00:00Z");
        let current = semver::Version::parse("0.4.44").unwrap();
        let now = 4_000_000_000; // well after 2020
        let err = resolve_tier1_update(
            &raw,
            &m,
            &current,
            "x86_64-unknown-linux-gnu",
            ".tar.gz",
            now,
            0,
        )
        .expect_err("a frozen manifest must be refused");
        assert!(err.contains("stale/frozen"), "got: {err}");
    }

    #[test]
    fn resolve_refuses_below_minimum_version_floor() {
        // current 0.3.0 is below the manifest minimum_version 0.4.0 → refused.
        let raw = raw_release("v0.5.0", "0.5.0");
        let m = tier1_manifest("0.5.0", 5000, FUTURE);
        let current = semver::Version::parse("0.3.0").unwrap();
        let err = resolve_tier1_update(
            &raw,
            &m,
            &current,
            "x86_64-unknown-linux-gnu",
            ".tar.gz",
            0,
            0,
        )
        .expect_err("a below-floor install must be refused");
        assert!(
            err.contains("below the manifest minimum_version"),
            "got: {err}"
        );
    }

    #[test]
    fn resolve_refuses_rollback_on_release_index() {
        // The persisted high-water index already exceeds the manifest's → a
        // replayed/superseded release is blocked even though it parses + verifies.
        let raw = raw_release("v0.5.0", "0.5.0");
        let m = tier1_manifest("0.5.0", 5000, FUTURE);
        let current = semver::Version::parse("0.4.44").unwrap();
        let err = resolve_tier1_update(
            &raw,
            &m,
            &current,
            "x86_64-unknown-linux-gnu",
            ".tar.gz",
            0,
            5000,
        )
        .expect_err("an equal/older release_index must be refused");
        assert!(err.contains("rollback blocked"), "got: {err}");
    }

    #[test]
    fn resolve_refuses_prerelease_channel() {
        let mut raw = raw_release("v0.5.0", "0.5.0");
        raw.prerelease = true;
        let m = tier1_manifest("0.5.0", 5000, FUTURE);
        let current = semver::Version::parse("0.4.44").unwrap();
        let err = resolve_tier1_update(
            &raw,
            &m,
            &current,
            "x86_64-unknown-linux-gnu",
            ".tar.gz",
            0,
            0,
        )
        .expect_err("a prerelease must be refused on the stable channel");
        assert!(err.contains("prerelease/draft"), "got: {err}");
    }

    #[test]
    fn resolve_refuses_unparseable_manifest_version() {
        let raw = raw_release("v0.5.0", "0.5.0");
        let mut m = tier1_manifest("0.5.0", 5000, FUTURE);
        m.version = "not-a-version".to_string();
        let current = semver::Version::parse("0.4.44").unwrap();
        assert!(resolve_tier1_update(
            &raw,
            &m,
            &current,
            "x86_64-unknown-linux-gnu",
            ".tar.gz",
            0,
            0
        )
        .is_err());
    }

    // --- build_tier1_release_info fail-closed cases -------------------------

    #[test]
    fn build_info_refuses_empty_sha_or_url() {
        let raw = raw_release("v0.5.0", "0.5.0");
        let cand = semver::Version::parse("0.5.0").unwrap();
        let m = tier1_manifest("0.5.0", 5000, FUTURE);

        let mut empty_sha = manifest_asset(
            "x86_64-unknown-linux-gnu",
            "tar.gz",
            "scr1b3-x86_64-unknown-linux-gnu.tar.gz",
            "",
        );
        empty_sha.sha256 = "   ".to_string();
        assert!(build_tier1_release_info(
            &raw,
            &m,
            &empty_sha,
            &cand,
            5000,
            "x86_64-unknown-linux-gnu"
        )
        .is_err());

        let mut empty_url = manifest_asset(
            "x86_64-unknown-linux-gnu",
            "tar.gz",
            "scr1b3-x86_64-unknown-linux-gnu.tar.gz",
            "2222bbbb",
        );
        empty_url.url = "".to_string();
        let err = build_tier1_release_info(
            &raw,
            &m,
            &empty_url,
            &cand,
            5000,
            "x86_64-unknown-linux-gnu",
        )
        .expect_err("empty url must be refused");
        assert!(err.contains("empty url"), "got: {err}");
    }

    #[test]
    fn build_info_refuses_missing_sidecar() {
        // A manifest archive whose per-asset `.minisig` sidecar is absent from
        // the release is a malformed release — fail-closed.
        let mut raw = raw_release("v0.5.0", "0.5.0");
        raw.assets
            .retain(|a| a.name != "scr1b3-x86_64-unknown-linux-gnu.tar.gz.minisig");
        let cand = semver::Version::parse("0.5.0").unwrap();
        let m = tier1_manifest("0.5.0", 5000, FUTURE);
        let masset = manifest_asset(
            "x86_64-unknown-linux-gnu",
            "tar.gz",
            "scr1b3-x86_64-unknown-linux-gnu.tar.gz",
            "2222bbbb",
        );
        let err =
            build_tier1_release_info(&raw, &m, &masset, &cand, 5000, "x86_64-unknown-linux-gnu")
                .expect_err("a missing sidecar must be refused");
        assert!(err.contains("missing its .minisig sidecar"), "got: {err}");
    }

    // --- resolve_expected_sha (manifest authoritative, sidecar must agree) ---

    #[test]
    fn expected_sha_agrees_case_insensitively() {
        assert_eq!(resolve_expected_sha("ABCDEF", "abcdef").unwrap(), "ABCDEF");
        assert_eq!(resolve_expected_sha("  dead  ", "dead").unwrap(), "dead");
    }

    #[test]
    fn expected_sha_disagreement_is_refused() {
        let err = resolve_expected_sha("aaaa", "bbbb")
            .expect_err("a manifest/sidecar sha disagreement must be refused");
        assert!(err.contains("sha256 disagreement"), "got: {err}");
    }

    // --- ensure_upgrade (apply-time anti-downgrade) -------------------------

    #[test]
    fn ensure_upgrade_enforces_strict_monotonic_version() {
        assert!(ensure_upgrade("v0.5.0", "0.4.9").is_ok());
        assert!(ensure_upgrade("0.4.10", "0.4.9").is_ok());
        assert!(
            ensure_upgrade("v0.4.9", "0.4.9").is_err(),
            "equal must be refused"
        );
        assert!(
            ensure_upgrade("v0.4.8", "0.4.9").is_err(),
            "older must be refused"
        );
        assert!(ensure_upgrade("0.3.0", "0.4.9").is_err());
        assert!(ensure_upgrade("not-a-version", "0.4.9").is_err());
    }

    #[test]
    fn archive_ext_is_tar_gz() {
        assert_eq!(archive_ext(), ".tar.gz");
    }

    // --- Archive extraction (decompression-bomb / tar-slip surface) ---------

    /// Build a real `.tar.gz` containing a single fake binary, then assert
    /// `extract_binary` pulls it back out.
    #[test]
    fn extract_binary_roundtrips_a_fake_binary() {
        let dir = tempfile::tempdir().unwrap();
        let bin_name = if cfg!(windows) {
            "scr1b3.exe"
        } else {
            "scr1b3"
        };
        let payload = b"#!/bin/sh\necho fake scr1b3 binary\n";

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

    /// Mutation guard for the `MAX_EXTRACTED_BINARY_BYTES` const: a 2 MiB binary
    /// (under the real 512 MiB cap, over every mutated value) must extract.
    #[test]
    fn extract_binary_accepts_a_binary_above_the_mutated_cap() {
        let dir = tempfile::tempdir().unwrap();
        let bin_name = if cfg!(windows) {
            "scr1b3.exe"
        } else {
            "scr1b3"
        };
        let payload = vec![0xABu8; 2 * 1024 * 1024];

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

        let extracted = extract_binary(&archive_bytes, dir.path())
            .expect("a 2 MiB binary is under the real cap and must extract");
        assert_eq!(fs::read(&extracted).unwrap().len(), payload.len());
    }

    #[cfg(unix)]
    #[test]
    fn extracted_binary_is_made_executable_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let payload = b"fake scr1b3";
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            builder
                .append_data(&mut header, "scr1b3", &payload[..])
                .unwrap();
            builder.finish().unwrap();
        }
        let archive_bytes = gz.finish().unwrap();
        let extracted = extract_binary(&archive_bytes, dir.path()).unwrap();
        let mode = fs::metadata(&extracted).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

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
        assert!(!dir.path().join(bin_name).exists());
    }

    #[test]
    fn extract_binary_rejects_hardlink_entry() {
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
            header.set_entry_type(tar::EntryType::Link);
            header.set_size(0);
            header.set_mode(0o777);
            builder
                .append_link(&mut header, bin_name, "scr1b3-real")
                .unwrap();
            builder.finish().unwrap();
        }
        let archive_bytes = gz.finish().unwrap();
        let err = extract_binary(&archive_bytes, dir.path()).unwrap_err();
        assert!(err.contains("non-regular"), "got: {err}");
    }

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
        assert_eq!(extracted, dir.path().join(bin_name));
        assert!(extracted.starts_with(dir.path()));
        assert!(!dir.path().join("nested").exists());
        assert_eq!(fs::read(&extracted).unwrap(), payload);
    }

    #[test]
    fn tar_builder_refuses_to_write_a_dotdot_entry() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(&mut gz);
        let mut header = tar::Header::new_gnu();
        let payload = b"x";
        header.set_size(payload.len() as u64);
        header.set_cksum();
        let res = builder.append_data(&mut header, "../../etc/evil", &payload[..]);
        assert!(res.is_err());
    }

    #[test]
    fn extract_binary_errs_when_no_binary_entry() {
        let dir = tempfile::tempdir().unwrap();
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

    #[test]
    fn extract_binary_rejects_decompression_bomb_over_size_cap() {
        let dir = tempfile::tempdir().unwrap();
        let bin_name = if cfg!(windows) {
            "scr1b3.exe"
        } else {
            "scr1b3"
        };
        let bomb_size = MAX_EXTRACTED_BINARY_BYTES + 1;

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(bomb_size);
            header.set_mode(0o755);
            header.set_cksum();
            let zeros = std::io::repeat(0u8).take(bomb_size);
            builder.append_data(&mut header, bin_name, zeros).unwrap();
            builder.finish().unwrap();
        }
        let archive_bytes = gz.finish().unwrap();
        assert!((archive_bytes.len() as u64) < MAX_EXTRACTED_BINARY_BYTES);

        let err = extract_binary(&archive_bytes, dir.path())
            .expect_err("a >cap expansion must be rejected");
        assert!(err.contains("safety cap"), "got: {err}");
        assert!(!dir.path().join(bin_name).exists());
    }

    #[test]
    fn sha_sidecar_first_token_matches_archive_digest() {
        let archive = b"pretend tarball bytes";
        let digest = sha256_hex(archive);
        let sidecar = format!("{digest}  scr1b3-x.tar.gz\n");
        let first = sidecar.split_whitespace().next().unwrap();
        assert_eq!(first, digest);
    }

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

    // ----------------------------------------------------------------------
    // Network path coverage against a hand-rolled, dependency-free mock HTTP
    // server (loopback `TcpListener`, zero new crates).
    // ----------------------------------------------------------------------

    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    struct CapturedRequest {
        start_line: String,
        headers: Vec<String>,
    }

    impl CapturedRequest {
        fn header(&self, name: &str) -> Option<String> {
            let prefix = format!("{}:", name.to_ascii_lowercase());
            self.headers
                .iter()
                .find(|h| h.to_ascii_lowercase().starts_with(&prefix))
                .map(|h| h[h.find(':').unwrap() + 1..].trim().to_string())
        }
    }

    struct OneShotServer {
        url: String,
        handle: JoinHandle<Option<CapturedRequest>>,
    }

    fn one_shot(status_line: &str, extra_headers: &[&str], body: Vec<u8>) -> OneShotServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let status_line = status_line.to_string();
        let extra: Vec<String> = extra_headers.iter().map(|s| s.to_string()).collect();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().ok()?;
            let mut reader = BufReader::new(stream.try_clone().ok()?);
            let mut start_line = String::new();
            reader.read_line(&mut start_line).ok()?;
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).ok()? == 0 {
                    break;
                }
                let trimmed = line.trim_end().to_string();
                if trimmed.is_empty() {
                    break;
                }
                headers.push(trimmed);
            }
            use std::io::Write as _;
            let head = format!(
                "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                body.len(),
                if extra.is_empty() {
                    String::new()
                } else {
                    format!("{}\r\n", extra.join("\r\n"))
                }
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
            Some(CapturedRequest {
                start_line: start_line.trim_end().to_string(),
                headers,
            })
        });
        OneShotServer {
            url: format!("http://127.0.0.1:{port}"),
            handle,
        }
    }

    impl OneShotServer {
        fn captured(self) -> CapturedRequest {
            self.handle
                .join()
                .expect("server thread panicked")
                .expect("server handled no request")
        }
    }

    #[test]
    fn fetch_releases_at_parses_a_release_list_and_sends_freshness_headers() {
        let json = br#"[
            {"tag_name":"v0.4.0","prerelease":false,"draft":false,
             "html_url":"https://github.com/o/r/releases/tag/v0.4.0","assets":[]},
            {"tag_name":"v0.3.0","prerelease":false,"draft":false,
             "html_url":"https://github.com/o/r/releases/tag/v0.3.0","assets":[]}
        ]"#
        .to_vec();
        let server = one_shot("200 OK", &[], json);
        let url = format!("{}/repos/o/r/releases?per_page=100", server.url);
        let releases = fetch_releases_at(&url).expect("parse the release list");
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].tag_name, "v0.4.0");

        let req = server.captured();
        assert!(req
            .start_line
            .starts_with("GET /repos/o/r/releases?per_page=100"));
        assert_eq!(req.header("Cache-Control").as_deref(), Some("no-cache"));
        assert_eq!(req.header("User-Agent").as_deref(), Some(USER_AGENT));
        assert_eq!(req.header("Accept").as_deref(), Some(GITHUB_ACCEPT));
        assert_eq!(
            req.header("X-GitHub-Api-Version").as_deref(),
            Some(GITHUB_API_VERSION)
        );
        assert!(!req.start_line.contains("&t=") && !req.start_line.contains("?t="));
    }

    #[test]
    fn fetch_releases_at_maps_rate_limit_403_to_a_distinct_message() {
        let server = one_shot("403 Forbidden", &[], b"rate limit exceeded".to_vec());
        let url = format!("{}/repos/o/r/releases", server.url);
        let err = fetch_releases_at(&url).expect_err("403 must be an error");
        assert!(err.to_lowercase().contains("rate limit"), "got: {err}");
        let _ = server.captured();
    }

    #[test]
    fn fetch_releases_at_errors_on_malformed_json() {
        let server = one_shot("200 OK", &[], b"this is not json".to_vec());
        let url = format!("{}/repos/o/r/releases", server.url);
        let err = fetch_releases_at(&url).expect_err("garbage body must be an error");
        assert!(err.contains("parse releases JSON"), "got: {err}");
        let _ = server.captured();
    }

    #[test]
    fn map_github_error_classifies_rate_limit_vs_generic() {
        let e429 = ureq::Error::StatusCode(429);
        assert!(map_github_error(e429).to_lowercase().contains("rate limit"));
        let e500 = ureq::Error::StatusCode(500);
        assert!(map_github_error(e500).contains("update check failed"));
    }

    #[test]
    fn download_small_returns_body_bytes() {
        let body = b"abc123-checksum-sidecar".to_vec();
        let server = one_shot("200 OK", &[], body.clone());
        let url = format!("{}/scr1b3.tar.gz.sha256", server.url);
        let got = download_small(&url).expect("download the small sidecar");
        assert_eq!(got, body);
        let req = server.captured();
        assert_eq!(req.header("User-Agent").as_deref(), Some(USER_AGENT));
    }

    #[test]
    fn download_small_capped_enforces_the_manifest_cap() {
        // A body just over an explicit small cap is rejected — the manifest
        // fetch uses MAX_MANIFEST_BYTES, so the cap parameter is load-bearing.
        let body = vec![b'm'; 4096];
        let server = one_shot("200 OK", &[], body);
        let url = format!("{}/latest.json", server.url);
        let err = download_small_capped(&url, 1024).expect_err("over-cap must be refused");
        assert!(err.contains("safety cap"), "got: {err}");
        let _ = server.captured();
    }

    #[test]
    fn download_small_accepts_a_body_above_the_mutated_cap() {
        let body = vec![b'q'; 2 * 1024 * 1024];
        let server = one_shot("200 OK", &[], body.clone());
        let url = format!("{}/big.sha256", server.url);
        let got = download_small(&url).expect("a 2 MiB body is under the real 512 MiB cap");
        assert_eq!(got.len(), body.len());
        let _ = server.captured();
    }

    #[test]
    fn download_small_errors_on_404() {
        let server = one_shot("404 Not Found", &[], b"nope".to_vec());
        let url = format!("{}/missing.sha256", server.url);
        let err = download_small(&url).expect_err("404 must be a download error");
        assert!(err.contains("download failed"), "got: {err}");
        let _ = server.captured();
    }

    #[test]
    fn download_asset_streams_and_reports_progress_total_from_content_length() {
        let body = vec![b'Z'; 200_000];
        let server = one_shot("200 OK", &[], body.clone());
        let url = format!("{}/scr1b3.tar.gz", server.url);

        let progress: std::sync::Arc<std::sync::Mutex<Vec<(u64, u64)>>> = Default::default();
        let p2 = progress.clone();
        let got = download_asset(&url, move |received, total| {
            p2.lock().unwrap().push((received, total));
        })
        .expect("download the asset");
        assert_eq!(got.len(), body.len());

        let ticks = progress.lock().unwrap();
        assert_eq!(ticks.first().copied(), Some((0, body.len() as u64)));
        assert_eq!(
            ticks.last().copied(),
            Some((body.len() as u64, body.len() as u64))
        );
        let _ = server.captured();
    }

    #[test]
    fn download_asset_errors_on_5xx() {
        let server = one_shot("503 Service Unavailable", &[], b"down".to_vec());
        let url = format!("{}/scr1b3.tar.gz", server.url);
        let err = download_asset(&url, |_, _| {}).expect_err("5xx must be a download error");
        assert!(err.contains("download failed"), "got: {err}");
        let _ = server.captured();
    }

    #[test]
    fn fetch_at_then_pick_highest_classifies_up_to_date() {
        // The fetch+discovery pipeline the worker runs (minus the hardcoded host,
        // covered by fetch_releases_at above): an only-current release is
        // UpToDate, never an error.
        let json = br#"[
            {"tag_name":"v1.0.0","prerelease":false,"draft":false,"html_url":"h","assets":[]}
        ]"#
        .to_vec();
        let server = one_shot("200 OK", &[], json);
        let url = format!("{}/repos/o/r/releases?per_page=100", server.url);
        let releases = fetch_releases_at(&url).unwrap();
        let current = semver::Version::parse("1.0.0").unwrap();
        let (latest, _r) = pick_highest_stable(&releases).expect("a stable release");
        assert!(latest <= current, "1.0.0 vs current 1.0.0 is up to date");
        let _ = server.captured();
    }

    #[test]
    fn empty_release_list_is_silent_no_update() {
        let server = one_shot("200 OK", &[], b"[]".to_vec());
        let url = format!("{}/repos/o/r/releases?per_page=100", server.url);
        let releases = fetch_releases_at(&url).expect("empty list parses");
        assert!(releases.is_empty());
        assert!(
            pick_highest_stable(&releases).is_none(),
            "an empty list yields no candidate → caller reports UpToDate"
        );
        let _ = server.captured();
    }

    #[test]
    fn download_verify_extract_wipes_staging_on_network_failure() {
        let server = one_shot("404 Not Found", &[], b"nope".to_vec());
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        let info = ReleaseInfo {
            version: semver::Version::parse("9.9.9").unwrap(),
            tag: "v9.9.9".to_string(),
            asset_url: format!("{}/scr1b3.tar.gz", server.url),
            sig_url: format!("{}/scr1b3.tar.gz.minisig", server.url),
            sha_url: format!("{}/scr1b3.tar.gz.sha256", server.url),
            html_url: "h".to_string(),
            pinned_sha256: "deadbeef".to_string(),
            release_index: Some(9_009_009),
            installer: None,
        };
        let err =
            download_verify_extract(&info, &staging, |_, _| {}).expect_err("a 404 asset must fail");
        assert!(err.contains("download failed"), "got: {err}");
        assert!(!staging.exists(), "staging must be wiped on failure");
        let _ = server.captured();
    }

    #[test]
    fn download_verify_installer_wipes_staging_on_network_failure() {
        let server = one_shot("500 Internal Server Error", &[], b"boom".to_vec());
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        let installer = InstallerAsset {
            url: format!("{}/scr1b3-setup.exe", server.url),
            sig_url: format!("{}/scr1b3-setup.exe.minisig", server.url),
            sha_url: format!("{}/scr1b3-setup.exe.sha256", server.url),
            pinned_sha256: "deadbeef".to_string(),
        };
        let err = download_verify_installer(&installer, &staging, |_, _| {})
            .expect_err("a 5xx installer download must fail");
        assert!(err.contains("download failed"), "got: {err}");
        assert!(!staging.exists(), "staging must be wiped on failure");
        let _ = server.captured();
    }

    // ---- silent-failure logging (plan: log silent supply-chain / session /
    // config / lsp failures) ----

    use crate::test_log_capture::with_captured_logs;
    use tracing::Level;

    #[test]
    fn verify_failure_kind_classifies_without_leaking_detail() {
        assert_eq!(
            verify_failure_kind("checksum mismatch"),
            "checksum-mismatch"
        );
        assert_eq!(
            verify_failure_kind("no trusted public keys configured"),
            "no-trusted-keys"
        );
        assert_eq!(verify_failure_kind("bad public key: x"), "bad-public-key");
        assert_eq!(
            verify_failure_kind("bad signature: junk"),
            "malformed-signature"
        );
        assert_eq!(
            verify_failure_kind("signature verification failed: whatever"),
            "signature-verify-failed"
        );
    }

    #[test]
    fn verify_failure_logs_error_with_kind_and_never_leaks_the_raw_detail() {
        // The supply-chain gate MUST leave a durable ERROR record on a failed
        // verification — and it must log only the coarse KIND, never the raw
        // error string (which could embed signature/key bytes).
        with_captured_logs(|logs| {
            log_verify_failure(
                "release-archive",
                "signature verification failed: SECRET_SIG_BYTES_DO_NOT_LEAK",
            );
            let records = logs.records();
            assert!(
                records.iter().any(|(lvl, text)| *lvl == Level::ERROR
                    && text.contains("artifact verification failed")
                    && text.contains("failure_kind=signature-verify-failed")
                    && text.contains("artifact=release-archive")),
                "expected an ERROR with the failure kind, got: {records:?}"
            );
            // The raw detail (a stand-in for signature bytes) must NEVER appear.
            assert!(
                !records
                    .iter()
                    .any(|(_, text)| text.contains("SECRET_SIG_BYTES_DO_NOT_LEAK")),
                "the raw verify-error detail must not be logged"
            );
        });
    }

    #[test]
    fn blocked_downgrade_emits_a_warn_with_versions_and_no_signature() {
        with_captured_logs(|logs| {
            let res = ensure_upgrade("v0.3.0", "0.4.9");
            assert!(res.is_err(), "a downgrade must be refused");
            assert!(
                logs.has(Level::WARN, "blocked downgrade/rollback"),
                "expected a WARN for the blocked downgrade, got: {:?}",
                logs.records()
            );
            // Versions are safe at warn+; assert they are present as fields.
            let warn_text = logs.warn_plus_text();
            assert!(
                warn_text.contains("attempted=0.3.0") && warn_text.contains("current=0.4.9"),
                "version fields missing from the warn record: {warn_text}"
            );
        });
    }
}
