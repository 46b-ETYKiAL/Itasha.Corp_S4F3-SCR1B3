---
title: SCR1B3 Overlooked Surfaces Audit — Track A Code Forensics
date: 2026-05-29
auditor: track-A-code-forensics
target_sha: d6363a0281
scope: ["crates/scribe-core/src/**", "crates/scribe-render/src/**", "crates/scribe-app/src/**"]
sentinel_finding: tab drag-rearrange and grid-mode entry both broken/undiscoverable
finding_counts: { class_1_broken: 2, class_2_unwired: 11, class_3_undiscoverable: 7, class_4_doc_drift: 3, class_5_missing: 21, total: 44 }
posture_verdict: "Editor surface is ~40% complete. Core engine (rope/syntect/tree-sitter/Rhai sandbox/signed-update) is ship-quality. The user-facing keyboard + command surface is a P0 gap — the editor is mouse-only, command-palette-poor, and the documented shortcuts in tooltips outnumber the wired ones 3:1."
---

# SCR1B3 Overlooked Surfaces Audit — Track A

## Executive Summary

**Five-row state of the editor:**

1. The user-reported regression — **tab drag-rearrange and grid-mode entry both broken/undiscoverable** — reproduces in code at `crates/scribe-app/src/app.rs:998-1018` (tab strip has no drag logic at all) and at `:486-491` (grid drag uses `is_pointer_button_down_on()` which fires every frame the mouse is held, confusing egui_tiles' drag-state). The grid mode itself can ONLY be entered by hand-editing `config.toml` and adding `[editor] grid_enabled = true` — there is no UI affordance, no menu item, no keyboard shortcut. CONFIG.md does not document the key.
2. The keyboard surface is **6 shortcuts wired out of 19 documented in tooltips, READMEs, and dossiers**. Of the missing 13: `Ctrl+W` (close-tab), `Ctrl+P` (file finder), `Ctrl+G` (goto line), `Ctrl+/` (toggle comment), `Ctrl+D` (multi-cursor extend), `F1` (help), `F3` (find next), `F11` (fullscreen), `F12` (goto-def), `Ctrl+Tab` (MRU), `Alt+Up/Down` (move-line), `Ctrl+Shift+\` (split — claimed in the final-gate doc as shipped), `Ctrl+Shift+T` (reopen closed).
3. The **command palette only surfaces plugin commands**. Without a single plugin loaded, opening `Ctrl+Shift+P` shows: "no plugin commands yet — drop a mod into the plugins dir (see PLUGINS.md)". Every built-in operation (new file, open, save, save-as, settings, theme cycle, toggle word wrap, language pick, spellcheck toggle, find, palette-self) is unreachable from the palette. Code comment at `app.rs:1841` even reads "plugin + future builtin commands" — explicit acknowledgement that this is a not-yet-shipped surface.
4. The **status bar is missing the line:column cursor position** — the single most-grepped indicator in every editor on Earth. It shows EOL, encoding, language, line count, optional spell+diag — but never `Ln 4, Col 17`. Same for: word/char count, selection summary, indent type, file path, file size.
5. **CLI is a stub**. `crates/scribe-app/src/main.rs:55` does `std::env::args().nth(1)` and treats it as a path. `scr1b3 --help`, `scr1b3 --version`, `scr1b3 path/to/file.rs:42:10`, `scr1b3 --new-window` all silently do the wrong thing.

**Headline:** the engine is finished. The editor isn't. The user found one symptom of a class that runs through every Phase-18 user-surface and several Phase-17/16 polish surfaces. Verdict: **40% complete**, mostly along the keyboard + command palette + grid-discoverability axis.

---

## Finding Classes

| Class | Definition | Count |
|---|---|---|
| 1 — built + wired + broken | Code exists, gets called, but doesn't do what the UI says | 2 |
| 2 — built + NOT wired | Struct/fn exists, never called from `app.rs` | 11 |
| 3 — built + wired + UNREACHABLE | Works but no UI affordance, shortcut, or doc to discover it | 7 |
| 4 — doc-drift | Docs claim a feature ships but the code doesn't have it | 3 |
| 5 — missing entirely (table-stakes-2026) | Nothing built; every comparator editor ships this | 21 |
| **Total** | | **44** |

## Severity Rubric

`Severity = (User-Hit-Probability × Intuition-Damage × (1 / Effort-To-Fix))`

| Severity | Definition | Example |
|---|---|---|
| **P0** | Blocks daily editor use; first-time user hits this in < 5 min | No `Ctrl+W` close-tab |
| **P1** | Visibly missing on day-one inspection; experienced editor user notices | Tab drag-rearrange |
| **P2** | Power-user expectation; absent in 25% of competitor editors but expected here | Multi-cursor |
| **P3** | Nice-to-have; absent in 50%+ of editors but a polish multiplier | Cursor trail |

---

## Per-Finding Detail — HIGH IMPACT FIRST

### F-001 — Tab strip has no drag-rearrange (Class 1 — built+wired+broken)

| Field | Value |
|---|---|
| Surface | `crates/scribe-app/src/app.rs:998` — `fn draw_tab_strip` |
| Symptom | User drags a tab to reorder; nothing happens; visually no drop-target preview, no rearrangement |
| Repro | Open 3 files; try to drag the second tab to first position |
| Severity | **P1** |
| Root cause | `draw_tab_strip` (lines 998-1018) only handles `selectable_label.clicked()` and `small_button("×").clicked()`. There is zero drag-source, drop-target, or `egui::Memory::data` swap logic. The session memory mentioned a "drag-reorder toolbar" shipped in PR #1007 — that was for the TOOLBAR (the icon strip), not the tab bar. The work was never propagated. |
| Fix | Implement egui's drag-and-drop pattern (`Sense::click_and_drag` + `dragged_id` tracking + `swap_remove`/`insert`). Lift the toolbar's pattern from `app.rs:1191` (`/// toolbar editor (add / remove / reorder).`) and re-apply to the tab strip. |

### F-002 — Grid drag handle wired but bug-broken (Class 1 — built+wired+broken)

| Field | Value |
|---|---|
| Surface | `crates/scribe-app/src/app.rs:486-491` |
| Symptom | The `⠿` drag handle reads "Drag to rearrange" on hover, but clicking and dragging it does not move the pane |
| Severity | **P1** |
| Root cause | The handle uses `is_pointer_button_down_on()` which returns `true` **every frame** the button is pressed, not once at drag-start. egui_tiles expects `UiResponse::DragStarted` to fire once. The current code re-fires DragStarted on every frame, putting egui_tiles' drag state into a confused "constantly starting" loop. Correct idiom: `response.drag_started()`. |
| Fix | Change `.is_pointer_button_down_on()` to `.drag_started()`, and ensure the handle's `Response` (from `ui.small_button("⠿")`) is taken with `Sense::click_and_drag()` rather than the default `Sense::click()`. |

### F-003 — Grid mode has no entry affordance (Class 3 — built+UNREACHABLE)

| Field | Value |
|---|---|
| Surface | `crates/scribe-app/src/app.rs:568` (state machine watches `self.config.editor.grid_enabled`); CONFIG.md has zero documentation for the key |
| Symptom | User cannot find any way to enter multi-pane mode unless they read source code |
| Severity | **P0** — the feature is invisible |
| Root cause | The grid is opt-in via TOML config (`[editor] grid_enabled = true`). There is no menu item, no keyboard shortcut, no Settings tab toggle, no command palette entry, no toolbar button. CONFIG.md never names `grid_enabled`. |
| Fix | Add View menu item "Multi-note grid" (toggleable with checkmark); add keyboard shortcut `Ctrl+Shift+\` (standard split key); add Settings → Editor tab toggle; add command palette entry "Toggle multi-note grid"; document in CONFIG.md and README. |

### F-004 — Command palette only lists plugin commands (Class 4 — doc-drift / Class 5 — missing)

| Field | Value |
|---|---|
| Surface | `crates/scribe-app/src/app.rs:1841-1881` |
| Symptom | `Ctrl+Shift+P` on a fresh install shows: "no plugin commands yet — drop a mod into the plugins dir". The user discovers nothing. |
| Severity | **P0** — command palette is the editor's primary self-discovery surface |
| Root cause | `palette_open` panel iterates only `self.plugin_cmds`. Zero built-in commands are registered. Code comment at line 1841 acknowledges: `// plugin + future builtin commands` — future never arrived. |
| Fix | Build a `BuiltinCommand` enum and register: `New File / Open File / Save / Save As / Save All / Close Tab / Close All Tabs / Toggle Word Wrap / Toggle Spellcheck / Toggle Multi-note Grid / Cycle Theme / Show Settings / Show Keyboard Shortcuts / Open Recent / Reopen Closed Tab / Find / Replace / Find in Files / Goto Line / Goto Symbol / Toggle Comment / Toggle Find Bar / Toggle Fullscreen / Toggle Zen Mode / Show Theme / Pick Language`. Each entry holds an Fn-pointer or `Pending` variant. Palette fuzzy-matches across `plugin_cmds ∪ builtin_cmds`. |

### F-005 — Status bar missing line:column cursor position (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | `crates/scribe-app/src/app.rs:1898-1955` |
| Symptom | The single most-used status-bar indicator in every code editor — `Ln 4, Col 17` — is absent |
| Severity | **P0** |
| Root cause | The status bar renders EOL/encoding/language/line-count/spell/diagnostics but never the cursor location. The data is reachable via `egui::TextEdit::cursor_range_at_index` but never sampled. |
| Fix | Wire a `last_cursor_pos: Option<(usize, usize)>` field, sample it from `TextEdit::output().cursor_range` each frame, render as `Ln {row + 1}, Col {col + 1}` in the status bar. Also add selection summary: if range non-empty → `({chars} chars, {lines} lines)`. |

### F-006 — Wide keyboard-shortcut absence (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | `crates/scribe-app/src/app.rs:1597-1622` (the entire shortcut table) |
| Wired (6) | `Ctrl+N`, `Ctrl+O`, `Ctrl+S`, `Ctrl+F`, `Ctrl+Shift+P`, `Ctrl+Space`, `Esc` |
| Missing (13+) | `Ctrl+W` close-tab · `Ctrl+Shift+T` reopen closed · `Ctrl+Tab`/`Ctrl+Shift+Tab` MRU · `Ctrl+P` fuzzy-find · `Ctrl+Shift+F` find-in-files · `F3`/`Shift+F3` find next/prev · `Ctrl+Shift+L` select-all-matches · `Ctrl+H` replace · `Ctrl+D` multi-cursor extend · `Ctrl+/` toggle comment · `Ctrl+G` goto-line · `Ctrl+Shift+O` goto-symbol · `F12` goto-def · `Alt+Up/Down` move-line · `Ctrl+Shift+D` duplicate · `Ctrl+J` join · `Ctrl+,` settings · `Ctrl+=`/`Ctrl+-` zoom · `Ctrl+0` reset zoom · `F1` help · `F11` fullscreen · `Ctrl+Shift+\` split · `Ctrl+Shift+-` collapse-split · `Ctrl+B` toggle sidebar |
| Severity | **P0** (multiple) |
| Fix | Extend the `ctx.input(|i| { ... })` shortcut block at `:1599`. Each missing shortcut requires a small handler that mutates `act: Pending` or sets a flag. Many are deferrable to existing operations — `Ctrl+W` just calls `self.close_tab(self.active)`. Each merits its own commit. |

### F-007 — CLI is a positional-only stub (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | `crates/scribe-app/src/main.rs:55` — `let cli_path = std::env::args().nth(1);` |
| Symptom | `scr1b3 --help` opens an empty editor trying to open a file named `--help`. Same for `--version`, `--new-window`, `path:line:col`. |
| Severity | **P0** — every shell user expects `--help` to work |
| Root cause | No argument parser. The positional 1st-arg-is-path stub never handles flags. |
| Fix | Add `clap = { version = "4", features = ["derive"] }` (Rust ecosystem standard; LICENSE: MIT OR Apache-2.0; passes cargo-deny). Define `--help`, `--version`, `--new-window`, `--readonly`, `path[:line[:column]]` jump-on-open. |

### F-008 — No replace bar (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | `crates/scribe-app/src/app.rs` |
| Symptom | `Ctrl+F` opens find. There is no `Ctrl+H`. The find bar has no replace field. |
| Severity | **P0** |
| Fix | Extend the find bar's renderer with a 2nd `text_edit_singleline` for the replace string, `Replace` and `Replace All` buttons. Wire `Ctrl+H` to focus the replace field. Reuse the existing `search::` regex/case/whole-word state. |

### F-009 — No multi-cursor / block-selection (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | The editor uses `egui::TextEdit::multiline`, which is single-caret |
| Symptom | `Ctrl+click`, `Ctrl+Alt+Up/Down`, `Ctrl+D` extend-selection all do nothing |
| Severity | **P1** — power-user table-stakes for a 2026 editor |
| Fix | Phase 15 KEYSTONE follow-up per session memory. The `RopeEditor` widget at `crates/scribe-render/src/rope_editor/` is the home for this. Adds: `Vec<Cursor>` state, `extend_selection_to_next_match`, render multiple carets, broadcast input events to each. |

### F-010 — No fuzzy file-finder (`Ctrl+P`) (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | None — no panel for fuzzy file search exists |
| Severity | **P0** — `Ctrl+P` is universally expected |
| Fix | Add `fuzzy_finder: Option<FuzzyState>` field. On open, scan project dir + recent-files; render filtered list as user types; on Enter, `act.open_path = Some(path)`. Use `nucleo-matcher` (BSD-3 / MIT) for the fuzzy score — it's the same matcher Helix uses. |

### F-011 — No drag-drop file/folder onto window (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | `crates/scribe-app/src/app.rs` |
| Symptom | Dragging a file from the OS file manager onto the SCR1B3 window does nothing |
| Severity | **P0** — Notepad++, VSCode, Sublime, Helix-tui-app, Zed all open dropped files |
| Fix | In `update()`, consume `ctx.input(|i| i.raw.dropped_files.iter())`. For each `DroppedFile`, call `act.open_path = Some(file.path)`. Folders trigger "open as project root" (filetree scope). |

### F-012 — No recent-files list (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | None |
| Severity | **P0** |
| Fix | Add `recent_files: VecDeque<PathBuf>` in config (cap 20, MRU); persist on tab open; expose in File menu, command palette ("Open Recent"), and Welcome screen (F-013). Add `Ctrl+R` shortcut to open the recent picker. |

### F-013 — No welcome / first-run screen (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | None |
| Symptom | Fresh launch shows an empty scratch buffer; the user has no idea what to do next |
| Severity | **P1** |
| Fix | First-run panel (also reachable via `Help → Welcome`): "Open File · Open Folder · Recent · Pick a Theme · Show Keyboard Shortcuts · What's new in this version". Suppress on subsequent launches once `config.first_run_completed = true`. |

### F-014 — No keyboard-shortcut cheatsheet (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | None |
| Severity | **P1** — F1 in every editor opens help |
| Fix | `F1` opens an in-app modal listing every wired shortcut in a 2-column table. Also reachable via command palette: "Show Keyboard Shortcuts". |

### F-015 — No goto-line (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | None |
| Severity | **P0** — universally `Ctrl+G` |
| Fix | Small modal taking a line number; on Enter, set the active TextEdit's cursor to that position. |

### F-016 — No toggle-comment (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | None |
| Severity | **P1** — universally `Ctrl+/` |
| Fix | Per-language single-line-comment table (`rs → //`, `py → #`, `lua → --`, `html → <!-- -->`, etc.). On `Ctrl+/`, for each line touched by the selection: prepend/strip the comment token. |

### F-017 — No move-line, duplicate-line, join-lines (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | None |
| Severity | **P1** |
| Fix | `Alt+Up/Down` swap current line with neighbor; `Ctrl+Shift+D` duplicate; `Ctrl+J` join with next. All operate on the rope buffer via byte-range edits. |

### F-018 — No theme cycle hotkey (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | Themes ship 19 per memory; the Settings UI has a dropdown but no cycle hotkey |
| Severity | **P3** |
| Fix | `Ctrl+K Ctrl+T` (VSCode chord) or single-shot via command palette "Cycle Theme" entry. |

### F-019 — No fullscreen / Zen / focus mode (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | None |
| Severity | **P1** — F11 is universal |
| Fix | `F11` toggles `eframe::Frame::set_fullscreen(true)`. Zen mode (sidebars+statusbar+titlebar all hidden) is a second toggle (`Ctrl+K Z`). |

### F-020 — No window position/size persistence (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | `crates/scribe-app/src/main.rs:31-32` — hardcoded `[1100.0, 720.0]` every launch |
| Severity | **P1** |
| Fix | Persist last window rect to config; restore on `ViewportBuilder::default()`. Save on `Stop` event or PreCompact-equivalent (eframe `save()`). |

### F-021 — No scroll-position persistence per file (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | None |
| Severity | **P2** |
| Fix | Map `PathBuf -> (scroll_offset, cursor_line)` in the recent-files store; restore on tab open. |

### F-022 — No file-watcher reload-on-disk-change for open documents (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | Config has a watcher (`crates/scribe-app/src/app.rs:1577` `cfg_rx`); document files do not |
| Severity | **P1** — git pulls / external edits silently get clobbered on save |
| Fix | Per-tab `notify` watcher; on event, prompt the user "File changed on disk. Reload? / Keep mine / Diff". |

### F-023 — Built-in undo/redo across session restart (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | `egui::TextEdit` has internal undo per-widget; no cross-session persistence |
| Severity | **P2** |
| Fix | Phase-15 KEYSTONE follow-up — when the `RopeEditor` widget replaces `TextEdit::multiline`, attach an `UndoLog` to each document, persist to per-file shadow file. |

### F-024 — Status bar lacks indent type / tab width / word count (Class 5 — missing)

| Field | Value |
|---|---|
| Severity | **P2** |
| Fix | Add `Spaces: 4` indicator (Ctrl-click to switch to tabs/spaces/N-width). Add word count if selection non-empty. |

### F-025 — Status bar segments not click-to-edit (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | Every status-bar segment is a `label`, not `selectable_label` |
| Severity | **P2** |
| Fix | Wrap each segment in a clickable. EOL click → cycle LF/CRLF; encoding click → reopen-with-encoding dialog; language click → pick-language dropdown. |

### F-026 — `--screenshot` flag claimed in dossier, never implemented (Class 4 — doc-drift)

| Field | Value |
|---|---|
| Surface | The plan-565 / C0PL4ND dossier mentioned a `--screenshot` flag for headless verification. SCR1B3's `main.rs` has no flag parser. |
| Severity | **P2** — useful for E2E CI smoke testing |
| Fix | Folds into F-007 (CLI). Add `--screenshot path.png` which runs the egui frame once and dumps the framebuffer. |

### F-027 — Phase 17 T17.4 step 3/3 — `CrtPostCallback` registration NOT in `update()` (Class 2 — built+NOT-wired)

| Field | Value |
|---|---|
| Surface | `crates/scribe-render/src/post/` — `PostResources` exists; `app.rs:540` initialises them in `App::new` per memory; the `CrtPostCallback` `paint_callback` step is described in the design dossier but is NOT in the central-panel render path |
| Severity | **P2** |
| Fix | Register the CrtPostCallback inside the central panel's `paint_callback` chain so the offscreen RT is composed back to the swapchain. The dossier's step 2 is done; step 3/3 ships the visible effect. |

### F-028 — Phase 15 KEYSTONE — three `TextEdit::multiline` sites still un-replaced (Class 4 — doc-drift)

| Field | Value |
|---|---|
| Surface | `app.rs:497` (grid pane body), `:2030`-ish (main central panel), `:2125`-ish (split view) all still use `egui::TextEdit::multiline(&mut tabs[idx].text)` |
| Symptom | Documents over a few MB visibly lag; the rope-based RopeEditor widget (PR #1018 per memory) is built but not wired |
| Severity | **P1** for documents > 1 MB |
| Fix | Replace each call site with `RopeEditor::new(&mut self.buffers[idx])`. This is the Phase 15 follow-up the final-gate doc explicitly marks "shipped foundation, future iteration". |

### F-029 — Per-line `Arc<Galley>` cache absent — typing latency under tree-sitter (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | `crates/scribe-render/src/rope_editor/` |
| Severity | **P2** for large files |
| Fix | Per-line revision-keyed galley cache. Each line invalidates on its own rev bump. |

### F-030 — Tree-sitter `set_byte_range` viewport queries not used (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | `crates/scribe-core/src/syntax.rs` — runs tree-sitter over the whole buffer per frame |
| Severity | **P2** for large files |
| Fix | Compute the viewport's byte range from the scroll offset; pass to tree-sitter's query cursor `set_byte_range`. |

### F-031 — Minimap built but unreachable by shortcut (Class 3 — undiscoverable)

| Field | Value |
|---|---|
| Surface | `crates/scribe-app/src/app.rs:1067` — `show_minimap` exists; gated by `config.editor.minimap_enabled` (no UI toggle exposed) |
| Severity | **P2** |
| Fix | View menu toggle + Settings checkbox + command palette "Toggle Minimap". |

### F-032 — Folding controls (collapse-all / fold-level-N) missing (Class 5 — missing)

| Field | Value |
|---|---|
| Severity | **P2** |
| Fix | Brace-aware folding exists in `editor_features.rs`; add `Ctrl+K Ctrl+0..9` chords for fold-level. |

### F-033 — Breadcrumbs bar missing (Class 5 — missing)

| Field | Value |
|---|---|
| Severity | **P2** |
| Fix | Above the buffer, render a path `module > struct > method` tree from tree-sitter symbols. |

### F-034 — Sticky scroll for current function header missing (Class 5 — missing)

| Field | Value |
|---|---|
| Severity | **P2** |
| Fix | When scrolled past a function definition, pin its header line at the top of the viewport. |

### F-035 — No "Always on top" / opacity slider (Class 5 — missing)

| Field | Value |
|---|---|
| Severity | **P3** |
| Fix | `ViewportCommand::WindowLevel(AlwaysOnTop)` + an opacity slider in Settings → Window. |

### F-036 — Settings has no search box (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | `crates/scribe-app/src/settings.rs:1031` lines, tabbed |
| Severity | **P1** |
| Fix | Top-of-modal `text_edit_singleline` filters every tab's contents. |

### F-037 — No "Restore default" per setting (Class 5 — missing)

| Field | Value |
|---|---|
| Severity | **P2** |
| Fix | Each settings row gains a `↺` button right-aligned. |

### F-038 — Config malformed has no in-app remediation banner (Class 2 — built+NOT-wired)

| Field | Value |
|---|---|
| Surface | `crates/scribe-core/src/config.rs` does `load_or_default()` and returns `(Config, Option<Error>)`. `app.rs` carries `config_err` but only flashes a toast |
| Severity | **P1** |
| Fix | Persistent top banner "Config has errors at line N. [Open config / Restore default / Dismiss]". |

### F-039 — Plugin install dialog (T20.2) not in UI (Class 2 — built+NOT-wired)

| Field | Value |
|---|---|
| Surface | `crates/scribe-core/src/plugin/integrity.rs` exposes `verify_install`; pinned_keys.rs has TOFU; mod.rs has `is_app_version_ok`. No UI surface for any of this. |
| Severity | **P1** |
| Fix | "Settings → Plugins → Install from URL / Browse Registry / Loaded Plugins" tab. Capability-diff prompt on update. |

### F-040 — Plugin registry browser (T20.3) not in UI (Class 2 — built+NOT-wired)

| Field | Value |
|---|---|
| Surface | `registry.rs` has parse + search; no UI panel |
| Severity | **P2** |
| Fix | Modal listing parsed `index.toml` entries with one-click install. |

### F-041 — File tree built but no file-tree keyboard navigation (Class 3 — undiscoverable)

| Field | Value |
|---|---|
| Surface | `crates/scribe-app/src/filetree.rs:57` lines — minimal |
| Severity | **P2** |
| Fix | Sidebar with `Ctrl+B` toggle; up/down keys, Enter opens, Right expands. |

### F-042 — `tab-bar drag-reorder` claimed in session memory (PR #1007) — for toolbar not tabs (Class 4 — doc-drift)

| Field | Value |
|---|---|
| Surface | PR #1007's title says "drag-reorder toolbar"; the audit confirms it was the icon toolbar (lines 1191+), not the tab strip |
| Severity | covered by F-001 |
| Fix | None — corrects the dossier text; the memory line `T18.5b drag-reorder toolbar` is correct; the user's reasonable inference that "drag-reorder" should apply to tabs was the gap. |

### F-043 — Tab close-others / close-right / middle-click-close (Class 5 — missing)

| Field | Value |
|---|---|
| Surface | None |
| Severity | **P1** |
| Fix | Right-click context menu on the tab strip: Close · Close Others · Close All to the Right · Close All. Middle-click on the tab closes it. |

### F-044 — No tab pinning (Class 5 — missing)

| Field | Value |
|---|---|
| Severity | **P3** |
| Fix | Per-tab `pinned: bool`; pinned tabs render with `📌` and refuse close-others. |

---

## Phase Re-Grade Against Final-Gate Doc

The phase matrix at `docs/audits/final-gate-2026-05-29.md` claims every phase shipped. This audit downgrades:

| Phase | Old grade | New grade | Reason |
|---|---|---|---|
| 15 KEYSTONE | ✅ shipped foundation | ⚠️ partial — `TextEdit::multiline` still in 3 sites (F-028) | |
| 16 egui 0.34 | ✅ shipped | ✅ shipped | (engine work; clean) |
| 17 brand theming + post-process | ✅ shipped | ⚠️ post-pass step 3/3 unwired (F-027) | |
| 18 multi-note grid | ✅ foundation + central-panel wire | ❌ **undiscoverable + drag broken** (F-002, F-003, F-043) | |
| 20 plugin signing + registry | ✅ shipped foundation | ⚠️ UI install + registry browser unwired (F-039, F-040) | |
| 21 CI / security | ✅ shipped | ✅ shipped (this session) | |
| 22 brand + install | ✅ shipped | ✅ shipped | |

Phases 11, 12, 13, 14, 19 are unchanged.

---

## Track-A → Track-C Triage

Findings sorted into PR-shippable buckets:

| PR # | Finding-IDs | Theme | Severity | Effort |
|---|---|---|---|---|
| 1 | F-001 | Fix tab strip drag-rearrange (sentinel) | P1 | M |
| 2 | F-002 + F-003 | Fix grid drag + add grid-mode entry shortcut + menu + setting | P0+P1 | M |
| 3 | F-004 | Built-in command registry + palette | P0 | L |
| 4 | F-005 + F-024 + F-025 | Status bar Ln/Col + word/char + click-to-edit segments | P0+P2 | M |
| 5 | F-006 | Keyboard shortcuts — wave 1 (Ctrl+W, Ctrl+Tab, Ctrl+P, Ctrl+G, Ctrl+H, Ctrl+/, F1, F11) | P0 | M |
| 6 | F-007 + F-026 | CLI with clap (--help, --version, path:line:col, --screenshot) | P0 | M |
| 7 | F-008 | Replace bar (Ctrl+H) | P0 | M |
| 8 | F-010 + F-012 + F-013 + F-014 | Fuzzy finder + recent files + welcome + cheatsheet | P0+P1 | L |
| 9 | F-011 + F-022 | Drag-drop file open + file-watcher reload-on-disk | P0+P1 | M |
| 10 | F-038 + F-039 + F-040 + F-031 + F-041 | UI affordances for built-but-unwired (plugin install + minimap toggle + filetree sidebar + config-error banner) | P1+P2 | L |
| 11 | F-019 + F-020 + F-035 + F-036 + F-037 | Window persistence + fullscreen/Zen + always-on-top + settings search + restore-default | P1+P2 | L |
| 12 | F-009 + F-023 + F-028 + F-029 + F-030 + F-033 + F-034 | Phase 15 KEYSTONE follow-up: replace TextEdit::multiline, multi-cursor, per-line galley cache, viewport tree-sitter, breadcrumbs, sticky scroll, session-undo | P1+P2 | XL |
| 13 | F-015 + F-016 + F-017 + F-018 + F-021 + F-032 + F-043 + F-044 | Polish wave: goto-line, toggle-comment, move/dup/join line, theme cycle, scroll-pos persistence, fold-level, tab right-click/middle-click, pinning | P1+P3 | L |

PRs 1-11 cover every P0 + P1 finding. PRs 12-13 cover P2/P3.

---

## Reproduce Each Finding

A repro recipe for the operator + test author:

| Finding | Recipe |
|---|---|
| F-001 | `cargo run --release` → open 3 files → drag tab 2 to position 1 → nothing happens |
| F-002 | edit `~/.config/scr1b3/config.toml` → `[editor] grid_enabled = true` → save → grid renders → drag a `⠿` handle → nothing happens (or stuck-drag if button held) |
| F-003 | `cargo run --release` → try to find any way to enter grid mode without editing config.toml → impossible |
| F-004 | `cargo run --release` → `Ctrl+Shift+P` → see "no plugin commands yet" |
| F-005 | look at the status bar → no `Ln 1, Col 1` ever appears |
| F-006 | press `Ctrl+W` → nothing |
| F-007 | `./target/release/scr1b3 --help` → opens an empty buffer trying to open file `--help` |
| F-008 | `Ctrl+F` opens find → no replace field → `Ctrl+H` does nothing |
| F-010 | `Ctrl+P` → nothing |
| F-011 | drag a .txt file from your desktop onto the window → nothing |
| F-013 | first launch → empty scratch buffer, no welcome |

---

## Verification Gate (post-PRs)

The loop closes only when:

- Operator launches SCR1B3 with no config, opens 3 files, drags second tab to first position, splits the third into a grid pane, closes any tab with middle-click — **without consulting docs**.
- The shortcut cheatsheet (F1) lists every wired shortcut and matches the README + CONFIG.md tooltip claims.
- `cargo build --release && ./target/release/scr1b3 --help` prints usable help.
- `python scripts/content_safety_audit.py` PASS.
- All 7 CI required status checks (now mandatory under the post-session branch protection) green.
- `docs/audits/final-gate-2026-05-29.md` regraded to reflect this audit's findings.

---

## Cross-References

- `crates/scribe-app/src/app.rs` — primary surface
- `crates/scribe-app/src/grid.rs` — grid behavior
- `crates/scribe-render/src/rope_editor/` — Phase 15 KEYSTONE
- `crates/scribe-core/src/plugin/` — Phase 20
- `docs/audits/final-gate-2026-05-29.md` — to be regraded
- `docs/audits/overlooked-features-bic-research-2026-05-29.md` — Track B BIC research (parallel)
- Session memory ref: PR #1007 was toolbar drag-reorder, NOT tab drag-reorder — clarifies the user's reasonable confusion

## Loop Posture

This is the first audit of the new "is-the-editor-easy-and-intuitive?" loop. The grade is **B-** for the engine, **D+** for the user surface. The fix list is concrete, sequenced, and entirely implementable inside the existing workspace.
