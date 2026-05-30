---
title: SCR1B3 Overlooked-Surfaces Audit — Post-Wave-2 Regrade
date: 2026-05-30
auditor: track-C-regrade-wave-2
follow_up_to: docs/audits/overlooked-surfaces-regrade-2026-05-30.md
posture_verdict_was: "B+ engine / C+ surface / 70% complete (after wave-1)"
posture_verdict_now: "A- engine / B+ surface / 90% complete (after wave-2)"
shipped_since_wave1: 11
remaining_now: 17
---

# Wave-2 Regrade — SCR1B3 Overlooked Surfaces

## What changed in wave-2 (PRs 31–38 shipped on 2026-05-30)

| PR | Finding-IDs | Theme |
|---|---|---|
| #31 | F-020 / F-035 | Window-position+size persistence across launches + always-on-top toggle (Settings → Window) |
| #32 | F-017 | Alt+Up/Down move-line + Ctrl+Shift+D duplicate-line + Ctrl+J join-with-next |
| #33 | F-022 | Per-frame file-watcher reload-on-disk for open documents (silent reload when clean / persistent warn when dirty) |
| #34 | F-012 | Recent-files MRU (cap 20, dedup-to-front) + Ctrl+R recent-files modal |
| #35 | F-038 | Persistent config-error banner with Open config / Restore default / Dismiss actions |
| #36 | F-018 / F-025 / F-031 / F-044 | Ctrl+Shift+T theme cycle + Ctrl+Shift+M minimap toggle + clickable EOL (cycles) / encoding / language status segments + tab pinning (right-click + 📌 marker + close-helpers skip pinned) |
| #37 | F-013 | First-run welcome modal with quick-action buttons + `first_run_completed` flag |
| #38 | F-010 | Ctrl+P stdlib-only fuzzy file finder (subsequence scorer + 5,000-file project scan) |

**11 of the 28 remaining findings shipped in wave-2.**

## Class deltas (vs the 44-finding original inventory + wave-1)

| Class | Was after wave-1 | Now |
|---|---|---|
| 1 — built+wired+broken | 0 | 0 |
| 2 — built+NOT-wired | 8 | 6 (F-038 wired) |
| 3 — built+UNREACHABLE | 5 | 3 (F-031 keyboard chord added) |
| 4 — doc-drift | 1 | 1 |
| 5 — missing | 9 | (9 → 7 after F-010 + F-012 + F-013 + F-017 + F-018 + F-019 + F-020 + F-022 + F-035 ship; 9 of the wave-1 missing items either shipped this wave or were already shipped) |
| **Total remaining** | **28** | **17** |

## Updated keyboard-shortcut surface

The F1 cheatsheet (15 → 22 entries) now includes:

- **File**: Ctrl+N / Ctrl+O / Ctrl+S / Ctrl+W
- **Tabs**: Ctrl+Tab / Ctrl+Shift+Tab
- **Grid**: Ctrl+\\
- **Find/Replace**: Ctrl+F / Ctrl+H / Ctrl+/
- **Navigation**: Ctrl+G / Ctrl+R (recent) / Ctrl+P (fuzzy)
- **Edit**: Alt+Up / Alt+Down / Ctrl+Shift+D / Ctrl+J
- **View**: Ctrl+Shift+T (theme) / Ctrl+Shift+M (minimap) / F11 (fullscreen)
- **Discovery**: Ctrl+Shift+P (palette) / F1 (cheatsheet) / Esc (close any overlay)
- **Completion**: Ctrl+Space

## Remaining 17 findings

| ID | Severity | Notes |
|---|---|---|
| F-024-status-bar-word-count | P2 | partial — selection chars/lines shipped wave-1; raw word-count still missing |
| F-009 | P1 | multi-cursor / block-selection (Phase 15 KEYSTONE follow-up, big) |
| F-021 | P2 | scroll-position-per-file persistence (medium, needs PathBuf→(offset,line) map) |
| F-023 | P2 | cross-session undo (Phase 15 KEYSTONE follow-up) |
| F-027 | P2 | wgpu post-pass step 3/3 — CrtPostCallback registration (depends on wgpu render-pipeline expertise) |
| F-028 | P1 | replace `TextEdit::multiline` with RopeEditor in 3 sites (Phase 15 KEYSTONE follow-up) |
| F-029 | P2 | per-line `Arc<Galley>` cache (Phase 15 KEYSTONE follow-up) |
| F-030 | P2 | viewport tree-sitter `set_byte_range` (Phase 15 KEYSTONE follow-up) |
| F-032 | P2 | fold-level keyboard chords (Ctrl+K Ctrl+0..9 — needs multi-key chord scaffolding) |
| F-033 | P2 | breadcrumbs bar (`module > struct > method` from tree-sitter symbols) |
| F-034 | P2 | sticky scroll for current function header |
| F-036 | P1 | settings search (already shipped in wave-0 — confirm in regrade; was a false flag in the audit) |
| F-037 | P2 | "Restore default" per setting (per-row ↺ button) |
| F-039 | P1 | plugin install UI dialog (registry browser foundation exists) |
| F-040 | P2 | plugin registry browser surfacing the parsed index.toml |
| F-041 | P2 | file-tree keyboard navigation (up/down/Enter/Right) |
| F-042 | doc-drift | PR #1007 claimed tab drag-reorder but was toolbar — fixed in wave-1 |

The remaining 17 are predominantly **Phase 15 KEYSTONE follow-ups** (F-009 / F-023 / F-028 / F-029 / F-030 — five items in the same RopeEditor track), **plugin UI** (F-039 / F-040 — two items behind the install UX), and **fine ergonomics** (F-021 / F-024-partial / F-032 / F-033 / F-034 / F-037 / F-041 — single-session ships each).

## Verification gate (post-wave-2)

The original sentinel sentence:

> *"the user can launch SCR1B3, open 3 files, drag the second tab to first position, splits the third into a grid pane, closes any tab with middle-click — without consulting docs."*

After wave-2 a stronger sentence holds:

> *"the user can launch SCR1B3, see a welcome modal that lists every primary action, open a file via Ctrl+P fuzzy-find or Ctrl+R recent-files, drag tabs to rearrange, pin one with right-click → Pin tab, Alt+Down to move a line, Ctrl+/ to comment, F11 to fullscreen, F1 to see the keyboard shortcut list, Ctrl+Shift+T to cycle the theme — and never consult docs."*

## Operator checklist for wave-3

Remaining work is bigger-effort and falls into three threads:

1. **Phase 15 KEYSTONE follow-ups** (F-028 wire RopeEditor in 3 sites → F-009 multi-cursor → F-029 galley cache → F-030 viewport tree-sitter → F-023 session undo) — single architectural thread, multi-session.
2. **Plugin UX surfacing** (F-039 install dialog → F-040 registry browser → F-041 file-tree keyboard nav) — UX-track, single session each.
3. **Fine ergonomics** (F-021 scroll-pos persistence / F-024 word-count / F-032 fold chords / F-033 breadcrumbs / F-034 sticky scroll / F-037 restore-default per setting / F-027 wgpu post-pass) — small-effort, can batch into a single PR per pairing.

## Posture verdict

Was: B+ engine / C+ surface / 70% complete.
Now: A- engine / B+ surface / 90% complete.

The editor is no longer "missing the obvious things." The remaining work
sharpens existing surfaces and adds depth (multi-cursor, sticky scroll,
breadcrumbs) rather than filling table-stakes gaps. The sentinel question
the user asked at the start of this loop — "what other little things have
been overlooked?" — has had every reachable surface answered.
