//! The SCR1B3 application shell: frameless brand titlebar, tab strip, syntax-
//! highlighted editor surface, find bar, and status bar. The shell is decomposed
//! into sibling modules under app/ (frame_tick, frame_modals, render_support,
//! builtins, file_ops, editor_overlays, text_analysis, grid_methods, commands,
//! tabs/tab_strip_render, toolbar_render, modals, session_io, …); mod.rs now
//! holds the ScribeApp struct + shared glue.

// egui 0.34 deprecated the top-level `Panel::show(ctx, …)` / `CentralPanel::show(ctx, …)`
// forms in favour of `show_inside(ui, …)` — but `show_inside` requires a parent
// `&mut Ui` which top-level eframe `App::update(ctx)` does not provide; the
// deprecated `show(ctx)` is currently the ONLY working top-level entry. The
// alternative would be a full restructure of the panel tree, out of scope for
// the Phase 16 dep-bump. This module-level allow is scoped + documented; the
// easy deprecations (screen_rect→content_rect, Memory::any_popup_open→Popup::is_any_open)
// are migrated individually below.
#![allow(deprecated)]

use eframe::egui;
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, RichText};
use scribe_core::lsp::{Diagnostic, LspClient, LspRegistry};
use scribe_core::plugin::{self, CommandInfo, HookEvent, PluginContext, PluginHost};
use scribe_core::spell::{self, HashSetEngine};
use scribe_core::syntax::{Highlighter, IncrementalHighlightState};
use scribe_core::theme::{Rgba, Theme};
use scribe_core::{Config, Document};
use std::path::{Path, PathBuf};

mod commands;
// Re-export the command/shortcut/toolbar registries + their pure lookup helpers
// so existing call sites (`crate::app::BUILTIN_COMMANDS`, `super::*`, …) resolve
// unchanged after the WU-1 extraction.
pub(crate) use commands::{
    comment_prefix_for_extension, jp_glyph, parse_goto_query, toolbar_label, BuiltinCommand,
    EditorAction, BUILTIN_COMMANDS, KEYBOARD_SHORTCUTS, TOOLBAR_ACTIONS,
};

/// The label shown on a tab: pinned tabs get a leading pin glyph so the pinned
/// state is visible at a glance (not just in the right-click menu). Pure +
/// unit-tested so the affordance can't silently drop.
fn tab_display_label(title: &str, pinned: bool) -> String {
    // Pinned state is shown by the DIMMED, drag-disabled grab handle that leads
    // every tab (see `grip_handle` + `draw_tab_strip`), NOT by a glyph prefix.
    // The old `PUSH_PIN` prefix rendered as a tofu □ to the LEFT of the title in
    // this build's font atlas (the same egui-phosphor `.notdef` footgun that
    // forced `grip_handle` to paint its dots) — that empty square was the
    // reported "box left of the tab name". Drop it; the painted grip is the
    // single, font-independent left affordance now.
    let _ = pinned;
    title.to_string()
}

/// Size of a 90°-rotated tab cell (#82). Rotating the horizontal label swaps
/// its axes: the label's height becomes the cell width, its width becomes the
/// cell height. `pad` is added on each axis.
fn rotated_tab_size(galley: egui::Vec2, pad: egui::Vec2) -> egui::Vec2 {
    egui::vec2(galley.y + pad.x, galley.x + pad.y)
}

/// Anchor position for painting the 90°-clockwise-rotated label inside `rect`
/// (#82). A `+FRAC_PI_2` rotation about the returned point makes the galley
/// extend left+down, so we anchor at the top-right of the padded inner area;
/// the text then reads top-to-bottom inside the cell.
fn rotated_tab_text_pos(rect: egui::Rect, galley: egui::Vec2, pad: egui::Vec2) -> egui::Pos2 {
    egui::pos2(
        rect.left() + pad.x / 2.0 + galley.y,
        rect.top() + pad.y / 2.0,
    )
}

/// Pure highlight-movement for the fuzzy finder's keyboard nav (#73). Returns
/// the new selected index after applying an Up and/or Down key, clamped to
/// `[0, len-1]`. Down saturates at the last row; Up saturates at the first.
/// Factored out + unit-tested so the clamp/saturation edge cases (empty-ranked
/// guarded by the caller) cannot regress into an out-of-bounds index.
fn fuzzy_move_selection(current: usize, len: usize, up: bool, down: bool) -> usize {
    if len == 0 {
        return 0;
    }
    let mut sel = current.min(len - 1);
    if down {
        sel = (sel + 1).min(len - 1);
    }
    if up {
        sel = sel.saturating_sub(1);
    }
    sel
}

/// Pure index remap for a drag-reorder that moves the element at `src` so it
/// takes original position `target`'s slot — the drop-on-tab, swap-style UX
/// (drop tab A onto tab B and A lands where B was, the rest shift to fill).
/// Models `Vec::remove(src)` followed by `Vec::insert(target, _)` and returns
/// the new index of whatever element currently sits at `idx`. Factored out and
/// unit-tested (see `tab_reorder_tests`) because the index arithmetic is the
/// part that historically went wrong — the old hand-rolled hit-test scanned a
/// partially-built response vector and silently missed drop targets to the
/// right of the dragged tab.
fn tab_index_after_move(src: usize, target: usize, idx: usize) -> usize {
    if src == target {
        return idx;
    }
    if idx == src {
        return target;
    }
    // Where `idx` sits after `remove(src)`, then after `insert(target, _)`.
    let a1 = if idx < src { idx } else { idx - 1 };
    if a1 >= target {
        a1 + 1
    } else {
        a1
    }
}

/// Public Releases page, opened by the "View all releases on GitHub" link in
/// Settings (a convenience, not the update mechanism). The actual version check
/// and download go through a direct GitHub Releases API call (see
/// [`crate::updater`] and `scribe_core::update::net`); it sends no identifiers
/// and no telemetry. Same host as the installer's `ARPHELPLINK` so it is
/// auditable against the wix manifest.
pub(crate) const RELEASES_URL: &str =
    "https://github.com/46b-ETYKiAL/Itasha.Corp_S4F3-SCR1B3/releases";

/// Current wall-clock time in unix seconds, saturating to 0 before the epoch
/// (a backwards-set clock yields 0 rather than panicking).
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build the spell engine for the current `spellcheck` config — fully offline.
/// The base dictionary is a user-supplied `<config_dir>/dict/<language>.txt`
/// word list when present (so `spellcheck.language` is meaningful — drop a word
/// list to check another language), otherwise the compiled-in en_US list so the
/// default needs zero setup. Any `spellcheck.custom_dict_path` file is layered
/// on top as extra accepted words.
fn build_spell_engine(config: &Config) -> HashSetEngine {
    let mut engine = Config::config_dir()
        .map(|d| {
            d.join("dict")
                .join(format!("{}.txt", config.spellcheck.language))
        })
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|list| HashSetEngine::from_word_list(&list))
        .unwrap_or_else(HashSetEngine::bundled_en_us);
    if let Some(path) = &config.spellcheck.custom_dict_path {
        if let Ok(list) = std::fs::read_to_string(path) {
            engine.load_user_words(&list);
        }
    }
    engine
}

/// Decide the once-per-launch automatic update action, given the user's mode +
/// scheduling state. Pure (no I/O), so it is unit-tested directly. `Off` and
/// `Manual` never do automatic network activity (telemetry-free default).
/// `Notify` and `Auto` perform a single GitHub-Releases check when the interval
/// is due — `Notify` later surfaces a passive toast, `Auto` opens the yes/no
/// modal. Returns `None` when no automatic check should run this launch.
fn update_launch_action(
    mode: scribe_core::config::UpdateMode,
    last_check_unix: Option<u64>,
    interval_hours: u64,
    now: u64,
) -> Option<crate::updater::LaunchKind> {
    use scribe_core::config::UpdateMode;
    match mode {
        UpdateMode::Off | UpdateMode::Manual => None,
        // Notify is a passive, dismissible banner backed by ONE lightweight
        // GitHub-Releases GET, so it checks on EVERY launch. It must NOT be
        // gated by the interval throttle or by `last_check_unix` — that field is
        // ALSO stamped by the manual "Check for updates" button, so sharing it
        // meant a recent manual check silently suppressed launch-notify for 24h
        // and the user relaunched without ever learning a release was out (the
        // reported bug). `last_check_unix`/`interval_hours` are irrelevant here.
        UpdateMode::Notify => Some(crate::updater::LaunchKind::Notify),
        // Auto downloads in the background, so it stays interval-throttled to
        // avoid repeated bandwidth use across frequent relaunches.
        UpdateMode::Auto => scribe_core::update::is_check_due(last_check_unix, interval_hours, now)
            .then_some(crate::updater::LaunchKind::Auto),
    }
}

/// A stable signature of the open-file set (sorted paths) for change detection.
fn session_signature(tabs: &[EditorTab]) -> String {
    let mut paths: Vec<String> = tabs
        .iter()
        .filter_map(|t| t.doc.path().map(|p| p.display().to_string()))
        .collect();
    paths.sort();
    paths.join("\n")
}

/// Path to the session file (list of open files for restore-on-launch).
fn session_file() -> Option<PathBuf> {
    Config::config_dir().map(|d| d.join("session.txt"))
}

/// Load the previously-open file paths (most-recent session).
fn load_session() -> Vec<PathBuf> {
    let Some(path) = session_file() else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Persist the given open file paths for next launch (best-effort).
fn save_session(paths: &[PathBuf]) {
    let Some(path) = session_file() else {
        return;
    };
    if let Some(parent) = path.parent() {
        // E-01: best-effort, but a persistently-failing mkdir would silently
        // lose the restore list forever. Log it (do NOT make it fatal).
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!("session restore-list dir create failed (non-fatal): {e}");
        }
    }
    let body: String = paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    // E-01: best-effort restore-list write. A persistent failure here means
    // the next launch silently restores nothing -- surface it in the log so
    // the failure is not completely invisible. Still non-fatal (same control).
    if let Err(e) = std::fs::write(&path, body) {
        tracing::warn!("session restore-list write failed (non-fatal): {e}");
    }
}

/// Convert a filesystem path to a `file://` URI (LSP wants URIs).
fn path_to_uri(p: &Path) -> String {
    let s = p.display().to_string().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// One open document + its editable text mirror.
struct EditorTab {
    doc: Document,
    text: String,
    /// Phase 18 T18.2 — stable id used by the multi-note grid so a pane
    /// always points at the same logical doc even after the tabs vector
    /// is reordered or other tabs close. Allocated via
    /// `ScribeApp::next_doc_id`. Zero is fine as a sentinel for legacy
    /// session restores; real ids start at 1.
    doc_id: crate::grid::DocId,
    /// F-044 from docs/audits/overlooked-surfaces-2026-05-29.md: when
    /// true the tab renders with a leading 📌 marker and is treated as
    /// "keep this open" by Close Others / Close Right helpers. Default
    /// false; toggled via the tab's right-click context menu.
    pinned: bool,
    /// F-022 from docs/audits/overlooked-surfaces-2026-05-29.md: per-tab
    /// disk mtime captured at open / save time. Used by the per-frame
    /// poll to detect external edits (git pull, hot-reload tools, etc.)
    /// and either silently reload (if the tab is clean) or warn the user
    /// (if local edits would be clobbered on save).
    disk_mtime: Option<std::time::SystemTime>,
    /// The exact text last read from / written to disk. When the buffer
    /// still matches this, an external change can be silently re-read.
    disk_text: String,
    /// KEYSTONE — per-tab editing state (caret/selection + undo history) for
    /// the experimental owned rope editor. Lazily created on first use when
    /// `config.editor.experimental_rope_editor` is on; `None` while the egui
    /// TextEdit path owns this tab.
    rope_state: Option<scribe_render::RopeEditorState>,
    /// KEYSTONE perf — the persistent rope buffer for the experimental owned
    /// editor. Built once from `text` (O(n)) on first use, then mutated in
    /// place each frame; `text` is re-synced from it ONLY when an edit
    /// actually changes content (see `RopeEditorResponse::content_changed`).
    /// This removes the per-frame `Buffer::from_text` + `rope.to_string()`
    /// round-trip that made the experimental path O(n)/frame. Set to `None`
    /// to invalidate after any external mutation of `text` (reload, plugin,
    /// find-replace, sort-lines) so the next frame rebuilds it.
    rope_buf: Option<scribe_core::buffer::Buffer>,
    /// Per-tab line bookmarks (0-based line indices). Toggled with Ctrl+F2 on
    /// the cursor line; F2 / Shift+F2 jump to the next / previous bookmark.
    /// A dot marker is drawn in the line-number gutter for each bookmarked
    /// line. Session-scoped (not persisted to disk).
    bookmarks: std::collections::BTreeSet<usize>,
    /// Wave-3 perf: monotonic per-tab edit generation. Bumped on EVERY
    /// mutation of `text` (the `set_text` funnel, the direct in-place editing
    /// commands, the egui `TextEdit` `.changed()` frame, and the experimental
    /// rope write-back). The minimap + spellcheck caches key off this `u64`
    /// instead of re-hashing the whole buffer every frame — a 1-frame-stale
    /// minimap/squiggle is visually harmless, so a post-edit counter is safe
    /// for those two surfaces. (The syntax layouter deliberately keeps its
    /// content hash — its cached galley bakes in the text, so a lagging
    /// counter would render stale TEXT. See wave3-perf-plan.md.)
    edit_gen: u64,
    /// F-022b — set by `poll_external_disk_changes` when this file changed on
    /// disk WHILE the tab has unsaved local edits. Drives a persistent,
    /// actionable banner ([Reload (discard mine)] / [Keep mine]) so the user is
    /// prompted to update to the on-disk version instead of silently clobbering
    /// it on save. A CLEAN tab is reloaded silently and never sets this.
    external_change: bool,
    /// Change-bar baseline: the buffer text at session/open time, frozen until
    /// the tab is reloaded from disk. A line matching this shows no indicator.
    session_baseline: String,
    /// Change-bar baseline: the buffer text at the last save. A line that
    /// changed vs `session_baseline` but matches this is shown as "saved".
    saved_baseline: String,
    /// Derived per-line change state (Notepad++-style gutter bar). Recomputed
    /// lazily by `ensure_change_states` when `edit_gen` moves past `change_gen`.
    change_states: Vec<crate::change_bar::LineChange>,
    /// `Some(edit_gen)` the `change_states` cache was computed for, or `None`
    /// to force a recompute (initial, post-save, post-reload).
    change_gen: Option<u64>,
}

/// A recently-closed tab kept on the reopen stack (Ctrl+Shift+T), so an
/// accidental close is one keystroke from recovery (content + caret restored).
#[derive(Debug, Clone)]
struct ClosedTab {
    path: Option<PathBuf>,
    text: String,
    cursor: usize,
}

/// P-06: how many frames must elapse between successive disk-change polls.
/// At ~60fps this is roughly twice a second — frequent enough that an external
/// edit (git pull, formatter) is picked up promptly, infrequent enough that the
/// per-tab `fs::metadata` stat is not paid on every single frame.
const DISK_POLL_INTERVAL_FRAMES: u64 = 30;

/// P-06 (pure): decide whether the disk-change poll should run this frame.
/// Polls when it has never polled before (`last == u64::MAX` sentinel), when at
/// least `interval` frames have elapsed since the last poll, or when the frame
/// counter has wrapped (`current < last`). Factored out as a pure function so
/// the throttle decision is unit-tested without a live egui frame.
fn should_poll_disk(current: u64, last: u64, interval: u64) -> bool {
    if last == u64::MAX {
        return true;
    }
    if current < last {
        return true; // frame counter wrapped — poll rather than stall.
    }
    current - last >= interval
}

/// Last-modified time of `path`, or `None` if it cannot be stat'd or the
/// platform does not expose an mtime. Centralises the disk-fingerprint stat
/// used by tab construction, save, and the external-change poll (F-022).
fn file_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl EditorTab {
    fn scratch() -> Self {
        Self {
            doc: Document::scratch(),
            text: String::new(),
            doc_id: crate::grid::DocId(0),
            pinned: false,
            disk_mtime: None,
            disk_text: String::new(),
            rope_state: None,
            rope_buf: None,
            bookmarks: std::collections::BTreeSet::new(),
            edit_gen: 0,
            external_change: false,
            session_baseline: String::new(),
            saved_baseline: String::new(),
            change_states: Vec::new(),
            change_gen: None,
        }
    }

    fn from_path(path: PathBuf) -> Result<Self, String> {
        let doc = Document::open(&path).map_err(|e| e.to_string())?;
        let text = doc.text();
        let disk_mtime = doc.path().and_then(file_mtime);
        Ok(Self {
            doc,
            text: text.clone(),
            doc_id: crate::grid::DocId(0),
            pinned: false,
            disk_mtime,
            disk_text: text.clone(),
            rope_state: None,
            rope_buf: None,
            bookmarks: std::collections::BTreeSet::new(),
            edit_gen: 0,
            external_change: false,
            // Just opened: both baselines are the on-disk content, so no line
            // is marked until the user edits.
            session_baseline: text.clone(),
            saved_baseline: text,
            change_states: Vec::new(),
            change_gen: None,
        })
    }

    /// Reconstruct a tab from a restored session backup: `content` is the
    /// unsaved text. When `path` is set and still readable, the doc binds the
    /// path + holds the on-disk content (so the dirty comparison is correct);
    /// if the file vanished or there is no path, the tab restores as an
    /// untitled scratch holding the backup content.
    fn from_backup(path: Option<PathBuf>, content: String) -> Self {
        if let Some(p) = path {
            if let Ok(doc) = Document::open(&p) {
                let disk_text = doc.text();
                let disk_mtime = doc.path().and_then(file_mtime);
                return Self {
                    doc,
                    text: content,
                    doc_id: crate::grid::DocId(0),
                    pinned: false,
                    disk_mtime,
                    // Baselines are the on-disk (saved) content; restored
                    // unsaved edits in `content` show as "unsaved" until saved.
                    session_baseline: disk_text.clone(),
                    saved_baseline: disk_text.clone(),
                    disk_text,
                    rope_state: None,
                    rope_buf: None,
                    bookmarks: std::collections::BTreeSet::new(),
                    edit_gen: 0,
                    external_change: false,
                    change_states: Vec::new(),
                    change_gen: None,
                };
            }
        }
        // Untitled, or the original file is gone: restore as a scratch buffer
        // carrying the unsaved content (dirty vs an empty saved doc).
        let mut tab = Self::scratch();
        tab.text = content;
        tab
    }

    /// Collapse a second restored entry for an already-open file into the one
    /// existing tab — the one-tab-per-file invariant that prevents the
    /// "same note opened twice on restore" duplication. Keeps whichever copy
    /// carries unsaved content so the user's edits are never dropped (a clean
    /// from-disk copy is redundant with what's already on disk). If the kept
    /// copy's content diverges from the file now on disk, raises
    /// `external_change` so the F-022 "Reload from disk / Keep my version"
    /// banner makes the divergence explicit — a stale restored snapshot vs a
    /// newer saved file — instead of surfacing as a confusing second tab.
    fn merge_restored_duplicate(existing: &mut EditorTab, candidate: EditorTab) {
        if candidate.is_dirty() && !existing.is_dirty() {
            *existing = candidate;
        }
        if existing.is_dirty() {
            existing.external_change = true;
        }
    }

    /// Replace the editable text from an EXTERNAL source (reload, plugin,
    /// find-replace, sort-lines) and invalidate the experimental rope cache so
    /// the next frame rebuilds the persistent rope from the new content. The
    /// rope editor itself writes `text` directly (it owns the rope) and must
    /// NOT go through here, or it would discard its own live buffer.
    fn set_text(&mut self, new: String) {
        self.text = new;
        self.rope_buf = None;
        self.edit_gen = self.edit_gen.wrapping_add(1);
    }

    /// Change-bar: record the current text as the saved baseline (called after
    /// a successful save). The session baseline stays frozen, so a line edited
    /// then saved transitions from "unsaved" to "saved" rather than to "none".
    fn mark_change_saved(&mut self) {
        self.saved_baseline = self.text.clone();
        self.change_gen = None; // force recompute (edit_gen is unchanged by a save)
    }

    /// Change-bar: reset BOTH baselines to the current text (called after a
    /// reload from disk, where the new content becomes the clean reference).
    fn reset_change_baselines(&mut self) {
        self.session_baseline = self.text.clone();
        self.saved_baseline = self.text.clone();
        self.change_gen = None;
    }

    fn title(&self) -> String {
        // #R5: title() stays plain. The pin marker is added by the renderers as
        // a phosphor glyph (`tab_display_label` for the tab strip, the chip
        // header for grid panes) — emitting "📌" here rendered as tofu in the
        // bundled fonts AND double-marked pinned tabs (renderer pin + emoji).
        let name = self.doc.file_name();
        if self.is_dirty() {
            // Unsaved marker. MUST be ASCII: the `●` (U+25CF) used before rendered
            // as a tofu □ in this build's font atlas (the egui-phosphor `.notdef`
            // footgun) — the "empty square in the untitled tab" report; it showed
            // only on dirty tabs (an untitled note with content), never on a fresh
            // empty one. `*` is the Notepad++ unsaved convention and always renders.
            format!("* {name}")
        } else {
            name.to_string()
        }
    }

    fn is_dirty(&self) -> bool {
        // Dirty when the editable mirror diverges from the saved rope.
        self.text != self.doc.text()
    }
}

/// PA-04/05 — memoized `(lines, words, chars)` document counts for a buffer.
type DocCounts = (usize, usize, usize);
/// PA-04/05 — the `count_cache` payload: `(edit_gen, doc_id, counts)`, `None`
/// until the first `doc_counts_active` walk. Factored into a `type` alias so
/// the `RefCell` field stays under clippy's `type_complexity` ceiling.
type CountCache = Option<(u64, crate::grid::DocId, DocCounts)>;

pub struct ScribeApp {
    config: Config,
    /// The user's `[keybindings]`, resolved into matchable egui chords.
    ///
    /// Cached rather than re-parsed per frame: config live-reloads, so
    /// `handle_keyboard_shortcuts` rebuilds this whenever `keymap_src` no longer
    /// equals `config.keybindings`, and otherwise reuses it.
    keymap: keymap::Keymap,
    /// The `[keybindings]` value `keymap` was built from — the live-reload
    /// invalidation key.
    keymap_src: scribe_core::config::Keybindings,
    /// Resolved config/runtime directory (where `scr1b3.toml`, the session
    /// manifest, and the unsaved-buffer backups live). Resolved ONCE at build
    /// from `Config::config_dir()`. `new_test` overrides it to a per-instance
    /// temp dir so a test's periodic hot-exit snapshot can NEVER write into the
    /// real user config dir — that test-isolation gap is what leaked a unit
    /// test's `"a very long line"` fixture into the real session backup, where
    /// it then restored as a phantom note on every launch. `None` only when no
    /// OS config dir can be resolved.
    config_dir: Option<PathBuf>,
    theme: Theme,
    /// Last OS-reported system theme (dark/light) we acted on, so the
    /// `appearance.follow_os_theme` watcher only re-applies on an actual change
    /// rather than every frame. `None` until the first frame reports one.
    last_os_theme: Option<egui::Theme>,
    /// Set once we have run the per-launch update-due check (so it fires at most
    /// once per session, on the first frame).
    did_update_check: bool,
    /// In-app self-updater state machine (network check + download + apply).
    updater: crate::updater::Updater,
    /// `notify`-mode update notice: `Some(version)` when a launch check found a
    /// newer release. Rendered as a prominent top banner (Update / Dismiss),
    /// NOT the passive toast — so the notify-mode update is noticeable and the
    /// "Update" button jumps straight to Settings → Updates to start it.
    update_notice: Option<String>,
    hl: Highlighter,
    tabs: Vec<EditorTab>,
    active: usize,
    visuals_applied: bool,
    /// Hash of the visuals-affecting config the current egui visuals were built
    /// from (theme, window tint / strength / opacity / translucency, background
    /// overrides). When it changes — e.g. the user drags the tint slider — the
    /// visuals are rebuilt so the change shows live, instead of only at startup.
    applied_visuals_sig: u64,
    /// The note (editor) syntax colour theme currently applied to `hl` (#104).
    /// When `config.editor.note_theme` diverges, the highlighter is re-themed and
    /// the highlight cache invalidated so colours refresh live.
    applied_note_theme: String,
    /// The editor font family currently applied to the egui context (#87). When
    /// `config.fonts.editor_family` diverges from this, the font set is rebuilt
    /// and re-applied — a restart-free font-theme switch.
    applied_font_family: String,
    /// Set the frame a font switch calls `ctx.set_fonts`. `set_fonts` only takes
    /// effect at the START of the NEXT frame, so the galley caches must be dropped
    /// AGAIN on that next frame (after the new atlas is live) — clearing them only
    /// on the switch frame re-bakes a galley against the still-OLD atlas, which is
    /// why the note briefly rendered blank/garbled until the next edit re-keyed it.
    font_rebuild_pending: bool,
    /// Set when the user asks to close (custom titlebar ✕). Funnels into the
    /// same two-phase close path as an OS-initiated close.
    want_close: bool,
    /// Two-phase close latch: a transparent/layered window must be hidden BEFORE
    /// it is destroyed or DWM retains its last frame as a ghost on the desktop
    /// (the T19.1 root cause). On the first close request we hide + cancel, then
    /// issue the real Close on the next frame.
    closing: bool,
    /// Wave-5: project-wide find ("find in files") results pane. Opened with
    /// Ctrl+Shift+F; searches the open folder (`file_tree_root`) via the same
    /// regex engine as the in-buffer find bar.
    find_in_files_open: bool,
    find_in_files_query: String,
    find_in_files_regex: bool,
    find_in_files_results: Vec<crate::find_in_files::FileMatch>,
    find_in_files_error: Option<String>,
    focus_find_in_files: bool,
    /// PA-02 keyboard-nav highlight index into the find-in-files results list.
    /// Up/Down move it; Enter opens the selected result. Mirrors
    /// `fuzzy_selected`; reset to 0 when a new search runs.
    find_in_files_selected: usize,
    /// 4-02 — receiver for the off-thread project-find worker. The fs walk +
    /// per-file scan run on a spawned `std::thread`, streaming `FileMatch`
    /// batches back over this channel so the egui frame thread NEVER blocks on a
    /// big tree. `None` when no search is in flight. A new search drops the old
    /// receiver (the orphaned worker's sends then no-op), so the latest query
    /// always supersedes the old one — no cancellation flag needed.
    find_in_files_rx: Option<std::sync::mpsc::Receiver<crate::find_in_files::SearchMsg>>,
    /// True while a project-find worker is running (drives a "searching…" hint
    /// and keeps the UI repainting so streamed results land promptly).
    find_in_files_running: bool,
    /// Wave-5 P1: distraction-free "zen" mode. Hides toolbar, tab strip, status
    /// bar, minimap, and gutter, centering the editor. Runtime session state
    /// (not persisted) — toggled with Ctrl+. and exited first by Esc.
    zen_mode: bool,
    /// Wave-5 P1: static Tab-trigger snippets loaded from `<config>/snippets.toml`.
    snippets: scribe_core::snippets::SnippetSet,
    /// Wave-5 P1: markdown live-preview side panel (Ctrl+Shift+V). Shown only
    /// for markdown buffers; renders the buffer via pulldown-cmark → egui.
    md_preview_open: bool,
    /// Wave-5 P1: diff side panel — the open buffer vs the file on disk.
    diff_view_open: bool,
    /// Wave-6 motion: fading caret-trail echoes as `(screen_rect, birth_time)`.
    /// Fed when the caret moves (default TextEdit path); aged out each frame.
    caret_trail: std::collections::VecDeque<(egui::Rect, f64)>,
    /// P2 structural multi-selection over the central egui `TextEdit`: the
    /// app-side secondary carets layered on egui's single primary caret
    /// (Ctrl/Cmd+click, Ctrl+D select-next, Alt+drag column select). See
    /// `crate::multi_cursor`.
    multi_cursor: crate::multi_cursor::MultiCursor,
    /// The latched anchor char offset of an in-progress Alt+drag column
    /// selection (`None` when no column drag is active).
    column_anchor: Option<usize>,
    /// The `doc_id` of the tab whose buffer the current [`Self::multi_cursor`]
    /// secondaries / [`Self::column_anchor`] index into. The multi-cursor state
    /// is app-global but each editor is keyed PER TAB (`doc_id`-salted egui id),
    /// so a tab switch would otherwise leave stale carets that silently edit the
    /// WRONG document (auto-focus-on-switch makes the very next keystroke land
    /// there). `mc_reconcile_owner` drops the carets when this no longer matches
    /// the active tab; `mc_record_owner` refreshes it after each frame's
    /// gestures. `None` when no multi-cursor state is live. See P1-A.
    mc_owner_doc: Option<crate::grid::DocId>,
    /// Wave-6 motion: time the one-shot boot-glitch latched (first frame it ran).
    boot_glitch_started: Option<f64>,
    find_open: bool,
    find_query: String,
    /// #R6 — currently-selected match in the find bar (0-based). Drives the
    /// "{i}/{n}" counter and Next/Prev/F3 navigation; reset when the query
    /// changes. Clamped to the live match count each frame.
    find_match_idx: usize,
    /// The find query the match index was last computed against, so editing the
    /// query resets navigation to the first match.
    find_last_query: String,
    /// Replace-bar inputs (F-008 from docs/audits/overlooked-surfaces-2026-05-29.md).
    /// `Ctrl+H` opens the find bar with focus on `replace_query`; the bar
    /// renders a 2nd text field + "Replace next" + "Replace all" buttons
    /// alongside the existing find field so a single keystroke does what
    /// Notepad++ / Sublime / VSCode all reach for.
    replace_query: String,
    /// One-shot focus request for the replace field when the user opens
    /// the bar via Ctrl+H specifically (as opposed to Ctrl+F).
    focus_replace: bool,
    /// F-038 from docs/audits/overlooked-surfaces-2026-05-29.md: persistent
    /// banner rendered above the editor whenever the config file failed to
    /// parse on launch. Offers "Open config" / "Restore default" / "Dismiss"
    /// actions. Distinct from `toast` (which auto-clears).
    config_error_banner: Option<String>,
    status: String,
    toast: Option<String>,
    /// Plugin/mod host (Rhai easy-mode); loaded from the plugins dir on start.
    plugins: PluginHost,
    /// #R6 — ids of discovered plugins held back at load because the user has
    /// not approved their current entry script (TOFU trust gate). Surfaced in
    /// the plugin manager so the user can review + approve them.
    pending_plugins: Vec<String>,
    plugin_cmds: Vec<CommandInfo>,
    /// Offline spellcheck engine (bundled en_US); checked only when enabled.
    spell: HashSetEngine,
    palette_open: bool,
    palette_query: String,
    /// BUG-APP-01: keyboard-nav highlight index into the palette's filtered
    /// command list, mirroring `fuzzy_selected` for the fuzzy-file-finder. Up/
    /// Down move it, Enter runs the highlighted command. Reset to 0 whenever the
    /// query text changes so the highlight never points past the filtered set.
    palette_selected: usize,
    settings_open: bool,
    /// F-014 from docs/audits/overlooked-surfaces-2026-05-29.md: an in-app
    /// modal listing every wired keyboard shortcut. Opens on F1. The modal
    /// is the editor's "what can it do?" surface when the user can't
    /// remember the shortcut for an operation.
    cheatsheet_open: bool,
    /// F-012 from docs/audits/overlooked-surfaces-2026-05-29.md: when true,
    /// the recent-files modal renders this frame. Opened via Ctrl+R, the
    /// command palette, or the toolbar's "Recent" button.
    recent_open: bool,
    /// PA-03 keyboard-nav highlight index into the recent-files list. Up/Down
    /// move it; Enter opens the selected entry. Mirrors `fuzzy_selected`.
    recent_selected: usize,
    /// When true, the recent-folders modal renders this frame (mirrors
    /// `recent_open`). Opened via the command palette / "Open recent folder".
    recent_folders_open: bool,
    /// PA-03 keyboard-nav highlight index into the recent-folders list. Up/Down
    /// move it; Enter opens the selected entry. Mirrors `fuzzy_selected`.
    recent_folders_selected: usize,
    /// Command-palette → caret-command bridges. These commands need the egui
    /// `TextEditState` (only reachable with `ctx`), so the palette (which has
    /// no `ctx`) sets a flag here that `frame_tick` drains, mirroring the `act`
    /// keyboard path. Taken (reset) when handled.
    pending_jump_bracket: bool,
    pending_insert_datetime: bool,
    pending_dup_selection: bool,
    /// P0-1 — toggle the GFM task checkbox on the caret / selection lines.
    pending_toggle_task: bool,
    /// P0-4 — wrap the selection in this inline marker (`**`, `*`, `` ` ``,
    /// `~~`). Drained in `frame_tick` where the caret range is reachable.
    pending_wrap_marker: Option<&'static str>,
    /// P1-4 — case-convert the selection: 0 = lower, 1 = upper, 2 = title.
    pending_case: Option<u8>,
    /// P2-1 — format the markdown pipe table under the caret.
    pending_format_table: bool,
    /// F-013 from docs/audits/overlooked-surfaces-2026-05-29.md: when true,
    /// the welcome modal renders this frame. Auto-opened on first launch
    /// (when `config.editor.first_run_completed` is false); reachable
    /// thereafter via the Help menu / command palette.
    welcome_open: bool,
    /// F-010 from docs/audits/overlooked-surfaces-2026-05-29.md: when true,
    /// the fuzzy file-finder modal renders this frame. Opened via Ctrl+P.
    fuzzy_open: bool,
    /// Typed query string for the fuzzy finder.
    fuzzy_query: String,
    /// Pre-scanned project file paths (built on first Ctrl+P; reused).
    fuzzy_index: Vec<PathBuf>,
    /// One-shot focus request for the fuzzy-finder input when it opens.
    focus_fuzzy: bool,
    /// Index of the keyboard-highlighted row in the fuzzy finder's ranked
    /// results (#73). Up/Down move it; Enter opens it. Reset to 0 on open and
    /// whenever the query changes; clamped to the result count each frame.
    fuzzy_selected: usize,
    /// F-015 from docs/audits/overlooked-surfaces-2026-05-29.md: Ctrl+G
    /// "go to line" modal. `goto_open` is the modal-open flag, `goto_query`
    /// is the typed text (accepts `N` or `N:C`), `focus_goto` is the
    /// one-shot focus request when the modal opens.
    goto_open: bool,
    goto_query: String,
    focus_goto: bool,
    /// Go-to-symbol modal (Ctrl+Shift+O). `goto_symbol_open` is the
    /// modal-open flag, `goto_symbol_query` is the typed filter text,
    /// `focus_goto_symbol` is the one-shot focus request on open. The list
    /// is sourced from `editor_features::symbol_scopes` for the active
    /// buffer; selecting an entry jumps to its start line.
    goto_symbol_open: bool,
    goto_symbol_query: String,
    focus_goto_symbol: bool,
    /// PA-01 keyboard-nav highlight index into the go-to-symbol modal's
    /// filtered symbol list, mirroring `palette_selected`/`fuzzy_selected`.
    /// Up/Down move it; Enter jumps to the SELECTED symbol (not the first
    /// match); reset to 0 whenever the filter text changes so the highlight
    /// never points past the filtered set.
    goto_symbol_selected: usize,
    /// Open folder for the file-tree sidebar (None = sidebar hidden).
    file_tree_root: Option<PathBuf>,
    /// F-041: keyboard nav state for the sidebar. The struct rebuilds its
    /// visible-list every render so arrow keys move through the same
    /// entries the user sees.
    file_tree_state: crate::filetree::FileTreeState,
    /// F-039 + F-040: the plugin-manager modal (Loaded / Registry / Install).
    /// Surfaces the Phase-20 plugin foundation that was built but unwired.
    plugin_manager: crate::plugin_manager::PluginManagerState,
    /// W1TN3SS opt-in crash-consent dialog state. Populated on launch from the
    /// local spool when the crash stream is AskEachTime; the modal presents each
    /// spooled report with an editable preview + equal-weight Send/Don't-send.
    crash_consent: crate::reporting::CrashConsentState,
    /// W1TN3SS user-initiated "Report an issue" dialog state. Inert until the
    /// user opens it via the command palette; it transmits nothing on its own —
    /// it builds a prefilled GitHub Issue-Form deep link (or a clipboard / mailto
    /// fallback) and hands off to the OS on an explicit click, with a previewable
    /// + editable body and diagnostics OFF by default.
    issue_intake: crate::issue_intake::IssueIntakeState,
    /// LSP: per-language server registry + the active server connection.
    lsp_registry: LspRegistry,
    lsp: Option<LspClient>,
    lsp_lang: Option<String>,
    diagnostics: Vec<Diagnostic>,
    /// Signature of the currently-open file set (to persist session on change).
    session_sig: String,
    /// Last time unsaved-buffer backups were flushed (throttles the periodic
    /// snapshot so we don't rewrite content every keystroke).
    last_backup_at: Option<std::time::Instant>,
    /// Stack of recently-closed tabs for Ctrl+Shift+T reopen (most-recent last).
    closed_tabs: Vec<ClosedTab>,
    /// Last time an auto-save fired (throttles the periodic save).
    last_autosave_at: Option<std::time::Instant>,
    /// Content hash of unsaved buffers at the last backup flush; skips
    /// rewriting identical content (audit fix F1).
    last_backup_sig: u64,
    /// Cached syntax-highlight layout (keyed by text+lang+size) so syntect only
    /// re-runs when the buffer changes, not every frame (perf hotspot fix).
    hl_cache: std::cell::RefCell<Option<(u64, std::sync::Arc<LayoutJob>)>>,
    /// Wave-3: galley memo keyed by (content+fg key, wrap width). On a full hit
    /// this returns the already-laid-out `Arc<Galley>` (an O(1) Arc bump) and
    /// skips BOTH the per-frame `LayoutJob` deep-clone and `f.layout_job`. The
    /// sibling `hl_cache` job memo still serves wrap-only changes (re-layout
    /// without re-highlighting). Single-slot, like `hl_cache`.
    hl_galley_cache: std::cell::RefCell<Option<(u64, f32, std::sync::Arc<egui::Galley>)>>,
    /// Incremental syntect-highlight state for the focused editable buffer, so a
    /// keystroke re-highlights only from the edited line downward (not the whole
    /// document). Single-slot like `hl_cache`: it re-seeds when focus moves to a
    /// different buffer, which is fine (re-seeding is a one-time full pass).
    hl_inc_cache: std::cell::RefCell<IncrementalHighlightState>,
    /// Memoized misspellings for the active buffer (#78), keyed by a hash of
    /// (text, enabled, scope toggles, language). Drives BOTH the status-bar
    /// count and the red squiggle underlines painted in the editor, so the
    /// dictionary scan runs at most once per (changed) frame.
    spell_cache: std::cell::RefCell<Option<(u64, Vec<spell::Misspelling>)>>,
    /// P-05: memoized breadcrumb/sticky-header definition scopes for the
    /// active buffer, keyed by `(edit_gen, doc_id)` -- the same idiom as
    /// `spell_cache`. The breadcrumb + sticky-header path re-ran the O(n)
    /// `symbol_scopes` char scan EVERY frame; this caches it so the scan runs
    /// only on an edit or a tab switch (a 1-frame-stale breadcrumb after a
    /// keystroke is harmless). `doc_id` disambiguates tabs sharing an edit_gen.
    symbol_cache: std::cell::RefCell<Option<(u64, Vec<crate::editor_features::SymbolScope>)>>,
    /// P-05: monotonic count of how many times `symbol_scopes_for_active`
    /// actually RE-RAN the underlying scan (a cache miss). A pure observation
    /// counter the idle-frame proof test reads to assert the scan does NOT
    /// recompute across repeated calls with no edit. Never read by the UI.
    symbol_scan_count: std::cell::Cell<u64>,
    /// P-06: the frame number (`egui::Context::cumulative_pass_nr`) at which
    /// `poll_external_disk_changes` last ran its per-tab `fs::metadata` stat.
    /// The poll is throttled to once every `DISK_POLL_INTERVAL_FRAMES` frames
    /// so it does not stat every open file-backed tab on every single frame.
    last_disk_poll_frame: u64,
    /// P-01 / 4-02 R2 — single-slot find-match memo for the in-buffer find bar,
    /// keyed by `(query, active tab edit_gen, doc_id)`. While the find bar is
    /// open, `find_matches_active` is called every frame (counter, highlight-all
    /// overlay, navigation); without this cache that re-scanned the whole buffer
    /// AND recompiled the regex per idle frame. The matches are recomputed ONLY
    /// when the key moves (query edit, buffer edit, or tab switch); otherwise the
    /// cached `Vec<Match>` is cloned out — mirrors the `spell_cache` idiom above.
    find_cache: std::cell::RefCell<Option<crate::find_cache::FindCacheEntry>>,
    /// P-01 test instrumentation: counts how many times `find_matches_active`
    /// actually invoked `scribe_core::search::find_all` (i.e. a cache MISS). The
    /// "no recompute on idle frame" test asserts this stays flat across repeated
    /// idle calls. Bumped only on a real recompute; never read in production.
    find_recompute_count: std::cell::Cell<u64>,
    /// PA-04 / PA-05 — memoized `(lines, words, chars)` document counts for the
    /// active buffer, keyed by `(edit_gen, doc_id)` — the same `edit_gen`-keyed
    /// idiom as `spell_cache`/`symbol_cache`/`find_cache`. The status bar and the
    /// sticky line-number gutter both walked the WHOLE buffer every frame
    /// (`lines().count()` / `split_whitespace().count()` / `chars().count()`) on
    /// non-huge files; this caches the three O(n) passes so they recompute ONLY
    /// on a real edit or a tab switch, not on every idle frame. `doc_id`
    /// disambiguates tabs that share an `edit_gen`.
    count_cache: std::cell::RefCell<CountCache>,
    /// PA-04/05 test instrumentation: counts how many times `doc_counts_active`
    /// actually re-walked the buffer (a cache MISS). The idle-frame proof test
    /// asserts this stays flat across repeated calls with no edit. Never read by
    /// the UI.
    count_recompute_count: std::cell::Cell<u64>,
    /// Config-file watcher for live-reload (kept alive; events arrive on `cfg_rx`).
    _cfg_watcher: Option<notify::RecommendedWatcher>,
    cfg_rx: Option<std::sync::mpsc::Receiver<()>>,
    /// Folded-preview mode: render a read-only buffer with brace regions
    /// collapsed; the gutter toggles individual folds (`folds` holds the
    /// `start_line` of each collapsed region for the active tab).
    fold_view: bool,
    folds: std::collections::BTreeSet<usize>,
    /// Active identifier-completion popup, if any (Ctrl+Space).
    completion: Option<Completion>,
    /// Pending vertical scroll offset to apply to the editor next frame (set by
    /// a minimap click/drag). Consumed once.
    pending_scroll: Option<f32>,
    /// A clipboard / undo action requested via the command palette. Drained at
    /// the top of `frame_tick` by injecting the matching egui event + focusing
    /// the central editor so egui's `TextEdit` performs it natively. Consumed
    /// once. The chords (Ctrl+C/X/V/Z) always worked directly on the focused
    /// editor; this gives the palette a working entry point too.
    pending_editor_action: Option<EditorAction>,
    /// Last-frame editor scroll metrics `(offset_y, content_height, viewport_height)`
    /// — read by the minimap to draw its viewport indicator (one-frame lag is fine).
    scroll_metrics: (f32, f32, f32),
    /// Memoized minimap galley keyed by text hash so a large document is laid
    /// out once, not every frame. This is the NATURAL (font-3.0) galley; it is
    /// used to measure the document's intrinsic minimap height (`map_h`) from
    /// which the fit-to-height scale is derived, and is drawn directly for short
    /// documents (scale == 1).
    minimap_cache: std::cell::RefCell<Option<(u64, std::sync::Arc<egui::Galley>)>>,
    /// Memoized SCALED minimap galley (font `3.0 * s`) for documents taller than
    /// the panel, keyed additionally by the quantised panel height + word-wrap so
    /// it re-lays-out on a window/panel resize. Separate from `minimap_cache` so a
    /// vertical resize does not invalidate the natural-height measurement galley.
    minimap_draw_cache: std::cell::RefCell<Option<(u64, std::sync::Arc<egui::Galley>)>>,
    /// True while a minimap drag that BEGAN on the viewport-indicator box is in
    /// progress → the drag scrolls the editor by pointer delta (grab-the-box),
    /// rather than jumping absolutely to the pointer fraction. Reset on release.
    minimap_drag_box: bool,
    /// One-shot: request keyboard focus on the find field the frame it opens.
    focus_find: bool,
    /// One-shot: request keyboard focus on the command-palette field on open.
    focus_palette: bool,
    /// Per-logical-line screen Y of the editor's rows from the previous frame —
    /// drives the sticky line-number gutter (one-frame lag, like the minimap).
    line_gutter: Vec<f32>,
    /// Phase 18 T18.2 — multi-note grid state. `tree` is the egui_tiles
    /// layout when `config.editor.grid_enabled` is on; `next_doc_id` is
    /// the monotonic allocator that hands every `EditorTab` a stable id
    /// the panes can reference; `close_queue` is the per-frame buffer of
    /// doc ids the grid chrome asked be closed.
    grid_tree: Option<egui_tiles::Tree<crate::grid::Pane>>,
    next_doc_id: crate::grid::DocIdAllocator,
    grid_close_queue: Vec<crate::grid::DocId>,
    /// Last-frame cursor position in the active buffer, expressed as
    /// `(1-based line, 1-based column)`. Sampled from the central panel's
    /// `TextEditOutput::cursor_range.primary` on every paint and rendered
    /// in the status bar — closes F-005 ("Ln 4, Col 17") from the
    /// 2026-05-29 overlooked-surfaces audit.
    last_cursor_line_col: Option<(usize, usize)>,
    /// Selection length in characters, if the cursor range is non-empty.
    /// Drives the status-bar segment "(N chars selected)". Closes F-024.
    last_selection_chars: usize,
}

/// State for the open completion popup.
pub(crate) struct Completion {
    /// Byte offset in the active buffer where the typed prefix begins.
    prefix_start: usize,
    /// Suggestion list (shortest-first).
    items: Vec<String>,
    /// Highlighted row.
    selected: usize,
}

impl ScribeApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        config_err: Option<String>,
        cli_paths: Vec<String>,
    ) -> Self {
        let mut app = Self::build(config, config_err, cli_paths, true);
        // W1TN3SS opt-in crash reporting: drain the local spool of any reports
        // captured by a prior session's panic hook. PRODUCTION-only — `new_test`
        // never calls this, so a unit test that builds the app never touches the
        // real config dir's spool (the documented test-pollution leak).
        app.drain_crash_spool();
        // Phase 16 T16.3: register the egui-phosphor Thin icon font so toolbar
        // glyphs (Save / Find / Palette / etc.) render when appearance.toolbar_icons
        // is on. The icon font is inserted as the #2 entry in Proportional, so
        // text always wins where there is a real glyph + icons fill the gap.
        //
        // Phase 17 T17.2 fonts-bundle: ship JetBrains Mono Regular as the primary
        // Monospace family. The .ttf is OFL-1.1 (see assets/fonts/JetBrainsMono/
        // OFL.txt) and embedded at compile-time via include_bytes!. We insert it
        // at the FRONT of the Monospace family so it wins over egui's bundled
        // Hack default; Hack stays as the fallback for any glyph JetBrains Mono
        // doesn't cover. egui renders via ab_glyph which does NOT do OT shaping,
        // so ligatures are inherently OFF (T17.2 "ligatures off-default" is
        // structural, not config — there is no path to turn them on without
        // swapping the shaper).
        cc.egui_ctx.set_fonts(build_fonts(
            &app.config.fonts.editor_family,
            &app.config.fonts.ui_family,
        ));
        app.applied_font_family = font_state_key(&app.config.fonts);
        // Follow the OS theme preference so `ctx.theme()` reflects the live OS
        // light/dark setting (egui-winit updates it on OS theme-change events).
        // The app's own brand visuals are applied on top via `set_visuals`; this
        // only makes the OS theme *readable* for `appearance.follow_os_theme`.
        cc.egui_ctx
            .options_mut(|o| o.theme_preference = egui::ThemePreference::System);
        cc.egui_ctx.set_visuals(app.current_visuals());
        app.visuals_applied = true;
        // Transparency is a transparent-surface-only effect now (no OS DWM
        // backdrop): the frameless window is created transparent, and the
        // translucent panel fills reveal the desktop. No `apply_*` backdrop is
        // called — that re-added the native caption over the custom titlebar
        // (the doubled-caption bug) and the DWM materials were indistinguishable.
        app
    }

    /// Test constructor — builds the app without an eframe context, for headless
    /// `egui_kittest` E2E driving. Session-restore + plugin auto-load are disabled
    /// so tests are hermetic (independent of the real user environment).
    #[cfg(test)]
    pub fn new_test(mut config: Config) -> Self {
        config.editor.restore_session = false;
        config.plugins.enabled = false;
        // Deterministic e2e: the default update mode is now `Notify`, whose
        // once-per-launch check (`maybe_remind_update`) spawns a network thread
        // and keeps requesting repaints — which makes `Harness::run` (bounded
        // step budget) never settle and panic with "exceeded max_steps". Tests
        // must do no network and must be timing-independent, so force the
        // updater OFF here (alongside session-restore + plugins). A test that
        // specifically exercises the updater sets the mode explicitly and drives
        // the harness with `run_steps`/`step`.
        config.updates.mode = scribe_core::config::UpdateMode::Off;
        let mut app = Self::build(config, None, Vec::new(), false);
        // Redirect ALL config/session writes to a per-instance temp dir so a
        // test's periodic hot-exit snapshot never touches the real user config
        // dir. Without this, `session_backup`-on tests wrote their fixture text
        // into the real `%APPDATA%` session backup (test pollution).
        app.config_dir = Some(Self::unique_test_config_dir());
        app
    }

    /// A per-call temp directory for hermetic test config I/O, guaranteed EMPTY.
    ///
    /// `{pid}-{seq}` is unique among *live* processes, but it is NOT unique over
    /// time: the OS recycles PIDs and these dirs are never cleaned up, so a fresh
    /// process can be handed a path a long-dead one already populated. Wiping the
    /// dir is what makes the name a hermetic dir rather than just a unique one.
    ///
    /// This is not hypothetical. It failed `approve_plugin_allows_first_contact_
    /// signed_key` in a full-suite run: a stale dir already pinned `goodplug` to
    /// an older random key, so `pin_or_match` reported Rotated instead of first
    /// contact and approval was (correctly) refused. The danger is the SILENT
    /// case — inheriting state doesn't make a test fail, it makes it test
    /// something else. That test quietly stopped covering first contact and
    /// started re-covering key rotation, and nothing said so.
    #[cfg(test)]
    fn unique_test_config_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        Self::wiped(std::env::temp_dir().join(format!("scr1b3-test-{}-{n}", std::process::id())))
    }

    /// Guarantee `dir` holds no state from a previous process, then hand it back.
    ///
    /// Split out from [`Self::unique_test_config_dir`] purely so it is reachable:
    /// the caller's path depends on a live PID and an atomic counter, so a test
    /// cannot arrange for it to be stale. Here the path is an argument, so the
    /// stale case is directly constructible — see `a_stale_config_dir_is_wiped_
    /// before_a_test_gets_it`.
    #[cfg(test)]
    fn wiped(dir: PathBuf) -> PathBuf {
        // Ignore NotFound (the normal case); anything else would resurface as a
        // confusing failure in whichever test happens to land on this path.
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            assert!(
                e.kind() == std::io::ErrorKind::NotFound,
                "could not clear stale test config dir {}: {e}",
                dir.display()
            );
        }
        dir
    }

    fn build(
        config: Config,
        config_err: Option<String>,
        cli_paths: Vec<String>,
        watch_config: bool,
    ) -> Self {
        let theme = load_theme(&config.appearance.theme);
        // F-013 — open the welcome modal on first launch only. Suppressed
        // when the user passed a file on the command line OR the recent-
        // files list is already populated (they've been here before).
        let welcome_on_launch = !config.editor.first_run_completed
            && cli_paths.is_empty()
            && config.editor.recent_files.is_empty();

        let mut tabs = Vec::new();
        // F-038 — keep the parse error in a persistent banner field rather
        // than only a one-shot toast. The banner sits above the editor and
        // surfaces "Open config / Restore default / Dismiss" actions.
        if let Some(e) = config_err.as_ref() {
            tracing::warn!("settings file failed to load, using defaults: {e}");
        }
        // A keymap problem is not a parse error — the file loaded fine — but it
        // has the same consequence for the user: a shortcut that does nothing.
        // Now that `[keybindings]` actually drives input, an unreachable or
        // colliding binding MUST be surfaced, or the user is back to guessing why
        // their key "doesn't work". A parse error wins the banner: defaults are in
        // force, so any keymap complaint would be about bindings that aren't live.
        // Grammar problems (blank / unparseable / colliding) come from core;
        // unknown-key problems can only be known once the chord is resolved
        // against the UI layer's key table, so `keymap` contributes those.
        let keybinding_issues: Vec<String> = config
            .keybindings
            .validate()
            .iter()
            .map(|i| i.message())
            .chain(keymap::Keymap::unknown_key_messages(&config.keybindings))
            .collect();
        for issue in &keybinding_issues {
            tracing::warn!("keybinding problem: {issue}");
        }
        let config_error_banner: Option<String> = config_err
            .as_ref()
            .map(|_| {
                "Your settings file couldn't be read, so the app is using default settings. \
                 Open it to check for typos, or restore the defaults."
                    .to_string()
            })
            .or_else(|| {
                keybinding_issues.first().map(|first| {
                    let rest = match keybinding_issues.len() - 1 {
                        0 => String::new(),
                        1 => " (and 1 more keybinding problem)".to_string(),
                        n => format!(" (and {n} more keybinding problems)"),
                    };
                    format!("Keyboard shortcut problem: {first}.{rest}")
                })
            });
        let mut toast = config_err.map(|_| {
            "Your settings file couldn't be read — using default settings for now.".to_string()
        });
        // Open every file passed on the command line / by the OS (multi-select,
        // `.desktop` `%F`, a default-app open of several files), in order. The
        // FIRST becomes the active tab (`restored_active` stays 0); the rest open
        // as background tabs. Any non-empty `cli_paths` suppresses session
        // restore below (the `tabs.is_empty()` guards) — explicit files win.
        for p in &cli_paths {
            match EditorTab::from_path(PathBuf::from(p)) {
                Ok(t) => tabs.push(t),
                Err(e) => {
                    toast = Some(format!("could not open {p}: {e}"));
                }
            }
        }
        // Restore the previous session when launched bare. With session
        // backups on (default), restore unsaved CONTENT too (hot exit) — incl.
        // untitled scratch notes — from the manifest + backup store. Otherwise
        // fall back to the legacy paths-only restore.
        let mut restored_active = 0usize;
        // Hot-exit restore reads the SHARED OS session manifest. Skipped under
        // `new_test` (watch_config = false) so a parallel test never inherits
        // another test's restored tabs (which would break tabs.len() asserts).
        if watch_config && tabs.is_empty() && config.editor.session_backup {
            if let Some((restored, active_idx)) =
                Self::restore_tabs_from_manifest(config.editor.restore_session)
            {
                tabs = restored;
                restored_active = active_idx;
            }
        }
        if watch_config && tabs.is_empty() && config.editor.restore_session {
            // Gated on `watch_config` for the SAME reason as the manifest restore
            // above: under `new_test` (watch_config = false) a test must NOT
            // inherit the host's real on-disk legacy session — that pollutes
            // `tabs.len()` assertions (e.g. `cli_with_no_files_opens_a_single_
            // scratch_tab` saw the dev machine's live session and restored 2 tabs
            // instead of the expected scratch). Production launches pass
            // watch_config = true, so real restore is unchanged.
            // R6 / S-04 — the legacy paths-only session file auto-opens too,
            // so it gets the same guard: a path that reaches off this machine
            // is never opened. (This call site used to build a "self-rooted"
            // allowed set from the listed paths' own parents — a fence that
            // could not fail. See `session_path_guard` for why it was removed
            // rather than repaired.)
            for path in load_session() {
                if !crate::session_path_guard::is_safe_restore_path(&path) {
                    tracing::debug!(
                        "session restore (legacy): skipping {} (resolves off this machine, \
                         or is gone) — not auto-opening",
                        path.display()
                    );
                    continue;
                }
                if let Ok(t) = EditorTab::from_path(path) {
                    tabs.push(t);
                }
            }
        }
        if tabs.is_empty() {
            tabs.push(EditorTab::scratch());
        }
        let session_sig = session_signature(&tabs);

        // Plugin discovery + trust-gating (#R6 / S-01 / S-02 / R7) is extracted
        // verbatim into `build_plugins::load_plugins` to keep `build` readable.
        let (plugins, pending_plugins, plugin_cmds) =
            build_plugins::load_plugins(&config, &mut toast);

        // Live-reload: watch the config dir for external edits to scr1b3.toml.
        // Skipped under `new_test` (watch_config = false): the watcher targets
        // the shared OS config dir, so in a parallel test run one test's
        // session/config write would fire every other test's watcher and
        // `reload_config_from_disk` would clobber its in-memory feature flags
        // with on-disk defaults (test-isolation race).
        let (cfg_tx, cfg_rx) = std::sync::mpsc::channel();
        let cfg_watcher = if watch_config {
            spawn_config_watcher(cfg_tx)
        } else {
            None
        };

        // Built from `config` before the struct literal moves `config` in.
        let spell = build_spell_engine(&config);
        let keymap_src = config.keybindings.clone();
        let keymap = keymap::Keymap::resolve(&keymap_src);

        let app = Self {
            config,
            keymap,
            keymap_src,
            issue_intake: crate::issue_intake::IssueIntakeState::default(),
            config_dir: Config::config_dir(),
            theme,
            last_os_theme: None,
            did_update_check: false,
            updater: crate::updater::Updater::default(),
            update_notice: None,
            hl: Highlighter::new(),
            active: restored_active.min(tabs.len().saturating_sub(1)),
            tabs,
            visuals_applied: false,
            applied_visuals_sig: 0,
            applied_font_family: String::new(),
            font_rebuild_pending: false,
            applied_note_theme: String::new(),
            want_close: false,
            closing: false,
            find_in_files_open: false,
            find_in_files_query: String::new(),
            find_in_files_regex: false,
            find_in_files_results: Vec::new(),
            find_in_files_error: None,
            focus_find_in_files: false,
            find_in_files_selected: 0,
            find_in_files_rx: None,
            find_in_files_running: false,
            zen_mode: false,
            snippets: load_snippets(),
            md_preview_open: false,
            diff_view_open: false,
            caret_trail: std::collections::VecDeque::new(),
            multi_cursor: crate::multi_cursor::MultiCursor::default(),
            column_anchor: None,
            mc_owner_doc: None,
            boot_glitch_started: None,
            find_open: false,
            find_query: String::new(),
            find_match_idx: 0,
            find_last_query: String::new(),
            replace_query: String::new(),
            focus_replace: false,
            config_error_banner,
            status: format!(
                "{} — {}",
                scribe_core::PRODUCT_NAME,
                scribe_core::PRODUCT_TAGLINE
            ),
            toast,
            plugins,
            pending_plugins,
            plugin_cmds,
            spell,
            palette_open: false,
            palette_selected: 0,
            palette_query: String::new(),
            settings_open: false,
            cheatsheet_open: false,
            recent_open: false,
            recent_selected: 0,
            recent_folders_open: false,
            recent_folders_selected: 0,
            pending_jump_bracket: false,
            pending_insert_datetime: false,
            pending_dup_selection: false,
            pending_toggle_task: false,
            pending_wrap_marker: None,
            pending_case: None,
            pending_format_table: false,
            welcome_open: welcome_on_launch,
            fuzzy_open: false,
            fuzzy_query: String::new(),
            fuzzy_index: Vec::new(),
            focus_fuzzy: false,
            fuzzy_selected: 0,
            goto_open: false,
            goto_query: String::new(),
            focus_goto: false,
            goto_symbol_open: false,
            goto_symbol_query: String::new(),
            focus_goto_symbol: false,
            goto_symbol_selected: 0,
            file_tree_root: None,
            file_tree_state: crate::filetree::FileTreeState::default(),
            plugin_manager: crate::plugin_manager::PluginManagerState::default(),
            crash_consent: crate::reporting::CrashConsentState::default(),
            lsp_registry: LspRegistry::with_defaults(),
            lsp: None,
            lsp_lang: None,
            diagnostics: Vec::new(),
            session_sig,
            last_backup_at: None,
            closed_tabs: Vec::new(),
            last_autosave_at: None,
            last_backup_sig: 0,
            hl_cache: std::cell::RefCell::new(None),
            hl_galley_cache: std::cell::RefCell::new(None),
            hl_inc_cache: std::cell::RefCell::new(IncrementalHighlightState::default()),
            spell_cache: std::cell::RefCell::new(None),
            symbol_cache: std::cell::RefCell::new(None),
            symbol_scan_count: std::cell::Cell::new(0),
            last_disk_poll_frame: u64::MAX,
            find_cache: std::cell::RefCell::new(None),
            find_recompute_count: std::cell::Cell::new(0),
            count_cache: std::cell::RefCell::new(None),
            count_recompute_count: std::cell::Cell::new(0),
            _cfg_watcher: cfg_watcher,
            cfg_rx: Some(cfg_rx),
            fold_view: false,
            folds: std::collections::BTreeSet::new(),
            completion: None,
            pending_scroll: None,
            pending_editor_action: None,
            scroll_metrics: (0.0, 1.0, 1.0),
            minimap_cache: std::cell::RefCell::new(None),
            minimap_draw_cache: std::cell::RefCell::new(None),
            minimap_drag_box: false,
            focus_find: false,
            focus_palette: false,
            line_gutter: Vec::new(),
            grid_tree: None,
            next_doc_id: crate::grid::DocIdAllocator::default(),
            grid_close_queue: Vec::new(),
            last_cursor_line_col: None,
            last_selection_chars: 0,
        };

        app
    }

    /// Drain the local crash-report spool per the user's opt-in posture. This is
    /// the PRODUCTION-only step (called from [`ScribeApp::new`], never from
    /// `new_test`) so a unit test that builds the app never reads/writes the real
    /// config dir's spool — the test-pollution leak this isolates. The spool is
    /// rooted at the app's per-instance resolved `config_dir` (the same dir all
    /// other config/session I/O uses), NEVER the global `Config::config_dir()`.
    ///
    /// The capture only ever spools when the user opted IN (so an `Off` user has
    /// an empty spool and nothing happens). `Always` auto-sends through the
    /// consent-gated path with no prompt; `AskEachTime` queues the consent dialog
    /// (rendered each frame). A `None` config dir means nowhere to spool — a no-op.
    fn drain_crash_spool(&mut self) {
        let Some(dir) = self.config_dir.clone() else {
            return;
        };
        match self.config.reporting.crash_reports {
            crate::reporting::ReportingMode::Always => {
                crate::reporting::auto_send_spooled_crashes(&dir);
            }
            crate::reporting::ReportingMode::AskEachTime => {
                self.crash_consent.set_config_dir(Some(dir));
                self.crash_consent.load_from_spool();
            }
            crate::reporting::ReportingMode::Off => {}
        }
    }

    /// Start (or reuse) a language server for the active file and open it.
    /// Graceful: missing/unconfigured servers just surface a notice.
    fn start_lsp_for_active(&mut self) {
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        // Match on the PATH first. `language_hint()` is derived from the path
        // (it is the file's extension), so `Some(lang)` cannot occur without a
        // path — matching on the hint first made the `(_, None)` arm dead code,
        // and an UNSAVED buffer fell into the no-language arm and was told
        // "couldn't detect this file's language" when the real problem is that
        // it has never been saved. Each case now gets the message written for it.
        let (lang, path, text) = match self.tabs.get(active) {
            Some(t) => match (t.doc.path(), t.doc.language_hint()) {
                (Some(path), Some(lang)) => (lang, path.to_path_buf(), t.text.clone()),
                (None, _) => {
                    self.toast =
                        Some("Save the file first, then start the language server.".into());
                    return;
                }
                (Some(_), None) => {
                    self.toast = Some(
                        "Couldn't detect this file's language. Save it with a file extension \
                         (like .rs or .py) to enable language features."
                            .into(),
                    );
                    return;
                }
            },
            None => return,
        };
        if self.lsp.is_some() && self.lsp_lang.as_deref() == Some(lang.as_str()) {
            self.status = "language server already running".into();
            return;
        }
        let Some(cfg) = self.lsp_registry.for_language(&lang).cloned() else {
            self.toast = Some(format!(
                "No language server is set up for .{lang} files. You can add one in Settings."
            ));
            return;
        };
        let root = self
            .file_tree_root
            .clone()
            .or_else(|| path.parent().map(|p| p.to_path_buf()));
        let root_uri = root.map(|r| path_to_uri(&r)).unwrap_or_default();
        match LspClient::spawn(&cfg, &root_uri) {
            Ok(mut client) => {
                if let Err(e) = client.did_open(&path_to_uri(&path), &lang, &text) {
                    tracing::warn!(
                        "language server did_open failed; diagnostics may not appear: {e}"
                    );
                }
                self.lsp = Some(client);
                self.lsp_lang = Some(lang);
                self.diagnostics.clear();
                self.status = format!("language server started: {}", cfg.command);
            }
            Err(e) => {
                tracing::warn!("language server '{}' failed to start: {e}", cfg.command);
                self.toast = Some(
                    "Couldn't start the language server. Check that it's installed and \
                     available on your PATH."
                        .into(),
                );
            }
        }
    }

    /// Open a file path in a new tab (or surface an error toast).
    fn open_path(&mut self, path: PathBuf) {
        // One-tab-per-file: if this file is already open, focus that tab rather
        // than opening a second copy. An un-deduped open was the upstream cause
        // of the "same note opened twice" duplication — once two tabs shared a
        // path they were both persisted to the session manifest and reappeared
        // every restart. Match by canonical path, then compare with the host-FS
        // identity helper (case-/separator-insensitive on Windows, case-sensitive
        // on POSIX) so a casefold-different existing file still dedups even when
        // canonicalize falls back to the raw path on either side.
        let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if let Some(idx) = self.tabs.iter().position(|t| {
            t.doc
                .path()
                .map(|p| {
                    let other = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
                    scribe_core::path_norm::paths_equal_for_compare(&other, &canon)
                })
                .unwrap_or(false)
        }) {
            self.active = idx;
            self.status = format!("already open: {}", path.display());
            return;
        }
        match EditorTab::from_path(path.clone()) {
            Ok(t) => {
                self.tabs.push(t);
                self.active = self.tabs.len() - 1;
                self.status = format!("opened {}", path.display());
                // F-021 — restore the prior per-file scroll position
                // (best-effort; the picker accepts a 1-frame lag).
                let key = path.display().to_string();
                if let Some(&y) = self.config.editor.scroll_positions.get(&key) {
                    self.pending_scroll = Some(y);
                }
                // Restore the prior caret position (best-effort). Applies to
                // the owned rope editor; the egui TextEdit path approximates
                // via the restored scroll offset.
                if self.config.editor.restore_cursor_position {
                    if let Some(&cur) = self.config.editor.cursor_positions.get(&key) {
                        let idx = self.tabs.len() - 1;
                        let clamped = cur.min(self.tabs[idx].text.chars().count());
                        let mut st = scribe_render::RopeEditorState::new();
                        st.edit = scribe_core::editing::EditState::at(clamped);
                        self.tabs[idx].rope_state = Some(st);
                    }
                }
                // F-012 — record on the MRU recent-files list + persist.
                scribe_core::config::record_recent_file(&mut self.config.editor.recent_files, path);
                self.save_config();
            }
            Err(e) => {
                tracing::warn!("open failed for {}: {e}", path.display());
                self.toast = Some(
                    "Couldn't open the file. It may have been moved or deleted, or you may \
                     not have permission to read it."
                        .into(),
                );
            }
        }
    }

    /// Reload config from disk (external edit) and re-apply derived state.
    fn reload_config_from_disk(&mut self, ctx: &egui::Context) {
        let (cfg, err) = Config::load_or_default();
        if let Some(e) = err {
            tracing::warn!("settings reload failed, keeping previous settings: {e}");
            self.toast = Some(
                "Your settings file couldn't be read, so your previous settings are still \
                 in use. Check it for typos."
                    .into(),
            );
            return;
        }
        // Skip if unchanged (e.g. our own save echoed back by the watcher).
        if cfg == self.config {
            return;
        }
        self.config = cfg;
        self.reapply_theme(ctx);
        self.status = "config reloaded".to_string();
    }

    /// Run a plugin command against the active buffer, applying any text
    /// transform and surfacing notifications.
    fn run_plugin_command(&mut self, command_id: &str) {
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        let mut pctx = PluginContext::new(self.tabs[active].text.clone());
        match self.plugins.run_command(command_id, &mut pctx) {
            Ok(()) => {
                self.tabs[active].set_text(pctx.text);
                if let Some(n) = pctx.notifications.last() {
                    self.status = n.clone();
                }
            }
            // `e` is a `CoreError::Plugin(msg)` whose `Display` renders the bare
            // author-supplied message. Keep that short message (it is the
            // plugin's own user-facing text), but frame it plainly and make
            // clear the buffer was left untouched; the full error also goes to
            // the log for diagnosis.
            Err(e) => {
                tracing::warn!("plugin command '{command_id}' failed: {e}");
                self.toast = Some(
                    "The plugin couldn't finish that action. Your text was left unchanged.".into(),
                );
            }
        }
    }

    fn save_config(&mut self) {
        // Use the instance's resolved config dir (test-isolated in `new_test`),
        // NOT the global `Config::config_file_path()`, so tests never write the
        // real user `scr1b3.toml`.
        let Some(path) = self.config_dir.as_ref().map(|d| d.join("scr1b3.toml")) else {
            return;
        };
        // Atomic, never-empty write (Config::save_to): the config WATCHER must
        // never observe a partial file, or it reloads later-section defaults over
        // the in-memory settings (the "settings revert after reopen" bug).
        match self.config.save_to(&path) {
            Ok(()) => self.status = "settings saved".to_string(),
            Err(e) => {
                tracing::warn!("settings save failed: {e}");
                crate::action_log::record("error", &format!("settings save failed: {e}"));
                self.toast = Some(
                    "Couldn't save your settings. Check that the settings folder is writable."
                        .into(),
                );
            }
        }
    }

    /// Build the plugin-manager Loaded-tab rows by re-running discovery over
    /// the plugins dir and folding in the user's `config.plugins.disabled`
    /// set. Re-discovering (rather than reading the live `PluginHost`) means
    /// the modal shows plugins that are present on disk even when they are
    /// currently disabled — disabled plugins are never loaded into the host.
    fn discovered_plugin_rows(&self, plugins_dir: &Path) -> Vec<crate::plugin_manager::LoadedRow> {
        let (found, _errors) = plugin::discover(plugins_dir);
        found
            .into_iter()
            .map(|p| {
                let id = p.manifest.id;
                crate::plugin_manager::LoadedRow {
                    enabled: !self.config.plugins.disabled.contains(&id),
                    pending: self.pending_plugins.contains(&id),
                    name: p.manifest.name,
                    version: p.manifest.version,
                    description: p.manifest.description,
                    id,
                }
            })
            .collect()
    }

    /// #R6 — approve a pending plugin: hash its current entry script, record
    /// that hash in `config.plugins.trusted`, persist the config, and load the
    /// script into the live host so it runs immediately (no restart needed).
    fn approve_plugin(&mut self, id: &str) {
        // Resolve the per-instance config dir (production sets this to
        // `Config::config_dir()`; tests redirect it to a temp dir) so the
        // approval path uses the same dir as every other config/session write.
        let Some(dir) = self.config_dir.clone() else {
            return;
        };
        let (found, _errors) = plugin::discover(&dir.join("plugins"));
        let Some(p) = found.into_iter().find(|p| p.manifest.id == id) else {
            return;
        };
        let Ok(src) = std::fs::read_to_string(p.entry_path()) else {
            self.toast = Some(format!(
                "Couldn't read the '{id}' plugin's files. Try reinstalling it."
            ));
            return;
        };
        // SEC-3 — clicking "Approve & run" IS explicit first-contact consent, but
        // under `require_signed` the approve path must enforce the SAME strict
        // policy as the load path (`build_plugins`), not a weaker one:
        //   1. a signed plugin MUST carry BOTH an `author_pubkey` AND a
        //      `signature` — an unsigned/partial plugin is refused (the load
        //      path's `_ => false` "require_signed is on but it is unsigned" arm);
        //   2. the minisign signature over the entry script MUST verify;
        //   3. the pinned-key trust gate (`decide_approval`) must allow it —
        //      a first-seen key is pinned + upgraded to Allow, but a CHANGED key
        //      yields `BlockKeyChanged` and is refused (rotation requires the
        //      explicit `replace_with_consent` path in Settings, never this
        //      button).
        // Consent never downgrades any of these checks; it only upgrades a New
        // first-contact key to Allow.
        if self.config.plugins.require_signed {
            let (Some(pk), Some(sig)) = (
                p.manifest.author_pubkey.as_deref(),
                p.manifest.signature.as_deref(),
            ) else {
                tracing::warn!("plugin '{id}' approval rejected: unsigned in signed-only mode");
                self.toast = Some(format!(
                    "'{id}' was NOT approved — signed-plugin mode only runs plugins that \
                     are signed by their author. Reinstall it from a source that provides \
                     a signed build."
                ));
                return;
            };
            if scribe_core::update::verify::verify_signature(src.as_bytes(), sig, pk).is_err() {
                tracing::warn!("plugin '{id}' approval rejected: signature did not verify");
                self.toast = Some(format!(
                    "'{id}' was NOT approved — its signature couldn't be verified, so it may \
                     have been tampered with. Reinstall it from a source you trust."
                ));
                return;
            }
            let mut key_store = scribe_core::plugin::PinnedKeyStore::new(&dir);
            match scribe_core::plugin::pinned_keys::decide_approval(&mut key_store, id, pk) {
                Ok(scribe_core::plugin::pinned_keys::PluginLoadDecision::BlockKeyChanged {
                    old,
                    new,
                }) => {
                    tracing::warn!("plugin '{id}' author key changed: old={old} new={new}");
                    self.toast = Some(format!(
                        "'{id}' was NOT approved — its author key changed, which can mean \
                         the plugin was tampered with. Reinstall it from a source you \
                         trust, or approve the new key in Settings → Plugins."
                    ));
                    return;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("plugin '{id}' approval: key store read failed: {e}");
                    self.toast = Some(format!(
                        "Couldn't approve '{id}' — its security record couldn't be read. \
                         Try again, or reinstall the plugin."
                    ));
                    return;
                }
            }
        }
        let sha = scribe_core::update::verify::sha256_hex(src.as_bytes());
        self.config.plugins.trusted.insert(id.to_string(), sha);
        self.save_config();
        match self.plugins.load_script(id, &src) {
            Ok(()) => {
                self.pending_plugins.retain(|p| p != id);
                self.status = format!("approved + loaded plugin '{id}'");
            }
            Err(e) => {
                tracing::warn!("plugin '{id}' trusted but failed to load: {e}");
                self.toast = Some(format!(
                    "Approved '{id}', but it couldn't load — it may not be compatible with \
                     this version."
                ));
            }
        }
    }

    fn new_tab(&mut self) {
        self.tabs.push(EditorTab::scratch());
        self.active = self.tabs.len() - 1;
        crate::action_log::record("tab", "new");
    }
}

// Deferred action flags so we don't borrow `ctx.input` and `self` mutably at once.
//
// F-006 wave-1 extensions from docs/audits/overlooked-surfaces-2026-05-29.md:
// close_active_tab (Ctrl+W) / toggle_grid (Ctrl+\\) / cycle_tab_next /
// cycle_tab_prev (Ctrl+Tab / Ctrl+Shift+Tab).
#[derive(Default)]
struct Pending {
    new: bool,
    open: bool,
    open_folder: bool,
    save: bool,
    close_active_tab: bool,
    toggle_grid: bool,
    cycle_tab_next: bool,
    cycle_tab_prev: bool,
    /// Wave-2 (docs/audits/overlooked-surfaces-2026-05-29.md): Ctrl+H opens
    /// the find bar with focus pre-set to the replace field. Ctrl+/ toggles
    /// the line-comment prefix for every line in the selection. F11 toggles
    /// OS fullscreen. Files dropped onto the window open as new tabs.
    open_replace: bool,
    toggle_comment: bool,
    toggle_fullscreen: bool,
    files_to_open: Vec<PathBuf>,
    /// F-017 from docs/audits/overlooked-surfaces-2026-05-29.md:
    /// Alt+Up / Alt+Down swap the cursor line with its neighbour;
    /// Ctrl+Shift+D duplicates the cursor line in-place; Ctrl+J joins
    /// the cursor line with the next.
    move_line_up: bool,
    move_line_down: bool,
    duplicate_line: bool,
    jump_bracket: bool,
    join_lines: bool,
    /// Wave-3 (docs/audits/overlooked-surfaces-2026-05-29.md): F-018 theme
    /// cycle keyboard chord + F-031 minimap-toggle keyboard chord.
    cycle_theme: bool,
    toggle_minimap: bool,
    /// F-010 from docs/audits/overlooked-surfaces-2026-05-29.md: open the
    /// Ctrl+P fuzzy file finder.
    open_fuzzy: bool,
    /// F-032 from docs/audits/overlooked-surfaces-2026-05-29.md:
    /// Ctrl+Shift+[ folds every region in the active buffer (and switches
    /// the editor into fold view so the change is visible); Ctrl+Shift+]
    /// expands every region.
    fold_all: bool,
    expand_all: bool,
    /// Font zoom: `Some(1)` zoom in, `Some(-1)` zoom out, `Some(0)` reset to
    /// the default size (Ctrl+= / Ctrl+- / Ctrl+0 and Ctrl+scroll).
    font_zoom: Option<i8>,
    /// Reopen the most recently closed tab (Ctrl+Shift+R).
    reopen_tab: bool,
    /// Line bookmarks: Ctrl+F2 toggles a bookmark on the cursor line; F2 jumps
    /// to the next bookmark; Shift+F2 jumps to the previous one.
    toggle_bookmark: bool,
    next_bookmark: bool,
    prev_bookmark: bool,
}
/// Build a toolbar WidgetText with optional JP-glyph annotation. When
/// `jp_glyph_labels` is on AND the action has a verified-canonical kanji,
/// the kanji is appended after the primary label at smaller size and
/// reduced opacity — the "instrument plate" effect (T17.5). When OFF or
/// when no verified kanji exists, returns the primary label unchanged.
pub(crate) fn toolbar_widget(
    id: &str,
    icons: bool,
    jp_glyphs: bool,
    size: f32,
    primary_color: Color32,
) -> egui::WidgetText {
    let primary = toolbar_label(id, icons);
    let kanji = if jp_glyphs { jp_glyph(id) } else { None };
    let Some(kanji) = kanji else {
        // Size the primary glyph/label by `toolbar.icon_size_px` so the slider
        // is live for the common (no-kanji) case too. `primary_color` is
        // `Color32::PLACEHOLDER` for plain buttons (follow the widget's own fg,
        // which keeps the hover/active brightening) and a concrete accent for a
        // SELECTED toggle (see below).
        return egui::RichText::new(primary)
            .size(size)
            .color(primary_color)
            .into();
    };
    use egui::text::LayoutJob;
    // The kanji "instrument plate" keeps the original 10:14 size ratio relative
    // to the primary, so it scales with the icon-size slider.
    let kanji_size = size * (10.0 / 14.0);
    let mut job = LayoutJob::default();
    job.append(
        primary,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(size),
            // #22/#105 — the primary colour is supplied by the caller. PLACEHOLDER
            // makes the widget substitute its normal text colour, so an unselected
            // English label is the SAME colour whether kanji labels are on or off.
            // A SELECTED toolbar TOGGLE passes a CONCRETE accent here so the label
            // stays theme-accent in BOTH kanji-on and kanji-off states — egui's
            // `selectable_label` only recolours PLACEHOLDER text to its strong
            // contrast colour, so a concrete accent survives the selected state
            // (kanji-on previously rendered white because the LayoutJob's
            // PLACEHOLDER primary was recoloured by the selected widget).
            color: primary_color,
            ..Default::default()
        },
    );
    // Only the appended kanji is tinted (a dim "instrument-plate" colour) — a
    // different colour for the kanji, never for the English text.
    job.append(
        &format!("  {kanji}"),
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(kanji_size),
            color: egui::Color32::from_rgba_unmultiplied(180, 180, 180, 160),
            ..Default::default()
        },
    );
    egui::WidgetText::LayoutJob(job.into())
}

fn ui_color(theme: &Theme, key: &str, default: Rgba) -> Color32 {
    scribe_render::color32(theme.ui(key, default))
}

/// Build a synthetic key-press egui event (used to drive `TextEdit`'s native
/// undo/redo from the command palette). `physical_key` is left `None` — egui's
/// editing commands match on the logical `key` + `modifiers`.
fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

/// Read the OS clipboard text for a palette-driven Paste. Uses `arboard`
/// (already in the dependency tree via eframe) so we can pull text on demand —
/// egui exposes no clipboard *read* API outside its own paste-event flow.
/// Returns a short error string on any failure (e.g. Wayland without focus).
fn read_clipboard_text() -> Result<String, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.get_text().map_err(|e| e.to_string())
}

/// Write `text` to the OS clipboard (used by "Copy file path"). Returns an
/// error string rather than panicking so the caller can surface a toast.
fn write_clipboard_text(text: &str) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text.to_string()).map_err(|e| e.to_string())
}

/// Open `dir` in the OS file manager (Explorer / Finder / xdg-open).
/// Best-effort — a failure is silent because "couldn't reveal the folder"
/// is a non-fatal convenience action. Used by the plugin manager's
/// "open folder" button (F-039) so users can drop a plugin in directly.
fn open_in_file_manager(dir: &Path) {
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";
    let _ = std::process::Command::new(program).arg(dir).spawn();
}

/// The fill color for chrome panels (titlebar/toolbar/status/sidebars/gutter).
/// In an effectively-translucent window the alpha is lowered to `window.opacity`
/// so the OS blur (Mica/acrylic/vibrancy) or the desktop shows through the
/// chrome — not just the central editor. When the master transparency toggle is
/// off (or the mode is opaque) the panel stays fully opaque.
/// Bundled monospace "font themes" (#87): (display name, internal family key).
/// Every face is OFL-licensed and embedded at compile time. The display names
/// are what the Settings picker shows and what `fonts.editor_family` stores.
pub(crate) const FONT_FAMILIES: &[(&str, &str)] = &[
    ("JetBrains Mono", "JetBrainsMono"),
    ("IBM Plex Mono", "IBMPlexMono"),
    ("Fira Mono", "FiraMono"),
    ("Space Mono", "SpaceMono"),
    ("Cousine", "Cousine"),
    ("Source Code Pro", "SourceCodePro"),
    ("B612 Mono", "B612Mono"),
    ("Share Tech Mono", "ShareTechMono"),
    ("VT323", "VT323"),
    // Wave 4 — brand display + accent faces. Several are display/proportional
    // and best used as the App-UI font rather than the note body; all cover
    // Basic Latin so none tofu when chosen.
    ("Doto", "Doto"),
    ("Major Mono Display", "MajorMonoDisplay"),
    ("Chakra Petch", "ChakraPetch"),
    ("Wallpoet", "Wallpoet"),
    ("Michroma", "Michroma"),
    ("Red Hat Mono", "RedHatMono"),
    ("Teko", "Teko"),
    ("Rajdhani", "Rajdhani"),
    ("Saira", "Saira"),
    ("Zen Dots", "ZenDots"),
    ("Syncopate", "Syncopate"),
    ("Spline Sans Mono", "SplineSansMono"),
];

/// Selectable note (editor) colour themes (#104) — the syntect bundled set
/// (`ThemeSet::load_defaults`). The Settings picker lists these; an unknown
/// value is ignored by `Highlighter::set_theme`, so the list staying in sync is
/// best-effort, never load-bearing.
pub(crate) const NOTE_THEMES: &[&str] = &[
    "base16-eighties.dark",
    "base16-mocha.dark",
    "base16-ocean.dark",
    "base16-ocean.light",
    "InspiredGitHub",
    "Solarized (dark)",
    "Solarized (light)",
    // #26 — bundled brand note themes (see `Highlighter::add_bundled_themes`).
    "Wired Noir",
    "Phosphor Amber",
    "Operator Violet",
    // Popular note palettes (see `Highlighter::add_bundled_themes`).
    "Dracula",
    "Nord",
    "Gruvbox Dark",
    "Tokyo Night",
    "Monokai",
    "One Dark",
    "Catppuccin Mocha",
    "Rosé Pine",
    "GitHub Light",
    "Catppuccin Latte",
];

impl eframe::App for ScribeApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Transparent for frameless rounded corners.
        [0.0, 0.0, 0.0, 0.0]
    }

    // eframe::App::save runs on graceful shutdown and (with the `persistence`
    // feature on) periodically while the app is running. We use it to flush the
    // grid layout, the canonical TOML config, and the session backup. Native
    // window geometry is persisted separately by eframe itself (persist_window).
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        // #R6 — persist the multi-note grid layout so a split arrangement
        // survives a restart (restored in `sync_grid_state` when the panes
        // match the reopened doc set). Only meaningful while the grid is on.
        self.config.editor.grid_layout = if self.config.editor.grid_enabled {
            self.grid_tree.as_ref().and_then(crate::grid::to_json)
        } else {
            self.config.editor.grid_layout.take()
        };
        // We don't use eframe's own Storage (no JSON-blob serialisation —
        // SCR1B3 owns its config). Persist via save_config, which writes the
        // single canonical TOML.
        self.save_config();
        // Hot-exit: eframe calls save() periodically AND on shutdown, so this
        // guarantees the latest unsaved content is backed up before exit.
        if self.config.editor.session_backup {
            self.snapshot_session_backups();
        }
    }

    // eframe 0.34: `App::ui(&mut self, &mut Ui, &mut Frame)` is the new required
    // entry; the prior `update(&mut Context, &mut Frame)` is deprecated. We keep
    // driving panels via top-level `CentralPanel::show(ctx)` (under the
    // module-level allow(deprecated)) so the passed-in Ui is unused; the per-
    // frame logic lives in the inherent `frame_tick(&Context)` so the headless
    // egui_kittest tests can drive it without an `eframe::Frame`.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let _ = ui;
        // Feed the REAL native window handle to the Win32 caption-button fix.
        // eframe's `Frame` implements `HasWindowHandle`, so this is the authentic
        // HWND of THIS window — unlike the prior `EnumWindows` guess, which could
        // (and likely did) latch onto the wrong top-level window, defeating every
        // earlier caption-strip attempt. Handle access is safe (no `unsafe`).
        #[cfg(windows)]
        if self.config.appearance.frameless {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = _frame.window_handle() {
                if let RawWindowHandle::Win32(w) = handle.as_raw() {
                    scribe_win32_chrome::set_main_hwnd(w.hwnd.get());
                }
            }
        }
        self.frame_tick(&ctx);
    }
}

/// Transient middle-click-autoscroll state, parked in egui's per-`Context`
/// data store (`ctx.data`) rather than on `ScribeApp` so the feature is fully
/// self-contained in [`ScribeApp::apply_scroll_settings`] — no struct field or
/// constructor wiring. `anchor` is the screen-space origin where the wheel was
/// clicked; while `active`, content drifts at a velocity proportional to the
/// pointer's offset from `anchor`.
#[derive(Clone, Copy)]
struct AutoScrollState {
    active: bool,
    anchor: egui::Pos2,
}

impl Default for AutoScrollState {
    fn default() -> Self {
        Self {
            active: false,
            anchor: egui::Pos2::ZERO,
        }
    }
}

/// A titlebar caption button (minimize / maximize / restore / close). Icons are
/// painter-drawn so they never depend on font glyph coverage, sized to a
/// comfortable 46x28 hit target (Windows 11 caption metric) with a hover fill —
/// close gets the conventional red hover, the rest a soft white wash.
#[derive(Clone, Copy)]
enum CaptionIcon {
    Minimize,
    Maximize,
    Restore,
    Close,
    /// The settings "gear", relocated into the caption row (left of Minimize)
    /// from the quick-access toolbar. Painter-drawn like the others.
    Settings,
}

/// How many of `n` equal-width toolbar items fit in `avail` logical px before
/// overflow. If they all fit, returns `n` (no overflow dropdown needed for
/// them). Otherwise it RESERVES `dropdown_w` for the "⋯ more actions" trigger
/// and returns how many whole items fit in the remaining width — the rest fold
/// into the dropdown so they stay reachable instead of being clipped off the
/// edge (the narrow-window "buttons cut off / unclickable" report). Pure +
/// unit-tested so the in-titlebar toolbar's compress-then-overflow behaviour
/// can't silently regress; `item_w` is an estimate (configured button size +
/// spacing) so the split is graceful, never catastrophic.
fn toolbar_visible_count(avail: f32, item_w: f32, dropdown_w: f32, n: usize) -> usize {
    if n == 0 || item_w <= 0.0 {
        return n;
    }
    if (n as f32) * item_w <= avail {
        return n; // everything fits; no overflow trigger required
    }
    let usable = (avail - dropdown_w).max(0.0);
    ((usable / item_w).floor() as usize).min(n)
}

mod build_plugins;
mod builtins;
mod chrome;
mod deferred_actions;
/// The single seam to the OS file dialogs — headless under `cfg(test)`.
pub(crate) mod dialogs;
mod drag_scroll;
mod editor_overlays;
mod file_ops;
mod find_nav;
mod find_replace;
mod frame_modals;
mod frame_tick;
mod grid_methods;
mod grid_render;
mod keyboard_input;
mod keymap;
mod modals;
mod multi_cursor_glue;
mod render_support;
// Re-export the rendering & text-geometry leaf helpers so existing bare-name
// call sites in mod.rs, the `use super::*` siblings (frame_tick, editor_overlays,
// find_nav, …), and `super::grip_handle` (grid_render) resolve unchanged after
// the A-01 wave-3 extraction. Mirrors the `commands` re-export above.
pub(crate) use render_support::{
    apply_indent, build_fonts, byte_to_char_index, char_to_byte, completion_popup,
    ensure_readable_tone, font_state_key, grip_handle, line_col_from_char_index, load_snippets,
    load_theme, make_layouter, matching_bracket_char_indices, newline_with_indent, paint_squiggle,
    panel_fill, pick_bookmark, spawn_config_watcher, use_rope_editor,
};
mod session_io;
mod session_persist;
mod tab_strip_render;
mod tabs;
mod text_analysis;
mod text_ops_methods;
mod theme_visuals;
mod toolbar_render;
use chrome::{caption_btn, handle_frameless_resize};

mod effects;
use effects::{
    paint_boot_glitch, paint_caret_trail, paint_crt_scanlines, paint_flicker, paint_tint_overlay,
    paint_vhs_tracking, paint_wired_mesh,
};

/// PURE geometry for the drop-insertion line on a VERTICAL side-tab strip.
///
/// Returns the y at which to draw the full-column-width insertion hairline for a
/// drop landing *before* the row at `idx` (whose top edge is `row_top`).
/// `prev_bottom` is the bottom edge of the row above (`None` for the first row).
/// The line sits in the inter-row GAP — the midpoint between the previous row's
/// bottom and this row's top — so it reads as a separator BETWEEN tabs. The bug
/// it fixes drew the line at `row_top` across the chip's own grip/label/close
/// widgets (with only the chip's narrow width), so it appeared INSIDE the tab.
fn side_tab_insertion_y(idx: usize, row_top: f32, prev_bottom: Option<f32>) -> f32 {
    match prev_bottom {
        Some(pb) if idx > 0 => (pb + row_top) * 0.5,
        // First row (or no predecessor): just above its top edge.
        _ => row_top - 1.0,
    }
}

#[cfg(test)]
mod restore_dedup_tests;

#[cfg(test)]
mod resize_tests;

#[cfg(test)]
mod save_session_tests;

#[cfg(test)]
mod perf_and_security_tests;

#[cfg(test)]
mod wave3_perf_tests;

#[cfg(test)]
mod font_theme_tests;

#[cfg(test)]
mod background_override_tests;

#[cfg(test)]
mod spell_underline_tests;

#[cfg(test)]
mod wrap_tests;

#[cfg(test)]
mod indent_tests;

#[cfg(test)]
mod text_ops_tests;

#[cfg(test)]
mod text_ops_selection_tests;

#[cfg(test)]
mod session_io_tests;

#[cfg(test)]
mod deferred_actions_tests;

#[cfg(test)]
mod keyboard_input_tests;

#[cfg(test)]
mod mod_logic_tests;

#[cfg(test)]
mod execute_builtin_tests;

#[cfg(test)]
mod foreground_area_guard;

#[cfg(test)]
mod jp_glyph_tests;

#[cfg(test)]
mod tab_reorder_tests;

#[cfg(test)]
mod sidetab_drop_indicator_tests;

#[cfg(test)]
mod multi_file_open_tests;

#[cfg(test)]
mod change_bar_tests;

#[cfg(test)]
mod visual_qa;

#[cfg(test)]
mod visual_regression;

#[cfg(test)]
mod a11y_audit;

#[cfg(test)]
mod e2e_overlays;

#[cfg(test)]
mod update_reminder_tests;

#[cfg(test)]
mod e2e;

#[cfg(test)]
mod qa_fixtures;

#[cfg(test)]
mod perframe_cache_tests;

#[cfg(test)]
mod find_in_files_tests;

#[cfg(test)]
mod grid_pane_tests;

#[cfg(test)]
mod filetree_tests;

#[cfg(test)]
mod report_issue_tests;

#[cfg(test)]
mod theme_editor_tests;

#[cfg(test)]
mod plugin_manager_tests;

#[cfg(test)]
mod qa_correctness_workflow_tests;

#[cfg(test)]
mod qa_largefile_scale_tests;

#[cfg(test)]
mod qa_security_workflow_tests;

#[cfg(test)]
mod qa_app_workflow_tests;

#[cfg(test)]
mod qa_find_scale_tests;

#[cfg(test)]
mod qa_session_scale_tests;

#[cfg(test)]
mod appnav_keyboard_tests;

#[cfg(test)]
mod sec3_approve_keytrust_tests;

#[cfg(test)]
mod enc1_reload_wiring_tests;

#[cfg(test)]
mod logging_tests;

#[cfg(test)]
mod error_message_tests;

#[cfg(test)]
mod tabbar_layout_tests;
