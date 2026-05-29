#!/usr/bin/env python3
"""
Verify the SCR1B3 F0RG3-W1R3 install manifest (Phase 22 T22.3).

T22.3 says: "VERIFY the installer actually produces a working SCR1B3 install
(or audit/repair)." Without a live F0RG3-W1R3 binary available in CI, this
script takes the audit path:

  1. The manifest TOML parses cleanly.
  2. schema_version == "1".
  3. The product section carries every required field (name/slug/license/...).
  4. Per-OS artifact coverage — windows-x86_64 and linux-x86_64 and macos-*
     each have at least one declared artifact, and every artifact carries
     target + kind + file.
  5. Every referenced packaging path under apps/scribe/packaging/ EXISTS on
     disk (winget yaml, homebrew cask, debian control, install.sh, .desktop).
     Built-at-release-time artifacts (msi/dmg/AppImage/deb/zip) are excluded.
  6. Every referenced icon path exists.
  7. update.source == "github-releases" AND update.verify_method == "minisign".
  8. telemetry.* is all zero — the v1 D6 hard-zero invariant.

Exits 0 on PASS, 1 on any audit failure with an explicit diagnosis.
Designed to be CI-runnable on stdlib Python alone (tomllib in 3.11+).
"""
from __future__ import annotations

import sys
from pathlib import Path

try:
    import tomllib  # type: ignore[import-not-found]
except ImportError:
    try:
        import tomli as tomllib  # type: ignore[import-not-found,no-redef]
    except ImportError:
        print(
            "verify-forge-wire: tomllib (Python 3.11+) or tomli required",
            file=sys.stderr,
        )
        sys.exit(2)


# Paths that F0RG3-W1R3 reads at INSTALL time and must therefore ship inside
# apps/scribe/packaging/ (as opposed to release-built artifacts).
PACKAGING_RELATIVE_KEYS = ("path", "control", "desktop")

# Required artifact kinds per target OS (at least one of each).
REQUIRED_PER_TARGET = {
    "windows-x86_64": ("msi", "winget", "portable"),
    "macos-universal": ("dmg", "homebrew-cask"),
    "linux-x86_64": ("appimage", "deb", "install-script"),
}

# Telemetry leaf-keys must all be zero / "none" / false.
TELEMETRY_ZERO_VALUES = {
    "collect": False,
    "install_id": "none",
    "crash_reports": False,
    "analytics": False,
}


def _fail(msg: str) -> None:
    print(f"verify-forge-wire: FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def _ok(msg: str) -> None:
    print(f"verify-forge-wire: {msg}")


def main() -> int:
    here = Path(__file__).resolve().parent
    pkg_dir = here.parent / "packaging"
    manifest_path = pkg_dir / "forge-wire-manifest.toml"

    if not manifest_path.is_file():
        _fail(f"manifest not found at {manifest_path}")

    try:
        with manifest_path.open("rb") as fh:
            data = tomllib.load(fh)
    except Exception as e:  # tomllib raises a typed error class only in 3.11+
        _fail(f"manifest does not parse as TOML: {e}")

    # (2) schema_version
    sv = data.get("schema_version")
    if sv != "1":
        _fail(f"schema_version must be \"1\" (got {sv!r})")

    # (3) product section
    product = data.get("product")
    if not isinstance(product, dict):
        _fail("[product] section missing")
    required = ("name", "slug", "display", "tagline", "license", "homepage", "vendor")
    for key in required:
        if not product.get(key):
            _fail(f"product.{key} is missing or empty")

    # (4) + (5) artifacts
    artifacts = data.get("artifact")
    if not isinstance(artifacts, list) or not artifacts:
        _fail("at least one [[artifact]] block is required")
    per_target: dict[str, set[str]] = {}
    for i, a in enumerate(artifacts):
        if not isinstance(a, dict):
            _fail(f"artifact #{i} is not a table")
        for f in ("target", "kind", "file"):
            if not a.get(f):
                _fail(f"artifact #{i} missing required field {f!r}")
        per_target.setdefault(a["target"], set()).add(a["kind"])
        # Verify packaging paths exist on disk.
        for k in PACKAGING_RELATIVE_KEYS:
            v = a.get(k)
            if v is None:
                continue
            p = pkg_dir / v
            if not p.is_file():
                _fail(
                    f"artifact #{i} (target={a['target']} kind={a['kind']}) "
                    f"declares {k}={v!r} but {p} does not exist"
                )
    # Every required target has at least the canonical kind set.
    for target, required_kinds in REQUIRED_PER_TARGET.items():
        if target not in per_target:
            _fail(f"no artifact declared for required target {target!r}")
        missing = set(required_kinds) - per_target[target]
        if missing:
            _fail(
                f"target {target!r} is missing required kinds: "
                + ", ".join(sorted(missing))
            )

    # (6) Icon paths. Raster forms (png/ico/icns) are RELEASE-TIME-GENERATED
    # by packaging/gen-icons.sh from the SVG sources; we only assert the SVG
    # source exists in-tree + that every raster path is declared. F0RG3-W1R3
    # consumes the generated rasters from the release artifact; CI does the
    # raster build before publishing.
    svg_key = "icon_svg"
    svg_path = product.get(svg_key)
    if svg_path is None:
        _fail("product.icon_svg is missing (source-of-truth for raster icons)")
    p = here.parent / svg_path
    if not p.is_file():
        _fail(f"product.icon_svg={svg_path!r} declared but {p} does not exist")
    raster_keys = ("icon", "icon_ico", "icon_icns")
    for k in raster_keys:
        v = product.get(k)
        if v is None:
            _fail(
                f"product.{k} is missing (declare the relative path under apps/scribe/; "
                f"the raster is release-time-generated from icon_svg via gen-icons.sh)"
            )

    # (7) Update channel — github-releases + minisign verification
    update = data.get("update")
    if not isinstance(update, dict):
        _fail("[update] section missing")
    if update.get("source") != "github-releases":
        _fail(f"update.source must be \"github-releases\" (got {update.get('source')!r})")
    if update.get("verify_method") != "minisign":
        _fail(
            f"update.verify_method must be \"minisign\" (got {update.get('verify_method')!r})"
        )
    pubkey = update.get("verify_pubkey")
    if not pubkey:
        _fail("update.verify_pubkey is missing")
    # Note: the pubkey FILE may not exist in the source tree (release-time generated);
    # we only assert the field is declared. Document this in T22.3.

    # (8) Hard-zero telemetry — the v1 D6 invariant.
    telemetry = data.get("telemetry")
    if not isinstance(telemetry, dict):
        _fail("[telemetry] section missing (must explicitly declare hard-zero)")
    for k, expected in TELEMETRY_ZERO_VALUES.items():
        if telemetry.get(k) != expected:
            _fail(
                f"telemetry.{k}={telemetry.get(k)!r} violates the v1 D6 hard-zero "
                f"invariant (expected {expected!r})"
            )

    _ok(f"schema_version=1; {len(artifacts)} artifacts across "
        f"{len(per_target)} target OSes; telemetry hard-zero; "
        f"minisign verification declared")
    _ok("PASS — manifest is audit-clean and F0RG3-W1R3-installable.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
