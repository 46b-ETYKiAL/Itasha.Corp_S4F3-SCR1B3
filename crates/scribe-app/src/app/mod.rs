//! The SCR1B3 application shell: frameless brand titlebar, tab strip, syntect-
//! highlighted editor surface, find bar, and status bar. v1 keeps the shell in
//! one focused module; later phases split tabs/titlebar/chrome into submodules.

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
    /// out once, not every frame.
    minimap_cache: std::cell::RefCell<Option<(u64, std::sync::Arc<egui::Galley>)>>,
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
struct Completion {
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
        cli_path: Option<String>,
    ) -> Self {
        let mut app = Self::build(config, config_err, cli_path, true);
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
        let mut app = Self::build(config, None, None, false);
        // Redirect ALL config/session writes to a per-instance temp dir so a
        // test's periodic hot-exit snapshot never touches the real user config
        // dir. Without this, `session_backup`-on tests wrote their fixture text
        // into the real `%APPDATA%` session backup (test pollution).
        app.config_dir = Some(Self::unique_test_config_dir());
        app
    }

    /// A process-unique, per-call temp directory for hermetic test config I/O.
    #[cfg(test)]
    fn unique_test_config_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("scr1b3-test-{}-{n}", std::process::id()))
    }

    fn build(
        config: Config,
        config_err: Option<String>,
        cli_path: Option<String>,
        watch_config: bool,
    ) -> Self {
        let theme = load_theme(&config.appearance.theme);
        // F-013 — open the welcome modal on first launch only. Suppressed
        // when the user passed a file on the command line OR the recent-
        // files list is already populated (they've been here before).
        let welcome_on_launch = !config.editor.first_run_completed
            && cli_path.is_none()
            && config.editor.recent_files.is_empty();

        let mut tabs = Vec::new();
        // F-038 — keep the parse error in a persistent banner field rather
        // than only a one-shot toast. The banner sits above the editor and
        // surfaces "Open config / Restore default / Dismiss" actions.
        let config_error_banner: Option<String> = config_err.as_ref().cloned();
        let mut toast = config_err.map(|e| format!("config: {e} (using defaults)"));
        if let Some(p) = cli_path {
            match EditorTab::from_path(PathBuf::from(&p)) {
                Ok(t) => tabs.push(t),
                Err(e) => toast = Some(format!("could not open {p}: {e}")),
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
        if tabs.is_empty() && config.editor.restore_session {
            // R6 / S-04 — the legacy paths-only session file is also a
            // user-writable on-disk artifact. Apply the same restore guard:
            // reject UNC / nonexistent / root-escaping paths, self-rooting the
            // allowed set on the listed paths' own parent directories.
            let listed = load_session();
            let legacy_roots = crate::session_path_guard::allowed_roots(
                listed
                    .iter()
                    .filter(|p| !crate::session_path_guard::is_unc_path(p))
                    .filter_map(|p| p.parent())
                    .collect::<Vec<_>>(),
            );
            for path in listed {
                if !crate::session_path_guard::is_safe_restore_path(&path, &legacy_roots) {
                    tracing::warn!(
                        "session restore (legacy): skipping untrusted path {} — not auto-opening",
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

        let app = Self {
            config,
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

    /// Phase 18 T18.2 — render the egui_tiles grid as the central
    /// editor surface. Each leaf pane wraps a `TextEdit::multiline` over
    /// the matching tab's text. The `Option::take`-then-put-back idiom
    /// hands `&mut self` to the callbacks while keeping the tree owned
    /// across frames.
    fn render_grid_central_panel(&mut self, ctx: &egui::Context, font: egui::FontId) {
        // Snapshot the titles up front so the behavior callback doesn't
        // need to re-borrow `self.tabs` (which is also borrowed mutably
        // by the body callback).
        let titles: Vec<(crate::grid::DocId, String)> =
            self.tabs.iter().map(|t| (t.doc_id, t.title())).collect();
        let Some(mut tree) = self.grid_tree.take() else {
            return;
        };
        let line_height = self.config.fonts.clamped_line_height();
        let word_wrap = self.config.editor.word_wrap;
        // #28 — render-whitespace toggle + editor font size captured as locals so
        // the per-pane body closure (which can't re-borrow `self.config`) can
        // paint the `·`/`→` whitespace overlay on each pane's galley too.
        let render_whitespace = self.config.editor.render_whitespace;
        let editor_font_size = self.config.fonts.clamped_editor_size();
        // Disjoint-field borrows captured as locals BEFORE the central-panel
        // closure (which mutably borrows `self.tabs`). The highlighter + its
        // cache are different fields than `tabs`, so the immutable borrows here
        // and the closure's `&mut self.tabs` coexist under disjoint closure
        // capture.
        let hl = &self.hl;
        let hl_cache = &self.hl_cache;
        let hl_galley_cache = &self.hl_galley_cache;
        let hl_inc_cache = &self.hl_inc_cache;
        // Wave-3: theme foreground for the highlighter tail colour (per-pane).
        let layout_fg = ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
        // #R5: theme colours + focused-pane id for the chip-styled pane headers
        // (the per-pane note bar now mirrors the top tab strip's chip look).
        let accent = ui_color(&self.theme, "accent", Rgba::new(0, 255, 254, 255));
        let muted = ui_color(&self.theme, "line_number", Rgba::new(0x5a, 0x58, 0x69, 255));
        let active_doc = self.tabs.get(self.active).map(|t| t.doc_id);
        // Per-frame shared close buffer. The pane `✕` button writes here and
        // `AppGridBehavior::retain_pane` reads it back during the SAME
        // `tree.ui()` call, so egui_tiles prunes exactly the closed pane and
        // preserves the rest of the user's arrangement (no full rebuild).
        let closes: std::cell::RefCell<Vec<crate::grid::DocId>> =
            std::cell::RefCell::new(Vec::new());
        egui::CentralPanel::default().show(ctx, |ui| {
            let tabs = &mut self.tabs;
            let render_closes = &closes;
            let mut render_body = |ui: &mut egui::Ui, doc_id: crate::grid::DocId| -> bool {
                let Some(idx) = tabs.iter().position(|t| t.doc_id == doc_id) else {
                    ui.weak("(document closed)");
                    return false;
                };
                // Per-pane header chip (wide one-row / narrow centered column, pin +
                // close + drag handle) extracted verbatim into grid_render::render_pane_header.
                let is_active = active_doc == Some(doc_id);
                let drag_started = grid_render::render_pane_header(
                    ui,
                    &mut tabs[idx],
                    doc_id,
                    is_active,
                    accent,
                    muted,
                    render_closes,
                );
                // Per-pane syntax highlighting via the same memoizing layouter
                // the single-pane + split paths use, keyed on THIS pane's own
                // language hint — so each pane highlights for its own file type
                // instead of the old plain-text downgrade. The shared single-
                // slot `hl_cache` recomputes as focus moves between panes, which
                // is fine at the 6-pane ceiling.
                let ext = tabs[idx].doc.language_hint();
                let mut layouter = make_layouter(
                    hl,
                    hl_cache,
                    hl_galley_cache,
                    hl_inc_cache,
                    ext.as_deref(),
                    font.clone(),
                    line_height,
                    word_wrap,
                    layout_fg,
                );
                egui::ScrollArea::both()
                    .id_salt(("scr1b3-grid-pane", doc_id.raw()))
                    .show(ui, |ui| {
                        let editor = egui::TextEdit::multiline(&mut tabs[idx].text)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .desired_rows(20)
                            .layouter(&mut layouter);
                        let out = editor.show(ui);
                        // Wave-3: per-pane edit-gen bump (grid panes share the
                        // single-slot caches; a focus/edit change is a key change).
                        if out.response.changed() {
                            tabs[idx].edit_gen = tabs[idx].edit_gen.wrapping_add(1);
                        }
                        // #28 — same render-whitespace overlay as the single-pane
                        // editor, so the markers appear in split/grid view too.
                        if render_whitespace {
                            let painter = ui.painter();
                            let ws_font = egui::FontId::monospace(editor_font_size);
                            let ws_color = muted.gamma_multiply(0.7);
                            let origin = out.galley_pos.to_vec2();
                            for row in &out.galley.rows {
                                let row_off = origin + row.pos.to_vec2();
                                let cy = row_off.y + row.size.y * 0.5;
                                for g in &row.glyphs {
                                    let marker = match g.chr {
                                        ' ' => "·",
                                        '\t' => "→",
                                        _ => continue,
                                    };
                                    let cx = row_off.x + g.pos.x + g.advance_width * 0.5;
                                    painter.text(
                                        egui::pos2(cx, cy),
                                        egui::Align2::CENTER_CENTER,
                                        marker,
                                        ws_font.clone(),
                                        ws_color,
                                    );
                                }
                            }
                        }
                    });
                drag_started
            };
            let mut behavior = crate::grid::AppGridBehavior {
                titles: &titles,
                render_body: &mut render_body,
                close_requests: &closes,
            };
            tree.ui(&mut behavior, ui);
        });
        // Phase 18 T18.2 / #R6 — 6-pane cap, now actually ENFORCED:
        // `build_default_grid` caps the tree at MAX_PANES panes, so the grid
        // never shows more than six. When more tabs than that are open, the
        // extras stay open as tabs and we tell the user why they aren't gridded.
        let shown = crate::grid::count_panes(&tree);
        if self.tabs.len() > shown {
            self.toast = Some(format!(
                "Grid shows the first {} notes; {} more stay open as tabs. Close a pane to \
                 show another.",
                shown,
                self.tabs.len() - shown
            ));
        }
        // Drop the tabs the user closed via the pane chrome. `retain_pane`
        // already pruned the matching pane(s) during the frame, so here we only
        // remove the backing tabs — the surviving panes keep their positions.
        let to_close = closes.into_inner();
        if !to_close.is_empty() {
            for doc_id in to_close {
                self.tabs.retain(|t| t.doc_id != doc_id);
            }
            if self.tabs.is_empty() {
                self.tabs.push(EditorTab::scratch());
            }
        }
        // Reconcile additions: a tab opened while the grid is live has no pane
        // yet. Rebuild ONLY when the (capped) doc set actually differs from the
        // pane set, so steady-state editing and drag-rearranging never reset the
        // layout. The want-set is capped to MAX_PANES to match the capped tree —
        // otherwise a 7th tab would force a rebuild every frame.
        let docs: Vec<crate::grid::DocId> = self
            .tabs
            .iter()
            .map(|t| t.doc_id)
            .take(crate::grid::MAX_PANES)
            .collect();
        let want: std::collections::BTreeSet<crate::grid::DocId> = docs.iter().copied().collect();
        if want != crate::grid::pane_doc_ids(&tree) {
            tree = crate::grid::build_default_grid(&docs);
        }
        self.grid_tree = Some(tree);
    }

    /// Phase 18 T18.2 — assign stable doc_ids to any tab missing one
    /// (e.g. restored from a pre-grid session). Then ensure the
    /// `grid_tree` matches the user's `editor.grid_enabled` preference.
    /// Called at the top of `update` so the grid catches up to any
    /// config-reload that flipped the flag.
    fn sync_grid_state(&mut self) {
        // Pass 1: fill missing doc_ids so the grid has a stable id to
        // reference. DocId(0) is the legacy / unallocated sentinel.
        for tab in self.tabs.iter_mut() {
            if tab.doc_id.0 == 0 {
                // The allocator reserves 0 and starts at 1, so a single next()
                // always yields a real (non-sentinel) id.
                tab.doc_id = self.next_doc_id.next();
            }
            self.next_doc_id.observe(tab.doc_id);
        }
        // Pass 2: align tree state with the config flag.
        match (self.config.editor.grid_enabled, self.grid_tree.is_some()) {
            (true, false) => {
                let docs: Vec<crate::grid::DocId> = self
                    .tabs
                    .iter()
                    .map(|t| t.doc_id)
                    .take(crate::grid::MAX_PANES)
                    .collect();
                // #R6 — restore the persisted layout if it still references
                // exactly the reopened doc set (DocIds are assigned in tab order,
                // so a stable session reproduces them); otherwise fall back to a
                // fresh default grid. A corrupt/stale layout never blocks startup.
                let want: std::collections::BTreeSet<crate::grid::DocId> =
                    docs.iter().copied().collect();
                let restored = self
                    .config
                    .editor
                    .grid_layout
                    .as_deref()
                    .and_then(crate::grid::from_json)
                    .filter(|t| crate::grid::pane_doc_ids(t) == want);
                self.grid_tree =
                    Some(restored.unwrap_or_else(|| crate::grid::build_default_grid(&docs)));
            }
            (false, true) => {
                self.grid_tree = None;
                self.grid_close_queue.clear();
            }
            _ => {}
        }
    }

    /// Start (or reuse) a language server for the active file and open it.
    /// Graceful: missing/unconfigured servers just surface a notice.
    fn start_lsp_for_active(&mut self) {
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        let (lang, path, text) = match self.tabs.get(active) {
            Some(t) => match (t.doc.language_hint(), t.doc.path()) {
                (Some(lang), Some(path)) => (lang, path.to_path_buf(), t.text.clone()),
                (None, _) => {
                    self.toast = Some("no language detected for this file".into());
                    return;
                }
                (_, None) => {
                    self.toast = Some("save the file before starting a language server".into());
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
            self.toast = Some(format!("no language server configured for .{lang}"));
            return;
        };
        let root = self
            .file_tree_root
            .clone()
            .or_else(|| path.parent().map(|p| p.to_path_buf()));
        let root_uri = root.map(|r| path_to_uri(&r)).unwrap_or_default();
        match LspClient::spawn(&cfg, &root_uri) {
            Ok(mut client) => {
                let _ = client.did_open(&path_to_uri(&path), &lang, &text);
                self.lsp = Some(client);
                self.lsp_lang = Some(lang);
                self.diagnostics.clear();
                self.status = format!("language server started: {}", cfg.command);
            }
            Err(e) => self.toast = Some(format!("could not start {}: {e}", cfg.command)),
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
            Err(e) => self.toast = Some(format!("open failed: {e}")),
        }
    }

    /// Reload config from disk (external edit) and re-apply derived state.
    fn reload_config_from_disk(&mut self, ctx: &egui::Context) {
        let (cfg, err) = Config::load_or_default();
        if let Some(e) = err {
            self.toast = Some(format!("config: {e} (kept previous on disk)"));
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
            // message (no category prefix), so this `format!` reproduces the
            // pre-A-05 toast text byte-for-byte.
            Err(e) => self.toast = Some(format!("plugin error: {e}")),
        }
    }

    /// Count misspellings in the active buffer when spellcheck is enabled.
    ///
    /// P-08: reads the memoized vec length through a borrow -- it does NOT
    /// clone the cached `Vec<Misspelling>` (the status-bar count runs every
    /// frame, so the per-frame clone-just-to-call-`.len()` was pure waste).
    fn spell_count(&self) -> usize {
        self.with_active_misspellings(|m| m.len())
    }

    /// Ensure the active buffer misspelling memo is current and run `f` over a
    /// BORROW of the cached slice (no clone). Shared by the status-bar count
    /// (`spell_count`, which only needs `.len()`) and the squiggle painter
    /// (`misspellings_for_active`, which clones exactly once because its owned
    /// snapshot has to outlive a later `&mut self` borrow). `f` sees an empty
    /// slice when spellcheck is off or there is no active buffer.
    fn with_active_misspellings<R>(&self, f: impl FnOnce(&[spell::Misspelling]) -> R) -> R {
        if !self.config.spellcheck.enabled {
            return f(&[]);
        }
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        if self.tabs.get(active).is_none() {
            return f(&[]);
        }
        let key = self.spell_cache_key(active);
        // Cache hit: borrow the cached vec and hand the slice to `f` (no clone).
        if let Some((k, v)) = self.spell_cache.borrow().as_ref() {
            if *k == key {
                return f(v);
            }
        }
        // Miss: recompute, store, then hand the stored slice to `f` via borrow.
        let result = self.compute_misspellings(active);
        *self.spell_cache.borrow_mut() = Some((key, result));
        let slot = self.spell_cache.borrow();
        f(&slot.as_ref().expect("just stored above").1)
    }

    /// Content+config cache key for the active buffer misspelling memo: the
    /// per-tab `edit_gen` (so the scan re-runs only on a real edit, not every
    /// frame), the `doc_id` (disambiguates tabs sharing an `edit_gen` in the
    /// single-slot cache), the three scope toggles, and the language hint.
    fn spell_cache_key(&self, active: usize) -> u64 {
        use std::hash::{Hash, Hasher};
        let Some(tab) = self.tabs.get(active) else {
            return 0;
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        tab.edit_gen.hash(&mut h);
        tab.doc_id.raw().hash(&mut h);
        self.config.spellcheck.check_comments.hash(&mut h);
        self.config.spellcheck.check_strings.hash(&mut h);
        self.config.spellcheck.check_identifiers.hash(&mut h);
        tab.doc.language_hint().hash(&mut h);
        h.finish()
    }

    /// Misspellings in the active buffer (#78), memoized by a content+config
    /// hash so the dictionary scan runs once per changed frame and is shared by
    /// the status-bar count and the editor underline painter. Empty when
    /// spellcheck is off or there is no active buffer.
    ///
    /// P-08: this returns an OWNED snapshot because its caller (the editor
    /// closure) holds the result across a later `&mut self` borrow, so a
    /// `Ref`/`&[..]` cannot be used there. The clone is now confined to this
    /// one call site -- `spell_count` reads the cache via
    /// `with_active_misspellings` without cloning.
    fn misspellings_for_active(&self) -> Vec<spell::Misspelling> {
        self.with_active_misspellings(|m| m.to_vec())
    }

    /// Run the dictionary scan for the active buffer (the cache-miss body,
    /// factored out of `with_active_misspellings`).
    fn compute_misspellings(&self, active: usize) -> Vec<spell::Misspelling> {
        let Some(tab) = self.tabs.get(active) else {
            return Vec::new();
        };
        // Scope the check to the requested token classes (comments / strings /
        // identifiers) using the highlighter's classified spans, so the three
        // Settings toggles actually constrain what gets flagged. With all three
        // off, or no derivable syntax, `check_text_scoped` falls back to the
        // whole-text behavior (no regression).
        let scope = spell::SpellScope::new(
            self.config.spellcheck.check_comments,
            self.config.spellcheck.check_strings,
            self.config.spellcheck.check_identifiers,
        );
        let ext = tab.doc.language_hint();
        let spans = self.hl.classify_document(&tab.text, ext.as_deref());
        // Scoping (comments / strings / identifiers) is a CODE concept. When the
        // buffer has no code structure — an untitled note, plain text, markdown —
        // those classes don't apply, so check the whole document as prose. Only
        // when there are real comment/string/identifier spans do the toggles
        // constrain the check.
        let has_code_structure = spans
            .iter()
            .any(|s| !matches!(s.class, spell::SpanClass::Other));
        if has_code_structure {
            spell::check_text_scoped(&self.spell, &tab.text, &spans, scope)
        } else {
            spell::check_text(&self.spell, &tab.text, true)
        }
    }

    /// P-05: brace-delimited definition scopes for the active buffer, memoized
    /// by `(edit_gen, doc_id)` so the O(n) `symbol_scopes` char scan that drives
    /// the breadcrumb bar + sticky-scroll headers runs ONLY on an edit or a tab
    /// switch, not every frame. A 1-frame-stale breadcrumb after a keystroke is
    /// visually harmless (same rationale as the spell + minimap memos). Returns
    /// an owned snapshot because the caller holds it across a later `&mut self`
    /// borrow. Buffers over `MAX_SYMBOL_SCAN_BYTES` are not scanned (the scan
    /// stays bounded), matching the prior inline guard.
    fn symbol_scopes_for_active(&self) -> Vec<crate::editor_features::SymbolScope> {
        /// Upper buffer size for the breadcrumb/sticky symbol scan.
        const MAX_SYMBOL_SCAN_BYTES: usize = 500_000;
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        let Some(tab) = self.tabs.get(active) else {
            return Vec::new();
        };
        if tab.text.len() > MAX_SYMBOL_SCAN_BYTES {
            return Vec::new();
        }
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            tab.edit_gen.hash(&mut h);
            tab.doc_id.raw().hash(&mut h);
            h.finish()
        };
        if let Some((k, v)) = self.symbol_cache.borrow().as_ref() {
            if *k == key {
                return v.clone();
            }
        }
        // Cache miss: run the scan, record that it re-ran (proof counter), store.
        self.symbol_scan_count
            .set(self.symbol_scan_count.get().wrapping_add(1));
        let scopes = crate::editor_features::symbol_scopes(&tab.text);
        *self.symbol_cache.borrow_mut() = Some((key, scopes.clone()));
        scopes
    }

    /// PA-04 / PA-05 — memoized `(lines, words, chars)` for the tab at `active`,
    /// keyed by `(edit_gen, doc_id)`. The status bar (line/word/char readout) and
    /// the sticky line-number gutter (digit-width) both walked the WHOLE buffer
    /// every frame; this caches the three `O(n)` passes so they recompute ONLY on
    /// a real edit or a tab switch (a 1-frame-stale count after a keystroke is
    /// harmless — `edit_gen` moves on the next frame), not on every idle frame.
    /// Word/char are 0 for `is_read_only_large()` buffers (the multi-GB rope-
    /// browser path), exactly as the un-memoized status bar short-circuited.
    /// Mirrors the `symbol_scopes_for_active` / `spell_count` memo idiom.
    fn doc_counts_active(&self, active: usize) -> DocCounts {
        let Some(tab) = self.tabs.get(active) else {
            return (1, 0, 0);
        };
        if let Some((gen, doc, counts)) = self.count_cache.borrow().as_ref() {
            if *gen == tab.edit_gen && *doc == tab.doc_id {
                return *counts;
            }
        }
        // Cache miss: re-walk the buffer once, record the re-walk (proof counter),
        // store keyed on (edit_gen, doc_id).
        self.count_recompute_count
            .set(self.count_recompute_count.get().wrapping_add(1));
        let lines = tab.text.lines().count().max(1);
        let (words, chars) = if tab.doc.is_read_only_large() {
            (0, 0)
        } else {
            (
                tab.text.split_whitespace().count(),
                tab.text.chars().count(),
            )
        };
        let counts = (lines, words, chars);
        *self.count_cache.borrow_mut() = Some((tab.edit_gen, tab.doc_id, counts));
        counts
    }

    /// Rebuild the spell engine from the current config — called after the user
    /// changes the spellcheck language or custom dictionary in Settings so the
    /// new dictionary takes effect without a restart.
    fn reload_spell_engine(&mut self) {
        self.spell = build_spell_engine(&self.config);
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
                let msg = format!("could not save settings: {e}");
                crate::action_log::record("error", &msg);
                self.toast = Some(msg);
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
            self.toast = Some(format!("could not read plugin '{id}' entry script"));
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
                self.toast = Some(format!(
                    "plugin '{id}' NOT approved: signed mode requires a signed plugin \
                     (missing author_pubkey or signature)"
                ));
                return;
            };
            if scribe_core::update::verify::verify_signature(src.as_bytes(), sig, pk).is_err() {
                self.toast = Some(format!(
                    "plugin '{id}' NOT approved: the signature does not verify against \
                     the declared author key"
                ));
                return;
            }
            let mut key_store = scribe_core::plugin::PinnedKeyStore::new(&dir);
            match scribe_core::plugin::pinned_keys::decide_approval(&mut key_store, id, pk) {
                Ok(scribe_core::plugin::pinned_keys::PluginLoadDecision::BlockKeyChanged {
                    old,
                    new,
                }) => {
                    self.toast = Some(format!(
                        "plugin '{id}' NOT approved: author key changed \
                         (old={old} new={new}) — possible takeover; rotate the \
                         key in Settings → Plugins before it can run"
                    ));
                    return;
                }
                Ok(_) => {}
                Err(e) => {
                    self.toast = Some(format!(
                        "plugin '{id}' NOT approved: pinned-key store error: {e}"
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
            Err(e) => self.toast = Some(format!("plugin '{id}' approved but failed to load: {e}")),
        }
    }

    fn new_tab(&mut self) {
        self.tabs.push(EditorTab::scratch());
        self.active = self.tabs.len() - 1;
        crate::action_log::record("tab", "new");
    }

    /// Dispatch a [`BuiltinCommand`] selected from the command palette.
    ///
    /// Every editor action surfaced in `BUILTIN_COMMANDS` routes through here
    /// so the keyboard shortcut and the palette entry produce identical state
    /// changes (no drift between the two surfaces). Touches `self.config`
    /// then persists via `save_config` so toggles survive a restart.
    fn execute_builtin(&mut self, cmd: BuiltinCommand) {
        // Action-log every command dispatch so a session is diagnosable: a
        // command the user invoked that "did nothing" still leaves a trace here.
        crate::action_log::record("cmd", &format!("{cmd:?}"));
        match cmd {
            BuiltinCommand::NewFile => self.new_tab(),
            BuiltinCommand::OpenFile => self.open_dialog(),
            BuiltinCommand::OpenFolder => {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.status = format!("folder: {}", folder.display());
                    self.open_folder_root(folder);
                }
            }
            BuiltinCommand::OpenRecentFolder => {
                self.recent_folders_open = true;
                self.recent_folders_selected = 0;
            }
            BuiltinCommand::Save => self.save_active(),
            BuiltinCommand::ReportIssue => self.issue_intake.open_fresh(),
            BuiltinCommand::ConvertToMarkdown => self.convert_to_markdown_active(),
            BuiltinCommand::ExportAsHtml => self.export_html_active(),
            BuiltinCommand::SetLineEndingsLf => self.set_active_eol(scribe_core::eol::Eol::Lf),
            BuiltinCommand::SetLineEndingsCrlf => self.set_active_eol(scribe_core::eol::Eol::Crlf),
            BuiltinCommand::SetLineEndingsCr => self.set_active_eol(scribe_core::eol::Eol::Cr),
            BuiltinCommand::CloseActiveTab => self.close_tab(self.active),
            BuiltinCommand::CloseAllTabs => {
                self.tabs.clear();
                self.tabs.push(EditorTab::scratch());
                self.active = 0;
            }
            BuiltinCommand::CycleTabNext => {
                if !self.tabs.is_empty() {
                    self.active = (self.active + 1) % self.tabs.len();
                }
            }
            BuiltinCommand::CycleTabPrev => {
                if !self.tabs.is_empty() {
                    self.active = if self.active == 0 {
                        self.tabs.len() - 1
                    } else {
                        self.active - 1
                    };
                }
            }
            BuiltinCommand::ToggleSplitView => {
                // Unified with the grid: "split" is the multi-pane view of the
                // open tabs (side-by-side for two, a grid for more).
                self.config.editor.grid_enabled = !self.config.editor.grid_enabled;
                self.save_config();
            }
            BuiltinCommand::ToggleMinimap => {
                self.config.editor.show_minimap = !self.config.editor.show_minimap;
                self.save_config();
            }
            BuiltinCommand::ToggleZen => {
                // Runtime session state — no config write (zen is never persisted).
                self.zen_mode = !self.zen_mode;
                if self.zen_mode {
                    self.find_open = false;
                    self.find_in_files_open = false;
                }
            }
            BuiltinCommand::ToggleMarkdownPreview => {
                self.md_preview_open = !self.md_preview_open;
            }
            BuiltinCommand::ToggleDiffView => {
                self.diff_view_open = !self.diff_view_open;
            }
            BuiltinCommand::ToggleSpellcheck => {
                self.config.spellcheck.enabled = !self.config.spellcheck.enabled;
                self.save_config();
            }
            BuiltinCommand::ToggleWordWrap => {
                self.config.editor.word_wrap = !self.config.editor.word_wrap;
                self.save_config();
            }
            BuiltinCommand::ToggleLineNumbers => {
                self.config.editor.show_line_numbers = !self.config.editor.show_line_numbers;
                self.save_config();
            }
            BuiltinCommand::ToggleChangeBar => {
                self.config.editor.show_change_bar = !self.config.editor.show_change_bar;
                self.save_config();
            }
            BuiltinCommand::OpenSettings => {
                self.settings_open = true;
            }
            BuiltinCommand::OpenFind => {
                self.find_open = true;
                self.focus_find = true;
            }
            BuiltinCommand::OpenPalette => {
                // Self-referential entry — leaves the palette open as it was.
                self.palette_open = true;
                self.focus_palette = true;
            }
            BuiltinCommand::CycleTheme => {
                let names = scribe_core::theme::Theme::builtin_names();
                if !names.is_empty() {
                    let cur = &self.config.appearance.theme;
                    let idx = names.iter().position(|n| *n == cur.as_str()).unwrap_or(0);
                    let next = names[(idx + 1) % names.len()].to_string();
                    self.config.appearance.theme = next.clone();
                    self.save_config();
                    self.status = format!("theme: {next}");
                }
            }
            BuiltinCommand::StartLsp => self.start_lsp_for_active(),
            BuiltinCommand::FoldAll => {
                if self.active < self.tabs.len() {
                    let text = self.tabs[self.active].text.clone();
                    let regions = crate::editor_features::fold_regions(&text);
                    self.folds = regions.iter().map(|r| r.start_line).collect();
                    self.fold_view = true;
                    self.status = format!("folded {} region(s)", regions.len());
                }
            }
            BuiltinCommand::ExpandAll => {
                self.folds.clear();
                self.status = String::from("expanded all");
            }
            BuiltinCommand::OpenPluginManager => {
                self.plugin_manager
                    .ensure_defaults(Config::config_dir().as_deref());
                self.plugin_manager.open = true;
            }
            BuiltinCommand::SortLines => {
                let active = self.active.min(self.tabs.len().saturating_sub(1));
                if active < self.tabs.len() && !self.tabs[active].doc.is_read_only_large() {
                    let sorted = scribe_core::text_ops::sort_lines(&self.tabs[active].text);
                    if sorted != self.tabs[active].text {
                        self.tabs[active].set_text(sorted);
                        self.tabs[active].doc.mark_dirty();
                        self.status = "sorted lines (A-Z)".to_string();
                    }
                }
            }
            BuiltinCommand::SortLinesUnique => self.apply_buffer_transform(
                "sorted lines (unique)",
                scribe_core::text_ops::sort_lines_unique,
            ),
            BuiltinCommand::TrimTrailingWhitespace => self.apply_buffer_transform(
                "trimmed trailing whitespace",
                scribe_core::text_ops::trim_trailing_whitespace,
            ),
            BuiltinCommand::EnsureFinalNewline => self.apply_buffer_transform(
                "ensured a final newline",
                scribe_core::text_ops::ensure_final_newline,
            ),
            BuiltinCommand::ConvertIndentToSpaces => {
                let w = self.config.editor.tab_width;
                self.apply_buffer_transform("converted indentation to spaces", |t| {
                    scribe_core::text_ops::tabs_to_spaces(t, w)
                });
            }
            BuiltinCommand::ConvertIndentToTabs => {
                let w = self.config.editor.tab_width;
                self.apply_buffer_transform("converted indentation to tabs", |t| {
                    scribe_core::text_ops::spaces_to_tabs(t, w)
                });
            }
            BuiltinCommand::RevealInExplorer => {
                let active = self.active.min(self.tabs.len().saturating_sub(1));
                let dir = self
                    .tabs
                    .get(active)
                    .and_then(|t| t.doc.path())
                    .and_then(|p| p.parent())
                    .map(|d| d.to_path_buf());
                match dir {
                    Some(d) => open_in_file_manager(&d),
                    None => self.toast = Some("This tab has no saved file to reveal.".to_string()),
                }
            }
            BuiltinCommand::CopyFilePath => {
                let active = self.active.min(self.tabs.len().saturating_sub(1));
                let path = self
                    .tabs
                    .get(active)
                    .and_then(|t| t.doc.path())
                    .map(|p| p.display().to_string());
                match path {
                    Some(p) => match write_clipboard_text(&p) {
                        Ok(()) => self.status = format!("copied path: {p}"),
                        Err(e) => self.toast = Some(format!("could not copy path: {e}")),
                    },
                    None => self.toast = Some("This tab has no saved file path.".to_string()),
                }
            }
            // These three need the egui TextEditState (ctx), unavailable here;
            // set a flag that frame_tick drains (see the act.* keyboard path).
            BuiltinCommand::JumpMatchingBracket => self.pending_jump_bracket = true,
            BuiltinCommand::InsertDateTime => self.pending_insert_datetime = true,
            BuiltinCommand::DuplicateSelection => self.pending_dup_selection = true,
            // Clipboard / history actions: record the request; `frame_tick`
            // drains it into the focused editor as a native egui event.
            BuiltinCommand::Copy => self.pending_editor_action = Some(EditorAction::Copy),
            BuiltinCommand::Cut => self.pending_editor_action = Some(EditorAction::Cut),
            BuiltinCommand::Paste => self.pending_editor_action = Some(EditorAction::Paste),
            BuiltinCommand::Undo => self.pending_editor_action = Some(EditorAction::Undo),
            BuiltinCommand::Redo => self.pending_editor_action = Some(EditorAction::Redo),
            BuiltinCommand::ToggleBookmark => self.toggle_bookmark(),
            BuiltinCommand::NextBookmark => self.navigate_bookmark(1),
            BuiltinCommand::PrevBookmark => self.navigate_bookmark(-1),
            BuiltinCommand::GoToSymbol => {
                self.goto_symbol_open = true;
                self.focus_goto_symbol = true;
                self.goto_symbol_selected = 0;
                self.goto_symbol_query.clear();
            }
        }
    }

    /// Drain a palette-requested clipboard/history action by injecting the
    /// corresponding egui event into the input queue and focusing the central
    /// editor, so egui's `TextEdit` performs it natively this frame. Called at
    /// the top of `frame_tick`, before any panel renders, so the editor (shown
    /// later in the same frame) sees the event. `Paste` reads the OS clipboard
    /// via `arboard`; a read failure surfaces a toast rather than panicking.
    fn drain_pending_editor_action(&mut self, ctx: &egui::Context) {
        let Some(action) = self.pending_editor_action.take() else {
            return;
        };
        let editor_id = egui::Id::new("scr1b3-central-editor");
        // Focus the editor so the injected event is delivered to it.
        ctx.memory_mut(|m| m.request_focus(editor_id));
        let event = match action {
            EditorAction::Copy => egui::Event::Copy,
            EditorAction::Cut => egui::Event::Cut,
            EditorAction::Paste => match read_clipboard_text() {
                Ok(text) => egui::Event::Paste(text),
                Err(e) => {
                    self.toast = Some(format!("paste unavailable: {e}"));
                    return;
                }
            },
            EditorAction::Undo => key_event(egui::Key::Z, egui::Modifiers::COMMAND),
            EditorAction::Redo => key_event(
                egui::Key::Z,
                egui::Modifiers {
                    command: true,
                    shift: true,
                    ..Default::default()
                },
            ),
        };
        ctx.input_mut(|i| i.events.push(event));
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            match EditorTab::from_path(path.clone()) {
                Ok(t) => {
                    self.tabs.push(t);
                    self.active = self.tabs.len() - 1;
                    self.status = format!("opened {}", path.display());
                }
                Err(e) => self.toast = Some(format!("open failed: {e}")),
            }
        }
    }

    /// Open `folder` as the file-tree root and record it in the recent-folders
    /// MRU (persisted), mirroring the recent-files discipline.
    fn open_folder_root(&mut self, folder: PathBuf) {
        scribe_core::config::record_recent_file(
            &mut self.config.editor.recent_folders,
            folder.clone(),
        );
        self.file_tree_root = Some(folder);
        self.save_config();
    }

    /// Apply a whole-buffer `&str -> String` transform to the active tab,
    /// skipping read-only-large buffers and no-op results. Shared by the
    /// line-operation commands (sort, trim, indent conversion, …).
    fn apply_buffer_transform(&mut self, status: &str, f: impl Fn(&str) -> String) {
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        if active < self.tabs.len() && !self.tabs[active].doc.is_read_only_large() {
            let new = f(&self.tabs[active].text);
            if new != self.tabs[active].text {
                self.tabs[active].set_text(new);
                self.tabs[active].doc.mark_dirty();
                self.status = status.to_string();
            }
        }
    }

    /// Recompute the active tab's per-line change-bar states if the buffer
    /// moved since the cache was last built. A cheap no-op when nothing
    /// changed (keyed off the monotonic `edit_gen`). The O(n) line diff is
    /// skipped above a size cap — large files are low-value for a change bar
    /// and the diff would cost a frame.
    fn ensure_change_states(&mut self, active: usize) {
        /// Max buffer size for which the change-bar diff runs.
        const CHANGE_BAR_MAX_BYTES: usize = 2 * 1024 * 1024;
        if !self.config.editor.show_change_bar || active >= self.tabs.len() {
            return;
        }
        let tab = &mut self.tabs[active];
        if tab.change_gen == Some(tab.edit_gen) {
            return; // cache is current
        }
        if tab.text.len() > CHANGE_BAR_MAX_BYTES {
            tab.change_states.clear();
            tab.change_gen = Some(tab.edit_gen);
            return;
        }
        tab.change_states = crate::change_bar::compute_change_states(
            &tab.session_baseline,
            &tab.saved_baseline,
            &tab.text,
        );
        tab.change_gen = Some(tab.edit_gen);
    }

    /// Convert the active buffer to Markdown (by file type) and save it as a
    /// `.md` file. The conversion is a pure `text -> markdown` transform
    /// (HTML/CSV/JSON/TOML/code/text); the source tab is left untouched — only
    /// the chosen `.md` file is written.
    fn convert_to_markdown_active(&mut self) {
        let active = self.active;
        if active >= self.tabs.len() {
            return;
        }
        let text = self.tabs[active].text.clone();
        let ext = self.tabs[active].doc.language_hint();
        let md = crate::to_markdown::to_markdown(&text, ext.as_deref());
        let suggested = self.tabs[active]
            .doc
            .path()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .map(|stem| format!("{stem}.md"))
            .unwrap_or_else(|| "untitled.md".to_string());
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Markdown", &["md"])
            .set_file_name(&suggested)
            .save_file()
        {
            match std::fs::write(&path, md) {
                Ok(()) => self.status = format!("converted to Markdown → {}", path.display()),
                Err(e) => self.toast = Some(format!("convert failed: {e}")),
            }
        }
    }

    /// Export the active buffer as a standalone HTML document (treating the
    /// buffer as Markdown). Writes a chosen `.html` file; the source is
    /// untouched. Pure pulldown-cmark rendering — no webview, no network.
    fn export_html_active(&mut self) {
        let active = self.active;
        if active >= self.tabs.len() {
            return;
        }
        let html = crate::md_preview::to_html(&self.tabs[active].text);
        let suggested = self.tabs[active]
            .doc
            .path()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .map(|stem| format!("{stem}.html"))
            .unwrap_or_else(|| "untitled.html".to_string());
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("HTML", &["html"])
            .set_file_name(&suggested)
            .save_file()
        {
            match std::fs::write(&path, html) {
                Ok(()) => self.status = format!("exported HTML → {}", path.display()),
                Err(e) => self.toast = Some(format!("export failed: {e}")),
            }
        }
    }

    /// Set the active document's line-ending style. The change applies on the
    /// next save (the on-disk EOL is written by `Document::save`).
    fn set_active_eol(&mut self, eol: scribe_core::eol::Eol) {
        let active = self.active;
        if active >= self.tabs.len() {
            return;
        }
        self.tabs[active].doc.set_eol(eol);
        self.status = format!("line endings set to {} — save to apply", eol.label());
    }

    /// True when a modal with a focused text field or arrow-key navigation
    /// currently owns the keyboard (#72). The editor-surface completion popup
    /// must defer to these so its ↑↓/Enter interception cannot steal the modal
    /// field's keys. Kept as one method so the set is defined in exactly one
    /// place. NOTE: the passive display modals (welcome, cheatsheet, recent —
    /// no text entry, no arrow navigation) are deliberately EXCLUDED; they have
    /// no keys for completion to conflict with, and the first-run welcome flag
    /// must not suppress completion in the editor behind it.
    fn modal_owns_keyboard(&self) -> bool {
        self.find_open
            || self.palette_open
            || self.settings_open
            || self.fuzzy_open
            || self.goto_open
            || self.goto_symbol_open
    }

    /// Open the identifier-completion popup for the prefix ending at `char_idx`
    /// in the active buffer. Sources suggestions from the buffer's own words
    /// (zero network / LSP dependency).
    fn open_completion(&mut self, active: usize, char_idx: Option<usize>) {
        let Some(ci) = char_idx else {
            self.completion = None;
            return;
        };
        let text = &self.tabs[active].text;
        let byte = char_to_byte(text, ci);
        let (start, prefix) = crate::editor_features::prefix_before(text, byte);
        let items = crate::editor_features::word_completions(text, &prefix, 8);
        self.completion = (!items.is_empty()).then_some(Completion {
            prefix_start: start,
            items,
            selected: 0,
        });
    }

    /// Insert the selected completion, replacing the typed prefix.
    fn accept_completion(&mut self, active: usize, char_idx: Option<usize>) {
        let Some(c) = self.completion.take() else {
            return;
        };
        let Some(ci) = char_idx else { return };
        let Some(item) = c.items.get(c.selected).cloned() else {
            return;
        };
        let text = &mut self.tabs[active].text;
        let byte = char_to_byte(text, ci);
        // `c.prefix_start` is a byte offset captured a frame EARLIER; the buffer
        // may have mutated since (e.g. an async edit between popup-open and
        // accept), leaving it mid-multibyte-char. `replace_range` panics on a
        // non-boundary offset → `panic = "abort"`. `char_to_byte` already clamps
        // `byte` to a boundary; re-validate `prefix_start` the same way before
        // splicing. On a stale offset we drop the completion rather than crash.
        if c.prefix_start <= byte && byte <= text.len() && text.is_char_boundary(c.prefix_start) {
            text.replace_range(c.prefix_start..byte, &item);
        }
        self.tabs[active].edit_gen = self.tabs[active].edit_gen.wrapping_add(1);
    }

    /// Render the minimap strip (rightmost): a memoized scaled overview of the
    /// active document with a viewport indicator; click/drag jumps the editor.
    fn show_minimap(&mut self, ctx: &egui::Context, panel: Color32, accent: Color32) {
        egui::SidePanel::right("minimap")
            // #86 — the Map view is now user-resizable (was a fixed exact_width).
            // The minimap galley re-lays out to `available_size` each frame, so
            // it tracks the dragged width. Floor keeps it legible; ceiling stops
            // it eating the editor.
            .default_width(110.0)
            .width_range(48.0..=260.0)
            .resizable(true)
            .frame(egui::Frame::default().fill(panel).inner_margin(4.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("MAP").color(accent).small().monospace());
                let avail = ui.available_size();
                // Wave-3: memoize the tiny galley keyed by (edit_gen, doc_id,
                // width) — no per-frame full-buffer hash AND no per-frame clone.
                // The owned String is built ONLY on a cache miss (egui `layout`
                // takes it by value); doc_id disambiguates tabs sharing edit_gen.
                let galley = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    self.tabs[self.active].edit_gen.hash(&mut h);
                    self.tabs[self.active].doc_id.raw().hash(&mut h);
                    avail.x.to_bits().hash(&mut h);
                    let key = h.finish();
                    let mut slot = self.minimap_cache.borrow_mut();
                    match slot.as_ref() {
                        Some((k, g)) if *k == key => g.clone(),
                        _ => {
                            // egui 0.34: layout caches into the FontsView so it now
                            // needs `&mut`; use fonts_mut(...) instead of fonts(...).
                            let g = ui.fonts_mut(|f| {
                                f.layout(
                                    self.tabs[self.active].text.clone(),
                                    FontId::monospace(3.0),
                                    Color32::from_rgb(0x8a, 0x88, 0x99),
                                    avail.x,
                                )
                            });
                            *slot = Some((key, g.clone()));
                            g
                        }
                    }
                };
                let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
                ui.painter().add(egui::epaint::TextShape::new(
                    rect.min,
                    galley.clone(),
                    Color32::from_rgb(0x8a, 0x88, 0x99),
                ));
                // Viewport indicator from last frame's editor scroll metrics.
                let (off_y, content_h, view_h) = self.scroll_metrics;
                let map_h = galley.size().y.max(1.0);
                let scale = (rect.height() / map_h).min(1.0);
                let ind_top = rect.top() + (off_y / content_h) * map_h * scale;
                let ind_h = (view_h / content_h) * map_h * scale;
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.left(), ind_top),
                        egui::vec2(rect.width(), ind_h.max(6.0)),
                    ),
                    2.0,
                    Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 40),
                );
                // Click/drag → jump the editor proportionally.
                if let Some(p) = resp.interact_pointer_pos() {
                    let frac = ((p.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
                    self.pending_scroll = Some((frac * (content_h - view_h)).max(0.0));
                }
            });
    }

    /// Render the folded read-only preview: per-region toggles plus the
    /// brace-collapsed projection of the active buffer.
    fn show_fold_view(&mut self, ui: &mut egui::Ui, font: FontId, ext: Option<&str>) {
        // Wave-3: borrow instead of cloning the whole buffer every frame the
        // fold view is shown. `fold_regions`/`project_folded` take &str, and the
        // toolbar closure below only captures `self.folds` (disjoint from
        // `self.tabs` under edition-2021 closure capture), so the borrow holds.
        let text = &self.tabs[self.active].text;
        let regions = crate::editor_features::fold_regions(text);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("FOLDS").small().monospace());
            if ui.small_button("fold all").clicked() {
                self.folds = regions.iter().map(|r| r.start_line).collect();
            }
            if ui.small_button("expand all").clicked() {
                self.folds.clear();
            }
            for r in &regions {
                let folded = self.folds.contains(&r.start_line);
                let label = format!(
                    "{} L{} ({})",
                    if folded { "▸" } else { "▾" },
                    r.start_line + 1,
                    r.hidden_len()
                );
                if ui.small_button(label).clicked() {
                    if folded {
                        self.folds.remove(&r.start_line);
                    } else {
                        self.folds.insert(r.start_line);
                    }
                }
            }
        });
        ui.separator();
        let (mut projected, _map) =
            crate::editor_features::project_folded(text, &regions, &self.folds);
        let line_height = self.config.fonts.clamped_line_height();
        let hl = &self.hl;
        let word_wrap = self.config.editor.word_wrap;
        let layout_fg = ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
        let mut layouter = make_layouter(
            hl,
            &self.hl_cache,
            &self.hl_galley_cache,
            &self.hl_inc_cache,
            ext,
            font,
            line_height,
            word_wrap,
            layout_fg,
        );
        egui::ScrollArea::both()
            .id_salt("fold-scroll")
            .show(ui, |ui| {
                let editor = egui::TextEdit::multiline(&mut projected)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(30)
                    .interactive(false)
                    .layouter(&mut layouter);
                ui.add_sized(ui.available_size(), editor);
            });
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
];

/// Change-detection key for the live font set (#103): note family + UI family.
/// When this string changes, the font set is rebuilt and re-applied.
fn font_state_key(fonts: &scribe_core::config::FontConfig) -> String {
    format!("{}\u{0}{}", fonts.editor_family, fonts.ui_family)
}

/// Resolve a font display name to its embedded family key, falling back to
/// JetBrains Mono for an unknown / stale config value.
fn font_family_key(display: &str) -> &'static str {
    FONT_FAMILIES
        .iter()
        .find(|(d, _)| *d == display)
        .map(|(_, k)| *k)
        .unwrap_or("JetBrainsMono")
}

/// Build the egui font set with `editor_family` as the primary Monospace face
/// (#87). All bundled coding fonts are registered; the selected one is placed
/// first in the Monospace family, JetBrains Mono is kept right behind it as a
/// fallback, and the Noto Sans JP kanji subset is appended to both families so
/// the toolbar kanji never tofu. egui's ab_glyph does no OT shaping, so
/// ligatures are structurally off regardless of face.
fn build_fonts(editor_family: &str, ui_family: &str) -> egui::FontDefinitions {
    use std::sync::Arc;
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Thin);

    macro_rules! embed {
        ($key:literal, $path:literal) => {
            fonts.font_data.insert(
                $key.to_owned(),
                Arc::new(egui::FontData::from_static(include_bytes!($path))),
            );
        };
    }
    embed!(
        "JetBrainsMono",
        "../../../../assets/fonts/JetBrainsMono/JetBrainsMono-Regular.ttf"
    );
    embed!(
        "IBMPlexMono",
        "../../../../assets/fonts/IBMPlexMono/IBMPlexMono-Regular.ttf"
    );
    embed!(
        "FiraMono",
        "../../../../assets/fonts/FiraMono/FiraMono-Regular.ttf"
    );
    embed!(
        "SpaceMono",
        "../../../../assets/fonts/SpaceMono/SpaceMono-Regular.ttf"
    );
    embed!(
        "Cousine",
        "../../../../assets/fonts/Cousine/Cousine-Regular.ttf"
    );
    embed!(
        "SourceCodePro",
        "../../../../assets/fonts/SourceCodePro/SourceCodePro-Regular.ttf"
    );
    embed!(
        "B612Mono",
        "../../../../assets/fonts/B612Mono/B612Mono-Regular.ttf"
    );
    embed!(
        "ShareTechMono",
        "../../../../assets/fonts/ShareTechMono/ShareTechMono-Regular.ttf"
    );
    embed!("VT323", "../../../../assets/fonts/VT323/VT323-Regular.ttf");
    // Wave 4 — brand display + accent faces (atomic with the FONT_FAMILIES
    // additions above; a key without its embed fails the registration test).
    embed!("Doto", "../../../../assets/fonts/Doto/Doto[ROND,wght].ttf");
    embed!(
        "MajorMonoDisplay",
        "../../../../assets/fonts/MajorMonoDisplay/MajorMonoDisplay-Regular.ttf"
    );
    embed!(
        "ChakraPetch",
        "../../../../assets/fonts/ChakraPetch/ChakraPetch-Regular.ttf"
    );
    embed!(
        "Wallpoet",
        "../../../../assets/fonts/Wallpoet/Wallpoet-Regular.ttf"
    );
    embed!(
        "Michroma",
        "../../../../assets/fonts/Michroma/Michroma-Regular.ttf"
    );
    embed!(
        "RedHatMono",
        "../../../../assets/fonts/RedHatMono/RedHatMono[wght].ttf"
    );
    embed!("Teko", "../../../../assets/fonts/Teko/Teko[wght].ttf");
    embed!(
        "Rajdhani",
        "../../../../assets/fonts/Rajdhani/Rajdhani-Regular.ttf"
    );
    embed!(
        "Saira",
        "../../../../assets/fonts/Saira/Saira[wdth,wght].ttf"
    );
    embed!(
        "ZenDots",
        "../../../../assets/fonts/ZenDots/ZenDots-Regular.ttf"
    );
    embed!(
        "Syncopate",
        "../../../../assets/fonts/Syncopate/Syncopate-Regular.ttf"
    );
    embed!(
        "SplineSansMono",
        "../../../../assets/fonts/SplineSansMono/SplineSansMono[wght].ttf"
    );
    embed!(
        "NotoSansJP-Subset",
        "../../../../assets/fonts/NotoSansJP/NotoSansJP-Subset.ttf"
    );

    let selected = font_family_key(editor_family);
    if let Some(mono) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        mono.insert(0, selected.to_owned());
        if selected != "JetBrainsMono" {
            mono.insert(1, "JetBrainsMono".to_owned());
        }
        // egui-phosphor's `add_to_fonts` only registers the icon font in the
        // Proportional family, so phosphor glyphs (CHECK, DOTS_SIX_VERTICAL, …)
        // render as tofu boxes in any `.monospace()` text (the status bar, the
        // pane-header note name). Append phosphor as a Monospace fallback too so
        // those glyphs resolve there as well — JetBrains Mono still leads.
        if !mono.iter().any(|f| f == "phosphor") {
            mono.push("phosphor".to_owned());
        }
    }
    // #103 — the UI (proportional) font is chosen SEPARATELY from the note font.
    // "System default" (or any unknown value) leaves egui's built-in UI font
    // untouched; a bundled family name puts that face first in the Proportional
    // family so the whole app UI (toolbar / settings / status) uses it.
    if let Some(&(_, ui_key)) = FONT_FAMILIES.iter().find(|(d, _)| *d == ui_family) {
        if let Some(prop) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            prop.insert(0, ui_key.to_owned());
        }
    }
    for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("NotoSansJP-Subset".to_owned());
    }
    fonts
}

fn panel_fill(
    theme: &Theme,
    window: &scribe_core::config::WindowConfig,
    background_override: Option<&str>,
) -> Color32 {
    // #88 — an explicit background override (hex) wins over the theme's panel
    // colour; otherwise follow the theme. Translucency (glass mode) still
    // applies its alpha on top, so the override composes with vibrancy.
    let base: Color32 = match background_override.and_then(Rgba::parse_hex) {
        Some(o) => Color32::from_rgb(o.r, o.g, o.b),
        None => ui_color(theme, "panel", Rgba::new(0x0d, 0x0b, 0x14, 255)),
    };
    if window.effective_translucent() {
        // 0.02 floor matches the settings slider min + scribe_render::apply_window_opacity
        // so the full slider travel is live (the old 0.30 floor was a dead band;
        // #24 dropped 0.05 → 0.02 for a more see-through lowest setting).
        let a = (window.opacity.clamp(0.02, 1.0) * 255.0).round() as u8;
        Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a)
    } else {
        base
    }
}

/// Paint a dot drag-grip and return its response. We paint the dots instead of
/// drawing the phosphor `DOTS_SIX_VERTICAL` glyph because that PUA codepoint
/// renders as a tofu square in this build's font atlas (the glyph IS in the
/// font and phosphor IS registered in both families, yet egui's atlas resolves
/// it to .notdef here — a known egui-phosphor footgun). Painted dots are
/// font-independent and always render as a clean, recognizable grip. `enabled`
/// = false dims it and drops the drag sense (used for pinned panes).
///
/// `rotated` flips the grip's orientation to MATCH the tab's text orientation:
/// `false` (default) paints a 2×3 column of dots (a tall handle) for horizontal
/// tabs/headers; `true` paints a 3×2 row of dots (a wide handle) for the rotated
/// (vertical-text) side tabs, so the grip reads as a handle in that orientation
/// instead of staying vertical against horizontal text.
pub(crate) fn grip_handle(
    ui: &mut egui::Ui,
    enabled: bool,
    color: Color32,
    rotated: bool,
) -> egui::Response {
    let h = ui.text_style_height(&egui::TextStyle::Body);
    let sense = if enabled {
        egui::Sense::click_and_drag()
    } else {
        egui::Sense::hover()
    };
    // Swap the allocation's aspect with the orientation: tall+narrow for the
    // vertical handle, wide+short for the rotated (horizontal) handle.
    let size = if rotated {
        egui::vec2(h.max(15.0), 11.0)
    } else {
        egui::vec2(11.0, h)
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let dim = if enabled {
        color
    } else {
        color.gamma_multiply(0.5)
    };
    let c = rect.center();
    let painter = ui.painter();
    // 2 cols × 3 rows (vertical) vs 3 cols × 2 rows (rotated) — the dot grid is
    // transposed so the handle's long axis follows the tab's long axis.
    let (xs, ys): (&[f32], &[f32]) = if rotated {
        (&[c.x - 4.5, c.x, c.x + 4.5], &[c.y - 2.5, c.y + 2.5])
    } else {
        (&[c.x - 2.5, c.x + 2.5], &[c.y - 4.5, c.y, c.y + 4.5])
    };
    for &x in xs {
        for &y in ys {
            painter.circle_filled(egui::pos2(x, y), 1.5, dim);
        }
    }
    resp
}

/// Build a syntect-colored `LayoutJob` for the editor surface. Free function so
/// the egui `layouter` closure captures only the highlighter, not `self`.
fn highlight_job(
    hl: &Highlighter,
    text: &str,
    ext: Option<&str>,
    font: FontId,
    line_height_mult: f32,
    inc_cache: &mut IncrementalHighlightState,
    fg: Color32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let lines = hl.highlight_document_incremental(text, ext, inc_cache);
    // Explicit per-row height honours the `fonts.line_height` setting (epaint
    // TextFormat.line_height; epaint defaults to the font's natural height).
    let lh = Some(font.size * line_height_mult);
    let plain = |color: Color32| {
        let mut f = TextFormat::simple(font.clone(), color);
        f.line_height = lh;
        f
    };
    let mut char_cursor = 0usize;
    // Reconstruct text with colored spans line by line.
    for (li, line) in text.split_inclusive('\n').enumerate() {
        if let Some(spans) = lines.get(li) {
            let mut byte = 0usize;
            for s in spans {
                let seg = &line.get(s.range.clone()).unwrap_or("");
                if !seg.is_empty() {
                    let mut fmt = plain(scribe_render::syntax_color32(s.color));
                    if s.italic {
                        fmt.italics = true;
                    }
                    job.append(seg, 0.0, fmt);
                }
                byte = s.range.end;
            }
            // Append any tail not covered by spans. Wave-3: use the theme
            // foreground (was hardcoded GRAY — washed out vs the body text and
            // mismatched the rope editor, which already uses the theme fg).
            // Use `get(..)` (like the per-span slice above) rather than a direct
            // `&line[byte..]`: if the highlighter ever emits a span boundary that
            // is not a UTF-8 char boundary, a direct slice would panic → abort.
            if let Some(tail) = line.get(byte..) {
                if !tail.is_empty() {
                    job.append(tail, 0.0, plain(fg));
                }
            }
        } else {
            job.append(line, 0.0, plain(fg));
        }
        char_cursor += line.len();
    }
    let _ = char_cursor;
    job
}

/// Build the memoizing egui `layouter` closure for a `TextEdit`. Reuses the
/// cached highlight `LayoutJob` unless the buffer/lang/font-size changed, so
/// syntect/tree-sitter only re-run when the text actually changes.
/// The wrap width our editor layouter should USE, given the word-wrap setting
/// and the width egui hands the layouter. egui's `TextEdit` always passes the
/// scroll-viewport `available_width` as the wrap width (NOT `desired_width`), so
/// a custom layouter that blindly honours it wraps even when wrap is off — the
/// "word wrap is always on" bug. When wrap is off we force infinite width so the
/// galley lays out on one line and the `ScrollArea::both` scrolls horizontally.
/// Wave-3: decide whether the *editable* central editor should render this
/// buffer through the in-house viewport-culled rope editor. True when the user
/// opted in (`experimental`), OR when auto-promotion is enabled (`threshold > 0`)
/// AND the buffer is at least `threshold` bytes. A pure function so the
/// branch-selection logic is unit-testable without driving an egui frame.
fn use_rope_editor(experimental: bool, text_len: usize, auto_threshold_bytes: usize) -> bool {
    experimental || (auto_threshold_bytes > 0 && text_len >= auto_threshold_bytes)
}

/// Load static Tab-trigger snippets from `<config-dir>/snippets.toml`. A missing
/// or malformed file yields an empty set (the feature is simply inert) — never
/// an error path, so a bad snippets file can't block the editor from starting.
fn load_snippets() -> scribe_core::snippets::SnippetSet {
    scribe_core::config::Config::config_dir()
        .map(|d| d.join("snippets.toml"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| scribe_core::snippets::SnippetSet::from_toml(&s).ok())
        .unwrap_or_default()
}

fn effective_wrap_width(word_wrap: bool, available: f32) -> f32 {
    if word_wrap {
        available
    } else {
        f32::INFINITY
    }
}

/// Char index of byte offset `byte` in `s` (#78 — spell spans are byte offsets,
/// galley cursors are char indices). Clamps to the nearest char boundary at or
/// before `byte`, so a mid-codepoint offset never panics.
fn byte_to_char_index(s: &str, byte: usize) -> usize {
    s.char_indices().take_while(|(i, _)| *i < byte).count()
}

/// Wave-6 bracket-match: find the bracket pair to highlight given a caret
/// char-index. Looks at the char just before and just after the caret for an
/// opener/closer, then scans for its partner respecting nesting. Returns
/// `(open_char_index, close_char_index)` in ascending order, or `None`. The scan
/// is bounded by the caller (skipped for very large buffers) to stay cheap.
fn matching_bracket_char_indices(text: &str, caret_ci: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let pairs = [('(', ')'), ('[', ']'), ('{', '}')];
    let is_open = |c: char| pairs.iter().any(|(o, _)| *o == c);
    let is_close = |c: char| pairs.iter().any(|(_, cl)| *cl == c);
    let partner = |c: char| -> Option<(char, bool)> {
        for (o, cl) in pairs {
            if c == o {
                return Some((cl, true)); // need a closer, scan forward
            }
            if c == cl {
                return Some((o, false)); // need an opener, scan backward
            }
        }
        None
    };
    // Prefer the char immediately to the LEFT of the caret (editor convention),
    // else the char to the RIGHT.
    let candidates = [caret_ci.checked_sub(1), Some(caret_ci)];
    for ci in candidates.into_iter().flatten() {
        let Some(&here) = chars.get(ci) else { continue };
        if !is_open(here) && !is_close(here) {
            continue;
        }
        let (want, forward) = partner(here)?;
        let mut depth = 0i32;
        if forward {
            let mut j = ci;
            while j < chars.len() {
                let c = chars[j];
                if c == here {
                    depth += 1;
                } else if c == want {
                    depth -= 1;
                    if depth == 0 {
                        return Some((ci, j));
                    }
                }
                j += 1;
            }
        } else {
            let mut j = ci as isize;
            while j >= 0 {
                let c = chars[j as usize];
                if c == here {
                    depth += 1;
                } else if c == want {
                    depth -= 1;
                    if depth == 0 {
                        return Some((j as usize, ci));
                    }
                }
                j -= 1;
            }
        }
    }
    None
}

/// Paint a red spellcheck squiggle from `x0` to `x1` along baseline `y` (#78).
/// A small triangle wave reads as the universal "misspelled" underline.
fn paint_squiggle(painter: &egui::Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    if x1 <= x0 {
        return;
    }
    let amp = 1.5;
    let step = 3.0;
    let stroke = egui::Stroke::new(1.0, color);
    let mut x = x0;
    let mut up = true;
    let mut prev = egui::pos2(x0, y);
    while x < x1 {
        x = (x + step).min(x1);
        let ny = if up { y - amp } else { y + amp };
        let next = egui::pos2(x, ny);
        painter.line_segment([prev, next], stroke);
        prev = next;
        up = !up;
    }
}

#[allow(clippy::too_many_arguments)]
fn make_layouter<'a>(
    hl: &'a Highlighter,
    cache: &'a std::cell::RefCell<Option<(u64, std::sync::Arc<LayoutJob>)>>,
    gcache: &'a std::cell::RefCell<Option<(u64, f32, std::sync::Arc<egui::Galley>)>>,
    inc_cache: &'a std::cell::RefCell<IncrementalHighlightState>,
    ext: Option<&'a str>,
    font: FontId,
    line_height: f32,
    word_wrap: bool,
    fg: Color32,
) -> impl FnMut(&egui::Ui, &dyn egui::TextBuffer, f32) -> std::sync::Arc<egui::Galley> + 'a {
    // egui 0.34: TextEdit::layouter callback now receives `&dyn TextBuffer`
    // instead of `&str` (so non-String buffers can be hosted). We still want
    // to hash + highlight by &str, so unpack via TextBuffer::as_str().
    move |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap: f32| {
        let text: &str = text.as_str();
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // P-07: this DELIBERATELY hashes the full buffer text every frame and
        // is NOT replaced by an `(edit_gen, len)` key. The layouter receives
        // egui's LIVE `&dyn TextBuffer` (it has no access to a per-tab
        // `edit_gen`), and the cached galley/job BAKES IN the text -- a lagging
        // counter would render STALE TEXT, not just a stale squiggle. See the
        // `edit_gen` field docs: the syntax layouter intentionally keeps its
        // content hash while the minimap/spell memos key off `edit_gen`.
        text.hash(&mut hasher);
        ext.hash(&mut hasher);
        font.size.to_bits().hash(&mut hasher);
        line_height.to_bits().hash(&mut hasher);
        // Wave-3: fold the tail/foreground colour into the key so a theme switch
        // (which changes `fg` but not the text) invalidates the cached job.
        let [r, g, b, a] = fg.to_array();
        r.hash(&mut hasher);
        g.hash(&mut hasher);
        b.hash(&mut hasher);
        a.hash(&mut hasher);
        let key = hasher.finish();
        let eff_wrap = effective_wrap_width(word_wrap, wrap);
        // Wave-3: full galley hit — same content key AND same wrap width. Return
        // the cached Arc<Galley> (O(1) bump); skip the LayoutJob deep-clone AND
        // the re-layout. egui's own FontsView cache does NOT save the clone.
        {
            let gslot = gcache.borrow();
            if let Some((gk, gw, gal)) = gslot.as_ref() {
                if *gk == key && *gw == eff_wrap {
                    return gal.clone();
                }
            }
        }
        let job_arc = {
            let mut slot = cache.borrow_mut();
            match slot.as_ref() {
                Some((k, j)) if *k == key => j.clone(),
                _ => {
                    let arc = std::sync::Arc::new(highlight_job(
                        hl,
                        text,
                        ext,
                        font.clone(),
                        line_height,
                        &mut inc_cache.borrow_mut(),
                        fg,
                    ));
                    *slot = Some((key, arc.clone()));
                    arc
                }
            }
        };
        let mut job = (*job_arc).clone();
        job.wrap.max_width = eff_wrap;
        // egui 0.34: FontsView::layout_job caches into the view → needs &mut.
        let galley = ui.fonts_mut(|f| f.layout_job(job));
        *gcache.borrow_mut() = Some((key, eff_wrap, galley.clone()));
        galley
    }
}

/// Byte offset of char index `ci` in `s` (clamped to `s.len()`).
fn char_to_byte(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
}

/// Find the bookmark to jump to from `from` line (0-based) in direction
/// `dir` (`1` = next/down, `-1` = previous/up). Bookmarks are an ordered set
/// of 0-based line indices. The search wraps around the buffer, so "next"
/// past the last bookmark returns the first, and "previous" before the
/// first returns the last. Returns `None` when there are no bookmarks.
fn pick_bookmark(
    bookmarks: &std::collections::BTreeSet<usize>,
    from: usize,
    dir: i32,
) -> Option<usize> {
    if bookmarks.is_empty() {
        return None;
    }
    if dir >= 0 {
        // First bookmark strictly after `from`; wrap to the lowest otherwise.
        bookmarks
            .range((from + 1)..)
            .next()
            .copied()
            .or_else(|| bookmarks.iter().next().copied())
    } else {
        // Last bookmark strictly before `from`; wrap to the highest otherwise.
        bookmarks
            .range(..from)
            .next_back()
            .copied()
            .or_else(|| bookmarks.iter().next_back().copied())
    }
}

/// Translate an egui [`egui::epaint::text::cursor::CCursor`] char index into
/// a human-visible `(1-based line, 1-based column)` pair. Counts a literal
/// `\n` as a line break; the column resets on every newline.
fn line_col_from_char_index(text: &str, char_index: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for ch in text.chars().take(char_index) {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Replace the `[lo, hi)` char-range of `text` with `width` spaces and return
/// `(new_text, new_caret_char_index)`. Pure core of the Tab→spaces handler so it
/// can be unit-tested without a live `TextEdit`.
fn apply_indent(text: &str, lo: usize, hi: usize, width: usize) -> (String, usize) {
    let spaces = " ".repeat(width.max(1));
    let blo = char_to_byte(text, lo);
    let bhi = char_to_byte(text, hi);
    let mut out = text.to_string();
    out.replace_range(blo..bhi, &spaces);
    (out, lo + spaces.chars().count())
}

/// Auto-indent on Enter (#107): insert a newline at `cursor` (char index) plus a
/// copy of the CURRENT line's leading whitespace, so the new line keeps the same
/// indentation. Returns the new text and the new cursor char index (after the
/// inserted newline + indent). Pure + unit-tested. Preserves whatever the line
/// uses (spaces or tabs); this is what makes `tab_width`/`insert_spaces`-driven
/// indentation actually persist line-to-line.
fn newline_with_indent(text: &str, cursor: usize) -> (String, usize) {
    let bcur = char_to_byte(text, cursor);
    // Start of the current line = byte after the previous '\n' (or 0).
    let line_start = text[..bcur].rfind('\n').map(|i| i + 1).unwrap_or(0);
    // Leading whitespace of the line, but not past the cursor.
    let indent: String = text[line_start..bcur]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let insert = format!("\n{indent}");
    let mut out = text.to_string();
    out.insert_str(bcur, &insert);
    (out, cursor + insert.chars().count())
}

/// Render the completion popup as a foreground `Area` anchored just below the
/// cursor row. Returns `Some(index)` if the user clicked a row.
fn completion_popup(ui: &egui::Ui, pos: egui::Pos2, c: &Completion) -> Option<usize> {
    let mut clicked = None;
    egui::Area::new(egui::Id::new("scr1b3-completion"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(280.0);
                for (i, item) in c.items.iter().enumerate() {
                    let label = egui::RichText::new(item).monospace();
                    if ui.selectable_label(i == c.selected, label).clicked() {
                        clicked = Some(i);
                    }
                }
            });
        });
    clicked
}

fn load_theme(name: &str) -> Theme {
    // Try a user theme file `<config_dir>/themes/<name>.toml` first so users can
    // override built-ins. Then try the built-in dispatch (Phase 17 T17.2 alt
    // themes). Final fallback is the wired-noir brand default so a misnamed
    // theme never blanks the UI.
    if let Some(dir) = Config::config_dir() {
        let p = dir.join("themes").join(format!("{name}.toml"));
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(t) = Theme::from_toml_str(&s) {
                return t;
            }
        }
    }
    Theme::builtin(name).unwrap_or_else(Theme::itasha_corp)
}

/// Spawn a filesystem watcher on the config directory; sends `()` on `tx` when
/// a `.toml` change is observed. Returns the watcher (kept alive by the app).
fn spawn_config_watcher(tx: std::sync::mpsc::Sender<()>) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher as _;
    let dir = Config::config_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if ev
                .paths
                .iter()
                .any(|p| p.extension().is_some_and(|e| e == "toml"))
            {
                let _ = tx.send(());
            }
        }
    })
    .ok()?;
    watcher
        .watch(&dir, notify::RecursiveMode::NonRecursive)
        .ok()?;
    Some(watcher)
}

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

impl ScribeApp {
    /// Apply the Wave-2 scroll knobs and drive middle-click autoscroll. Called
    /// at the very top of [`Self::frame_tick`], before any `ScrollArea` shows.
    ///
    /// - **Wheel speed** is `line_scroll_speed` (pre-smoothing; egui's built-in
    ///   "reach 90% in 0.1s" wheel smoothing still applies, so no double-smooth).
    /// - **Jump animation** eases programmatic scrolls (goto-line / find-next).
    /// - **Autoscroll** injects into `smooth_scroll_delta` so the ScrollArea the
    ///   pointer is over consumes it — the same additive contract `ScrollArea`
    ///   uses for the wheel, without threading a handle through every pane.
    fn apply_scroll_settings(&self, ctx: &egui::Context) {
        let scroll = self.config.scroll;
        ctx.options_mut(|o| o.input_options.line_scroll_speed = scroll.clamped_speed());
        // Wave-6 smooth-scroll: when the editor's smooth_scroll is OFF, kill the
        // jump easing so the wheel moves in discrete notches (snappier).
        let smooth = scroll.animate_jumps && self.config.editor.smooth_scroll;
        ctx.all_styles_mut(|s| {
            s.scroll_animation = if smooth {
                egui::style::ScrollAnimation::new(1500.0, egui::Rangef::new(0.05, 0.20))
            } else {
                egui::style::ScrollAnimation::none()
            };
        });
        if !scroll.autoscroll {
            return;
        }
        let id = egui::Id::new("scr1b3_autoscroll");
        let mut st: AutoScrollState = ctx.data(|d| d.get_temp(id).unwrap_or_default());
        // The central editor region (everything left after the titlebar / toolbar
        // / tab / status panels). At the top of a frame `available_rect` still
        // holds last frame's central area, so this excludes a middle-click on a
        // tab / toolbar button (which must keep its own middle-click meaning, e.g.
        // close-tab) from starting an autoscroll drift + repaint loop.
        let editor_area = ctx.available_rect();
        let (mb_pressed, exit_pressed, pos, dt) = ctx.input(|i| {
            (
                i.pointer.button_pressed(egui::PointerButton::Middle),
                i.pointer.button_pressed(egui::PointerButton::Primary)
                    || i.pointer.button_pressed(egui::PointerButton::Secondary),
                i.pointer.latest_pos(),
                i.stable_dt,
            )
        });
        // Enter on a middle press (toggles off if already active); otherwise a
        // left/right press exits. `entered` gates the entering frame so the same
        // press can't both enter and immediately drift.
        let mut entered = false;
        if mb_pressed {
            if st.active {
                st.active = false;
            } else if let Some(p) = pos {
                // Only arm autoscroll for a middle-click inside the editor surface
                // — never on the tabs / toolbar / status chrome.
                if editor_area.contains(p) {
                    st.active = true;
                    st.anchor = p;
                    entered = true;
                }
            }
        } else if st.active && exit_pressed {
            st.active = false;
        }
        if st.active && !entered {
            if let Some(p) = pos {
                let from_anchor = p - st.anchor;
                let dead = scroll.clamped_dead_zone();
                let drifting = from_anchor.length() >= dead;
                if drifting {
                    // smooth_scroll_delta +y moves content down (view toward the
                    // top), so to scroll toward the END when the pointer is BELOW
                    // the anchor (from_anchor.y > 0) the injected delta is negated.
                    let delta = -from_anchor * scroll.clamped_sensitivity() * dt;
                    // ScrollArea consumes `smooth_scroll_delta` (zeroing it when it
                    // takes it), so injecting here scrolls the hovered area.
                    ctx.input_mut(|i| i.smooth_scroll_delta += delta);
                    // Keep integrating the drift even when the pointer is held
                    // stationary-but-offset (no input event would otherwise wake
                    // the reactive loop). Crucially, when the pointer is AT rest in
                    // the dead-zone we do NOT request a repaint — otherwise a plain
                    // middle-click (e.g. that also closed a tab) would spin forever.
                    ctx.request_repaint();
                }
                // Origin glyph on a foreground layer + a directional cursor so the
                // affordance reads like the Windows wheel-click autoscroll. Drawn
                // whenever active (cheap; persists between input events at rest).
                let col = ctx.style().visuals.text_color();
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("scr1b3_autoscroll_glyph"),
                ));
                painter.circle_stroke(st.anchor, 11.0, egui::Stroke::new(1.5, col));
                painter.circle_filled(st.anchor, 1.5, col);
                let icon = if !drifting {
                    egui::CursorIcon::Move
                } else if from_anchor.y.abs() >= from_anchor.x.abs() {
                    if from_anchor.y < 0.0 {
                        egui::CursorIcon::ResizeNorth
                    } else {
                        egui::CursorIcon::ResizeSouth
                    }
                } else if from_anchor.x < 0.0 {
                    egui::CursorIcon::ResizeWest
                } else {
                    egui::CursorIcon::ResizeEast
                };
                ctx.set_cursor_icon(icon);
            }
        }
        ctx.data_mut(|d| d.insert_temp(id, st));
    }

    /// Drop every cached, atlas-baked galley (the note-text highlight galley and
    /// the minimap galley) plus the highlight-job memo. MUST be called right
    /// after `ctx.set_fonts()` rebuilds the font atlas: an `Arc<Galley>` baked
    /// against the OLD atlas keeps stale glyph→texture UVs, so reusing it after
    /// the rebuild paints garbled "broken" text. The layouter cache key
    /// (`make_layouter`) keys on font SIZE but not the family face — and
    /// `FontId::monospace` is identical before/after a face swap — so the cache
    /// cannot self-invalidate on a family change; this explicit drop is the only
    /// signal. (Bug: changing the app UI font silently rebuilt the atlas and the
    /// note text rendered from the stale galley.)
    fn invalidate_galley_caches(&self) {
        *self.hl_cache.borrow_mut() = None;
        *self.hl_galley_cache.borrow_mut() = None;
        *self.minimap_cache.borrow_mut() = None;
    }

    /// One per-frame tick of the editor UI. Separated from `eframe::App::ui` so
    /// `egui_kittest` E2E tests can drive it through `Context::run` without an
    /// `eframe::Frame`. Drives every top-level panel via the deprecated-but-
    /// functional `Panel::show(ctx, …)` path.
    pub(crate) fn frame_tick(&mut self, ctx: &egui::Context) {
        // Font-switch step 2 (see step 1 at the `ctx.set_fonts` call below):
        // `set_fonts` took effect at the START of this frame, so the NEW atlas is
        // now live. Drop the galley caches that were (re)baked against the OLD
        // atlas on the switch frame, BEFORE any panel renders this frame — the
        // editor then re-bakes against the new atlas and the note paints correctly
        // immediately (no blank/garbled frame, no need to type to refresh).
        if self.font_rebuild_pending {
            self.invalidate_galley_caches();
            self.font_rebuild_pending = false;
        }
        // Wave 2 scroll: apply the wheel-speed + jump-animation knobs and run the
        // middle-click autoscroll state machine BEFORE any ScrollArea shows this
        // frame (egui reads line_scroll_speed while building the wheel delta, and
        // the autoscroll injects into smooth_scroll_delta which the hovered
        // ScrollArea consumes when it renders later this tick).
        self.apply_scroll_settings(ctx);
        // Drain a palette-requested clipboard/history action BEFORE any panel
        // renders, so the injected event reaches the central editor (shown
        // later this frame) and egui's TextEdit performs it natively.
        self.drain_pending_editor_action(ctx);
        // F-022 — poll the disk mtimes of every open file-backed tab. Cheap
        // when nothing changed (one stat per tab); silent reload when the
        // buffer is clean; status toast when local edits would be clobbered.
        // P-06: throttled to once every N frames (see `should_poll_disk`).
        self.poll_external_disk_changes(ctx.cumulative_pass_nr());
        // Phase 18 T18.2 — keep the grid in step with the editor.grid_enabled
        // config preference (toggled in Settings or via TOML edit + watcher).
        // This is cheap on the common path (config unchanged + ids already
        // assigned) and lets the grid show up the same frame the user flips
        // the checkbox.
        self.sync_grid_state();
        // Follow-OS-theme watcher: when `appearance.follow_os_theme` is on,
        // re-resolve + apply the theme whenever the OS flips light/dark. Cheap
        // — one input read; only re-applies on an actual change.
        {
            let os_theme = ctx.theme();
            if self.config.appearance.follow_os_theme && Some(os_theme) != self.last_os_theme {
                self.reapply_theme(ctx);
            }
        }
        // Once per launch: kick off an automatic update check if opted in.
        self.maybe_remind_update(ctx);
        // Drain the updater worker each frame. A `notify`-mode launch check that
        // found a release raises a prominent top banner (Update / Dismiss) instead
        // of the easily-missed passive toast — see the "update-notice" panel below.
        self.updater.poll(ctx);
        if let Some(v) = self.updater.toast_pending.take() {
            self.update_notice = Some(v);
        }
        // `auto`-mode found-an-update yes/no modal.
        self.render_update_prompt(ctx);
        // W1TN3SS opt-in crash-consent modal (ask-each-time). Renders only when a
        // prior session spooled a crash report AND the user opted into
        // AskEachTime; presents an editable preview + equal-weight Send/Don't-send.
        self.render_crash_consent(ctx);
        // W1TN3SS user-initiated "Report an issue" modal. Renders only when the
        // user has opened it from the command palette; previews the exact body,
        // diagnostics OFF by default, and launches the GitHub deep-link / mailto
        // only on an explicit button click.
        self.render_report_issue(ctx);
        // Keep egui's animation time + caret style in sync with the motion
        // preferences every frame (cheap; also covers startup before any
        // theme reapply).
        self.apply_motion_style(ctx);
        // ---- Two-phase close (T19.1 ghost-window fix) ----
        // A transparent / layered window (frameless or translucent) must be
        // HIDDEN one frame before it is destroyed, or the Windows DWM keeps its
        // last composited frame on screen as a ghost after the process exits.
        // Phase 1: on any close request (custom ✕ or OS close) cancel the
        // immediate close, hide the window, repaint. Phase 2 (next frame): the
        // window is hidden, so issue the real Close.
        if self.closing {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let os_close = ctx.input(|i| i.viewport().close_requested());
        if os_close || self.want_close {
            self.want_close = false;
            self.closing = true;
            if os_close {
                // Stop eframe acting on the OS close THIS frame; we drive it.
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            ctx.request_repaint();
            return;
        }

        if !self.visuals_applied {
            ctx.set_visuals(self.current_visuals());
            self.visuals_applied = true;
        }

        // #24/#40 — the "doubled caption buttons" fix. ROOT CAUSE: winit keeps
        // `WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX` on undecorated TOP-LEVEL
        // windows (only the WS_CHILD branch strips caption bits — winit #2754), and
        // Windows 11 DWM paints the three native caption buttons from those residual
        // style bits over our custom titlebar. It is NOT the DWM backdrop (removing
        // window-vibrancy changed nothing) and NOT transparency; the old per-frame
        // `Decorations(false)` re-assert only toggled winit's decorations marker,
        // never the style bits, so it was a no-op. The real fix strips those bits
        // off the HWND — quarantined in the `scribe-win32-chrome` crate (the only
        // `unsafe` besides scribe-core's mmap). Called every frame because it is
        // idempotent + cheap (a single GetWindowLongPtrW read once stripped) and a
        // maximize re-applies winit's styles, which would otherwise re-add them.
        // `!cfg!(test)`: the headless kittest harness has no real OS window.
        if !cfg!(test) && self.config.appearance.frameless {
            scribe_win32_chrome::ensure_caption_stripped();
        }

        // #87/#103 — restart-free font switch: rebuild + re-apply the font set
        // whenever the chosen note OR UI family changes (cheap string compare).
        let font_key = font_state_key(&self.config.fonts);
        if font_key != self.applied_font_family {
            // Font-switch step 1: queue the new font set. `set_fonts` only takes
            // effect at the START of the NEXT frame — THIS frame still renders with
            // the old atlas. So: drop the stale caches now (cheap), then mark a
            // rebuild pending + request a repaint so step 2 (top of `frame_tick`)
            // drops the caches AGAIN next frame once the new atlas is live. Without
            // the next-frame drop, this frame re-bakes a galley against the still-
            // old atlas and the note renders blank/garbled until the next edit.
            ctx.set_fonts(build_fonts(
                &self.config.fonts.editor_family,
                &self.config.fonts.ui_family,
            ));
            self.applied_font_family = font_key;
            self.invalidate_galley_caches();
            self.font_rebuild_pending = true;
            ctx.request_repaint();
        }

        // #104 — apply the note syntax colour theme to the highlighter when it
        // changes (also runs once on the first frame to honour the saved
        // config). Clearing the highlight cache forces a re-colour next render.
        if self.config.editor.note_theme != self.applied_note_theme {
            self.hl.set_theme(&self.config.editor.note_theme);
            *self.hl_cache.borrow_mut() = None;
            *self.hl_galley_cache.borrow_mut() = None;
            self.applied_note_theme = self.config.editor.note_theme.clone();
        }

        // Live-reload config when the file changes on disk (external edit).
        let mut reload_cfg = false;
        if let Some(rx) = &self.cfg_rx {
            while rx.try_recv().is_ok() {
                reload_cfg = true;
            }
        }
        if reload_cfg {
            self.reload_config_from_disk(ctx);
        }

        // 4-02 — drain any batches the off-thread project-find worker streamed
        // back this frame so the results pane fills in progressively. Cheap
        // (one `try_recv` loop) and a no-op when no search is in flight.
        self.drain_find_in_files();

        // Drain LSP diagnostics published by the server thread.
        let mut new_diags: Option<Vec<Diagnostic>> = None;
        if let Some(client) = &self.lsp {
            while let Ok(d) = client.diagnostics.try_recv() {
                new_diags = Some(d);
            }
        }
        if let Some(d) = new_diags {
            self.diagnostics = d;
        }

        // Collect deferred actions from shortcuts.
        let mut act = Pending::default();
        // #R6 — find-bar F3 navigation direction, recorded here and applied
        // after the input closure so `find_navigate` can re-borrow `self`.
        let mut find_nav: Option<bool> = None;
        self.handle_keyboard_shortcuts(ctx, &mut act, &mut find_nav);
        // #R6 — apply the find-bar F3 navigation collected above (outside the
        // input borrow so `find_navigate` can re-borrow `self`).
        if let Some(forward) = find_nav {
            self.find_navigate(forward);
        }
        // #72 — identifier completion is an EDITOR-surface popup. While any
        // text-input / navigation modal owns the keyboard (find bar, command
        // palette, fuzzy finder, go-to-symbol / go-to-line, recent files,
        // settings, cheatsheet, welcome), completion must NOT open and must NOT
        // intercept ↑↓/Enter — otherwise a Ctrl+Space typed into (say) the find
        // field would spawn a popup that then steals the find bar's navigation
        // keys. Force any open popup closed and leave Ctrl+Space for the modal.
        let modal_owns_keys = self.modal_owns_keyboard();
        if modal_owns_keys {
            self.completion = None;
        }
        // Ctrl/Cmd+Space requests identifier completion at the cursor (only when
        // the editor — not a modal — owns the keyboard; short-circuits so the
        // key is left unconsumed for a focused modal field).
        let want_completion = !modal_owns_keys
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Space));
        // While the completion popup is open, intercept navigation keys BEFORE
        // the TextEdit sees them so arrows/enter drive the list, not the caret.
        let mut accept_completion = false;
        if self.completion.is_some() {
            ctx.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    if let Some(c) = &mut self.completion {
                        c.selected = (c.selected + 1).min(c.items.len().saturating_sub(1));
                    }
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    if let Some(c) = &mut self.completion {
                        c.selected = c.selected.saturating_sub(1);
                    }
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                {
                    accept_completion = true;
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    self.completion = None;
                }
            });
        }
        // Deferred plugin-command invocation (set by palette/menu, applied after UI).
        let mut run_cmd: Option<String> = None;
        // Deferred config persistence (set by View-menu toggles).
        let mut save_cfg = false;
        // Deferred file-tree actions.
        let mut open_from_tree: Option<PathBuf> = None;
        let mut close_tree = false;
        // Deferred LSP start (set by the Language menu).
        let mut start_lsp = false;

        let accent = ui_color(&self.theme, "accent", Rgba::new(0, 255, 254, 255));
        // Secondary brand colour for the split-tone wordmark (`1 B 3` half). Falls
        // back to a complementary violet when a theme does not define `accent_alt`,
        // so existing single-accent themes keep working; the 12 brand themes each
        // set their own. Chrome stays one-accent everywhere ELSE (the split wordmark
        // is the single deliberate two-tone mark, per the brand discipline).
        let accent_alt = ui_color(&self.theme, "accent_alt", Rgba::new(0x9d, 0x7c, 0xff, 255));
        let muted = ui_color(&self.theme, "line_number", Rgba::new(0x5a, 0x58, 0x69, 255));
        // Chrome panels (titlebar/toolbar/status/filetree/split/gutter/minimap) all
        // fill with this color. In a translucent window mode the fill MUST carry the
        // reduced alpha — otherwise opaque chrome covers the transparent/blurred
        // surface and "transparency doesn't work" (the T19.2 root cause). The master
        // `transparency_enabled` toggle gates this via `effective_translucent()`.
        let panel = panel_fill(
            &self.theme,
            &self.config.window,
            self.config.appearance.background_override.as_deref(),
        );
        let warn = ui_color(&self.theme, "warning", Rgba::new(0xfb, 0xbf, 0x24, 255));

        // F11 fullscreen (editor-only): derive the OS fullscreen state each frame
        // (no separate field — avoids a re-sync race when the user exits via the
        // OS). `chrome_hidden` hides the toolbar/tabs/status/minimap/gutter; the
        // custom titlebar additionally hides in fullscreen (the OS gives no frame),
        // whereas zen keeps it for window dragging.
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        let chrome_hidden = self.zen_mode || fullscreen;
        // Toolbar-in-titlebar mode (only meaningful with the custom titlebar).
        let toolbar_in_titlebar =
            self.config.appearance.toolbar_in_titlebar && self.config.appearance.frameless;

        // ---- Custom frameless titlebar ----
        // Height is CONSTANT regardless of `toolbar_in_titlebar` — it is sized to
        // fit the quick-access toolbar buttons in BOTH states (so toggling the
        // option never resizes the titlebar). It grows only if the user raises the
        // toolbar button-size setting (a separate, expected knob), and never drops
        // below the bare-chrome baseline (34). Previously it was 40 when the
        // toolbar lived here and 34 otherwise, so flipping the toggle jumped it.
        let titlebar_h = (self.config.toolbar.clamped_button_size() + 10.0).max(34.0);
        if self.config.appearance.frameless && !fullscreen {
            egui::TopBottomPanel::top("titlebar")
                .exact_height(titlebar_h)
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    let resp = ui.interact(
                        ui.max_rect(),
                        egui::Id::new("titlebar-drag"),
                        egui::Sense::click_and_drag(),
                    );
                    if resp.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if resp.double_clicked() {
                        let is_max = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_max));
                    }
                    ui.horizontal_centered(|ui| {
                        // RESERVE the window caption buttons on the RIGHT first. The
                        // wordmark + in-titlebar toolbar then fill the space to their
                        // LEFT, clipped to that boundary — so on a narrow window the
                        // toolbar compresses/clips instead of the min/max/close buttons
                        // being painted over by it (the "caption buttons go over the
                        // toolbar when narrow" report). Previously the left content was
                        // laid out first and the caption buttons took only the leftover
                        // width, so a wide toolbar pushed them under itself / off-edge.
                        // Caption-button height tracks the toolbar button size so
                        // they stay consistent when the user picks a large size,
                        // while preserving the default 28px (`.max(28.0)`).
                        let cap_h = self.config.toolbar.clamped_button_size().max(28.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let is_max = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
                            let close_hover = Color32::from_rgb(0xE8, 0x11, 0x23);
                            let soft_hover = Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 26);
                            if caption_btn(ui, CaptionIcon::Close, muted, close_hover, cap_h)
                                .clicked()
                            {
                                // Funnel into the two-phase close (hide-before-destroy)
                                // so a transparent window leaves no DWM ghost (T19.1).
                                self.want_close = true;
                            }
                            let max_icon = if is_max {
                                CaptionIcon::Restore
                            } else {
                                CaptionIcon::Maximize
                            };
                            if caption_btn(ui, max_icon, muted, soft_hover, cap_h).clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_max));
                            }
                            if caption_btn(ui, CaptionIcon::Minimize, muted, soft_hover, cap_h)
                                .clicked()
                            {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                            }
                            // LEFT content: wordmark + (optional) in-titlebar toolbar,
                            // laid out left-to-right in the width remaining to the left
                            // of the caption buttons. The clip rect is pinned to that
                            // region so an overflowing toolbar can never paint over the
                            // reserved caption buttons.
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                                    ui.add_space(10.0);
                                    // Chrome text follows the APP UI font (Proportional
                                    // family), NOT the note/editor font. Split-tone
                                    // wordmark: "S C R " accent, "1 B 3" secondary;
                                    // painted with zero item-spacing so they read as ONE
                                    // wordmark.
                                    let saved_spacing = ui.spacing().item_spacing.x;
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    ui.label(RichText::new("S C R ").color(accent).strong());
                                    ui.label(RichText::new("1 B 3").color(accent_alt).strong());
                                    ui.spacing_mut().item_spacing.x = saved_spacing;
                                    // Decorative separator + JP subtitle (写本 —
                                    // shahon) drop out FIRST when the titlebar is
                                    // tight, so the core "SCR1B3" wordmark never has
                                    // to clip mid-glyph on a narrow window.
                                    if ui.available_width() > 120.0 {
                                        ui.add_space(6.0);
                                        ui.label(RichText::new("//").color(muted));
                                        ui.label(
                                            RichText::new(scribe_core::PRODUCT_SUBTITLE_JP)
                                                .color(muted)
                                                .small(),
                                        );
                                    }
                                    if toolbar_in_titlebar {
                                        ui.add_space(12.0);
                                        // Button PARITY with the standalone toolbar row:
                                        // same configured height + spacing so the buttons
                                        // are identical whether the toolbar lives here or
                                        // in its own row.
                                        let btn = self.config.toolbar.clamped_button_size();
                                        let gap = self.config.toolbar.clamped_button_spacing();
                                        ui.spacing_mut().interact_size.y = btn;
                                        ui.spacing_mut().item_spacing.x = gap;
                                        self.toolbar_contents(
                                            ui,
                                            &mut act,
                                            &mut save_cfg,
                                            &mut start_lsp,
                                        );
                                    }
                                },
                            );
                        });
                    });
                });
        }

        // ---- Quick-access toolbar (replaces the classic menu bar) ----
        // Hidden in zen / fullscreen; suppressed when moved into the titlebar.
        if !chrome_hidden && !toolbar_in_titlebar {
            egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
                // Phase 18 T18.5: apply the user-configurable button size + spacing
                // BEFORE the horizontal row so every quick-access item inherits the
                // sizing. All values are clamped at the config layer to defend
                // against a malformed user toml producing a 4000-px-tall toolbar.
                let btn = self.config.toolbar.clamped_button_size();
                let gap = self.config.toolbar.clamped_button_spacing();
                ui.spacing_mut().interact_size.y = btn;
                ui.spacing_mut().item_spacing.x = gap;
                ui.horizontal(|ui| {
                    self.toolbar_contents(ui, &mut act, &mut save_cfg, &mut start_lsp);
                });
            });
        }

        // ---- Tab strip in its OWN bar (T18.4) — separate from the toolbar ----
        //
        // #R5: in split/grid view the top tab strip is redundant — every pane
        // now carries its own chip header (note name + pin + close), so the
        // global strip is suppressed. New notes remain reachable via Ctrl+N,
        // the command palette, and the toolbar's customizable items.
        // The whole tab strip is hidden in zen mode and F11 fullscreen.
        // Set when the tab bar is at Bottom: its panel is rendered later, AFTER
        // the status bar, so the status bar keeps the very bottom screen edge and
        // the tab strip stacks directly above it (egui gives the first-shown bottom
        // panel the outermost slot).
        let mut bottom_tabs_deferred = false;
        if !chrome_hidden && !self.config.editor.grid_enabled {
            match self.config.editor.tab_bar_position {
                scribe_core::config::TabBarPosition::Top => {
                    // A dedicated tab bar directly below the quick-access toolbar
                    // (added after the "toolbar" top panel, so it stacks beneath it).
                    egui::TopBottomPanel::top("tabs-top")
                        .frame(egui::Frame::default().fill(panel))
                        .show(ctx, |ui| {
                            // PA-06: wrap the top strip in a HORIZONTAL ScrollArea
                            // (mirroring the side strips' vertical ScrollArea in
                            // `draw_side_tab_strip`) so that with many open tabs the
                            // overflowing tabs stay scroll-reachable instead of
                            // clipping off the right edge with no affordance.
                            egui::ScrollArea::horizontal()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| self.draw_tab_strip(ui, accent, muted));
                                });
                        });
                }
                scribe_core::config::TabBarPosition::Bottom => {
                    // DEFERRED: a bottom tab bar must sit ABOVE the status bar, but
                    // egui gives the FIRST-shown bottom panel the screen edge. The
                    // status panel is shown later (below), so rendering the tab strip
                    // here would pin it under the status bar. Defer it and render it
                    // immediately AFTER the status panel so status keeps the very
                    // bottom edge and the tab strip stacks directly above it.
                    bottom_tabs_deferred = true;
                }
                scribe_core::config::TabBarPosition::Left => {
                    let rotated = self.config.editor.side_tabs_rotated;
                    // Fit-to-content width (#16): the bar hugs the widest tab
                    // rather than a fixed 180px slab, so a short note name doesn't
                    // leave a big empty bar. `exact_width` auto-tracks the content
                    // every frame (no manual resize needed — it just fits).
                    let w = self.side_tab_bar_width(ctx, rotated);
                    egui::SidePanel::left("tabs-left")
                        .exact_width(w)
                        .frame(egui::Frame::default().fill(panel).inner_margin(4.0))
                        .show(ctx, |ui| {
                            self.draw_side_tab_strip(ui, accent, muted, rotated);
                        });
                }
                scribe_core::config::TabBarPosition::Right => {
                    let rotated = self.config.editor.side_tabs_rotated;
                    let w = self.side_tab_bar_width(ctx, rotated);
                    egui::SidePanel::right("tabs-right")
                        .exact_width(w)
                        .frame(egui::Frame::default().fill(panel).inner_margin(4.0))
                        .show(ctx, |ui| {
                            self.draw_side_tab_strip(ui, accent, muted, rotated);
                        });
                }
            }
        }

        // ---- Config-error banner (F-038) ----
        //
        // Persistent top banner when the config TOML failed to parse on
        // launch. Surfaces the error message + actionable choices:
        // "Open config" (opens the TOML file as a new tab so the user can
        // hand-edit it), "Restore default" (overwrites the file with the
        // default Config and reloads), and "Dismiss" (clears the banner
        // for the session — the user took ownership of the warning).
        let mut want_open_cfg = false;
        let mut want_restore_cfg = false;
        let mut want_dismiss_cfg = false;
        if let Some(msg) = self.config_error_banner.clone() {
            egui::TopBottomPanel::top("config-error-banner")
                .frame(
                    egui::Frame::default()
                        .fill(warn.linear_multiply(0.20))
                        .inner_margin(egui::Margin::same(6)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(egui_phosphor::thin::WARNING)
                                .color(warn)
                                .strong(),
                        );
                        ui.label(
                            RichText::new(format!("Config has errors: {msg}"))
                                .color(warn)
                                .monospace(),
                        );
                        if ui.button("Open config").clicked() {
                            want_open_cfg = true;
                        }
                        if ui.button("Restore default").clicked() {
                            want_restore_cfg = true;
                        }
                        if ui.button("Dismiss").clicked() {
                            want_dismiss_cfg = true;
                        }
                    });
                });
        }

        // ---- Update-available notice (notify mode) ----
        //
        // A PROMINENT top banner (accent-filled, bold) — not the passive toast —
        // so a found update is actually noticeable. Carries an "Update" button
        // that jumps straight to Settings → Updates to begin the update, plus a
        // "Dismiss" button. Shown only in `notify` mode (auto mode uses the modal).
        if let Some(v) = self.update_notice.clone() {
            let mut want_update = false;
            let mut want_dismiss = false;
            egui::TopBottomPanel::top("update-notice")
                .frame(
                    egui::Frame::default()
                        .fill(accent.linear_multiply(0.22))
                        .inner_margin(egui::Margin::symmetric(10, 7)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "SCR1B3 v{v} is available — you have v{}.",
                                crate::updater::current_version()
                            ))
                            .color(accent)
                            .strong(),
                        );
                        ui.add_space(8.0);
                        if ui.button(RichText::new("Update").strong()).clicked() {
                            want_update = true;
                        }
                        if ui.button("Dismiss").clicked() {
                            want_dismiss = true;
                        }
                    });
                });
            if want_update {
                // Jump to Settings → Updates so the user can start the update
                // (download → verify → restart) from the manual update controls.
                crate::settings::request_category(ctx, "Updates");
                self.settings_open = true;
                self.update_notice = None;
            }
            if want_dismiss {
                self.update_notice = None;
            }
        }

        // ---- External-change banner (F-022b) ----
        // A file open here was modified on disk WHILE it holds unsaved local
        // edits. Prompt the user to update to the saved version (or keep theirs)
        // instead of silently overwriting the newer file on save. A CLEAN tab is
        // reloaded silently by `poll_external_disk_changes` and never reaches here.
        if self.active < self.tabs.len() && self.tabs[self.active].external_change {
            let name = self.tabs[self.active].doc.file_name();
            let warn = egui::Color32::from_rgb(0xE0, 0x9A, 0x20);
            let mut want_reload = false;
            let mut want_keep = false;
            egui::TopBottomPanel::top("external-change-notice")
                .frame(
                    egui::Frame::default()
                        .fill(warn.linear_multiply(0.20))
                        .inner_margin(egui::Margin::symmetric(10, 7)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "{}  \"{name}\" was changed on disk, and you have unsaved edits here.",
                                egui_phosphor::thin::WARNING
                            ))
                            .color(warn)
                            .strong(),
                        );
                        ui.add_space(8.0);
                        if ui
                            .button(RichText::new("Reload from disk").strong())
                            .on_hover_text(
                                "Discard your unsaved edits and load the current saved version.",
                            )
                            .clicked()
                        {
                            want_reload = true;
                        }
                        if ui
                            .button("Keep my version")
                            .on_hover_text(
                                "Keep your edits — the next save will overwrite the disk version.",
                            )
                            .clicked()
                        {
                            want_keep = true;
                        }
                    });
                });
            let i = self.active;
            if want_reload {
                if let Some(path) = self.tabs[i].doc.path().map(|p| p.to_path_buf()) {
                    // ENC-1: encoding-preserving reload (see session_io.rs) — the
                    // user's explicit "reload from disk" must honour the file's
                    // detected encoding, not assume UTF-8.
                    if self.tabs[i].doc.reload_from_disk().is_ok() {
                        let fresh = self.tabs[i].doc.text();
                        self.tabs[i].set_text(fresh.clone());
                        self.tabs[i].disk_text = fresh;
                        if let Some(m) = file_mtime(&path) {
                            self.tabs[i].disk_mtime = Some(m);
                        }
                        // Change-bar: reloaded content is the new clean baseline.
                        self.tabs[i].reset_change_baselines();
                        self.status = format!("reloaded {} from disk", path.display());
                    }
                }
                self.tabs[i].external_change = false;
            }
            if want_keep {
                // Accept the current disk mtime as known so we stop re-prompting,
                // but keep the buffer + its unsaved edits (a later save overwrites
                // the disk file).
                if let Some(path) = self.tabs[i].doc.path().map(|p| p.to_path_buf()) {
                    if let Some(m) = file_mtime(&path) {
                        self.tabs[i].disk_mtime = Some(m);
                    }
                }
                self.tabs[i].external_change = false;
            }
        }

        // ---- Find / Replace bar ----
        //
        // F-008 from docs/audits/overlooked-surfaces-2026-05-29.md: the
        // pre-audit find bar had no replace field. Ctrl+F still opens
        // find-only; Ctrl+H opens the same bar with focus pre-set to the
        // replace field. "Replace next" replaces only the first match,
        // "Replace all" walks every match in the active buffer.
        if self.find_open {
            egui::TopBottomPanel::top("find").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("find").color(accent).monospace());
                    let r = ui.text_edit_singleline(&mut self.find_query);
                    if self.focus_find {
                        r.request_focus();
                        self.focus_find = false;
                    }
                    // Editing the query restarts navigation at the first match.
                    if self.find_query != self.find_last_query {
                        self.find_match_idx = 0;
                        self.find_last_query = self.find_query.clone();
                    }
                    let count = self.find_matches_active().len();
                    self.find_match_idx = self.find_match_idx.min(count.saturating_sub(1));
                    // Enter in the find field jumps to the next match.
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.find_navigate(true);
                    }
                    if ui
                        .add_enabled(
                            count > 0,
                            egui::Button::new(egui_phosphor::thin::ARROW_UP).small(),
                        )
                        .on_hover_text("Previous match (Shift+F3)")
                        .clicked()
                    {
                        self.find_navigate(false);
                    }
                    if ui
                        .add_enabled(
                            count > 0,
                            egui::Button::new(egui_phosphor::thin::ARROW_DOWN).small(),
                        )
                        .on_hover_text("Next match (F3 / Enter)")
                        .clicked()
                    {
                        self.find_navigate(true);
                    }
                    let counter = if count == 0 {
                        if self.find_query.is_empty() {
                            String::new()
                        } else {
                            "no matches".to_string()
                        }
                    } else {
                        format!("{}/{}", self.find_match_idx + 1, count)
                    };
                    ui.label(RichText::new(counter).color(muted).small());
                    if ui.button("close").clicked() {
                        self.find_open = false;
                    }
                });
                // Second row: replace field + actions.
                ui.horizontal(|ui| {
                    ui.label(RichText::new("with").color(accent).monospace());
                    let rr = ui.text_edit_singleline(&mut self.replace_query);
                    if self.focus_replace {
                        rr.request_focus();
                        self.focus_replace = false;
                    }
                    if ui.button("Replace next").clicked() {
                        self.replace_in_active(false);
                    }
                    if ui.button("Replace all").clicked() {
                        self.replace_in_active(true);
                    }
                });
            });
        }

        // ---- Wave-5: find in files (project-wide search results pane) ----
        if self.find_in_files_open {
            // PA-02: read Up/Down/Enter for RESULT navigation here (outside the
            // panel body), mirroring the command-palette / fuzzy-finder list-nav.
            // Enter opens the selected result, but ONLY when the query field is
            // not focused — an Enter in the query field triggers SEARCH (handled
            // below via `lost_focus()`), so the two Enter meanings never collide.
            let result_count = self.find_in_files_results.len();
            let (up, down, enter_pressed) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if result_count == 0 {
                self.find_in_files_selected = 0;
            } else {
                self.find_in_files_selected =
                    fuzzy_move_selection(self.find_in_files_selected, result_count, up, down);
            }
            let selected = self.find_in_files_selected;
            let mut open_selected_via_enter = false;
            egui::SidePanel::right("find_in_files")
                .resizable(true)
                .default_width(360.0)
                .frame(egui::Frame::default().fill(panel).inner_margin(6.0))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("find in files").color(accent).monospace());
                        if ui.button("close").clicked() {
                            self.find_in_files_open = false;
                        }
                    });
                    let r = ui.text_edit_singleline(&mut self.find_in_files_query);
                    if self.focus_find_in_files {
                        r.request_focus();
                        self.focus_find_in_files = false;
                    }
                    let query_focused = r.has_focus();
                    // Enter while the query is NOT focused (e.g. after arrow-key
                    // navigation moved focus into the results) opens the selected
                    // result — the keyboard-activate leg the audit (PA-02) flagged.
                    if enter_pressed && !query_focused && result_count > 0 {
                        open_selected_via_enter = true;
                    }
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.find_in_files_regex, "regex");
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if enter || ui.button("search").clicked() {
                            self.run_find_in_files(ctx);
                        }
                    });
                    if let Some(err) = &self.find_in_files_error {
                        ui.colored_label(Color32::from_rgb(0xe5, 0x3e, 0x3e), err);
                    }
                    // 4-02: streaming hint while the off-thread worker is walking.
                    if self.find_in_files_running {
                        ui.label(
                            RichText::new(format!(
                                "searching… {} so far",
                                self.find_in_files_results.len()
                            ))
                            .color(muted)
                            .small(),
                        );
                    }
                    ui.separator();
                    let mut open_target: Option<(PathBuf, usize)> = None;
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for (idx, m) in self.find_in_files_results.iter().enumerate() {
                            let name = m.path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                            let label = format!("{}:{}  {}", name, m.line, m.line_text.trim());
                            // PA-02: a highlighted selectable row (mirroring the
                            // palette / fuzzy finder) replaces the bare click-Label,
                            // so the keyboard-selected result is visibly distinct
                            // and Up/Down/Enter drive it — not mouse-click only.
                            let row = ui.selectable_label(
                                idx == selected,
                                RichText::new(label).monospace().small(),
                            );
                            if row.clicked() {
                                open_target = Some((m.path.clone(), m.line));
                            }
                            if idx == selected && (up || down) {
                                row.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                    });
                    // Enter (query unfocused) opens the keyboard-selected result.
                    if open_selected_via_enter {
                        if let Some(m) = self.find_in_files_results.get(selected) {
                            open_target = Some((m.path.clone(), m.line));
                        }
                    }
                    if let Some((path, line)) = open_target {
                        self.open_find_in_files_result(path, line);
                    }
                });
        }

        // ---- Command palette (built-in + plugin commands) ----
        //
        // F-004 fix from docs/audits/overlooked-surfaces-2026-05-29.md:
        // the palette previously surfaced only plugin commands. On a fresh
        // install (zero plugins loaded), opening Ctrl+Shift+P showed
        // "no plugin commands yet" — the editor's primary self-discovery
        // surface was empty. Now every built-in editor action is listed
        // alphabetically alongside plugin commands and the fuzzy filter
        // searches both.
        let mut run_builtin: Option<BuiltinCommand> = None;
        if self.palette_open {
            // BUG-APP-01 fix: build the filtered command list ONCE up front so
            // keyboard nav (Up/Down/Enter) and the rendered rows agree on the
            // same set — mirroring the fuzzy-file-finder's "rank once up front"
            // pattern (mod.rs `if self.fuzzy_open`). Each entry carries what to
            // run; the index into this Vec is the selectable highlight.
            enum PaletteAction {
                Builtin(BuiltinCommand),
                Plugin(String),
            }
            struct PaletteItem {
                display: String,
                action: PaletteAction,
                /// True for the first plugin command — render a separator above
                /// it, preserving the prior built-in/plugin visual split.
                separator_before: bool,
            }
            let q = self.palette_query.to_lowercase();
            let mut items: Vec<PaletteItem> = Vec::new();
            // Built-in commands first — universally available even with zero
            // plugins.
            for cmd in BUILTIN_COMMANDS {
                let label = cmd.label;
                let shortcut = cmd.shortcut;
                if q.is_empty()
                    || label.to_lowercase().contains(&q)
                    || shortcut.to_lowercase().contains(&q)
                {
                    let display = if shortcut.is_empty() {
                        label.to_string()
                    } else {
                        format!("{label}  ·  {shortcut}")
                    };
                    items.push(PaletteItem {
                        display,
                        action: PaletteAction::Builtin(cmd.action),
                        separator_before: false,
                    });
                }
            }
            let mut first_plugin = true;
            for c in &self.plugin_cmds {
                if q.is_empty() || c.label.to_lowercase().contains(&q) || c.id.contains(&q) {
                    items.push(PaletteItem {
                        display: format!("{}  ·  {}", c.label, c.plugin_id),
                        action: PaletteAction::Plugin(c.id.clone()),
                        separator_before: first_plugin,
                    });
                    first_plugin = false;
                }
            }

            // Read Up/Down/Enter here (outside the window body). A singleline
            // TextEdit ignores these keys, so this does not fight the query
            // field's caret — same rationale as the fuzzy finder.
            let (up, down, enter) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if items.is_empty() {
                self.palette_selected = 0;
            } else {
                self.palette_selected =
                    fuzzy_move_selection(self.palette_selected, items.len(), up, down);
                if enter {
                    match &items[self.palette_selected].action {
                        PaletteAction::Builtin(a) => run_builtin = Some(*a),
                        PaletteAction::Plugin(id) => run_cmd = Some(id.clone()),
                    }
                }
            }
            let selected = self.palette_selected;

            let mut query_changed = false;
            egui::Window::new(
                RichText::new(format!("{}  command palette", egui_phosphor::thin::COMMAND))
                    .color(accent)
                    .monospace(),
            )
            .collapsible(false)
            .resizable(false)
            // A fixed width so the primary command-discovery surface opens at a
            // consistent size (matching the other modal pickers) instead of
            // sizing to its content. Aligns with go-to-symbol/recent/fuzzy.
            .default_width(600.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 64.0])
            .show(ctx, |ui| {
                let r = ui.text_edit_singleline(&mut self.palette_query);
                if self.focus_palette {
                    r.request_focus();
                    self.focus_palette = false;
                }
                query_changed = r.changed();
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for (idx, item) in items.iter().enumerate() {
                            if item.separator_before {
                                ui.separator();
                            }
                            let row = ui.selectable_label(idx == selected, item.display.clone());
                            if row.clicked() {
                                match &item.action {
                                    PaletteAction::Builtin(a) => run_builtin = Some(*a),
                                    PaletteAction::Plugin(id) => run_cmd = Some(id.clone()),
                                }
                            }
                            // Keep the keyboard-highlighted row in view.
                            if idx == selected && (up || down) {
                                row.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                        if items.is_empty() {
                            ui.label(RichText::new("no match").color(muted).small());
                        }
                    });
            });
            // A new query invalidates the old highlight position — reset to the
            // top so Enter runs the new top match (acceptance criterion 2).
            if query_changed {
                self.palette_selected = 0;
            }
        }

        // ---- Settings window (deep customization, live preview) ----
        if self.settings_open {
            let changed = crate::settings::show(
                ctx,
                &mut self.config,
                &mut self.settings_open,
                &mut self.updater,
            );
            // F-039 — the Plugins section's "Manage plugins…" button stashes a
            // request flag; pick it up and open the plugin-manager modal.
            if crate::settings::take_open_plugin_manager_request(ctx) {
                self.plugin_manager
                    .ensure_defaults(Config::config_dir().as_deref());
                self.plugin_manager.open = true;
            }
            if changed {
                self.reapply_theme(ctx);
                // Spellcheck language / custom-dict edits take effect live.
                self.reload_spell_engine();
                // F-035 — push the always-on-top flag to the viewport
                // immediately so the toggle is live (no restart required).
                let level = if self.config.window.always_on_top {
                    egui::WindowLevel::AlwaysOnTop
                } else {
                    egui::WindowLevel::Normal
                };
                ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
                self.save_config();
            }
        }

        // ---- Keyboard cheatsheet (F1) ----
        //
        // F-014 from docs/audits/overlooked-surfaces-2026-05-29.md. Lists
        // every wired shortcut so the user doesn't have to guess. The table
        // is rendered as a markdown-like 2-column grid; the data lives in
        // KEYBOARD_SHORTCUTS so any future shortcut addition lands in one
        // place + the modal stays current.
        if self.cheatsheet_open {
            let mut still_open = true;
            egui::Window::new(
                RichText::new(format!(
                    "{}  keyboard shortcuts",
                    egui_phosphor::thin::KEYBOARD
                ))
                .color(accent)
                .monospace(),
            )
            .open(&mut still_open)
            .collapsible(false)
            .resizable(true)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| {
                        egui::Grid::new("cheatsheet-grid")
                            .num_columns(2)
                            .spacing([24.0, 6.0])
                            .striped(true)
                            .show(ui, |ui| {
                                for entry in KEYBOARD_SHORTCUTS {
                                    ui.label(RichText::new(entry.chord).color(accent).monospace());
                                    ui.label(RichText::new(entry.action).color(muted).small());
                                    ui.end_row();
                                }
                            });
                    });
                ui.add_space(8.0);
                ui.label(
                    RichText::new("press F1 or Esc to close")
                        .color(muted)
                        .small()
                        .monospace(),
                );
            });
            if !still_open {
                self.cheatsheet_open = false;
            }
        }

        // ---- Plugin manager modal (F-039 + F-040) ----
        //
        // Surfaces the Phase-20 plugin foundation. The host builds the Loaded
        // rows from `discover()` + `config.plugins.disabled`, passes the
        // plugins dir, and applies whatever action the modal returns.
        if self.plugin_manager.open {
            let plugins_dir = Config::config_dir()
                .map(|d| d.join("plugins"))
                .unwrap_or_else(|| PathBuf::from("plugins"));
            let loaded = self.discovered_plugin_rows(&plugins_dir);
            let action = self
                .plugin_manager
                .show(ctx, accent, muted, &loaded, &plugins_dir);
            if let Some(id) = action.toggle_disabled {
                if let Some(pos) = self.config.plugins.disabled.iter().position(|d| *d == id) {
                    self.config.plugins.disabled.remove(pos);
                } else {
                    self.config.plugins.disabled.push(id);
                }
                self.save_config();
            }
            if action.open_plugins_dir {
                // Best-effort: create the dir so the reveal lands somewhere,
                // then open it in the OS file manager.
                let _ = std::fs::create_dir_all(&plugins_dir);
                open_in_file_manager(&plugins_dir);
            }
            if let Some(id) = action.approve {
                self.approve_plugin(&id);
            }
        }

        // ---- Go-to-line modal (Ctrl+G) ----
        //
        // F-015 from docs/audits/overlooked-surfaces-2026-05-29.md. Accepts
        // a 1-based line number, or `N:C` for line + column. On Enter, the
        // editor's scroll-to-line path (existing `pending_scroll`) takes
        // the modal's target.
        if self.goto_open {
            let mut want_apply = false;
            let mut want_close = false;
            egui::Window::new(
                RichText::new(format!(
                    "{}  go to line",
                    egui_phosphor::thin::ARROW_LINE_RIGHT
                ))
                .color(accent)
                .monospace(),
            )
            .collapsible(false)
            .resizable(false)
            // Consistent fixed width like the other modal pickers (was
            // content-sized, so it opened narrower/inconsistent).
            .default_width(400.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 100.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let r = ui.text_edit_singleline(&mut self.goto_query);
                    if self.focus_goto {
                        r.request_focus();
                        self.focus_goto = false;
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        want_apply = true;
                    }
                    if ui.button("Go").clicked() {
                        want_apply = true;
                    }
                    if ui.button("Close").clicked() {
                        want_close = true;
                    }
                });
                ui.label(
                    RichText::new("line, or line:column (e.g. 42:10)")
                        .color(muted)
                        .small(),
                );
            });
            if want_apply {
                if let Some((line, _col)) = parse_goto_query(&self.goto_query) {
                    self.goto_line(line);
                    self.goto_open = false;
                }
            }
            if want_close {
                self.goto_open = false;
            }
        }

        // ---- Go-to-symbol modal (Ctrl+Shift+O) ----
        //
        // Lists the active buffer's definition scopes (from
        // `editor_features::symbol_scopes`), filterable by a substring query.
        // Selecting an entry jumps to its start line via the existing
        // `goto_line` scroll pipe. Modelled on the recent-files modal.
        if self.goto_symbol_open {
            let active = self.active.min(self.tabs.len().saturating_sub(1));
            // Bound the scan like the breadcrumb/sticky path does.
            let symbols = if !self.tabs.is_empty() && self.tabs[active].text.len() <= 500_000 {
                crate::editor_features::symbol_scopes(&self.tabs[active].text)
            } else {
                Vec::new()
            };
            let q = self.goto_symbol_query.trim().to_lowercase();
            // PA-01: filter ONCE up front so keyboard nav (Up/Down/Enter) and the
            // rendered rows agree on the same set — mirroring the command-palette /
            // fuzzy-finder "rank once up front" pattern. Each entry carries its
            // start line + a display string; the index into this Vec is the
            // selectable highlight.
            let matches: Vec<(usize, String)> = symbols
                .iter()
                .filter(|s| q.is_empty() || s.label.to_lowercase().contains(&q))
                .map(|s| {
                    let indent = "  ".repeat(s.depth);
                    (
                        s.start_line,
                        format!("{indent}{}  ·  {}", s.label, s.start_line + 1),
                    )
                })
                .collect();
            let mut chosen: Option<usize> = None;
            let mut want_close = false;
            // Read Up/Down/Enter here (outside the window body). A singleline
            // TextEdit ignores these keys, so this does not fight the query field's
            // caret — same rationale as the command palette / fuzzy finder.
            let (up, down, enter) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if matches.is_empty() {
                self.goto_symbol_selected = 0;
                if enter {
                    want_close = true;
                }
            } else {
                self.goto_symbol_selected =
                    fuzzy_move_selection(self.goto_symbol_selected, matches.len(), up, down);
                if enter {
                    chosen = Some(matches[self.goto_symbol_selected].0);
                }
            }
            let selected = self.goto_symbol_selected;
            let mut query_changed = false;
            egui::Window::new(
                RichText::new(format!("{}  go to symbol", egui_phosphor::thin::DIAMOND))
                    .color(accent)
                    .monospace(),
            )
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 100.0])
            .show(ctx, |ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.goto_symbol_query)
                        .hint_text("filter symbols")
                        .desired_width(f32::INFINITY),
                );
                if self.focus_goto_symbol {
                    r.request_focus();
                    self.focus_goto_symbol = false;
                }
                query_changed = r.changed();
                ui.separator();
                if symbols.is_empty() {
                    ui.label(
                        RichText::new("no symbols in this buffer")
                            .color(muted)
                            .small(),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            for (idx, (start_line, display)) in matches.iter().enumerate() {
                                let label = RichText::new(display.clone()).monospace();
                                let row = ui.selectable_label(idx == selected, label);
                                if row.clicked() {
                                    chosen = Some(*start_line);
                                }
                                // Keep the keyboard-highlighted row in view.
                                if idx == selected && (up || down) {
                                    row.scroll_to_me(Some(egui::Align::Center));
                                }
                            }
                            if matches.is_empty() {
                                ui.label(RichText::new("no match").color(muted).small());
                            }
                        });
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "{}{} select · Enter jumps to selection · Esc closes",
                        egui_phosphor::thin::ARROW_UP,
                        egui_phosphor::thin::ARROW_DOWN
                    ))
                    .color(muted)
                    .small()
                    .monospace(),
                );
            });
            // A new filter invalidates the old highlight position — reset to the
            // top so Enter runs the new top match.
            if query_changed {
                self.goto_symbol_selected = 0;
            }
            if let Some(line0) = chosen {
                self.goto_line(line0 + 1);
                self.goto_symbol_open = false;
            } else if want_close {
                self.goto_symbol_open = false;
            }
        }

        // ---- Recent files modal (Ctrl+R) ----
        //
        // F-012 from docs/audits/overlooked-surfaces-2026-05-29.md. Pops
        // a list of the MRU recent files. Click an entry → open. Esc →
        // close. Persists nothing — the recent list itself is owned by
        // EditorConfig::recent_files (already saved on every open).
        if self.recent_open {
            let mut chosen: Option<PathBuf> = None;
            let mut still_open = true;
            // PA-03: Up/Down move the highlight, Enter opens the selection —
            // mirroring the fuzzy finder. Read the keys outside the window body.
            let count = self.config.editor.recent_files.len();
            let (up, down, enter) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if count == 0 {
                self.recent_selected = 0;
            } else {
                self.recent_selected = fuzzy_move_selection(self.recent_selected, count, up, down);
                if enter {
                    chosen = self
                        .config
                        .editor
                        .recent_files
                        .get(self.recent_selected)
                        .cloned();
                }
            }
            let selected = self.recent_selected;
            egui::Window::new(
                RichText::new(format!(
                    "{}  recent files",
                    egui_phosphor::thin::CLOCK_COUNTER_CLOCKWISE
                ))
                .color(accent)
                .monospace(),
            )
            .open(&mut still_open)
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 100.0])
            .show(ctx, |ui| {
                if self.config.editor.recent_files.is_empty() {
                    ui.label(
                        RichText::new("no recent files yet — open something first")
                            .color(muted)
                            .small(),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            for (idx, p) in self.config.editor.recent_files.iter().enumerate() {
                                let label = RichText::new(p.display().to_string()).monospace();
                                let row = ui.selectable_label(idx == selected, label);
                                if row.clicked() {
                                    chosen = Some(p.clone());
                                }
                                if idx == selected && (up || down) {
                                    row.scroll_to_me(Some(egui::Align::Center));
                                }
                            }
                        });
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "{}{} select · Enter opens · Ctrl+R or Esc to close",
                        egui_phosphor::thin::ARROW_UP,
                        egui_phosphor::thin::ARROW_DOWN
                    ))
                    .color(muted)
                    .small()
                    .monospace(),
                );
            });
            if let Some(p) = chosen {
                self.open_path(p);
                self.recent_open = false;
            } else if !still_open {
                self.recent_open = false;
            }
        }

        // ---- Recent folders modal ----
        // Mirrors the recent-files modal for folders opened as the file-tree
        // root. Click an entry → set it as the root (and re-record it MRU-front
        // via open_folder_root). The list is owned by EditorConfig::recent_folders.
        if self.recent_folders_open {
            let mut chosen: Option<PathBuf> = None;
            let mut still_open = true;
            // PA-03: Up/Down move the highlight, Enter opens the selection.
            let count = self.config.editor.recent_folders.len();
            let (up, down, enter) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if count == 0 {
                self.recent_folders_selected = 0;
            } else {
                self.recent_folders_selected =
                    fuzzy_move_selection(self.recent_folders_selected, count, up, down);
                if enter {
                    chosen = self
                        .config
                        .editor
                        .recent_folders
                        .get(self.recent_folders_selected)
                        .cloned();
                }
            }
            let selected = self.recent_folders_selected;
            egui::Window::new(
                RichText::new(format!(
                    "{}  recent folders",
                    egui_phosphor::thin::CLOCK_COUNTER_CLOCKWISE
                ))
                .color(accent)
                .monospace(),
            )
            .open(&mut still_open)
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 100.0])
            .show(ctx, |ui| {
                if self.config.editor.recent_folders.is_empty() {
                    ui.label(
                        RichText::new("no recent folders yet — open a folder first")
                            .color(muted)
                            .small(),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            for (idx, p) in self.config.editor.recent_folders.iter().enumerate() {
                                let label = RichText::new(p.display().to_string()).monospace();
                                let row = ui.selectable_label(idx == selected, label);
                                if row.clicked() {
                                    chosen = Some(p.clone());
                                }
                                if idx == selected && (up || down) {
                                    row.scroll_to_me(Some(egui::Align::Center));
                                }
                            }
                        });
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "{}{} select · Enter opens · Esc to close",
                        egui_phosphor::thin::ARROW_UP,
                        egui_phosphor::thin::ARROW_DOWN
                    ))
                    .color(muted)
                    .small()
                    .monospace(),
                );
            });
            if let Some(p) = chosen {
                self.open_folder_root(p);
                self.recent_folders_open = false;
            } else if !still_open {
                self.recent_folders_open = false;
            }
        }

        // ---- Welcome modal (F-013) ----
        //
        // First-launch greeter: open file, open folder, pick from recent,
        // open settings, see keyboard shortcuts. Dismiss with the close
        // button (sets first_run_completed) or Esc (suppress this session
        // only). The decision-to-open happens at build() time; this
        // renderer just paints the state.
        if self.welcome_open {
            let mut want_new = false;
            let mut want_open = false;
            let mut want_open_folder = false;
            let mut want_recent = false;
            let mut want_settings = false;
            let mut want_cheatsheet = false;
            let mut want_dismiss_permanent = false;
            let mut still_open = true;
            egui::Window::new(
                RichText::new(format!("welcome to {}", scribe_core::PRODUCT_NAME))
                    .color(accent)
                    .monospace(),
            )
            .open(&mut still_open)
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(scribe_core::PRODUCT_TAGLINE)
                        .color(muted)
                        .monospace(),
                );
                ui.add_space(10.0);
                // Phosphor glyphs (loaded thin font); the old emoji (📄📂🗂⌖⌨✓)
                // have no glyph in JetBrains Mono and rendered as tofu (#R5).
                if ui
                    .button(format!(
                        "{}  New file (Ctrl+N)",
                        egui_phosphor::thin::FILE_PLUS
                    ))
                    .clicked()
                {
                    want_new = true;
                }
                if ui
                    .button(format!(
                        "{}  Open file… (Ctrl+O)",
                        egui_phosphor::thin::FILE_TEXT
                    ))
                    .clicked()
                {
                    want_open = true;
                }
                if ui
                    .button(format!(
                        "{}  Open folder…",
                        egui_phosphor::thin::FOLDER_OPEN
                    ))
                    .clicked()
                {
                    want_open_folder = true;
                }
                if ui
                    .button(format!(
                        "{}  Recent files (Ctrl+R)",
                        egui_phosphor::thin::CLOCK_COUNTER_CLOCKWISE
                    ))
                    .clicked()
                {
                    want_recent = true;
                }
                ui.separator();
                if ui
                    .button(format!("{}  Open Settings", egui_phosphor::thin::GEAR_SIX))
                    .clicked()
                {
                    want_settings = true;
                }
                if ui
                    .button(format!(
                        "{}  Show keyboard shortcuts (F1)",
                        egui_phosphor::thin::KEYBOARD
                    ))
                    .clicked()
                {
                    want_cheatsheet = true;
                }
                ui.add_space(10.0);
                if ui
                    .button(format!(
                        "{}  Don't show this again",
                        egui_phosphor::thin::CHECK
                    ))
                    .clicked()
                {
                    want_dismiss_permanent = true;
                }
                ui.label(
                    RichText::new("Esc dismisses for this session only.")
                        .color(muted)
                        .small(),
                );
            });
            if want_new {
                self.new_tab();
                self.welcome_open = false;
            }
            if want_open {
                self.open_dialog();
                self.welcome_open = false;
            }
            if want_open_folder {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.file_tree_root = Some(folder);
                }
                self.welcome_open = false;
            }
            if want_recent {
                self.recent_open = true;
                self.recent_selected = 0;
                self.welcome_open = false;
            }
            if want_settings {
                self.settings_open = true;
                self.welcome_open = false;
            }
            if want_cheatsheet {
                self.cheatsheet_open = true;
                self.welcome_open = false;
            }
            if want_dismiss_permanent {
                self.config.editor.first_run_completed = true;
                self.save_config();
                self.welcome_open = false;
            }
            if !still_open {
                self.welcome_open = false;
            }
        }

        // ---- Fuzzy file finder modal (Ctrl+P) ----
        //
        // F-010 from docs/audits/overlooked-surfaces-2026-05-29.md. Pre-
        // scanned project paths filtered by a stdlib-only subsequence
        // scorer (crate::fuzzy). Up to 200 ranked matches.
        if self.fuzzy_open {
            let mut chosen: Option<PathBuf> = None;
            let mut still_open = true;
            // Rank once up front so keyboard nav + the row list agree on the set.
            let ranked = crate::fuzzy::rank(&self.fuzzy_index, &self.fuzzy_query, 200);
            // #73 keyboard nav: Up/Down move the highlight, Enter opens it. A
            // singleline TextEdit ignores Up/Down/Enter-as-newline, so reading
            // these keys here does not fight the query field's caret.
            let (up, down, enter) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if !ranked.is_empty() {
                self.fuzzy_selected =
                    fuzzy_move_selection(self.fuzzy_selected, ranked.len(), up, down);
                if enter {
                    chosen = Some(ranked[self.fuzzy_selected].clone());
                }
            } else {
                self.fuzzy_selected = 0;
            }
            let selected = self.fuzzy_selected;
            let mut query_changed = false;
            egui::Window::new(
                RichText::new(format!(
                    "{}  open file",
                    egui_phosphor::thin::MAGNIFYING_GLASS
                ))
                .color(accent)
                .monospace(),
            )
            .open(&mut still_open)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 80.0])
            .show(ctx, |ui| {
                let r = ui.text_edit_singleline(&mut self.fuzzy_query);
                if self.focus_fuzzy {
                    r.request_focus();
                    self.focus_fuzzy = false;
                }
                query_changed = r.changed();
                ui.label(
                    RichText::new(format!(
                        "indexed {} files · {}{} select · Enter open · Esc close",
                        self.fuzzy_index.len(),
                        egui_phosphor::thin::ARROW_UP,
                        egui_phosphor::thin::ARROW_DOWN
                    ))
                    .color(muted)
                    .small(),
                );
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        if ranked.is_empty() {
                            ui.label(RichText::new("no match").color(muted).small().monospace());
                        }
                        for (idx, p) in ranked.iter().enumerate() {
                            let label = RichText::new(p.display().to_string()).monospace();
                            let row = ui.selectable_label(idx == selected, label);
                            if row.clicked() {
                                chosen = Some(p.clone());
                            }
                            // Keep the keyboard-highlighted row in view.
                            if idx == selected && (up || down) {
                                row.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                    });
            });
            // A new query invalidates the old highlight position.
            if query_changed {
                self.fuzzy_selected = 0;
            }
            if let Some(p) = chosen {
                self.open_path(p);
                self.fuzzy_open = false;
            } else if !still_open {
                self.fuzzy_open = false;
            }
        }

        // Spellcheck status (computed before the status-bar closure borrows self).
        let spell_on = self.config.spellcheck.enabled;
        let spell_misspellings = self.spell_count();
        let diag_errors = self.diagnostics.iter().filter(|d| d.severity == 1).count();
        let diag_total = self.diagnostics.len();

        // ---- Status bar ----
        let mut cycle_eol_for_active = false;
        let mut open_settings_for = None;
        // Hidden in zen / distraction-free mode and in F11 fullscreen.
        if !chrome_hidden {
            egui::TopBottomPanel::bottom("status")
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // Edge padding so the leftmost status segment isn't flush against
                        // the window edge (mirrors the titlebar's 10px lead-in).
                        ui.add_space(8.0);
                        let active = self.active.min(self.tabs.len().saturating_sub(1));
                        // PA-04: line/word/char counts via the (edit_gen, doc_id)
                        // memo — recomputed only on edit, not every idle frame.
                        let (lines, words, chars) = self.doc_counts_active(active);
                        if let Some(t) = self.tabs.get(active) {
                            // F-025 — clickable EOL segment cycles LF → CRLF → CR.
                            if ui
                                .selectable_label(
                                    false,
                                    RichText::new(t.doc.eol().label().to_string())
                                        .color(muted)
                                        .small()
                                        .monospace(),
                                )
                                .on_hover_text("Click to cycle line-ending: LF → CRLF → CR")
                                .clicked()
                            {
                                cycle_eol_for_active = true;
                            }
                            // F-025 — encoding + language: click opens Settings
                            // so the user lands on the relevant editor section.
                            if ui
                                .selectable_label(
                                    false,
                                    RichText::new(t.doc.encoding().name.clone())
                                        .color(muted)
                                        .small()
                                        .monospace(),
                                )
                                .on_hover_text("Click to open Settings → Editor")
                                .clicked()
                            {
                                open_settings_for = Some("Editor");
                            }
                            let lang = t.doc.language_hint().unwrap_or_else(|| "text".into());
                            if ui
                                .selectable_label(
                                    false,
                                    RichText::new(lang).color(accent).small().monospace(),
                                )
                                .on_hover_text("Click to open Settings → Editor (language hint)")
                                .clicked()
                            {
                                open_settings_for = Some("Editor");
                            }
                            // F-024 — word + line counters in the status bar.
                            // Computed via `doc_counts_active` (PA-04 memo): the
                            // three O(n) passes run once per edit, not per frame.
                            // Word/char are 0 for is_read_only_large() (multi-GB
                            // rope-browser) buffers, as before.
                            ui.label(
                                RichText::new(format!("{lines} ln · {words} w · {chars} ch"))
                                    .color(muted)
                                    .small()
                                    .monospace(),
                            );
                            // F-005 / F-024 from docs/audits/overlooked-surfaces-2026-05-29.md:
                            // Render the caret position ("Ln 4, Col 17") + the selection
                            // length when non-empty. Every editor on Earth ships this
                            // indicator; SCR1B3 used to omit it.
                            if let Some((ln, col)) = self.last_cursor_line_col {
                                ui.label(
                                    RichText::new(format!("Ln {ln}, Col {col}"))
                                        .color(muted)
                                        .small()
                                        .monospace(),
                                );
                            }
                            if self.last_selection_chars > 0 {
                                let sel = self.last_selection_chars;
                                let noun = if sel == 1 { "char" } else { "chars" };
                                ui.label(
                                    RichText::new(format!("({sel} {noun} sel)"))
                                        .color(accent)
                                        .small()
                                        .monospace(),
                                );
                            }
                            if t.doc.is_read_only_large() {
                                ui.label(
                                    RichText::new("[ large file: read-only ]")
                                        .color(muted)
                                        .small()
                                        .monospace(),
                                );
                            }
                            if spell_on {
                                let (txt, col) = if spell_misspellings == 0 {
                                    (format!("spell {}", egui_phosphor::thin::CHECK), accent)
                                } else {
                                    (format!("spell: {spell_misspellings}"), warn)
                                };
                                ui.label(RichText::new(txt).color(col).small().monospace());
                            }
                            if diag_total > 0 {
                                let col = if diag_errors > 0 { warn } else { muted };
                                ui.label(
                                    RichText::new(format!(
                                        "{} {diag_errors}e / {diag_total}",
                                        egui_phosphor::thin::PROHIBIT
                                    ))
                                    .color(col)
                                    .small()
                                    .monospace(),
                                );
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Inset from the right window edge so the last glyph
                            // isn't flush against it (right_to_left places this
                            // space at the right edge, before the text).
                            ui.add_space(8.0);
                            ui.label(RichText::new(&self.status).color(muted).small().monospace());
                        });
                    });
                });
        }
        // Bottom tab bar (deferred from the tab-position match): rendered HERE,
        // after the status panel, so the status bar keeps the very bottom edge
        // and the tab strip sits directly above it.
        if bottom_tabs_deferred {
            egui::TopBottomPanel::bottom("tabs-bottom")
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| self.draw_tab_strip(ui, accent, muted));
                });
        }
        // F-025 — apply the click-to-edit status-bar actions captured above.
        if cycle_eol_for_active {
            let active = self.active.min(self.tabs.len().saturating_sub(1));
            if let Some(t) = self.tabs.get_mut(active) {
                let next = match t.doc.eol() {
                    scribe_core::eol::Eol::Lf => scribe_core::eol::Eol::Crlf,
                    scribe_core::eol::Eol::Crlf => scribe_core::eol::Eol::Cr,
                    scribe_core::eol::Eol::Cr => scribe_core::eol::Eol::Lf,
                };
                t.doc.set_eol(next);
                self.status = format!("line-ending: {}", next.label());
            }
        }
        if let Some(section) = open_settings_for {
            // Honour the deep-link: open Settings ON the advertised category
            // (the tooltips promise "Settings → Editor"), not the last-used one.
            crate::settings::request_category(ctx, section);
            self.settings_open = true;
        }

        // ---- Toast (errors / notices) ----
        if let Some(msg) = self.toast.clone() {
            egui::TopBottomPanel::bottom("toast").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("!")
                            .color(ui_color(
                                &self.theme,
                                "warning",
                                Rgba::new(0xfb, 0xbf, 0x24, 255),
                            ))
                            .strong(),
                    );
                    ui.label(RichText::new(&msg).small());
                    if ui.small_button("dismiss").clicked() {
                        self.toast = None;
                    }
                });
            });
        }

        // ---- File-tree sidebar ----
        if let Some(root) = self.file_tree_root.clone() {
            egui::SidePanel::left("filetree")
                .default_width(220.0)
                .frame(egui::Frame::default().fill(panel).inner_margin(6.0))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // #74 — the tree supports ↑↓ Home End ⏎ navigation, but
                        // that was undiscoverable. Surface it: a hover tip on the
                        // header plus a always-visible muted key hint.
                        ui.label(RichText::new("EXPLORER").color(accent).small().monospace())
                            .on_hover_text(
                                "File explorer. Keyboard: ↑/↓ move · Home/End jump to first/last \
                                 · Enter open · (works when no dialog is open and the editor isn't \
                                 focused).",
                            );
                        ui.label(
                            RichText::new(format!(
                                "{}{} Home End {}",
                                egui_phosphor::thin::ARROW_UP,
                                egui_phosphor::thin::ARROW_DOWN,
                                egui_phosphor::thin::ARROW_ELBOW_DOWN_LEFT
                            ))
                            .color(muted)
                            .small()
                            .monospace(),
                        )
                        .on_hover_text("Navigate the file tree from the keyboard.");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").clicked() {
                                close_tree = true;
                            }
                        });
                    });
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if let Some(p) = self.file_tree_state.show(ui, &root) {
                            open_from_tree = Some(p);
                        }
                    });
                });
            // F-041: arrow-key / Enter / Home / End nav for the sidebar.
            // Only fires when no modal is open AND the editor isn't focused
            // (egui owns key events when a TextEdit holds focus, so we don't
            // need to gate explicitly on that — `consume_key` is a no-op
            // when the key was already routed to a widget).
            let modal_open = self.palette_open
                || self.find_open
                || self.fuzzy_open
                || self.goto_open
                || self.goto_symbol_open
                || self.recent_open
                || self.recent_folders_open
                || self.cheatsheet_open
                || self.settings_open
                || self.welcome_open;
            if !modal_open {
                if let Some(p) = self.file_tree_state.handle_input(ctx) {
                    open_from_tree = Some(p);
                }
            }
        }

        let active = self.active.min(self.tabs.len().saturating_sub(1));
        self.active = active;
        let font = FontId::monospace(self.config.fonts.clamped_editor_size());
        let line_height = self.config.fonts.clamped_line_height();
        let word_wrap = self.config.editor.word_wrap;
        let show_line_numbers = self.config.editor.show_line_numbers;
        let gutter_row_h = font.size * line_height;
        let ext = self.tabs[active].doc.language_hint();
        let read_only = self.tabs[active].doc.is_read_only_large();
        // The editor should be ready to type whenever no field/menu is open.
        let overlay_open = self.find_open || self.palette_open || self.settings_open;

        // ---- Wave-5 P1: markdown live preview (right side panel) ----
        // Only for markdown buffers; renders the buffer via pulldown-cmark.
        if self.md_preview_open && !chrome_hidden {
            let is_md = self
                .tabs
                .get(active)
                .and_then(|t| t.doc.language_hint())
                .map(|l| l == "md" || l == "markdown")
                .unwrap_or(false);
            if is_md {
                let md = self.tabs[active].text.clone();
                egui::SidePanel::right("md-preview")
                    .default_width(360.0)
                    .frame(egui::Frame::default().fill(panel).inner_margin(8.0))
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Markdown preview").color(muted).small());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("close").clicked() {
                                        self.md_preview_open = false;
                                    }
                                },
                            );
                        });
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                crate::md_preview::show(ui, &md, accent, muted);
                            });
                    });
            }
        }

        // ---- Wave-5 P1: diff vs disk (right side panel) ----
        if self.diff_view_open && !chrome_hidden {
            let cur = self.tabs.get(active).map(|t| t.text.clone());
            let disk = self
                .tabs
                .get(active)
                .and_then(|t| t.doc.path())
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default();
            let colors = crate::diff_view::DiffColors {
                insert: ui_color(&self.theme, "ok", Rgba::new(0x6e, 0xc7, 0x7a, 255)),
                delete: ui_color(&self.theme, "error", Rgba::new(0xd0, 0x6e, 0x6e, 255)),
                context: muted,
            };
            if let Some(cur) = cur {
                egui::SidePanel::right("diff-view")
                    .default_width(420.0)
                    .frame(egui::Frame::default().fill(panel).inner_margin(8.0))
                    .show(ctx, |ui| {
                        let rows = crate::diff_view::diff_lines(&disk, &cur);
                        let (ins, del) = crate::diff_view::summary(&rows);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Diff vs disk").color(muted).small());
                            ui.label(
                                RichText::new(format!("+{ins}"))
                                    .color(colors.insert)
                                    .small(),
                            );
                            ui.label(
                                RichText::new(format!("-{del}"))
                                    .color(colors.delete)
                                    .small(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("close").clicked() {
                                        self.diff_view_open = false;
                                    }
                                },
                            );
                        });
                        ui.separator();
                        crate::diff_view::show_rows(ui, &rows, colors);
                    });
            }
        }

        // ---- Minimap (rightmost strip) ----
        // Skipped for read-only huge files: the minimap hashes + lays out the
        // whole buffer, which defeats the viewport-culled browse path below.
        if self.config.editor.show_minimap && !read_only && !chrome_hidden {
            self.show_minimap(ctx, panel, accent);
        }

        // Split is no longer a separate same-buffer side panel — it is unified
        // with the multi-note grid (`editor.grid_enabled`): the open tabs render
        // as panes (two = side-by-side split, more = grid) via
        // `render_grid_central_panel`. See the "split" toolbar button + the
        // grid central-panel branch above.

        // ---- Line-number gutter (sticky left strip; numbers are synced to the
        // editor galley rows captured last frame — one-frame lag, like minimap).
        // The external gutter is driven by the TextEdit's per-line galley Ys
        // (`line_gutter`). The read-only RopeEditor draws its OWN gutter, so
        // skip this one there (and avoid the O(n) `lines().count()` on a
        // 256 MiB+ buffer).
        if show_line_numbers && !self.fold_view && !read_only && !chrome_hidden {
            // Change-bar: refresh the per-line state cache before borrowing it.
            self.ensure_change_states(active);
            // PA-05: reuse the PA-04 (edit_gen, doc_id) memo for the gutter
            // digit-width line count — no extra per-frame O(n) `lines().count()`.
            let total = self.doc_counts_active(active).0;
            let digits = total.to_string().len().max(2);
            let gutter_w = digits as f32 * (font.size * 0.62) + 16.0;
            let rows = &self.line_gutter;
            let bookmarks = &self.tabs[active].bookmarks;
            let show_change_bar = self.config.editor.show_change_bar;
            let change_states = &self.tabs[active].change_states;
            let cb_unsaved = ui_color(
                &self.theme,
                "change_bar_unsaved",
                Rgba::new(0xf2, 0xb3, 0x3d, 255),
            );
            let cb_saved = ui_color(
                &self.theme,
                "change_bar_saved",
                Rgba::new(0x6f, 0xb8, 0x9a, 255),
            );
            egui::SidePanel::left("line-gutter")
                .exact_width(gutter_w)
                .resizable(false)
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    let painter = ui.painter();
                    let clip = ui.clip_rect();
                    let rx = ui.max_rect().right() - 8.0;
                    let lx = ui.max_rect().left() + 4.0;
                    // Change bar sits flush against the gutter's right edge
                    // (between the numbers and the text), Notepad++-style.
                    let bar_r = ui.max_rect().right();
                    let nfont = FontId::monospace((font.size * 0.92).max(8.0));
                    for (i, &y) in rows.iter().enumerate() {
                        if y < clip.top() - gutter_row_h || y > clip.bottom() {
                            continue;
                        }
                        // Change-bar stripe: amber for edited-unsaved lines,
                        // green for edited-then-saved; untouched lines have none.
                        if show_change_bar {
                            let col = match change_states.get(i) {
                                Some(crate::change_bar::LineChange::Unsaved) => Some(cb_unsaved),
                                Some(crate::change_bar::LineChange::Saved) => Some(cb_saved),
                                _ => None,
                            };
                            if let Some(col) = col {
                                // 3.5px stripe flush to the gutter's right edge
                                // (Notepad++/VS Code use ~3px; a touch wider here
                                // so it reads clearly at the gutter boundary).
                                painter.rect_filled(
                                    egui::Rect::from_min_max(
                                        egui::pos2(bar_r - 3.5, y),
                                        egui::pos2(bar_r, y + gutter_row_h),
                                    ),
                                    0.0,
                                    col,
                                );
                            }
                        }
                        // Bookmark marker: a small filled dot at the gutter's
                        // left edge for each bookmarked (0-based) line.
                        if bookmarks.contains(&i) {
                            painter.circle_filled(
                                egui::pos2(lx, y + gutter_row_h * 0.5),
                                3.0,
                                accent,
                            );
                        }
                        painter.text(
                            egui::pos2(rx, y),
                            egui::Align2::RIGHT_TOP,
                            (i + 1).to_string(),
                            nfont.clone(),
                            muted,
                        );
                    }
                });
        }

        // ---- Central editor surface ----
        // Phase 18 T18.2 — when the multi-note grid is enabled, render
        // every open tab as a movable / resizable pane via egui_tiles.
        // The single-pane code path below stays the default for users
        // who don't opt in.
        if self.grid_tree.is_some() {
            self.render_grid_central_panel(ctx, font.clone());
        } else {
            egui::CentralPanel::default().show(ctx, |ui| {
                // Folded read-only preview is a distinct surface (no live editing).
                if self.fold_view {
                    self.show_fold_view(ui, font.clone(), ext.as_deref());
                    return;
                }

                // Read-only huge-file browse (KEYSTONE): a file past the
                // 256 MiB threshold opens read-only. Rendering it through the
                // viewport-culled RopeEditor — instead of laying out the whole
                // multi-hundred-MiB string in a TextEdit every frame — is the
                // O(viewport) browse path. Read-only ⇒ no editing regression;
                // the widget draws its own line numbers + viewport-scoped
                // syntax highlighting (F-030).
                if read_only {
                    let rope = self.tabs[active].doc.rope().clone();
                    let mut buf = scribe_core::buffer::Buffer::Rope(rope);
                    let fg = ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
                    scribe_render::RopeEditor::new(&mut buf, font.clone(), gutter_row_h)
                        .with_text_color(fg)
                        .with_gutter_color(muted)
                        .with_line_numbers(show_line_numbers)
                        .with_syntax(&self.hl, ext.clone())
                        .show(ui);
                    return;
                }

                // KEYSTONE — experimental owned rope editor (opt-in). Renders
                // normal files through the in-house editor (own caret /
                // selection / undo) instead of egui's TextEdit. The rope is
                // bridged from `text` each frame and written back after, so the
                // rest of the app (save, status bar, find) keeps seeing a
                // String. Default OFF — the egui path below stays canonical.
                // Wave-3: ALSO auto-engaged for buffers past the configured byte
                // threshold (default 16 MiB) so a multi-MiB file gets O(viewport)
                // rendering instead of the per-frame O(n) egui TextEdit.
                if use_rope_editor(
                    self.config.editor.experimental_rope_editor,
                    self.tabs[active].text.len(),
                    self.config.editor.rope_editor_auto_threshold_bytes,
                ) {
                    let fg = ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
                    // KEYSTONE perf: the rope persists across frames in the tab.
                    // Build it once (O(n)) from `text`; thereafter the widget
                    // mutates it in place and we sync back to `text` ONLY when
                    // an edit actually changed content. `ropey` clones are O(1)
                    // (Arc-shared), so persistence costs no extra memory churn.
                    // Capture the disjoint fields the editor needs BEFORE the
                    // `&mut self.tabs` borrow (Wave-5 P1 snippets — gated on the
                    // config toggle; `&self.snippets` coexists with the tab's
                    // mutable rope borrow as a disjoint-field borrow).
                    let render_whitespace = self.config.editor.render_whitespace;
                    let snippets_enabled = self.config.editor.snippets_enabled;
                    let snippets = &self.snippets;
                    let hl = &self.hl;
                    let tab = &mut self.tabs[active];
                    // Lazily (re)build the persistent rope from `text`. Done as a
                    // separate `is_none` check rather than `get_or_insert_with`
                    // so the closure does not capture `tab` while `rope_buf` is
                    // mutably borrowed (disjoint-field borrow).
                    if tab.rope_buf.is_none() {
                        tab.rope_buf = Some(scribe_core::buffer::Buffer::from_text(&tab.text));
                    }
                    let buf = tab.rope_buf.as_mut().expect("rope_buf set above");
                    let state = tab
                        .rope_state
                        .get_or_insert_with(scribe_render::RopeEditorState::new);
                    let mut editor =
                        scribe_render::RopeEditor::new(buf, font.clone(), gutter_row_h)
                            .with_text_color(fg)
                            .with_gutter_color(muted)
                            .with_line_numbers(show_line_numbers)
                            .with_render_whitespace(render_whitespace)
                            .with_syntax(hl, ext.clone());
                    if snippets_enabled {
                        editor = editor.with_snippets(snippets);
                    }
                    let (resp, clipboard) = editor.show_editable(ui, state);
                    // Sync `text` from the rope ONLY on a real content edit — the
                    // O(n) `to_string()` now runs on keystrokes, not every frame.
                    if resp.content_changed {
                        if let Some(rope) = tab.rope_buf.as_ref().and_then(|b| b.as_rope()) {
                            tab.text = rope.to_string();
                            tab.doc.mark_dirty();
                        }
                        // Wave-3: rope write-back bypasses set_text + the egui
                        // Response, so bump the gen counter here for parity.
                        tab.edit_gen = tab.edit_gen.wrapping_add(1);
                    }
                    if let Some(text) = clipboard {
                        if let Ok(mut cb) = arboard::Clipboard::new() {
                            let _ = cb.set_text(text);
                        }
                    }
                    return;
                }

                // F-033 / F-034 from docs/audits/overlooked-surfaces-2026-05-29.md:
                // brace-delimited definition scopes for the breadcrumb bar (above
                // the editor) and the sticky-scroll headers (pinned at the
                // viewport top). P-05: memoized by `(edit_gen, doc_id)` so the
                // O(n) scan runs only on an edit or a tab switch, not every
                // frame. Still skipped for very large buffers inside the memo.
                let scopes = self.symbol_scopes_for_active();
                // Breadcrumb bar (F-033): the enclosing-symbol path of the
                // cursor line, outermost first (`mod foo › impl Bar › fn baz`).
                if !scopes.is_empty() {
                    let cursor_line0 = self
                        .last_cursor_line_col
                        .map(|(l, _)| l.saturating_sub(1))
                        .unwrap_or(0);
                    let crumbs = crate::editor_features::breadcrumb_at(&scopes, cursor_line0);
                    if !crumbs.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for (i, s) in crumbs.iter().enumerate() {
                                if i > 0 {
                                    ui.label(RichText::new("›").color(muted).small());
                                }
                                ui.label(RichText::new(&s.label).color(accent).small().monospace());
                            }
                        });
                        ui.separator();
                    }
                }

                // Tab inserts the configured number of spaces (when insert_spaces is
                // on) rather than a literal tab — honours editor.tab_width /
                // insert_spaces. Consume the key before the TextEdit can see it.
                let editor_id = egui::Id::new("scr1b3-central-editor");
                if !read_only
                    && self.config.editor.insert_spaces
                    && ctx.memory(|m| m.has_focus(editor_id))
                    && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
                {
                    self.indent_with_spaces(ctx, editor_id, active);
                }

                // #107 — auto-indent on Enter: the new line keeps the current
                // line's leading whitespace. Only consume Enter when there IS
                // indentation to carry (otherwise let egui insert the plain
                // newline). Skipped while the completion popup owns Enter.
                if !read_only
                    && self.completion.is_none()
                    && ctx.memory(|m| m.has_focus(editor_id))
                    && ctx.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.is_none())
                    && self.auto_indent_newline(ctx, editor_id, active)
                {
                    ctx.input_mut(|i| {
                        i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                    });
                }

                // Caret commands from the keyboard (`act.*`) or the command
                // palette (`self.pending_*`). They need the egui TextEditState,
                // so they run here — after the editor stored its state this
                // frame — and `store` takes effect next frame.
                if !read_only {
                    if act.jump_bracket || std::mem::take(&mut self.pending_jump_bracket) {
                        self.jump_matching_bracket(ctx, editor_id, active);
                    }
                    if std::mem::take(&mut self.pending_insert_datetime) {
                        self.insert_datetime_at_caret(ctx, editor_id, active);
                    }
                    if std::mem::take(&mut self.pending_dup_selection) {
                        self.duplicate_selection(ctx, editor_id, active);
                    }
                }

                // #78 — misspellings for the active buffer, computed (memoized)
                // BEFORE the partial borrows below so the owned Vec can move into
                // the editor closure and drive the red underline painter.
                let misspellings = self.misspellings_for_active();
                // Wave-5: compute all find matches once (needs &self) so the
                // highlight-all overlay can paint every match, not just the
                // navigated one. Empty when the find bar is closed.
                let find_hits: Vec<scribe_core::search::Match> = if self.find_open {
                    self.find_matches_active()
                } else {
                    Vec::new()
                };
                let find_cur = self.find_match_idx;
                // Scope the layouter (which borrows `self.hl`) so it drops before
                // the `&mut self` completion calls below.
                let mut new_gutter: Vec<f32> = Vec::new();
                // F-034: a clicked sticky header records its target line here;
                // it is applied to `pending_scroll` after the hl borrow drops.
                let mut sticky_jump: Option<usize> = None;
                let anchor: Option<(egui::Pos2, usize)> = {
                    let hl = &self.hl;
                    let ext_ref = ext.as_deref();
                    let layout_fg =
                        ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
                    let mut layouter = make_layouter(
                        hl,
                        &self.hl_cache,
                        &self.hl_galley_cache,
                        &self.hl_inc_cache,
                        ext_ref,
                        font.clone(),
                        line_height,
                        word_wrap,
                        layout_fg,
                    );
                    let mut sa = if word_wrap {
                        egui::ScrollArea::vertical()
                    } else {
                        egui::ScrollArea::both()
                    };
                    if let Some(off) = self.pending_scroll.take() {
                        sa = sa.vertical_scroll_offset(off);
                    }
                    // Wave-6 scrollbar style.
                    sa = match self.config.editor.scrollbar_style {
                        scribe_core::config::ScrollbarStyle::Hidden => sa.scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                        ),
                        scribe_core::config::ScrollbarStyle::Thin
                        | scribe_core::config::ScrollbarStyle::Auto => sa.scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                        ),
                    };
                    let thin_scrollbar = self.config.editor.scrollbar_style
                        == scribe_core::config::ScrollbarStyle::Thin;
                    let mut a: Option<(egui::Pos2, usize)> = None;
                    let sa_out = sa.show(ui, |ui| {
                        if thin_scrollbar {
                            ui.style_mut().spacing.scroll.bar_width = 6.0;
                        }
                        let dw = if word_wrap {
                            ui.available_width()
                        } else {
                            f32::INFINITY
                        };
                        let editor = egui::TextEdit::multiline(&mut self.tabs[active].text)
                            .id(editor_id)
                            .code_editor()
                            .desired_width(dw)
                            .desired_rows(30)
                            .lock_focus(true)
                            .interactive(!read_only)
                            .layouter(&mut layouter);
                        let out = editor.show(ui);
                        // Wave-3: the egui in-place edit happened inside show();
                        // `.changed()` is true exactly on the edited frame, so this
                        // is the ONLY hook for the default editor's text mutation.
                        // Bump the gen counter so the minimap + spell caches refresh.
                        if out.response.changed() {
                            self.tabs[active].edit_gen = self.tabs[active].edit_gen.wrapping_add(1);
                        }
                        // #78 — paint a red squiggle under each misspelling. Map
                        // the byte span to galley cursor rects and draw a wavy
                        // underline along the word's baseline. Painted on the
                        // editor's own layer so it scrolls with the text.
                        if !misspellings.is_empty() {
                            let text_ref = &self.tabs[active].text;
                            let painter = ui.painter();
                            let red = Color32::from_rgb(0xe5, 0x3e, 0x3e);
                            for m in &misspellings {
                                let c0 = byte_to_char_index(text_ref, m.start);
                                let c1 = byte_to_char_index(text_ref, m.end);
                                let r0 = out.galley.pos_from_cursor(egui::text::CCursor::new(c0));
                                let r1 = out.galley.pos_from_cursor(egui::text::CCursor::new(c1));
                                // Same row only (words don't wrap); skip if the
                                // span spans rows (rare) to avoid a stray line.
                                if (r0.min.y - r1.min.y).abs() > 0.5 {
                                    continue;
                                }
                                let y = out.galley_pos.y + r0.max.y;
                                let x0 = out.galley_pos.x + r0.min.x;
                                let x1 = out.galley_pos.x + r1.min.x;
                                paint_squiggle(painter, x0, x1, y, red);
                            }
                        }
                        // Wave-5: incremental highlight-all — paint a translucent
                        // accent wash behind EVERY live find match (the current
                        // match stronger). Same galley-rect mapping as the
                        // squiggle painter; low alpha keeps the glyph legible.
                        if !find_hits.is_empty() {
                            let text_ref = &self.tabs[active].text;
                            let painter = ui.painter();
                            let hl_fill = accent.gamma_multiply(0.28);
                            let cur_fill = accent.gamma_multiply(0.5);
                            for (idx, m) in find_hits.iter().enumerate() {
                                let c0 = byte_to_char_index(text_ref, m.start);
                                let c1 = byte_to_char_index(text_ref, m.end);
                                let r0 = out.galley.pos_from_cursor(egui::text::CCursor::new(c0));
                                let r1 = out.galley.pos_from_cursor(egui::text::CCursor::new(c1));
                                if (r0.min.y - r1.min.y).abs() > 0.5 {
                                    continue;
                                }
                                let top = out.galley_pos.y + r0.min.y;
                                let bot = out.galley_pos.y + r0.max.y;
                                let x0 = out.galley_pos.x + r0.min.x;
                                let x1 = out.galley_pos.x + r1.min.x;
                                let fill = if idx == find_cur { cur_fill } else { hl_fill };
                                painter.rect_filled(
                                    egui::Rect::from_min_max(
                                        egui::pos2(x0, top),
                                        egui::pos2(x1, bot),
                                    ),
                                    2.0,
                                    fill,
                                );
                            }
                        }
                        // #28 — render-whitespace overlay for the DEFAULT egui
                        // TextEdit path. Previously the `·`/`→` markers only drew
                        // in the experimental rope editor, so the toggle did
                        // nothing in the default editor. Walk the laid-out galley
                        // glyphs (so the markers follow wrapping AND the chosen
                        // monospace face) and paint a faint `·` centred in each
                        // space cell, `→` in each tab cell. Pure overlay — the
                        // buffer text and the syntax spans are untouched.
                        if self.config.editor.render_whitespace {
                            let painter = ui.painter();
                            let ws_font =
                                FontId::monospace(self.config.fonts.clamped_editor_size());
                            let ws_color = muted.gamma_multiply(0.7);
                            let origin = out.galley_pos.to_vec2();
                            for row in &out.galley.rows {
                                let row_off = origin + row.pos.to_vec2();
                                let cy = row_off.y + row.size.y * 0.5;
                                for g in &row.glyphs {
                                    let marker = match g.chr {
                                        ' ' => "·",
                                        '\t' => "→",
                                        _ => continue,
                                    };
                                    let cx = row_off.x + g.pos.x + g.advance_width * 0.5;
                                    painter.text(
                                        egui::pos2(cx, cy),
                                        egui::Align2::CENTER_CENTER,
                                        marker,
                                        ws_font.clone(),
                                        ws_color,
                                    );
                                }
                            }
                        }
                        // Wave-6 indent guides: faint vertical lines at each
                        // tab_width column, drawn by walking the laid-out galley so
                        // they follow the chosen monospace face + wrapping.
                        if self.config.editor.indent_guides {
                            let painter = ui.painter();
                            let origin = out.galley_pos.to_vec2();
                            let cell_w = out
                                .galley
                                .rows
                                .iter()
                                .flat_map(|r| r.glyphs.iter())
                                .map(|g| g.advance_width)
                                .find(|w| *w > 0.0)
                                .unwrap_or(self.config.fonts.clamped_editor_size() * 0.6);
                            let step = cell_w * self.config.editor.tab_width as f32;
                            if step > 1.0 {
                                let guide = Color32::from_rgba_unmultiplied(
                                    muted.r(),
                                    muted.g(),
                                    muted.b(),
                                    40,
                                );
                                for row in &out.galley.rows {
                                    let row_off = origin + row.pos.to_vec2();
                                    let lead: f32 = row
                                        .glyphs
                                        .iter()
                                        .take_while(|g| g.chr == ' ' || g.chr == '\t')
                                        .map(|g| g.advance_width)
                                        .sum();
                                    let top = row_off.y;
                                    let bot = row_off.y + row.size.y;
                                    let mut x = row_off.x + step;
                                    while x <= row_off.x + lead + 0.5 {
                                        painter.line_segment(
                                            [egui::pos2(x, top), egui::pos2(x, bot)],
                                            egui::Stroke::new(1.0, guide),
                                        );
                                        x += step;
                                    }
                                }
                            }
                        }
                        // Trailing-whitespace tint: faintly mark the trailing
                        // space/tab run on each line (distinct from
                        // render_whitespace, which marks ALL whitespace).
                        if self.config.editor.highlight_trailing_whitespace {
                            let painter = ui.painter();
                            let tint = ui_color(
                                &self.theme,
                                "trailing_whitespace",
                                Rgba::new(0xd0, 0x6e, 0x6e, 28),
                            );
                            let origin = out.galley_pos.to_vec2();
                            for row in &out.galley.rows {
                                let row_off = origin + row.pos.to_vec2();
                                let mut run_start: Option<f32> = None;
                                let mut run_end = 0.0;
                                for g in &row.glyphs {
                                    if g.chr == ' ' || g.chr == '\t' {
                                        if run_start.is_none() {
                                            run_start = Some(row_off.x + g.pos.x);
                                        }
                                        run_end = row_off.x + g.pos.x + g.advance_width;
                                    } else {
                                        run_start = None;
                                    }
                                }
                                if let Some(sx) = run_start {
                                    painter.rect_filled(
                                        egui::Rect::from_min_max(
                                            egui::pos2(sx, row_off.y),
                                            egui::pos2(run_end, row_off.y + row.size.y),
                                        ),
                                        0.0,
                                        tint,
                                    );
                                }
                            }
                        }
                        // Column rulers: thin vertical guides at the configured
                        // 1-based columns (monospace; most meaningful without wrap).
                        if !self.config.editor.rulers.is_empty() {
                            let painter = ui.painter();
                            let cell_w = out
                                .galley
                                .rows
                                .iter()
                                .flat_map(|r| r.glyphs.iter())
                                .map(|g| g.advance_width)
                                .find(|w| *w > 0.0)
                                .unwrap_or(self.config.fonts.clamped_editor_size() * 0.6);
                            let ruler = ui_color(
                                &self.theme,
                                "ruler",
                                Rgba::new(muted.r(), muted.g(), muted.b(), 40),
                            );
                            let top = out.galley_pos.y;
                            let bot =
                                out.galley_pos.y + out.galley.size().y.max(ui.available_height());
                            for &col in &self.config.editor.rulers {
                                let x = out.galley_pos.x + cell_w * col as f32;
                                painter.line_segment(
                                    [egui::pos2(x, top), egui::pos2(x, bot)],
                                    egui::Stroke::new(1.0, ruler),
                                );
                            }
                        }
                        if let Some(range) = out.cursor_range {
                            // egui 0.34: CursorRange.primary is a CCursor directly
                            // (no nested .ccursor); Galley::pos_from_ccursor was
                            // renamed to pos_from_cursor (takes CCursor by value).
                            let cc = range.primary;
                            let rect = out.galley.pos_from_cursor(cc);
                            let pos = out.galley_pos + egui::vec2(rect.min.x, rect.max.y);
                            a = Some((pos, cc.index));
                            // F-005 / F-024 from docs/audits/overlooked-surfaces-2026-05-29.md:
                            // compute the human-visible (1-based) line + column and the
                            // selection-length-in-chars from the rope buffer + the
                            // egui CursorRange. This drives the status-bar "Ln N, Col N"
                            // and "(N chars selected)" indicators.
                            let text_ref = &self.tabs[active].text;
                            self.last_cursor_line_col =
                                Some(line_col_from_char_index(text_ref, cc.index));
                            self.last_selection_chars =
                                range.primary.index.abs_diff(range.secondary.index);
                            // Wave-6 motion: feed the caret-trail when the caret moves.
                            if self.config.motion.enabled && self.config.motion.caret_trail {
                                let t = ui.input(|i| i.time);
                                let caret_rect = egui::Rect::from_min_max(
                                    out.galley_pos + rect.min.to_vec2(),
                                    out.galley_pos + rect.max.to_vec2(),
                                )
                                .expand2(egui::vec2(1.0, 0.0));
                                let moved = self
                                    .caret_trail
                                    .back()
                                    .is_none_or(|(r, _)| r.min.distance(caret_rect.min) > 1.0);
                                if moved {
                                    self.caret_trail.push_back((caret_rect, t));
                                    while self.caret_trail.len() > 24 {
                                        self.caret_trail.pop_front();
                                    }
                                }
                            }
                            let collapsed = range.primary.index == range.secondary.index;
                            // Highlight every OTHER occurrence of the current
                            // selection (VS Code style). Single-line,
                            // non-whitespace selections only; bounded like
                            // bracket_match to stay cheap on huge files.
                            if self.config.editor.highlight_selection_occurrences
                                && !collapsed
                                && self.tabs[active].text.len() <= 500_000
                            {
                                let text_ref = &self.tabs[active].text;
                                let lo_ci = range.primary.index.min(range.secondary.index);
                                let hi_ci = range.primary.index.max(range.secondary.index);
                                let lo_b = char_to_byte(text_ref, lo_ci);
                                let hi_b = char_to_byte(text_ref, hi_ci);
                                let selected = &text_ref[lo_b..hi_b];
                                if !selected.trim().is_empty() && !selected.contains('\n') {
                                    let q = scribe_core::search::Query {
                                        pattern: selected.to_string(),
                                        case_sensitive: true,
                                        ..Default::default()
                                    };
                                    if let Ok(hits) = scribe_core::search::find_all(text_ref, &q) {
                                        let painter = ui.painter();
                                        let occ = ui_color(
                                            &self.theme,
                                            "selection_occurrence",
                                            Rgba::new(accent.r(), accent.g(), accent.b(), 130),
                                        );
                                        for m in &hits {
                                            if m.start == lo_b {
                                                continue; // skip the active selection itself
                                            }
                                            let c0 = byte_to_char_index(text_ref, m.start);
                                            let c1 = byte_to_char_index(text_ref, m.end);
                                            let r0 = out
                                                .galley
                                                .pos_from_cursor(egui::text::CCursor::new(c0));
                                            let r1 = out
                                                .galley
                                                .pos_from_cursor(egui::text::CCursor::new(c1));
                                            if (r0.min.y - r1.min.y).abs() > 0.5 {
                                                continue; // wrapped span; skip
                                            }
                                            let bx = egui::Rect::from_min_max(
                                                out.galley_pos + egui::vec2(r0.min.x, r0.min.y),
                                                out.galley_pos + egui::vec2(r1.min.x, r0.max.y),
                                            );
                                            painter.rect_stroke(
                                                bx,
                                                2.0,
                                                egui::Stroke::new(1.0, occ),
                                                egui::StrokeKind::Inside,
                                            );
                                        }
                                    }
                                }
                            }
                            // Wave-6 current-line highlight: a faint full-width band
                            // across the caret's galley row. Low alpha so it reads as
                            // a tint behind the (opaque) glyphs. Skipped on selection.
                            if self.config.editor.current_line_highlight && collapsed {
                                let painter = ui.painter();
                                let y0 = out.galley_pos.y + rect.min.y;
                                let y1 = out.galley_pos.y + rect.max.y;
                                let band = egui::Rect::from_min_max(
                                    egui::pos2(out.galley_pos.x, y0),
                                    egui::pos2(
                                        out.galley_pos.x
                                            + out.galley.size().x.max(ui.available_width()),
                                        y1,
                                    ),
                                );
                                let hl = Color32::from_rgba_unmultiplied(
                                    accent.r(),
                                    accent.g(),
                                    accent.b(),
                                    22,
                                );
                                painter.rect_filled(band, 0.0, hl);
                            }
                            // Wave-6 bracket-match: box the bracket next to the caret
                            // and its partner. The O(n) scan is bounded to a sane
                            // buffer size to stay cheap on huge files.
                            if self.config.editor.bracket_match
                                && collapsed
                                && self.tabs[active].text.len() <= 500_000
                            {
                                let text_ref = &self.tabs[active].text;
                                if let Some((open_ci, close_ci)) =
                                    matching_bracket_char_indices(text_ref, cc.index)
                                {
                                    let painter = ui.painter();
                                    let box_col = Color32::from_rgba_unmultiplied(
                                        accent.r(),
                                        accent.g(),
                                        accent.b(),
                                        60,
                                    );
                                    for ci in [open_ci, close_ci] {
                                        let r0 = out
                                            .galley
                                            .pos_from_cursor(egui::text::CCursor::new(ci));
                                        let r1 = out
                                            .galley
                                            .pos_from_cursor(egui::text::CCursor::new(ci + 1));
                                        if (r0.min.y - r1.min.y).abs() > 0.5 {
                                            continue; // span wrapped; skip
                                        }
                                        let bx = egui::Rect::from_min_max(
                                            out.galley_pos + egui::vec2(r0.min.x, r0.min.y),
                                            out.galley_pos + egui::vec2(r1.min.x, r0.max.y),
                                        );
                                        painter.rect_stroke(
                                            bx,
                                            1.0,
                                            egui::Stroke::new(1.0, box_col),
                                            egui::StrokeKind::Inside,
                                        );
                                    }
                                }
                            }
                            // Wave-6 caret style: draw a Block/Underline shape over
                            // egui's native caret (focus + no selection only). Honour
                            // blink when motion.cursor_blink is on.
                            if self.config.editor.caret_style
                                != scribe_core::config::CaretStyle::Bar
                                && collapsed
                                && out.response.has_focus()
                            {
                                let now = ui.ctx().input(|i| i.time);
                                let blink =
                                    self.config.motion.enabled && self.config.motion.cursor_blink;
                                let visible = if blink {
                                    (now / 1.06).rem_euclid(1.0) < 0.6
                                } else {
                                    true
                                };
                                if blink {
                                    ui.ctx().request_repaint_after(
                                        std::time::Duration::from_millis(120),
                                    );
                                }
                                if visible {
                                    let painter = ui.painter();
                                    let caret_col = ui_color(
                                        &self.theme,
                                        "caret",
                                        Rgba::new(accent.r(), accent.g(), accent.b(), 255),
                                    );
                                    let x = out.galley_pos.x + rect.min.x;
                                    let y0 = out.galley_pos.y + rect.min.y;
                                    let y1 = out.galley_pos.y + rect.max.y;
                                    let w = self.config.editor.clamped_caret_width();
                                    let cell_w = out
                                        .galley
                                        .rows
                                        .iter()
                                        .flat_map(|r| r.glyphs.iter())
                                        .map(|g| g.advance_width)
                                        .find(|w| *w > 0.0)
                                        .unwrap_or(self.config.fonts.clamped_editor_size() * 0.6);
                                    match self.config.editor.caret_style {
                                        scribe_core::config::CaretStyle::Block => {
                                            let blk = Color32::from_rgba_unmultiplied(
                                                caret_col.r(),
                                                caret_col.g(),
                                                caret_col.b(),
                                                110,
                                            );
                                            painter.rect_filled(
                                                egui::Rect::from_min_max(
                                                    egui::pos2(x, y0),
                                                    egui::pos2(x + cell_w, y1),
                                                ),
                                                0.0,
                                                blk,
                                            );
                                        }
                                        scribe_core::config::CaretStyle::Underline => {
                                            painter.rect_filled(
                                                egui::Rect::from_min_max(
                                                    egui::pos2(x, y1 - w.max(2.0)),
                                                    egui::pos2(x + cell_w, y1),
                                                ),
                                                0.0,
                                                caret_col,
                                            );
                                        }
                                        scribe_core::config::CaretStyle::Bar => {}
                                    }
                                }
                            }
                            // Wider Bar caret (width only): egui's caret is ~1px;
                            // overpaint a wider bar at the same x when width > 1.5.
                            if self.config.editor.caret_style
                                == scribe_core::config::CaretStyle::Bar
                                && self.config.editor.clamped_caret_width() > 1.5
                                && collapsed
                                && out.response.has_focus()
                            {
                                let painter = ui.painter();
                                let caret_col = ui_color(
                                    &self.theme,
                                    "caret",
                                    Rgba::new(accent.r(), accent.g(), accent.b(), 255),
                                );
                                let x = out.galley_pos.x + rect.min.x;
                                let y0 = out.galley_pos.y + rect.min.y;
                                let y1 = out.galley_pos.y + rect.max.y;
                                painter.rect_filled(
                                    egui::Rect::from_min_max(
                                        egui::pos2(x, y0),
                                        egui::pos2(
                                            x + self.config.editor.clamped_caret_width(),
                                            y1,
                                        ),
                                    ),
                                    0.0,
                                    caret_col,
                                );
                            }
                        }
                        // Capture each logical line's screen Y for the gutter (a row
                        // starts a logical line iff the previous row ended with \n).
                        if show_line_numbers {
                            let top = out.galley_pos.y;
                            let mut prev_newline = true;
                            for row in &out.galley.rows {
                                if prev_newline {
                                    // egui 0.34: PlacedRow.rect is now a method, not a field.
                                    new_gutter.push(top + row.rect().min.y);
                                }
                                prev_newline = row.ends_with_newline;
                            }
                        }
                        // Auto-focus the editor so typing works immediately on launch,
                        // new tab, or tab switch — no click required — unless a field,
                        // menu, or popup currently owns keyboard focus.
                        if !read_only
                            && !overlay_open
                            && ui.ctx().memory(|m| m.focused().is_none())
                            && !egui::Popup::is_any_open(ui.ctx())
                        {
                            out.response.request_focus();
                        }
                    });
                    // Record scroll metrics for the minimap's viewport indicator.
                    self.scroll_metrics = (
                        sa_out.state.offset.y,
                        sa_out.content_size.y.max(1.0),
                        sa_out.inner_rect.height().max(1.0),
                    );
                    // F-034 sticky scroll: pin the enclosing definition headers
                    // at the top of the viewport once their own header line has
                    // scrolled above it. Drawn with an opaque chrome fill so the
                    // pinned line occludes the scrolled body behind it. Clicking
                    // a pinned header jumps to that definition.
                    if !scopes.is_empty() {
                        let lh_px = (font.size * line_height).max(1.0);
                        let first_visible_line = (sa_out.state.offset.y / lh_px).floor() as usize;
                        let pinned =
                            crate::editor_features::sticky_chain_at(&scopes, first_visible_line, 5);
                        let vp = sa_out.inner_rect;
                        let bg = Color32::from_rgb(panel.r(), panel.g(), panel.b());
                        let painter = ui.painter_at(vp);
                        for (i, s) in pinned.iter().enumerate() {
                            let y = vp.top() + (i as f32) * lh_px;
                            let row = egui::Rect::from_min_max(
                                egui::pos2(vp.left(), y),
                                egui::pos2(vp.right(), y + lh_px),
                            );
                            painter.rect_filled(row, 0.0, bg);
                            let indent = 6.0 + (s.depth as f32) * 12.0;
                            painter.text(
                                egui::pos2(vp.left() + indent, y + lh_px * 0.5),
                                egui::Align2::LEFT_CENTER,
                                &s.label,
                                font.clone(),
                                accent,
                            );
                            if i + 1 == pinned.len() {
                                // Underline the bottom of the pinned stack so it
                                // reads as a header band, not part of the buffer.
                                painter.line_segment(
                                    [
                                        egui::pos2(vp.left(), row.bottom()),
                                        egui::pos2(vp.right(), row.bottom()),
                                    ],
                                    egui::Stroke::new(1.0, muted),
                                );
                            }
                            let resp = ui.interact(
                                row,
                                ui.id().with(("scr1b3-sticky", i)),
                                egui::Sense::click(),
                            );
                            if resp.clicked() {
                                sticky_jump = Some(s.start_line);
                            }
                        }
                    }
                    a
                };
                self.line_gutter = new_gutter;
                // F-034: apply a sticky-header click now that the hl borrow is
                // released. Scrolls so the clicked definition sits at the top.
                if let Some(line0) = sticky_jump {
                    let lh_px = (font.size * line_height).max(1.0);
                    self.pending_scroll = Some((line0 as f32) * lh_px);
                }

                // Completion: open on Ctrl+Space, accept on Enter/Tab, render popup.
                let cursor_idx = anchor.map(|(_, i)| i);
                if want_completion {
                    self.open_completion(active, cursor_idx);
                }
                if accept_completion {
                    self.accept_completion(active, cursor_idx);
                }
                if let Some((pos, _)) = anchor {
                    let choice = self
                        .completion
                        .as_ref()
                        .and_then(|c| completion_popup(ui, pos, c));
                    if let Some(idx) = choice {
                        if let Some(c) = self.completion.as_mut() {
                            c.selected = idx;
                        }
                        self.accept_completion(active, cursor_idx);
                    }
                }
            });
        }

        // Window color-tint overlay (subtle wash; portable across modes/OSes).
        if self.config.window.tint_strength > 0.0 {
            paint_tint_overlay(
                ctx,
                &self.config.window.tint,
                self.config.window.tint_strength,
            );
        }
        // CRT scanlines post-effect (#14, ported from C0PL4ND). A calm animated
        // retro overlay; only when motion AND scanlines are both enabled. Drives
        // a modest ~30 fps repaint while on so the bands drift (no busy-spin), and
        // never paints in the headless test harness (no real window to overlay).
        if !cfg!(test) && self.config.motion.enabled && self.config.motion.crt_scanlines {
            let t = ctx.input(|i| i.time);
            paint_crt_scanlines(ctx, self.config.motion.scanline_darkness, t);
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
        // Wave-6 motion overlays (master-gated; never in the headless harness).
        // Each is a calm post-effect; while any is active we drive a ~30 fps
        // repaint so it animates. The resting (motion-off) frame is unchanged.
        if !cfg!(test) && self.config.motion.enabled {
            let t = ctx.input(|i| i.time);
            let accent = ui_color(&self.theme, "accent", Rgba::new(0x4c, 0xc2, 0xff, 255));
            let mut animating = false;
            if self.config.motion.wired_ambient {
                paint_wired_mesh(ctx, self.config.motion.clamped_mesh_density(), accent, t);
                animating = true;
            }
            if self.config.motion.vhs_tracking {
                paint_vhs_tracking(ctx, t);
                animating = true;
            }
            if self.config.motion.flicker {
                paint_flicker(ctx, self.config.motion.clamped_flicker_strength(), t);
                animating = true;
            }
            if self.config.motion.caret_trail {
                while let Some(&(_, born)) = self.caret_trail.front() {
                    if t - born > 0.45 {
                        self.caret_trail.pop_front();
                    } else {
                        break;
                    }
                }
                paint_caret_trail(ctx, &self.caret_trail, accent, t);
                if !self.caret_trail.is_empty() {
                    animating = true;
                }
            }
            if self.config.motion.boot_glitch {
                let started = *self.boot_glitch_started.get_or_insert(t);
                let elapsed = t - started;
                if elapsed <= 0.55 {
                    paint_boot_glitch(ctx, elapsed);
                    animating = true;
                }
            }
            if animating {
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
            }
        }
        // Phase 18 T18.1: 8-zone resize overlay for the frameless window. egui
        // doesn't restore OS resize when window decorations are off (winit
        // #4186) so we paint invisible interact rectangles at the edges + four
        // corners that send `ViewportCommand::BeginResize(dir)` on drag and
        // hint the right cursor on hover.
        //
        // No persistent Foreground Areas (those swallowed tab/settings clicks
        // window-wide and could leave resize stuck after the first drag). This
        // is a pure per-frame check: hint the resize cursor at an edge and start
        // an OS resize on a press there — only when egui isn't already using the
        // pointer for a widget. Works repeatedly by construction.
        let _ = overlay_open;
        if self.config.appearance.frameless {
            let maximized = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
            if !maximized && !fullscreen {
                handle_frameless_resize(ctx);
            }
        }

        // Apply deferred actions after all UI borrows are released.
        self.apply_deferred_actions(
            ctx,
            &mut act,
            deferred_actions::DeferredFlags {
                run_cmd,
                run_builtin,
                save_cfg,
                open_from_tree,
                close_tree,
                start_lsp,
                want_open_cfg,
                want_restore_cfg,
                want_dismiss_cfg,
            },
        );

        self.persist_session_and_autosave();
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
mod chrome;
mod deferred_actions;
mod find_nav;
mod find_replace;
mod grid_render;
mod keyboard_input;
mod modals;
mod session_io;
mod session_persist;
mod tab_strip_render;
mod tabs;
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
mod execute_builtin_tests;

#[cfg(test)]
mod foreground_area_guard;

#[cfg(test)]
mod jp_glyph_tests;

#[cfg(test)]
mod tab_reorder_tests;

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
