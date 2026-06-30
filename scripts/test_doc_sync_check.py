#!/usr/bin/env python3
"""Self-test for scripts/doc_sync_check.py.

Two guarantees:
  (a) the checker exits 0 on the real, freshly-converged current tree;
  (b) on a tiny synthetic fixture where a doc disagrees with code, the relevant
      check reports drift and the process exits 1.

Stdlib + pytest only.
"""
from __future__ import annotations

import importlib.util
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
CHECKER = REPO_ROOT / "scripts" / "doc_sync_check.py"


def _load_module():
    """Import doc_sync_check.py as a module for direct function calls."""
    spec = importlib.util.spec_from_file_location("doc_sync_check", CHECKER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# --- (a) real tree is in sync ----------------------------------------------
def test_checker_exits_zero_on_current_tree():
    """The converged tree must pass with exit 0 (warnings allowed)."""
    proc = subprocess.run(
        [sys.executable, str(CHECKER)],
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
    )
    assert proc.returncode == 0, (
        f"doc_sync_check reported drift on the current tree:\n{proc.stdout}\n{proc.stderr}"
    )


def test_json_mode_is_valid_and_ok():
    import json

    proc = subprocess.run(
        [sys.executable, str(CHECKER), "--json"],
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
    )
    assert proc.returncode == 0
    payload = json.loads(proc.stdout)
    assert payload["summary"]["ok"] is True
    assert payload["summary"]["drift_findings"] == 0
    assert len(payload["checks"]) == len(_load_module().CHECKS)


# --- (b) synthetic drift is detected ---------------------------------------
def test_msrv_drift_is_detected(tmp_path, monkeypatch):
    """A fake Cargo.toml + doc that disagree on MSRV must produce drift."""
    mod = _load_module()

    # Build a minimal fake tree rooted at tmp_path.
    (tmp_path / "Cargo.toml").write_text(
        '[workspace.package]\nrust-version = "1.92"\n', encoding="utf-8"
    )
    # SECURITY.md states a DIFFERENT MSRV -> drift.
    (tmp_path / "SECURITY.md").write_text(
        'The declared MSRV is `rust-version = "1.80"` here.\n', encoding="utf-8"
    )
    # threat-model lives under docs/.
    (tmp_path / "docs").mkdir()
    (tmp_path / "docs" / "threat-model.md").write_text(
        "The MSRV pin (`rust-version = 1.92`) is fine.\n", encoding="utf-8"
    )

    monkeypatch.setattr(mod, "ROOT", tmp_path)
    result = mod.check_msrv()

    assert result["ok"] is False
    assert result["drift"], "expected an MSRV drift finding"
    assert any("1.80" in d and "1.92" in d for d in result["drift"])


def test_theme_count_drift_is_detected(tmp_path, monkeypatch):
    """A doc that states the wrong number of themes must produce drift."""
    mod = _load_module()

    theme_dir = tmp_path / "crates" / "scribe-core" / "src"
    theme_dir.mkdir(parents=True)
    (theme_dir / "theme.rs").write_text(
        "pub fn builtin_names() -> &'static [&'static str] {\n"
        "    &[\n"
        '        "alpha",\n'
        '        "beta",\n'
        '        "gamma", // a comment with no string\n'
        "    ]\n"
        "}\n",
        encoding="utf-8",
    )
    # Doc claims 99 themes; code registers 3 -> drift.
    (tmp_path / "README.md").write_text(
        "SCR1B3 ships **99 built-in themes** today.\n", encoding="utf-8"
    )
    (tmp_path / "THEMING.md").write_text(
        "SCR1B3 ships **3 themes**.\n", encoding="utf-8"
    )

    monkeypatch.setattr(mod, "ROOT", tmp_path)
    result = mod.check_theme_count()

    assert result["ok"] is False
    assert any("99" in d and "3" in d for d in result["drift"])


def test_missing_anchor_warns_not_fails(tmp_path, monkeypatch):
    """A doc that lacks the anchor warns (does not hard-fail)."""
    mod = _load_module()

    (tmp_path / "Cargo.toml").write_text(
        '[workspace.package]\nrust-version = "1.92"\n', encoding="utf-8"
    )
    (tmp_path / "SECURITY.md").write_text(
        "This document mentions no rust version at all.\n", encoding="utf-8"
    )
    (tmp_path / "docs").mkdir()
    (tmp_path / "docs" / "threat-model.md").write_text(
        "Also silent on the toolchain floor.\n", encoding="utf-8"
    )

    monkeypatch.setattr(mod, "ROOT", tmp_path)
    result = mod.check_msrv()

    assert result["ok"] is True  # no drift
    assert result["warnings"], "expected a missing-anchor warning"


if __name__ == "__main__":
    sys.exit(subprocess.call([sys.executable, "-m", "pytest", __file__, "-q"]))
