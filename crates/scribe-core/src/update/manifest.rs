//! Tier-1 signed-update-manifest identity binding for the in-app self-updater.
//!
//! ## Why a signed manifest on top of per-asset signatures
//!
//! [`super::verify`] already proves each downloaded artifact's INTEGRITY
//! (SHA-256) and AUTHENTICITY (minisign against the embedded key set). The
//! signed manifest adds a layer the per-asset gates cannot: a single, signed,
//! deterministic document that binds together the release's IDENTITY — its
//! product, schema, version, a strictly-monotonic `release_index`, a
//! `minimum_version` floor, a freshness window (`valid_until_utc`, a "freeze
//! beacon"), and the expected SHA-256 of every platform asset. A MITM or a
//! compromised CDN that swaps one asset for an older-but-genuine one, replays a
//! stale listing, or freezes the user on a vulnerable version is caught here
//! even though every individual artifact still verifies on its own.
//!
//! `release.yml` emits this manifest as a signed `latest.json` (+ a
//! `latest.json.minisig` produced by the SAME signing key the client already
//! embeds), with deterministically sorted keys. The schema:
//!
//! ```json
//! { "schema":"itasha.update.manifest/v1","product":"scr1b3","version":"0.4.44",
//!   "release_index":4044,"minimum_version":"0.4.0",
//!   "published_utc":"2026-06-29T14:17:42Z","valid_until_utc":"2026-07-13T14:17:42Z",
//!   "assets":[ {"platform":"x86_64-pc-windows-msvc","kind":"zip",
//!     "asset_name":"scr1b3-x86_64-pc-windows-msvc.tar.gz",
//!     "url":"https://github.com/.../scr1b3-...tar.gz","size":8095481,
//!     "sha256":"1963210d…0eb0f510"}, ... ] }
//! ```
//!
//! `release_index = major*1_000_000 + minor*1_000 + patch` — a total order over
//! releases that the persisted high-water mark in [`super::update_state`] uses
//! as the anti-rollback floor.
//!
//! ## Fail-closed, always
//!
//! Every operation here fails CLOSED. The signature is verified over the RAW
//! JSON bytes BEFORE the manifest is parsed (an unverified manifest is NEVER
//! deserialized, let alone trusted). An unparseable date is NOT fresh. An
//! unparseable version is an error, never silently "newer". Unknown JSON fields
//! are tolerated (forward-compat with a future schema revision), but the
//! identity fields the gates read are validated by the caller
//! ([`super::net`]'s Tier-1 resolver).

use serde::Deserialize;

use super::verify;

/// The schema-id prefix every SCR1B3 update manifest carries. The Tier-1
/// resolver binds on this (plus `product`) so a manifest for a DIFFERENT
/// product or an unrecognised schema family is refused, fail-closed.
pub const MANIFEST_SCHEMA_PREFIX: &str = "itasha.update.manifest/";

/// The product id this client accepts in a manifest's `product` field.
pub const MANIFEST_PRODUCT: &str = "scr1b3";

/// One platform asset entry in the signed manifest.
///
/// `#[serde(default)]` on every field makes parsing tolerant of a partial or
/// forward-revised entry — a missing field reads as its type default rather
/// than failing the whole parse. The Tier-1 resolver validates the fields it
/// actually consumes, so a defaulted-empty critical field simply fails to match
/// (e.g. an empty `sha256` can never equal a real digest).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ManifestAsset {
    /// The Rust target triple this asset is built for (e.g.
    /// `x86_64-pc-windows-msvc`).
    #[serde(default)]
    pub platform: String,
    /// The artifact kind: `"zip"` / `"tar.gz"` (an in-place-updatable ARCHIVE)
    /// or `"exe"` (the setup installer — the self-elevating Program-Files path,
    /// NEVER an in-place archive swap). [`Manifest::archive_for`] selects only
    /// archive kinds; [`Manifest::installer_for`] selects the `exe`.
    #[serde(default)]
    pub kind: String,
    /// The asset's file name (matches the GitHub release asset name).
    #[serde(default)]
    pub asset_name: String,
    /// The asset's signed download URL.
    #[serde(default)]
    pub url: String,
    /// The asset's size in bytes (informational; the streamed download cap is
    /// the load-bearing DoS guard).
    #[serde(default)]
    pub size: u64,
    /// The asset's expected SHA-256 (lower-hex). This is the SIGNED digest the
    /// download path pins the bytes to.
    #[serde(default)]
    pub sha256: String,
}

/// The signed update manifest (`latest.json`).
///
/// Unknown fields are tolerated (no `deny_unknown_fields`) for forward
/// compatibility with a later schema revision; the identity fields the gates
/// read are validated by the caller.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    /// The manifest schema id (e.g. `itasha.update.manifest/v1`).
    #[serde(default)]
    pub schema: String,
    /// The product id (must be [`MANIFEST_PRODUCT`] for this client).
    #[serde(default)]
    pub product: String,
    /// The release version string (semver, tolerant of a leading `v`).
    #[serde(default)]
    pub version: String,
    /// `major*1_000_000 + minor*1_000 + patch` — the strictly-monotonic
    /// anti-rollback ordinal.
    #[serde(default)]
    pub release_index: u64,
    /// The lowest still-supported version (semver). The Tier-1 resolver refuses
    /// an in-place update when the running version is below this floor.
    #[serde(default)]
    pub minimum_version: String,
    /// When the manifest was published (RFC3339 / `%Y-%m-%dT%H:%M:%SZ`).
    #[serde(default)]
    pub published_utc: String,
    /// The freshness deadline (RFC3339). After this instant the manifest is a
    /// frozen/stale beacon and is refused — the anti-freeze defense.
    #[serde(default)]
    pub valid_until_utc: String,
    /// The per-platform asset list.
    #[serde(default)]
    pub assets: Vec<ManifestAsset>,
}

/// Verify a signed manifest and parse it — **signature first, ALWAYS**.
///
/// The minisign signature (`sig_str`, the `latest.json.minisig` contents) is
/// verified over the RAW `json_bytes` against the trusted `pubkeys` SET BEFORE
/// any deserialization. Passing the full [`verify::EMBEDDED_PUBLIC_KEYS`] set
/// (not a single key) keeps the manifest verification ROTATION-safe, identical
/// to the per-asset gate. An unverified manifest is never parsed — so a
/// tampered or forged `latest.json` cannot reach the serde layer (let alone the
/// gates). Fails closed: any signature OR parse error returns `Err`.
pub fn parse_and_verify(
    json_bytes: &[u8],
    sig_str: &str,
    pubkeys: &[&str],
) -> Result<Manifest, String> {
    // Cryptographic gate first — verify the signature over the exact bytes
    // against at least one trusted key (rotation-safe, fail-closed).
    verify::verify_any_signature(json_bytes, sig_str, pubkeys)?;
    // Only a verified manifest is ever deserialized.
    serde_json::from_slice::<Manifest>(json_bytes)
        .map_err(|e| format!("manifest parse failed after signature verified: {e}"))
}

/// Parse a semver string tolerating a single leading `v`. A shared helper for
/// [`Manifest::version`] and [`Manifest::minimum_version`] so both fail closed
/// identically (an unparseable version is an `Err`, never silently "newer").
fn parse_semver_lenient(s: &str) -> Result<semver::Version, String> {
    let t = s.trim();
    let t = t.strip_prefix('v').unwrap_or(t);
    semver::Version::parse(t).map_err(|e| format!("unparseable version {s:?}: {e}"))
}

impl Manifest {
    /// The release version as a [`semver::Version`] (tolerant of a leading `v`).
    /// `Err` on a malformed version — fail-closed, never treated as newer.
    pub fn version(&self) -> Result<semver::Version, String> {
        parse_semver_lenient(&self.version)
    }

    /// The `minimum_version` floor as a [`semver::Version`] (tolerant of a
    /// leading `v`). `Err` on a malformed value — fail-closed.
    pub fn minimum_version(&self) -> Result<semver::Version, String> {
        parse_semver_lenient(&self.minimum_version)
    }

    /// True iff the manifest is still FRESH at `now_unix` — i.e. `now <=`
    /// `valid_until_utc`. An unparseable / unsupported-timezone `valid_until_utc`
    /// is treated as NOT fresh (fail-closed): a manifest whose deadline cannot
    /// be read is refused rather than trusted.
    pub fn is_fresh(&self, now_unix: i64) -> bool {
        match rfc3339_to_unix(&self.valid_until_utc) {
            Some(valid_until) => now_unix <= valid_until,
            None => false,
        }
    }

    /// Select the in-place-updatable ARCHIVE asset for this `target` + `ext`.
    ///
    /// An asset matches iff: its `platform` equals `target` OR its `asset_name`
    /// contains `target` (robust to a tag prefix in the name); AND its `kind`
    /// is an archive (`"zip"` / `"tar.gz"`, NEVER `"exe"` — the setup installer
    /// is for the elevated Program-Files path, never an in-place swap); AND its
    /// `asset_name` ends with `ext` (`.tar.gz` for the SCR1B3 archive). Returns
    /// the FIRST match, or `None` when no archive asset exists for this platform.
    pub fn archive_for(&self, target: &str, ext: &str) -> Option<&ManifestAsset> {
        if target.is_empty() {
            return None; // no baked target → no asset can match this build
        }
        self.assets.iter().find(|a| {
            (a.platform == target || a.asset_name.contains(target))
                && is_archive_kind(&a.kind)
                && a.asset_name.ends_with(ext)
        })
    }

    /// Select the self-elevating Windows installer (`exe`) asset for a Windows
    /// `target`, if the manifest enumerates one. This is the SCR1B3-specific
    /// Program-Files apply path: the `setup.exe` self-elevates so it can write a
    /// protected install location an in-place swap cannot. Returns `None` for a
    /// non-Windows target or when the manifest carries no `exe` asset. The
    /// matched asset's signed `sha256` pins the installer download.
    pub fn installer_for(&self, target: &str) -> Option<&ManifestAsset> {
        if !target.contains("windows") {
            return None;
        }
        self.assets
            .iter()
            .find(|a| a.kind == "exe" && a.asset_name.ends_with("-x86_64-setup.exe"))
    }
}

/// True for the in-place-updatable archive kinds ONLY. `"exe"` (the setup
/// installer) is deliberately excluded — it is the elevated-install artifact
/// selected by [`Manifest::installer_for`], never an in-place archive swap.
fn is_archive_kind(kind: &str) -> bool {
    matches!(kind, "zip" | "tar.gz")
}

/// Convert civil `(year, month, day)` to days since the Unix epoch
/// (1970-01-01), via Howard Hinnant's well-known `days_from_civil` algorithm
/// (valid across the full proleptic Gregorian range, no external crate). `month`
/// is 1..=12 and `day` is 1..=31; callers range-check before calling.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Parse the canonical UTC RFC3339 form the manifest emits
/// (`YYYY-MM-DDTHH:MM:SSZ`) to a Unix timestamp (seconds). Tolerates a `t`/space
/// date-time separator, optional fractional seconds, and the explicit-UTC zone
/// forms (`Z`, `+00:00`, `-00:00`, or an absent zone). Returns `None` on any
/// malformed input or a non-UTC offset we do not normalize — the caller treats
/// `None` as "not fresh" (fail-closed). Pure, no external crate.
fn rfc3339_to_unix(s: &str) -> Option<i64> {
    let s = s.trim();
    let bytes = s.as_bytes();
    // Need at least "YYYY-MM-DDTHH:MM:SS" (19 chars).
    if s.len() < 19 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    if *bytes.get(4)? != b'-' {
        return None;
    }
    let month: i64 = s.get(5..7)?.parse().ok()?;
    if *bytes.get(7)? != b'-' {
        return None;
    }
    let day: i64 = s.get(8..10)?.parse().ok()?;
    match *bytes.get(10)? {
        b'T' | b't' | b' ' => {}
        _ => return None,
    }
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    if *bytes.get(13)? != b':' {
        return None;
    }
    let minute: i64 = s.get(14..16)?.parse().ok()?;
    if *bytes.get(16)? != b':' {
        return None;
    }
    let second: i64 = s.get(17..19)?.parse().ok()?;

    // Remainder: an optional `.fff…` fractional part then the zone.
    let rest = &s[19..];
    let rest = match rest.strip_prefix('.') {
        Some(frac) => {
            let end = frac
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(frac.len());
            if end == 0 {
                return None; // a bare '.' with no digits is malformed
            }
            &frac[end..]
        }
        None => rest,
    };
    // Only explicit-UTC zones are accepted; any real offset we cannot normalize
    // is refused (fail-closed) rather than silently mis-read as UTC.
    if !matches!(rest, "" | "Z" | "z" | "+00:00" | "-00:00") {
        return None;
    }

    // Range-check the civil fields. Allow a leap second (60).
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical manifest JSON fixture (matches the `release.yml` schema).
    fn fixture_json(version: &str, release_index: u64, valid_until: &str) -> String {
        format!(
            r#"{{"schema":"itasha.update.manifest/v1","product":"scr1b3","version":"{version}",
"release_index":{release_index},"minimum_version":"0.4.0",
"published_utc":"2026-06-29T14:17:42Z","valid_until_utc":"{valid_until}",
"assets":[
 {{"platform":"x86_64-pc-windows-msvc","kind":"tar.gz",
  "asset_name":"scr1b3-x86_64-pc-windows-msvc.tar.gz",
  "url":"https://github.com/o/r/releases/download/v{version}/scr1b3-x86_64-pc-windows-msvc.tar.gz",
  "size":8095481,"sha256":"1963210d0eb0f510"}},
 {{"platform":"x86_64-unknown-linux-gnu","kind":"tar.gz",
  "asset_name":"scr1b3-x86_64-unknown-linux-gnu.tar.gz",
  "url":"https://github.com/o/r/releases/download/v{version}/scr1b3-x86_64-unknown-linux-gnu.tar.gz",
  "size":7000000,"sha256":"deadbeefcafef00d"}},
 {{"platform":"x86_64-pc-windows-msvc","kind":"exe",
  "asset_name":"scr1b3-v{version}-x86_64-setup.exe",
  "url":"https://github.com/o/r/releases/download/v{version}/scr1b3-v{version}-x86_64-setup.exe",
  "size":9000000,"sha256":"00000000feedface"}}
]}}"#
        )
    }

    /// Generate a throwaway minisign keypair and SIGN `bytes`, returning
    /// `(public_key_box, signature_string)`. Mirrors the in-test signing style
    /// in `verify.rs` (the production path only VERIFIES via `minisign-verify`).
    fn sign(bytes: &[u8]) -> (String, String) {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let sig = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(bytes),
            Some("scr1b3 manifest"),
            Some("comment"),
        )
        .unwrap()
        .to_string();
        (pk_box, sig)
    }

    #[test]
    fn good_manifest_verifies_and_parses() {
        let json = fixture_json("0.4.9", 4009, "2099-01-01T00:00:00Z");
        let (pk, sig) = sign(json.as_bytes());
        let m = parse_and_verify(json.as_bytes(), &sig, &[pk.as_str()])
            .expect("a valid manifest parses");
        assert_eq!(m.schema, "itasha.update.manifest/v1");
        assert_eq!(m.product, "scr1b3");
        assert_eq!(m.release_index, 4009);
        assert_eq!(
            m.version().unwrap(),
            semver::Version::parse("0.4.9").unwrap()
        );
        assert_eq!(
            m.minimum_version().unwrap(),
            semver::Version::parse("0.4.0").unwrap()
        );
        assert_eq!(m.assets.len(), 3);
    }

    #[test]
    fn manifest_verifies_against_any_key_in_rotation_set() {
        // Rotation-safe: a manifest signed by a key that is NOT the first entry
        // must still verify as long as it is somewhere in the trust set.
        let json = fixture_json("0.4.9", 4009, "2099-01-01T00:00:00Z");
        let (pk, sig) = sign(json.as_bytes());
        let keys = [verify::EMBEDDED_PUBLIC_KEY, pk.as_str()];
        assert!(parse_and_verify(json.as_bytes(), &sig, &keys).is_ok());
    }

    #[test]
    fn bad_signature_is_rejected_before_parse() {
        // A manifest whose signature does NOT verify against the key is refused
        // — and is never parsed (the signature gate runs first).
        let json = fixture_json("0.4.9", 4009, "2099-01-01T00:00:00Z");
        let (pk, _good_sig) = sign(json.as_bytes());
        // A signature over DIFFERENT bytes (same key) must not verify this json.
        let (_pk2, sig_other) = sign(b"a different document entirely");
        let err = parse_and_verify(json.as_bytes(), &sig_other, &[pk.as_str()])
            .expect_err("a non-matching signature must be rejected");
        assert!(
            err.contains("signature verification failed") || err.contains("bad signature"),
            "expected a signature-failure error, got: {err}"
        );
    }

    #[test]
    fn tampered_json_with_valid_old_signature_is_rejected() {
        // Sign the original json, then tamper one byte: the signature no longer
        // matches and parse_and_verify fails closed (defends against an attacker
        // editing release_index/version after signing).
        let json = fixture_json("0.4.9", 4009, "2099-01-01T00:00:00Z");
        let (pk, sig) = sign(json.as_bytes());
        let mut tampered = json.into_bytes();
        // Flip a byte inside the release_index region.
        let pos = tampered
            .windows(4)
            .position(|w| w == b"4009")
            .expect("fixture contains release_index 4009");
        tampered[pos] = b'9'; // 4009 -> 9009 (a forged higher index)
        assert!(
            parse_and_verify(&tampered, &sig, &[pk.as_str()]).is_err(),
            "a tampered manifest must fail signature verification"
        );
    }

    #[test]
    fn parse_and_verify_rejects_empty_key_set() {
        // An empty trust set must never accept anything (fail-closed).
        let json = fixture_json("0.4.9", 4009, "2099-01-01T00:00:00Z");
        let (_pk, sig) = sign(json.as_bytes());
        assert!(parse_and_verify(json.as_bytes(), &sig, &[]).is_err());
    }

    #[test]
    fn is_fresh_true_before_and_false_after_valid_until() {
        let m = Manifest {
            valid_until_utc: "2026-07-13T14:17:42Z".to_string(),
            ..Default::default()
        };
        let deadline = rfc3339_to_unix("2026-07-13T14:17:42Z").unwrap();
        assert!(m.is_fresh(deadline - 1), "before the deadline is fresh");
        assert!(m.is_fresh(deadline), "exactly at the deadline is fresh");
        assert!(!m.is_fresh(deadline + 1), "after the deadline is NOT fresh");
    }

    #[test]
    fn is_fresh_false_on_unparseable_valid_until_fail_closed() {
        // An unreadable deadline is NEVER trusted — a manifest whose freshness
        // window cannot be parsed is refused (treated as not fresh).
        for bad in ["", "not-a-date", "2026-13-40T99:99:99Z", "2026-07-13"] {
            let m = Manifest {
                valid_until_utc: bad.to_string(),
                ..Default::default()
            };
            assert!(
                !m.is_fresh(0),
                "an unparseable valid_until {bad:?} must read as NOT fresh"
            );
        }
    }

    #[test]
    fn version_and_minimum_version_fail_closed_on_garbage() {
        let m = Manifest {
            version: "not-a-version".to_string(),
            minimum_version: "also-bad".to_string(),
            ..Default::default()
        };
        assert!(m.version().is_err());
        assert!(m.minimum_version().is_err());
    }

    #[test]
    fn version_tolerates_leading_v() {
        let m = Manifest {
            version: "v1.2.3".to_string(),
            minimum_version: "v1.0.0".to_string(),
            ..Default::default()
        };
        assert_eq!(
            m.version().unwrap(),
            semver::Version::parse("1.2.3").unwrap()
        );
        assert_eq!(
            m.minimum_version().unwrap(),
            semver::Version::parse("1.0.0").unwrap()
        );
    }

    #[test]
    fn archive_for_skips_exe_and_picks_the_matching_archive() {
        let json = fixture_json("0.4.9", 4009, "2099-01-01T00:00:00Z");
        let (pk, sig) = sign(json.as_bytes());
        let m = parse_and_verify(json.as_bytes(), &sig, &[pk.as_str()]).unwrap();

        // Windows: must pick the .tar.gz ARCHIVE, never the setup .exe.
        let win = m
            .archive_for("x86_64-pc-windows-msvc", ".tar.gz")
            .expect("a windows archive must be selected");
        assert_eq!(win.kind, "tar.gz");
        assert!(win.asset_name.ends_with(".tar.gz"));
        assert!(
            !win.asset_name.contains("setup"),
            "the setup installer must never be selected for in-place update"
        );

        // Linux: must pick the .tar.gz archive.
        let nix = m
            .archive_for("x86_64-unknown-linux-gnu", ".tar.gz")
            .expect("a linux tar.gz archive must be selected");
        assert_eq!(nix.kind, "tar.gz");
        assert!(nix.asset_name.ends_with(".tar.gz"));
    }

    #[test]
    fn installer_for_picks_the_exe_on_windows_only() {
        let json = fixture_json("0.4.9", 4009, "2099-01-01T00:00:00Z");
        let (pk, sig) = sign(json.as_bytes());
        let m = parse_and_verify(json.as_bytes(), &sig, &[pk.as_str()]).unwrap();

        // Windows resolves the self-elevating setup.exe with its signed sha.
        let inst = m
            .installer_for("x86_64-pc-windows-msvc")
            .expect("a windows installer exe must be selected");
        assert_eq!(inst.kind, "exe");
        assert!(inst.asset_name.ends_with("-x86_64-setup.exe"));
        assert_eq!(inst.sha256, "00000000feedface");

        // A non-Windows target never offers the installer.
        assert!(m.installer_for("x86_64-unknown-linux-gnu").is_none());
    }

    #[test]
    fn archive_for_never_selects_an_exe_even_when_only_exe_exists() {
        // A manifest whose only asset for the platform is an `exe` yields no
        // in-place archive — the updater reports "no update for this platform"
        // rather than trying to swap in a setup installer.
        let m = Manifest {
            assets: vec![ManifestAsset {
                platform: "x86_64-pc-windows-msvc".to_string(),
                kind: "exe".to_string(),
                asset_name: "scr1b3-v0.4.9-x86_64-setup.exe".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(m.archive_for("x86_64-pc-windows-msvc", ".exe").is_none());
        assert!(m.archive_for("x86_64-pc-windows-msvc", ".tar.gz").is_none());
    }

    #[test]
    fn archive_for_empty_target_never_matches() {
        let json = fixture_json("0.4.9", 4009, "2099-01-01T00:00:00Z");
        let (pk, sig) = sign(json.as_bytes());
        let m = parse_and_verify(json.as_bytes(), &sig, &[pk.as_str()]).unwrap();
        assert!(m.archive_for("", ".tar.gz").is_none());
    }

    #[test]
    fn archive_for_matches_on_asset_name_substring_when_platform_blank() {
        // Robustness: even if `platform` is empty, a name containing the target
        // triple + the right extension still matches.
        let m = Manifest {
            assets: vec![ManifestAsset {
                platform: String::new(),
                kind: "tar.gz".to_string(),
                asset_name: "scr1b3-x86_64-unknown-linux-gnu.tar.gz".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(m
            .archive_for("x86_64-unknown-linux-gnu", ".tar.gz")
            .is_some());
    }

    #[test]
    fn unknown_fields_are_tolerated_forward_compat() {
        // A future schema revision may add fields; an unverified-then-verified
        // parse must not choke on unknown keys.
        let json = r#"{"schema":"itasha.update.manifest/v2","product":"scr1b3",
"version":"0.5.0","release_index":5000,"minimum_version":"0.4.0",
"published_utc":"2026-06-29T14:17:42Z","valid_until_utc":"2099-01-01T00:00:00Z",
"assets":[],"future_field":{"nested":true},"another":42}"#;
        let (pk, sig) = sign(json.as_bytes());
        let m = parse_and_verify(json.as_bytes(), &sig, &[pk.as_str()])
            .expect("unknown fields must be tolerated");
        assert_eq!(m.release_index, 5000);
    }

    #[test]
    fn rfc3339_known_epoch_values() {
        // Anchor the date math against known Unix timestamps.
        assert_eq!(rfc3339_to_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_to_unix("2000-01-01T00:00:00Z"), Some(946_684_800));
        // The schema's own example instant (2026-07-13T14:17:42Z).
        assert_eq!(rfc3339_to_unix("2026-07-13T14:17:42Z"), Some(1_783_952_262));
    }

    #[test]
    fn rfc3339_tolerates_fraction_and_space_separator_and_offsets() {
        // Fractional seconds are skipped; a space separator and +00:00/-00:00
        // zones normalize to the same instant as the canonical `Z` form.
        let base = rfc3339_to_unix("2026-07-13T14:17:42Z").unwrap();
        assert_eq!(rfc3339_to_unix("2026-07-13T14:17:42.123Z"), Some(base));
        assert_eq!(rfc3339_to_unix("2026-07-13 14:17:42Z"), Some(base));
        assert_eq!(rfc3339_to_unix("2026-07-13T14:17:42+00:00"), Some(base));
        assert_eq!(rfc3339_to_unix("2026-07-13T14:17:42"), Some(base));
    }

    #[test]
    fn rfc3339_rejects_malformed_and_non_utc() {
        for bad in [
            "",
            "2026-07-13",
            "2026/07/13T14:17:42Z",
            "2026-13-01T00:00:00Z",      // month 13
            "2026-07-32T00:00:00Z",      // day 32
            "2026-07-13T24:00:00Z",      // hour 24
            "2026-07-13T14:60:00Z",      // minute 60
            "2026-07-13T14:17:42+05:00", // a real offset we do not normalize
            "2026-07-13T14:17:42.Z",     // bare dot, no digits
        ] {
            assert_eq!(rfc3339_to_unix(bad), None, "{bad:?} must be rejected");
        }
    }

    #[test]
    fn release_index_ordering_matches_version_ordering() {
        // Sanity on the release_index formula's intent: a higher version yields
        // a higher index (the total order the anti-rollback floor relies on).
        let idx = |maj: u64, min: u64, pat: u64| maj * 1_000_000 + min * 1_000 + pat;
        assert!(idx(0, 4, 9) > idx(0, 4, 0));
        assert!(idx(0, 5, 0) > idx(0, 4, 999));
        assert!(idx(1, 0, 0) > idx(0, 999, 999));
    }
}
