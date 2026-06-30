#!/usr/bin/env python3
"""Self-test for scripts/doc_sync_check.py.

Two guarantees:
  (a) the checker exits 0 on the real, freshly-converged current tree;
  (b) on a tiny synthetic fixture where a doc disagrees with code, the relevant
      check reports drift and the process exits 1.

Stdlib only (unittest) -- no pytest, so CI needs no pip install. Run with:
  python -m unittest scripts.test_doc_sync_check
  python scripts/test_doc_sync_check.py
"""
from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parents[1]
CHECKER = REPO_ROOT / "scripts" / "doc_sync_check.py"


def _load_module():
    """Import doc_sync_check.py as a module for direct function calls."""
    spec = importlib.util.spec_from_file_location("doc_sync_check", CHECKER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class RealTreeTest(unittest.TestCase):
    """(a) the converged tree must pass with exit 0 (warnings allowed)."""

    def test_checker_exits_zero_on_current_tree(self):
        proc = subprocess.run(
            [sys.executable, str(CHECKER)],
            capture_output=True, text=True, cwd=str(REPO_ROOT),
        )
        self.assertEqual(
            proc.returncode, 0,
            f"doc_sync_check reported drift on the current tree:\n{proc.stdout}\n{proc.stderr}",
        )

    def test_json_mode_is_valid_and_ok(self):
        proc = subprocess.run(
            [sys.executable, str(CHECKER), "--json"],
            capture_output=True, text=True, cwd=str(REPO_ROOT),
        )
        self.assertEqual(proc.returncode, 0)
        payload = json.loads(proc.stdout)
        self.assertIs(payload["summary"]["ok"], True)
        self.assertEqual(payload["summary"]["drift_findings"], 0)
        self.assertEqual(len(payload["checks"]), len(_load_module().CHECKS))


class SyntheticDriftTest(unittest.TestCase):
    """(b) a doc that disagrees with code must produce drift; a missing anchor warns."""

    def setUp(self):
        self.mod = _load_module()
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)
        # Redirect every check's file reads into the fixture tree.
        patcher = mock.patch.object(self.mod, "ROOT", self.root)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_msrv_drift_is_detected(self):
        (self.root / "Cargo.toml").write_text(
            '[workspace.package]\nrust-version = "1.92"\n', encoding="utf-8"
        )
        # SECURITY.md states a DIFFERENT MSRV -> drift.
        (self.root / "SECURITY.md").write_text(
            'The declared MSRV is `rust-version = "1.80"` here.\n', encoding="utf-8"
        )
        (self.root / "docs").mkdir()
        (self.root / "docs" / "threat-model.md").write_text(
            "The MSRV pin (`rust-version = 1.92`) is fine.\n", encoding="utf-8"
        )
        result = self.mod.check_msrv()
        self.assertIs(result["ok"], False)
        self.assertTrue(result["drift"], "expected an MSRV drift finding")
        self.assertTrue(any("1.80" in d and "1.92" in d for d in result["drift"]))

    def test_theme_count_drift_is_detected(self):
        theme_dir = self.root / "crates" / "scribe-core" / "src"
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
        (self.root / "README.md").write_text(
            "SCR1B3 ships **99 built-in themes** today.\n", encoding="utf-8"
        )
        (self.root / "THEMING.md").write_text(
            "SCR1B3 ships **3 themes**.\n", encoding="utf-8"
        )
        result = self.mod.check_theme_count()
        self.assertIs(result["ok"], False)
        self.assertTrue(any("99" in d and "3" in d for d in result["drift"]))

    def test_missing_anchor_warns_not_fails(self):
        (self.root / "Cargo.toml").write_text(
            '[workspace.package]\nrust-version = "1.92"\n', encoding="utf-8"
        )
        (self.root / "SECURITY.md").write_text(
            "This document mentions no rust version at all.\n", encoding="utf-8"
        )
        (self.root / "docs").mkdir()
        (self.root / "docs" / "threat-model.md").write_text(
            "Also silent on the toolchain floor.\n", encoding="utf-8"
        )
        result = self.mod.check_msrv()
        self.assertIs(result["ok"], True)  # no drift
        self.assertTrue(result["warnings"], "expected a missing-anchor warning")


if __name__ == "__main__":
    unittest.main()
