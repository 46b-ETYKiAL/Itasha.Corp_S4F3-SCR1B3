#!/usr/bin/env python3
"""Public-repo content-safety audit for SCR1B3.

Fails (exit 1) if any tracked text file under the product root contains an
internal reference, an absolute developer path, an internal plan-ID token, an
agent-system reference, or a secret-shaped string. This is the gate that keeps
SCR1B3 safe to publish as a standalone public repository.

Runs identically in the monorepo (apps/scribe/) and in the extracted public
repo (repo root), because it resolves the product root as this script's parent
directory and only scans that subtree.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]  # apps/scribe (or extracted repo root)
SELF = Path(__file__).resolve()

# Directories never scanned.
SKIP_DIRS = {".git", "target", "dist", "node_modules", ".github/workflows/cache"}
# Files never scanned (binary-ish or intentional-token-bearing).
SKIP_FILES = {SELF}
# Extensions we treat as text.
TEXT_EXT = {
    ".rs", ".toml", ".md", ".yml", ".yaml", ".json", ".txt", ".svg",
    ".sh", ".ps1", ".lua", ".wgsl", ".cfg", ".lock",
}

# (label, compiled pattern). Each match is a violation.
PATTERNS = [
    ("agent-system dir (.s4f3)", re.compile(r"\.s4f3\b")),
    ("agent-system dir (.claude)", re.compile(r"\.claude\b")),
    ("internal plan-ID token", re.compile(r"\bplan-\d{2,4}\b")),
    ("internal repo name", re.compile(r"S4F3-R0UT3|4RB1T3R|R0UT3-4RB1T3R")),
    ("windows dev path", re.compile(r"[A-Za-z]:\\\\Users\\\\")),
    ("windows dev path (single-slash)", re.compile(r"[A-Za-z]:\\Users\\")),
    ("home dev path", re.compile(r"/home/[a-z0-9_.-]+/")),
    ("private key block", re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")),
    ("aws access key", re.compile(r"\bAKIA[0-9A-Z]{16}\b")),
    ("generic secret assignment", re.compile(r"(?i)(secret|api[_-]?key|token|password)\s*[=:]\s*['\"][A-Za-z0-9/+]{20,}['\"]")),
]

# The bundled dictionary is a plain word list; skip secret-shape heuristics there
# (long lowercase runs would false-positive), but still scan it for paths/ids.
DICT_REL = Path("crates/scribe-core/assets/dict/en_US.txt")


def iter_text_files():
    for p in ROOT.rglob("*"):
        if not p.is_file():
            continue
        if p.resolve() in SKIP_FILES:
            continue
        rel = p.relative_to(ROOT)
        if any(part in SKIP_DIRS for part in rel.parts):
            continue
        if p.suffix.lower() in TEXT_EXT:
            yield p, rel


def main() -> int:
    violations: list[str] = []
    scanned = 0
    for path, rel in iter_text_files():
        scanned += 1
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError as e:
            print(f"WARN: cannot read {rel}: {e}", file=sys.stderr)
            continue
        is_dict = rel == DICT_REL
        for label, pat in PATTERNS:
            if is_dict and label.startswith(("generic secret", "private key", "aws")):
                continue
            for m in pat.finditer(text):
                line = text[: m.start()].count("\n") + 1
                violations.append(f"{rel}:{line}: {label}: {m.group(0)!r}")

    print(f"content-safety: scanned {scanned} files under {ROOT.name}/")
    if violations:
        print(f"\nFAIL — {len(violations)} internal-reference/secret violation(s):\n")
        for v in violations:
            print(f"  {v}")
        print("\nThis content is NOT safe for a public repository. Remove the references above.")
        return 1
    print("PASS — no internal references or secrets found. Safe to publish.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
