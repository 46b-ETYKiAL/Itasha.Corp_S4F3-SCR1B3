#!/usr/bin/env python3
"""Deterministic documentation-drift check for SCR1B3.

Verifies a small, high-value, stable set of doc<->code invariants. For each
check it extracts the authoritative value from CODE/CONFIG, extracts the value
as stated in the DOC(s), compares them, and reports any mismatch with
file:line evidence.

Design goals:
  * Conservative matching. A false CI failure that blocks PRs is worse than a
    missed item, so anchors are tight and ambiguous cases never hard-fail.
  * Missing-anchor tolerance. If a check cannot find its anchor in either code
    or doc, it emits a WARNING (not a hard failure) and continues -- the shape
    of the doc/code changed, so a human should look, but a PR is not blocked.
  * Never crash. Missing files / unreadable text degrade to warnings.

Exit codes:
  0  every check passed (warnings are allowed)
  1  one or more checks reported drift

CLI:
  python scripts/doc_sync_check.py            human-readable report
  python scripts/doc_sync_check.py --json     machine-readable JSON

Stdlib only -- no third-party dependencies.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

# Repo root = this script's parent directory's parent (scripts/ -> repo root),
# identical resolution to scripts/content_safety_audit.py.
ROOT = Path(__file__).resolve().parents[1]

# --- number-word mapping (small, deterministic) -----------------------------
_NUMWORDS = {
    0: "zero", 1: "one", 2: "two", 3: "three", 4: "four", 5: "five",
    6: "six", 7: "seven", 8: "eight", 9: "nine", 10: "ten", 11: "eleven",
    12: "twelve",
}
_WORDNUM = {w: n for n, w in _NUMWORDS.items()}


# --- low-level helpers ------------------------------------------------------
def _read(rel: str) -> str | None:
    """Read a repo-relative text file; return None if absent/unreadable."""
    p = ROOT / rel
    try:
        return p.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None


def _lineno(text: str, idx: int) -> int:
    """1-based line number of character offset ``idx`` in ``text``."""
    return text.count("\n", 0, idx) + 1


def _find(text: str, pattern: str, group: int = 1, flags: int = re.M):
    """Return (captured-value, lineno) of the first match, or (None, None)."""
    m = re.search(pattern, text, flags)
    if not m:
        return (None, None)
    return (m.group(group), _lineno(text, m.start()))


def _finding(doc: str, lineno, stated, code_value, label: str) -> str:
    loc = f"{doc}:{lineno}" if lineno else doc
    return f"{loc}: doc states {label}={stated!r} but code says {code_value!r}"


def _result(name: str, drift=None, warnings=None) -> dict:
    return {
        "name": name,
        "drift": list(drift or []),
        "warnings": list(warnings or []),
        "ok": not (drift or []),
    }


# --- code-side extractors ---------------------------------------------------
def _builtin_theme_count(text: str):
    """Count theme-name string-literals in ``Theme::builtin_names()``."""
    m = re.search(
        r"pub fn builtin_names\(\)[^{]*\{\s*&\[(.*?)\]", text, re.S
    )
    if not m:
        return None
    body = re.sub(r"//[^\n]*", "", m.group(1))  # strip line comments
    names = re.findall(r'"([a-z0-9][a-z0-9-]*)"', body)
    return len(names) if names else None


def _workspace_members(text: str):
    """Count quoted member entries in Cargo.toml ``[workspace] members``."""
    m = re.search(r"members\s*=\s*\[(.*?)\]", text, re.S)
    if not m:
        return None
    members = re.findall(r'"[^"]+"', m.group(1))
    return len(members) if members else None


# ===========================================================================
# CHECKS
# Each check returns a result dict via _result(name, drift=[...], warnings=[...]).
# ===========================================================================
def check_msrv() -> dict:
    """1. MSRV in Cargo.toml must match SECURITY.md + docs/threat-model.md."""
    name = "msrv"
    warnings, drift = [], []
    cargo = _read("Cargo.toml")
    if cargo is None:
        return _result(name, warnings=["Cargo.toml not readable"])
    code_msrv, _ = _find(
        cargo, r'rust-version\s*=\s*"([0-9]+\.[0-9]+(?:\.[0-9]+)?)"'
    )
    if code_msrv is None:
        return _result(name, warnings=["Cargo.toml: no rust-version anchor"])

    for doc in ("SECURITY.md", "docs/threat-model.md"):
        text = _read(doc)
        if text is None:
            warnings.append(f"{doc}: not readable")
            continue
        stated, ln = _find(
            text, r'rust-version\s*=\s*"?([0-9]+\.[0-9]+(?:\.[0-9]+)?)"?'
        )
        if stated is None:
            warnings.append(f"{doc}: no MSRV / rust-version anchor found")
        elif stated != code_msrv:
            drift.append(_finding(doc, ln, stated, code_msrv, "MSRV"))
    return _result(name, drift=drift, warnings=warnings)


def check_default_theme() -> dict:
    """2. Default theme in appearance.rs must match README/CONFIG/THEMING."""
    name = "default_theme"
    warnings, drift = [], []
    rs = _read("crates/scribe-core/src/config/appearance.rs")
    if rs is None:
        return _result(name, warnings=["appearance.rs not readable"])
    code_theme, _ = _find(
        rs, r'theme:\s*"([a-z0-9][a-z0-9-]*)"\.to_string\(\)'
    )
    if code_theme is None:
        return _result(name, warnings=["appearance.rs: no theme default anchor"])

    doc_patterns = {
        # README: "...the default theme is `itasha-corp`..."
        "README.md": r"default theme is\s+`([a-z0-9][a-z0-9-]*)`",
        # CONFIG table row: | `theme` | string | `"itasha-corp"` | ...
        "CONFIG.md": r'\|\s*`theme`\s*\|\s*string\s*\|\s*`"([a-z0-9][a-z0-9-]*)"`',
        # THEMING table row whose last cell starts **Default**.
        "THEMING.md": r"\|\s*`([a-z0-9][a-z0-9-]*)`\s*\|[^|\n]*\|[^|\n]*\|\s*\*\*Default\*\*",
    }
    for doc, pat in doc_patterns.items():
        text = _read(doc)
        if text is None:
            warnings.append(f"{doc}: not readable")
            continue
        stated, ln = _find(text, pat)
        if stated is None:
            warnings.append(f"{doc}: no default-theme anchor found")
        elif stated != code_theme:
            drift.append(_finding(doc, ln, stated, code_theme, "default-theme"))
    return _result(name, drift=drift, warnings=warnings)


def check_theme_count() -> dict:
    """3. Count of built-in themes in theme.rs must match README + THEMING."""
    name = "theme_count"
    warnings, drift = [], []
    rs = _read("crates/scribe-core/src/theme.rs")
    if rs is None:
        return _result(name, warnings=["theme.rs not readable"])
    code_count = _builtin_theme_count(rs)
    if code_count is None:
        return _result(name, warnings=["theme.rs: no builtin_names() registry anchor"])

    # \b before the digit avoids matching the '3' in 'SCR1B3 themes'.
    pat = re.compile(r"\b([0-9]+)\s+(?:built-in\s+)?themes?\b")
    for doc in ("README.md", "THEMING.md"):
        text = _read(doc)
        if text is None:
            warnings.append(f"{doc}: not readable")
            continue
        matches = list(pat.finditer(text))
        if not matches:
            warnings.append(f"{doc}: no 'NN themes' anchor found")
            continue
        for m in matches:
            stated = int(m.group(1))
            if stated != code_count:
                ln = _lineno(text, m.start())
                drift.append(
                    _finding(doc, ln, stated, code_count, "theme-count")
                )
    return _result(name, drift=drift, warnings=warnings)


def check_schema_version() -> dict:
    """4. CURRENT_SCHEMA_VERSION must match CONFIG.md + adr/0003."""
    name = "schema_version"
    warnings, drift = [], []
    rs = _read("crates/scribe-core/src/config/mod.rs")
    if rs is None:
        return _result(name, warnings=["config/mod.rs not readable"])
    code_v, _ = _find(rs, r"CURRENT_SCHEMA_VERSION\s*:\s*u32\s*=\s*([0-9]+)")
    if code_v is None:
        return _result(name, warnings=["config/mod.rs: no CURRENT_SCHEMA_VERSION anchor"])

    for doc in ("CONFIG.md", "docs/adr/0003-config-format.md"):
        text = _read(doc)
        if text is None:
            warnings.append(f"{doc}: not readable")
            continue
        # Accept either "CURRENT_SCHEMA_VERSION = N" or "schema ... version N".
        stated, ln = _find(text, r"CURRENT_SCHEMA_VERSION\s*=\s*([0-9]+)")
        if stated is None:
            stated, ln = _find(
                text, r"schema[^.\n]{0,40}?version[^0-9\n]{0,8}([0-9]+)", flags=re.I | re.M
            )
        if stated is None:
            warnings.append(f"{doc}: no schema-version anchor found")
        elif stated != code_v:
            drift.append(_finding(doc, ln, stated, code_v, "schema-version"))
    return _result(name, drift=drift, warnings=warnings)


def check_coverage_floor() -> dict:
    """5. coverage.yml --fail-under-lines must match adr/0007 floor."""
    name = "coverage_floor"
    warnings, drift = [], []
    cov = _read(".github/workflows/coverage.yml")
    if cov is None:
        return _result(name, warnings=["coverage.yml not readable"])
    code_floor, _ = _find(cov, r"--fail-under-lines\s+([0-9]+)")
    if code_floor is None:
        return _result(name, warnings=["coverage.yml: no --fail-under-lines anchor"])

    doc = "docs/adr/0007-coverage-exclusions-and-floor.md"
    text = _read(doc)
    if text is None:
        return _result(name, warnings=[f"{doc}: not readable"])
    # Find the line that mentions fail-under-lines and take its LAST integer
    # (the doc phrases the floor as e.g. "raised from 74 -> 85").
    stated = None
    ln = None
    for i, line in enumerate(text.splitlines(), start=1):
        if "fail-under-lines" in line:
            nums = re.findall(r"[0-9]+", line)
            if nums:
                stated = nums[-1]
                ln = i
                break
    if stated is None:
        warnings.append(f"{doc}: no fail-under-lines floor anchor found")
    elif stated != code_floor:
        drift.append(_finding(doc, ln, stated, code_floor, "coverage-floor"))
    return _result(name, drift=drift, warnings=warnings)


def check_crate_count() -> dict:
    """6. Workspace member count must match adr/0001 + CONTRIBUTING diagram."""
    name = "crate_count"
    warnings, drift = [], []
    cargo = _read("Cargo.toml")
    if cargo is None:
        return _result(name, warnings=["Cargo.toml not readable"])
    code_count = _workspace_members(cargo)
    if code_count is None:
        return _result(name, warnings=["Cargo.toml: no workspace members anchor"])

    # adr/0001 states the count as a number-word ("four crates").
    adr = "docs/adr/0001-stack-and-architecture.md"
    text = _read(adr)
    if text is None:
        warnings.append(f"{adr}: not readable")
    else:
        word_alt = "|".join(_WORDNUM.keys())
        m = re.search(rf"\b({word_alt}|[0-9]+)\s+crates?\b", text, re.I)
        if not m:
            warnings.append(f"{adr}: no 'N crates' anchor found")
        else:
            token = m.group(1).lower()
            stated = _WORDNUM.get(token, None)
            if stated is None and token.isdigit():
                stated = int(token)
            ln = _lineno(text, m.start())
            if stated != code_count:
                drift.append(
                    _finding(adr, ln, m.group(1), code_count, "crate-count")
                )

    # CONTRIBUTING layout diagram: count the scribe-* crate tree entries.
    contributing = "CONTRIBUTING.md"
    text = _read(contributing)
    if text is None:
        warnings.append(f"{contributing}: not readable")
    else:
        entries = re.findall(r"[├└]──\s+scribe-[a-z0-9-]+", text)
        if not entries:
            warnings.append(f"{contributing}: no crate tree-diagram entries found")
        elif len(entries) != code_count:
            drift.append(
                f"{contributing}: layout diagram lists {len(entries)} crate(s) "
                f"but Cargo.toml declares {code_count}"
            )
    return _result(name, drift=drift, warnings=warnings)


def check_update_endpoint() -> dict:
    """7. update/net.rs releases-list endpoint must agree with adr/0004."""
    name = "update_endpoint"
    warnings, drift = [], []
    net = _read("crates/scribe-core/src/update/net.rs")
    if net is None:
        return _result(name, warnings=["update/net.rs not readable"])
    # Does the code fetch the FULL releases list (not /releases/latest)?
    code_uses_list = bool(re.search(r"releases\?per_page=", net))
    if not code_uses_list:
        # Code shape changed; warn so a human re-checks the invariant.
        return _result(
            name,
            warnings=["update/net.rs: no 'releases?per_page=' list-endpoint anchor"],
        )

    doc = "docs/adr/0004-telemetry-free-auto-update.md"
    text = _read(doc)
    if text is None:
        return _result(name, warnings=[f"{doc}: not readable"])
    doc_documents_list = re.search(r"releases\?per_page=", text)
    if not doc_documents_list:
        ln = None
        m = re.search(r"/releases/latest", text)
        if m:
            ln = _lineno(text, m.start())
        drift.append(
            f"{doc}{':' + str(ln) if ln else ''}: code uses the releases LIST "
            f"endpoint (releases?per_page=) but the doc does not document it "
            f"(it references /releases/latest as the endpoint)"
        )
    return _result(name, drift=drift, warnings=warnings)


CHECKS = [
    check_msrv,
    check_default_theme,
    check_theme_count,
    check_schema_version,
    check_coverage_floor,
    check_crate_count,
    check_update_endpoint,
]


def run_checks() -> list[dict]:
    results = []
    for fn in CHECKS:
        try:
            results.append(fn())
        except Exception as exc:  # never crash the gate on a single check
            results.append(
                _result(fn.__name__, warnings=[f"check raised {type(exc).__name__}: {exc}"])
            )
    return results


def _summary(results: list[dict]) -> dict:
    drift = sum(len(r["drift"]) for r in results)
    warnings = sum(len(r["warnings"]) for r in results)
    failed = [r["name"] for r in results if r["drift"]]
    return {
        "checks": len(results),
        "drift_findings": drift,
        "warnings": warnings,
        "failed_checks": failed,
        "ok": drift == 0,
    }


def render_human(results: list[dict]) -> str:
    lines = ["SCR1B3 doc-sync check", "=" * 22, ""]
    for r in results:
        if r["drift"]:
            status = "DRIFT"
        elif r["warnings"]:
            status = "WARN "
        else:
            status = "OK   "
        lines.append(f"[{status}] {r['name']}")
        for d in r["drift"]:
            lines.append(f"    DRIFT: {d}")
        for w in r["warnings"]:
            lines.append(f"    warn:  {w}")
    s = _summary(results)
    lines.append("")
    lines.append(
        f"Summary: {s['checks']} checks, {s['drift_findings']} drift finding(s), "
        f"{s['warnings']} warning(s)."
    )
    if s["failed_checks"]:
        lines.append(f"Drift in: {', '.join(s['failed_checks'])}")
        lines.append("Documentation is out of sync with code -- run a doc-sync pass.")
    else:
        lines.append("All doc<->code invariants are in sync.")
    return "\n".join(lines)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="SCR1B3 documentation-drift check")
    parser.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    args = parser.parse_args(argv)

    results = run_checks()
    summary = _summary(results)

    if args.json:
        print(json.dumps({"summary": summary, "checks": results}, indent=2))
    else:
        print(render_human(results))

    return 0 if summary["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
