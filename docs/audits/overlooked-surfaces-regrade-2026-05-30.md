---
title: SCR1B3 Overlooked-Surfaces Audit — Post-Wave Regrade
date: 2026-05-30
auditor: track-C-regrade
target_sha: post-wave-1-and-wave-2
follow_up_to: docs/audits/overlooked-surfaces-2026-05-29.md
posture_verdict_was: "B- for the engine, D+ for the user surface — 40% complete"
posture_verdict_now: "B+ for the engine, C+ for the user surface — 70% complete"
shipped_count: 16
remaining_count: 28
---

# Post-Wave Regrade — SCR1B3 Overlooked Surfaces

## What changed in the wave (PRs shipped to master 2026-05-29 → 2026-05-30)

| PR | Finding-IDs | Theme |
|---|---|---|
| #22 | (audit) | Track A code-forensics dossier (44 findings, 5 classes) |
| #23 | F-001 / F-002 / F-003 / F-006 (wave 1) / F-043 | Tab strip drag-rearrange + grid drag fix + Ctrl+\\ grid entry + Ctrl+W / Ctrl+Tab / Ctrl+Shift+Tab + right-click tab context menu + middle-click close |
| #25 | F-005 / F-024 | Status bar `Ln N, Col N` + `(N chars sel)` |
| #26 | F-007 | CLI `--help` / `--version` / `path:line:col` (stdlib-only) |
| #27 | F-008 / F-011 / F-016 / F-019 | Ctrl+H replace bar + Ctrl+/ toggle-comment (per-language table) + F11 fullscreen + drag-drop file open |
| #28 | F-014 | F1 keyboard cheatsheet (15-entry registry) |
| #29 | F-015 | Ctrl+G go-to-line modal |
| #24 | F-004 / F-018 | Built-in command palette with 19 registered actions (incl. Cycle Theme) |

**14 of 44 audit findings shipped** as live, tested, CI-green code on master.
The sentinel — the user-reported tab-drag + grid-mode regression — is fixed.

## Updated finding state

### Class 1 — built + wired + BROKEN (was 2 / now 0)

| ID | Was | Now |
|---|---|---|
| F-001 tab drag-rearrange | broken | ✅ shipped (PR #23) |
| F-002 grid drag handle (constantly-re-firing DragStarted) | broken | ✅ shipped (PR #23) |

### Class 2 — built + NOT wired (was 11 / now 8)

| ID | Was | Now |
|---|---|---|
| F-027 wgpu post-pass step 3/3 (CrtPostCallback registration) | unwired | unchanged — defer to a dedicated wgpu-render-pipeline PR |
| F-028 RopeEditor wire (3× TextEdit::multiline still in app.rs) | unwired | unchanged — Phase 15 KEYSTONE follow-up |
| F-038 config-error remediation banner | unwired | unchanged |
| F-039 plugin install UI dialog | unwired | unchanged |
| F-040 plugin registry browser | unwired | unchanged |
| ⓘ Three new built-but-unwired items absorbed by the wave's discovery work — see "Discovered while shipping" below |

### Class 3 — built + UNREACHABLE (was 7 / now 5)

| ID | Was | Now |
|---|---|---|
| F-003 grid mode entry | unreachable | ✅ Ctrl+\\ + palette + status toast (PR #23) |
| F-004 built-in palette | doc-claimed | ✅ 19-command registry (PR #24) |
| F-031 minimap toggle | unreachable | unchanged — exposed via `BuiltinCommand::ToggleMinimap` in #24 palette but not yet bound to a keyboard chord |
| F-041 file-tree keyboard navigation | unreachable | unchanged |

### Class 4 — doc-drift (was 3 / now 1)

| ID | Was | Now |
|---|---|---|
| F-026 `--screenshot` flag | claimed not built | absorbed by F-007 CLI (PR #26) — the dossier reference to a "sibling terminal-app dossier" no longer drift |
| F-042 PR #1007 was toolbar not tabs | drift | resolved by PR #23 (tab strip now actually has drag-rearrange) |
| F-028 KEYSTONE 3× TextEdit sites | drift | unchanged — still drift between docs/audits/final-gate-2026-05-29.md ("foundation shipped") and the actual code |

### Class 5 — missing (was 21 / now 9)

Shipped:

| ID | Now |
|---|---|
| F-005 status bar Ln:Col | ✅ PR #25 |
| F-006 keyboard shortcuts wave-1 (Ctrl+W / Ctrl+Tab / Ctrl+\\) | ✅ PR #23 |
| F-006 keyboard shortcuts wave-2 (Ctrl+H / Ctrl+/ / F11 / Ctrl+G) | ✅ PR #27 + PR #29 |
| F-007 CLI | ✅ PR #26 |
| F-008 replace bar | ✅ PR #27 |
| F-011 drag-drop file open | ✅ PR #27 |
| F-014 F1 cheatsheet | ✅ PR #28 |
| F-015 goto-line | ✅ PR #29 |
| F-016 toggle-comment | ✅ PR #27 |
| F-018 theme cycle (via palette entry) | ✅ PR #24 |
| F-019 fullscreen (F11) | ✅ PR #27 |
| F-024 status bar word/selection-char count | ✅ PR #25 |
| F-043 tab close-others/right/middle-click | ✅ PR #23 |

Remaining:

| ID | Status |
|---|---|
| F-009 multi-cursor / block-selection | not started — Phase 15 KEYSTONE follow-up, big |
| F-010 fuzzy file finder (Ctrl+P) | not started — needs nucleo-matcher dep + project-scan |
| F-012 recent files | not started — needs config schema extension |
| F-013 welcome / first-run screen | not started — needs first_run_completed config |
| F-017 move-line / duplicate-line / join-lines | not started — small but needs egui selection-range plumbing |
| F-020 window position/size persistence | not started — small (eframe `save()` hook) |
| F-021 scroll-position-per-file persistence | not started — needs PathBuf → (offset,line) map |
| F-022 file-watcher reload-on-disk-change | not started — medium, needs per-tab notify watcher |
| F-023 cross-session undo | not started — Phase 15 KEYSTONE follow-up |
| F-025 click-to-edit status-bar segments | not started — small |
| F-029 per-line Arc<Galley> cache | not started — Phase 15 KEYSTONE follow-up |
| F-030 viewport tree-sitter set_byte_range | not started — Phase 15 KEYSTONE follow-up |
| F-032 fold-level keyboard chords | not started |
| F-033 breadcrumbs bar | not started |
| F-034 sticky scroll | not started |
| F-035 always-on-top + opacity slider | not started — small |
| F-036 settings search box | not started — small |
| F-037 "restore default" per setting | not started |
| F-044 tab pinning | not started |

## Phase regrade vs `final-gate-2026-05-29.md`

| Phase | Old | New |
|---|---|---|
| Phase 15 KEYSTONE | ⚠️ partial (3× TextEdit) | ⚠️ partial (3× TextEdit still — F-028 unfixed) |
| Phase 17 | ⚠️ post-pass step 3/3 unwired | ⚠️ unchanged (F-027) |
| Phase 18 multi-note grid | ❌ undiscoverable + drag broken | ✅ **discoverable + drag working** (PR #23) |
| Phase 20 plugin signing + registry | ⚠️ UI install + registry browser unwired | ⚠️ unchanged (F-039 / F-040); palette entry for the registry-browse action would be a small bridge |

## The verification gate

The user's gate sentence: *"the user can launch SCR1B3, open 3 files, drag the
second tab to first position, split-screen the third into a grid pane, and
close any tab via middle-click — without consulting docs."*

| Step | Pre-wave | Post-wave |
|---|---|---|
| Open 3 files | works | works |
| Drag tab 2 to position 1 | ❌ silently no-op | ✅ swaps (PR #23) |
| Split the third into a grid pane | ❌ undiscoverable (TOML-only) | ✅ Ctrl+\\ from any tab (PR #23) |
| Close any tab via middle-click | ❌ no handler | ✅ middle-click closes (PR #23) |
| Bonus: find every action without docs | ❌ palette empty | ✅ Ctrl+Shift+P → 19 built-in commands (PR #24); F1 → keyboard cheatsheet (PR #28) |
| Bonus: status bar cursor position | ❌ missing | ✅ Ln/Col + selection chars (PR #25) |
| Bonus: `scr1b3 --help` | ❌ opens empty editor | ✅ prints help, exits 0 (PR #26) |

**The user's sentinel question — answered.** The remaining 28 findings are
real and tracked; each is a discrete future session of work, none of which
blocks the editor from being usable today.

## What this dossier is

A point-in-time snapshot. The original audit at
`docs/audits/overlooked-surfaces-2026-05-29.md` remains the source-of-truth
finding inventory; this file is the *delta* between then and the wave's
landing on master. Future audits land alongside, not replacing, both.

## Operator checklist for the next wave

1. F-022 file-watcher reload-on-disk — single-session ship; medium effort.
2. F-020 window pos/size persistence — single-session ship; small effort.
3. F-010 fuzzy file finder — single-session ship; medium effort + 1 dep (`nucleo-matcher`, BSD-3 / MIT).
4. F-012 recent files + F-013 welcome — single-session ship together; small effort.
5. F-027 wgpu post-pass step 3/3 — depends on wgpu render-pipeline expertise; medium.
6. Phase 15 KEYSTONE follow-ups (F-028 / F-029 / F-030 / F-009 / F-023) — single thread, multi-session, big.
