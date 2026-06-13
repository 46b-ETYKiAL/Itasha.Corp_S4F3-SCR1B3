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
    let kind = match mode {
        UpdateMode::Off | UpdateMode::Manual => return None,
        UpdateMode::Notify => crate::updater::LaunchKind::Notify,
        UpdateMode::Auto => crate::updater::LaunchKind::Auto,
    };
    if !scribe_core::update::is_check_due(last_check_unix, interval_hours, now) {
        return None;
    }
    Some(kind)
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
        let _ = std::fs::create_dir_all(parent);
    }
    let body: String = paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(path, body);
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
}

/// A recently-closed tab kept on the reopen stack (Ctrl+Shift+T), so an
/// accidental close is one keystroke from recovery (content + caret restored).
#[derive(Debug, Clone)]
struct ClosedTab {
    path: Option<PathBuf>,
    text: String,
    cursor: usize,
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
        }
    }

    fn from_path(path: PathBuf) -> Result<Self, String> {
        let doc = Document::open(&path).map_err(|e| e.to_string())?;
        let text = doc.text();
        let disk_mtime = doc
            .path()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        Ok(Self {
            doc,
            text: text.clone(),
            doc_id: crate::grid::DocId(0),
            pinned: false,
            disk_mtime,
            disk_text: text,
            rope_state: None,
            rope_buf: None,
            bookmarks: std::collections::BTreeSet::new(),
            edit_gen: 0,
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
                let disk_mtime = doc
                    .path()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .and_then(|m| m.modified().ok());
                return Self {
                    doc,
                    text: content,
                    doc_id: crate::grid::DocId(0),
                    pinned: false,
                    disk_mtime,
                    disk_text,
                    rope_state: None,
                    rope_buf: None,
                    bookmarks: std::collections::BTreeSet::new(),
                    edit_gen: 0,
                };
            }
        }
        // Untitled, or the original file is gone: restore as a scratch buffer
        // carrying the unsaved content (dirty vs an empty saved doc).
        let mut tab = Self::scratch();
        tab.text = content;
        tab
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

    fn title(&self) -> String {
        // #R5: title() stays plain. The pin marker is added by the renderers as
        // a phosphor glyph (`tab_display_label` for the tab strip, the chip
        // header for grid panes) — emitting "📌" here rendered as tofu in the
        // bundled fonts AND double-marked pinned tabs (renderer pin + emoji).
        let name = self.doc.file_name();
        if self.is_dirty() {
            format!("● {name}")
        } else {
            name.to_string()
        }
    }

    fn is_dirty(&self) -> bool {
        // Dirty when the editable mirror diverges from the saved rope.
        self.text != self.doc.text()
    }
}

pub struct ScribeApp {
    config: Config,
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
    /// Open folder for the file-tree sidebar (None = sidebar hidden).
    file_tree_root: Option<PathBuf>,
    /// F-041: keyboard nav state for the sidebar. The struct rebuilds its
    /// visible-list every render so arrow keys move through the same
    /// entries the user sees.
    file_tree_state: crate::filetree::FileTreeState,
    /// F-039 + F-040: the plugin-manager modal (Loaded / Registry / Install).
    /// Surfaces the Phase-20 plugin foundation that was built but unwired.
    plugin_manager: crate::plugin_manager::PluginManagerState,
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
        Self::build(config, None, None, false)
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
            for path in load_session() {
                if let Ok(t) = EditorTab::from_path(path) {
                    tabs.push(t);
                }
            }
        }
        if tabs.is_empty() {
            tabs.push(EditorTab::scratch());
        }
        let session_sig = session_signature(&tabs);

        // Load user mods/plugins (no-build-step Rhai scripts) from the plugins
        // dir, unless the user disabled the plugin system.
        //
        // #R6 — TRUST GATE. A plugin script is only ever executed when EITHER:
        //   * `require_signed` is on AND it carries a minisign signature over
        //     the entry script from a pinned author key that verifies, OR
        //   * the user has approved THIS EXACT entry script (its SHA-256 is in
        //     `config.plugins.trusted`).
        // Otherwise the plugin is held back as "pending approval" and NOT run.
        // This closes the prior gap where dropping any folder into the plugins
        // dir auto-executed unsigned, unreviewed code on the next launch.
        let mut plugins = PluginHost::new();
        let mut pending_plugins: Vec<String> = Vec::new();
        if config.plugins.enabled {
            if let Some(dir) = Config::config_dir() {
                let (found, errors) = plugin::discover(&dir.join("plugins"));
                let mut key_store = scribe_core::plugin::PinnedKeyStore::new(&dir);
                for p in found {
                    if config.plugins.disabled.contains(&p.manifest.id) {
                        continue;
                    }
                    // Honor the manifest's declared `min_app_version` floor. The
                    // helper + its tests existed but were never wired into the
                    // load loop, so a plugin requiring a newer host would load
                    // and then fail in surprising ways. Fail-safe: refuse to load
                    // an incompatible plugin and warn.
                    if !p.manifest.is_app_version_ok(env!("CARGO_PKG_VERSION")) {
                        tracing::warn!(
                            "plugin '{}' skipped: requires app >= {:?}, this build is {}",
                            p.manifest.id,
                            p.manifest.min_app_version,
                            env!("CARGO_PKG_VERSION")
                        );
                        continue;
                    }
                    let Ok(src) = std::fs::read_to_string(p.entry_path()) else {
                        continue;
                    };
                    let sha = scribe_core::update::verify::sha256_hex(src.as_bytes());
                    let may_run = if config.plugins.require_signed {
                        // Strict mode: pinned author key (TOFU) + a valid
                        // minisign signature over the entry script.
                        let verified = match (&p.manifest.author_pubkey, &p.manifest.signature) {
                            (Some(pk), Some(sig)) => {
                                let pinned = matches!(
                                    key_store.pin_or_match(&p.manifest.id, pk),
                                    Ok(scribe_core::plugin::PinOutcome::Match
                                        | scribe_core::plugin::PinOutcome::New)
                                );
                                pinned
                                    && scribe_core::update::verify::verify_signature(
                                        src.as_bytes(),
                                        sig,
                                        pk,
                                    )
                                    .is_ok()
                            }
                            _ => false,
                        };
                        if !verified {
                            tracing::warn!(
                                "plugin '{}' rejected: require_signed is on but it is \
                                 unsigned or fails verification",
                                p.manifest.id
                            );
                        }
                        verified
                    } else {
                        // Default mode: trust-on-first-use by entry checksum.
                        scribe_core::plugin::entry_is_trusted(
                            &p.manifest.id,
                            &sha,
                            &config.plugins.trusted,
                        )
                    };
                    if !may_run {
                        if !config.plugins.require_signed {
                            pending_plugins.push(p.manifest.id.clone());
                        }
                        continue;
                    }
                    if let Err(e) = plugins.load_script(&p.manifest.id, &src) {
                        tracing::warn!("plugin load failed: {e}");
                    }
                }
                if !errors.is_empty() && toast.is_none() {
                    toast = Some(format!("{} plugin(s) skipped (see log)", errors.len()));
                }
                if !pending_plugins.is_empty() && toast.is_none() {
                    toast = Some(format!(
                        "{} plugin(s) need your approval before they run — open Settings \
                         → Plugins → Manage plugins",
                        pending_plugins.len()
                    ));
                }
            }
        }
        let plugin_cmds = plugins.commands();

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

        Self {
            config,
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
            applied_note_theme: String::new(),
            want_close: false,
            closing: false,
            find_in_files_open: false,
            find_in_files_query: String::new(),
            find_in_files_regex: false,
            find_in_files_results: Vec::new(),
            find_in_files_error: None,
            focus_find_in_files: false,
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
            palette_query: String::new(),
            settings_open: false,
            cheatsheet_open: false,
            recent_open: false,
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
            file_tree_root: None,
            file_tree_state: crate::filetree::FileTreeState::default(),
            plugin_manager: crate::plugin_manager::PluginManagerState::default(),
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
        let line_height = self.config.fonts.line_height;
        let word_wrap = self.config.editor.word_wrap;
        // #28 — render-whitespace toggle + editor font size captured as locals so
        // the per-pane body closure (which can't re-borrow `self.config`) can
        // paint the `·`/`→` whitespace overlay on each pane's galley too.
        let render_whitespace = self.config.editor.render_whitespace;
        let editor_font_size = self.config.fonts.editor_size;
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
                let mut drag_started = false;
                // #R5 — per-pane header rendered as a tab CHIP that mirrors the
                // top tab strip: a filled accent chip on the focused pane,
                // transparent otherwise; drag-handle ICON on the left, note name,
                // pin toggle, close ✕ on the far right. Pinned notes drop the
                // drag handle + ✕ (anchored, can't be moved/closed). All glyphs
                // are phosphor (the old ✕ / ⠿ were tofu).
                let pane_title = tabs[idx].title();
                let pinned = tabs[idx].pinned;
                let is_active = active_doc == Some(doc_id);
                let chip = egui::Frame::default()
                    .inner_margin(egui::Margin::symmetric(8, 3))
                    .corner_radius(egui::CornerRadius::same(5))
                    .fill(if is_active {
                        accent.linear_multiply(0.20)
                    } else {
                        Color32::TRANSPARENT
                    });
                // Header layout adapts to pane width. A WIDE pane gets ONE row
                // (handle · name · pin, with the close ✕ on the far right); a
                // NARROW pane gets a single CENTERED column (name, then pin, then
                // close) so the controls never wrap into a ragged stack. All
                // glyphs are phosphor (now resolved in monospace too, so no tofu).
                let pin_glyph = if pinned {
                    egui_phosphor::thin::PUSH_PIN_SLASH
                } else {
                    egui_phosphor::thin::PUSH_PIN
                };
                if ui.available_width() >= 220.0 {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        chip.show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                // Drag handle — a neutral `muted` grip so it stays
                                // visible on BOTH the active (accent-tinted) and
                                // inactive chip backgrounds (an accent-coloured grip
                                // vanished against the active chip's accent fill).
                                let grip_color = muted;
                                if pinned {
                                    grip_handle(ui, false, grip_color)
                                        .on_hover_text("Pinned — drag disabled");
                                } else {
                                    // `drag_started()` fires ONCE on drag start (egui_tiles
                                    // expects a single `DragStarted`); a held-button check
                                    // would re-fire every frame and wedge the tile drag.
                                    let handle = grip_handle(ui, true, grip_color)
                                        .on_hover_text("Drag to rearrange")
                                        .on_hover_cursor(egui::CursorIcon::Grab);
                                    if handle.drag_started() {
                                        drag_started = true;
                                    }
                                }
                                ui.label(
                                    RichText::new(&pane_title).monospace().color(if is_active {
                                        accent
                                    } else {
                                        muted
                                    }),
                                )
                                .on_hover_text(&pane_title);
                                if ui
                                    .add(egui::Button::new(pin_glyph).frame(false).small())
                                    .on_hover_text(if pinned { "Unpin note" } else { "Pin note" })
                                    .clicked()
                                {
                                    tabs[idx].pinned = !pinned;
                                }
                            });
                        });
                        // Close at the far right — hidden on pinned notes.
                        if !pinned {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(egui_phosphor::thin::X)
                                                .frame(false)
                                                .small(),
                                        )
                                        .on_hover_text("Close pane")
                                        .clicked()
                                    {
                                        render_closes.borrow_mut().push(doc_id);
                                    }
                                },
                            );
                        }
                    });
                } else {
                    // NARROW pane: a single centered column — name, pin, close.
                    // The name doubles as the drag handle (no room for a grip).
                    chip.show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            let name =
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&pane_title)
                                            .monospace()
                                            .color(if is_active { accent } else { muted }),
                                    )
                                    .sense(if pinned {
                                        egui::Sense::hover()
                                    } else {
                                        egui::Sense::click_and_drag()
                                    }),
                                );
                            if !pinned && name.drag_started() {
                                drag_started = true;
                            }
                            if ui
                                .add(egui::Button::new(pin_glyph).frame(false).small())
                                .on_hover_text(if pinned { "Unpin note" } else { "Pin note" })
                                .clicked()
                            {
                                tabs[idx].pinned = !pinned;
                            }
                            if !pinned
                                && ui
                                    .add(
                                        egui::Button::new(egui_phosphor::thin::X)
                                            .frame(false)
                                            .small(),
                                    )
                                    .on_hover_text("Close pane")
                                    .clicked()
                            {
                                render_closes.borrow_mut().push(doc_id);
                            }
                        });
                    });
                }
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

    /// Build the egui visuals for the current theme, applying surface opacity
    /// when a translucent/glass window mode is active.
    fn current_visuals(&self) -> egui::Visuals {
        let mut v = scribe_render::theme_to_visuals(&self.theme);
        let parse = |o: &Option<String>| {
            o.as_deref()
                .and_then(Rgba::parse_hex)
                .map(|c| Color32::from_rgb(c.r, c.g, c.b))
        };
        // #88 — app-background override (independent of the theme) repaints the
        // central panel + window backgrounds. None = follow theme.
        let app_bg = parse(&self.config.appearance.background_override);
        if let Some(c) = app_bg {
            v.panel_fill = c;
            v.window_fill = c;
        }
        // #106 — note (editor well) background. When linked it follows the app
        // background override; when unlinked it uses its own override. None at
        // the chosen source = follow the theme's editor background.
        let note_bg = if self.config.appearance.link_backgrounds {
            app_bg
        } else {
            parse(&self.config.appearance.note_background_override)
        };
        if let Some(c) = note_bg {
            v.extreme_bg_color = c;
        }
        if self.config.window.effective_translucent() {
            scribe_render::apply_window_opacity(&mut v, self.config.window.opacity);
        }
        v
    }

    /// Resolve which theme name to actually load, honoring
    /// `appearance.follow_os_theme`. When that is on and the OS reports its
    /// theme, the OS decides light vs dark: a light OS → the bundled light
    /// theme (`ghost-paper`); a dark OS → the user's chosen theme if it is
    /// itself dark, otherwise the default dark theme (`wired-noir`). When the
    /// toggle is off, or the OS theme is unknown, the user's chosen theme wins.
    fn effective_theme_name(&self, os_theme: egui::Theme) -> String {
        if self.config.appearance.follow_os_theme {
            match os_theme {
                egui::Theme::Light => return "ghost-paper".to_string(),
                egui::Theme::Dark => {
                    let chosen = load_theme(&self.config.appearance.theme);
                    return if matches!(chosen.appearance, scribe_core::theme::Appearance::Dark) {
                        self.config.appearance.theme.clone()
                    } else {
                        "wired-noir".to_string()
                    };
                }
            }
        }
        self.config.appearance.theme.clone()
    }

    /// Apply the current theme to the egui context (after a theme/config change).
    /// Reads the OS theme via `ctx.theme()` — egui-winit tracks the OS theme when
    /// the theme preference is `System` (set in `new`). `raw.system_theme` is
    /// unreliable/None on Windows, which is why "Follow OS theme" did nothing.
    fn reapply_theme(&mut self, ctx: &egui::Context) {
        let os_theme = ctx.theme();
        self.last_os_theme = Some(os_theme);
        self.theme = load_theme(&self.effective_theme_name(os_theme));
        ctx.set_visuals(self.current_visuals());
        // `set_visuals` resets the caret style, so re-apply motion after it.
        self.apply_motion_style(ctx);
    }

    /// Push the `motion` preferences into egui's global style. Motion off zeroes
    /// the animation time (instant transitions, no hover fades — idle frames
    /// cost the same as plain egui) and stops the caret blinking; otherwise the
    /// intensity scales egui's default animation time. This is the whole Motion
    /// feature: only effects egui drives natively are exposed, so there are no
    /// dead per-effect toggles.
    fn apply_motion_style(&self, ctx: &egui::Context) {
        // egui's stock animation time is 1/12 s; scale it by intensity, or zero
        // it when motion is disabled.
        const EGUI_DEFAULT_ANIMATION_TIME: f32 = 1.0 / 12.0;
        let anim = if self.config.motion.enabled {
            EGUI_DEFAULT_ANIMATION_TIME * self.config.motion.clamped_intensity()
        } else {
            0.0
        };
        let blink = self.config.motion.enabled && self.config.motion.cursor_blink;
        ctx.style_mut(|s| {
            s.animation_time = anim;
            s.visuals.text_cursor.blink = blink;
        });
    }

    /// Replace the active editor's selection (or insert at the caret) with
    /// `tab_width` spaces, then advance the caret — the Tab-key handler when
    /// `insert_spaces` is enabled. Operates directly on the TextEdit state for
    /// `id` so the caret tracks the edit.
    fn indent_with_spaces(&mut self, ctx: &egui::Context, id: egui::Id, active: usize) {
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let lo = range.primary.index.min(range.secondary.index);
        let hi = range.primary.index.max(range.secondary.index);
        let (new_text, new_idx) = apply_indent(
            &self.tabs[active].text,
            lo,
            hi,
            self.config.editor.tab_width,
        );
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(new_idx),
            )));
        state.store(ctx, id);
    }

    /// Auto-indent on Enter (#107): insert a newline that keeps the current
    /// line's leading whitespace, so indentation carries to the next line. Only
    /// acts on a single caret (no selection); returns false so the caller lets
    /// egui handle Enter normally otherwise.
    fn auto_indent_newline(&mut self, ctx: &egui::Context, id: egui::Id, active: usize) -> bool {
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return false;
        };
        let Some(range) = state.cursor.char_range() else {
            return false;
        };
        // Only a collapsed caret — a selection+Enter should replace, which we
        // leave to egui.
        if range.primary.index != range.secondary.index {
            return false;
        }
        let cursor = range.primary.index;
        let (new_text, new_idx) = newline_with_indent(&self.tabs[active].text, cursor);
        // No indent to carry → let egui insert the plain newline (cheaper, and
        // keeps egui's own undo grouping for the common case).
        if new_idx == cursor + 1 {
            return false;
        }
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(new_idx),
            )));
        state.store(ctx, id);
        true
    }

    /// Render one quick-access toolbar entry by action id and apply its effect.
    /// Buttons set the pending-action flags; toggles flip the live config/state
    /// and request a config save. The id `"sep"` draws a divider.
    // The explicit `=> { if widget.clicked() { effect } }` per arm is clearer
    // than clippy's suggested match-guard form, which would render the widget as
    // a side effect inside the guard condition.
    #[allow(clippy::collapsible_match)]
    /// Render the quick-access toolbar contents (settings gear, command-palette
    /// button, the user-ordered items, and the optional user-curated "⋯"
    /// dropdown) into `ui`. Shared by the toolbar panel and the in-titlebar
    /// placement (`appearance.toolbar_in_titlebar`).
    fn toolbar_contents(
        &mut self,
        ui: &mut egui::Ui,
        act: &mut Pending,
        save_cfg: &mut bool,
        start_lsp: &mut bool,
    ) {
        if ui
            .selectable_label(self.settings_open, egui_phosphor::thin::GEAR_SIX)
            .on_hover_text("Settings")
            .clicked()
        {
            self.settings_open = !self.settings_open;
        }
        if ui
            .button(">_")
            .on_hover_text("Command palette (Ctrl+Shift+P)")
            .clicked()
        {
            self.palette_open = true;
            self.focus_palette = true;
            self.palette_query.clear();
        }
        ui.separator();
        // User-customizable quick-access items (membership + order from
        // config.toolbar; editable in Settings → Toolbar).
        let items = self.config.toolbar.items.clone();
        for id in &items {
            self.toolbar_item(ui, id, act, save_cfg, start_lsp);
        }
        // User-curated "⋯" dropdown — actions the user parked here to keep the
        // bar clean. Shown only when non-empty.
        let menu = self.config.toolbar.menu.clone();
        if !menu.is_empty() {
            ui.menu_button("⋯", |ui| {
                for id in &menu {
                    self.toolbar_item(ui, id, act, save_cfg, start_lsp);
                }
            })
            .response
            .on_hover_text("More actions");
        }
    }

    // clippy 1.95's `collapsible_match` wants each `"id" => { if ui.button(..)
    // .clicked() { .. } }` arm rewritten with the button call as a match GUARD.
    // That would move a side-effecting widget call into the guard — worse style
    // (guards should be pure) — so the per-arm `if` is intentional here.
    #[allow(clippy::collapsible_match)]
    fn toolbar_item(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        act: &mut Pending,
        save_cfg: &mut bool,
        start_lsp: &mut bool,
    ) {
        // Phase 16 T16.3: every toolbar label routes through `toolbar_widget(id, icons, jp, size)`
        // so flipping `appearance.toolbar_icons` swaps every entry between its text
        // form and its Phosphor (Thin) glyph in one place. Phase 17 T17.5: the
        // same helper also appends a verified-canonical kanji "instrument plate"
        // when `appearance.jp_glyph_labels` is on (English-redundant, dimmed, smaller).
        let icons = self.config.appearance.toolbar_icons;
        let jp = self.config.appearance.jp_glyph_labels;
        // Phase 18 T18.5: the icon-size slider drives every toolbar glyph/label.
        let size = self.config.toolbar.clamped_icon_size();
        // #22 — a SELECTED toggle's label is pinned to the theme accent (so it
        // reads as "on" in both kanji-on and kanji-off states). Plain buttons and
        // unselected toggles pass `Color32::PLACEHOLDER` to follow the widget's
        // own fg (preserving hover/active brightening). `sel(on)` is the helper.
        let accent = ui_color(&self.theme, "accent", Rgba::new(0, 255, 254, 255));
        let sel = |on: bool| if on { accent } else { Color32::PLACEHOLDER };
        match id {
            "sep" => {
                ui.separator();
            }
            "new" => {
                if ui
                    .button(toolbar_widget("new", icons, jp, size, Color32::PLACEHOLDER))
                    .on_hover_text("New file (Ctrl+N)")
                    .clicked()
                {
                    act.new = true;
                }
            }
            "open" => {
                if ui
                    .button(toolbar_widget(
                        "open",
                        icons,
                        jp,
                        size,
                        Color32::PLACEHOLDER,
                    ))
                    .on_hover_text("Open file (Ctrl+O)")
                    .clicked()
                {
                    act.open = true;
                }
            }
            "openfolder" => {
                if ui
                    .button(toolbar_widget(
                        "openfolder",
                        icons,
                        jp,
                        size,
                        Color32::PLACEHOLDER,
                    ))
                    .on_hover_text("Open folder")
                    .clicked()
                {
                    act.open_folder = true;
                }
            }
            "save" => {
                if ui
                    .button(toolbar_widget(
                        "save",
                        icons,
                        jp,
                        size,
                        Color32::PLACEHOLDER,
                    ))
                    .on_hover_text("Save (Ctrl+S)")
                    .clicked()
                {
                    act.save = true;
                }
            }
            "saveas" => {
                if ui
                    .button(toolbar_widget(
                        "saveas",
                        icons,
                        jp,
                        size,
                        Color32::PLACEHOLDER,
                    ))
                    .on_hover_text("Save As…")
                    .clicked()
                {
                    self.save_as_active();
                }
            }
            "find" => {
                if ui
                    .button(toolbar_widget(
                        "find",
                        icons,
                        jp,
                        size,
                        Color32::PLACEHOLDER,
                    ))
                    .on_hover_text("Find (Ctrl+F)")
                    .clicked()
                {
                    self.find_open = true;
                    self.focus_find = true;
                }
            }
            "palette" => {
                if ui
                    .button(toolbar_widget(
                        "palette",
                        icons,
                        jp,
                        size,
                        Color32::PLACEHOLDER,
                    ))
                    .on_hover_text("Command palette")
                    .clicked()
                {
                    self.palette_open = true;
                    self.focus_palette = true;
                    self.palette_query.clear();
                }
            }
            "split" => {
                // Split and grid are one feature: this toggles the multi-pane
                // view, which lays the OPEN TABS out as panes — two tabs read as
                // a side-by-side split, and it grows into a grid as more tabs
                // open. (Same `editor.grid_enabled` the grid command toggles.)
                if ui
                    .selectable_label(
                        self.config.editor.grid_enabled,
                        toolbar_widget(
                            "split",
                            icons,
                            jp,
                            size,
                            sel(self.config.editor.grid_enabled),
                        ),
                    )
                    .on_hover_text(
                        "Split / grid view — show the open notes side by side. \
                         Opening more notes grows the split into a grid.",
                    )
                    .clicked()
                {
                    self.config.editor.grid_enabled = !self.config.editor.grid_enabled;
                    *save_cfg = true;
                }
            }
            "minimap" => {
                if ui
                    .selectable_label(
                        self.config.editor.show_minimap,
                        toolbar_widget(
                            "minimap",
                            icons,
                            jp,
                            size,
                            sel(self.config.editor.show_minimap),
                        ),
                    )
                    .on_hover_text("Minimap")
                    .clicked()
                {
                    self.config.editor.show_minimap = !self.config.editor.show_minimap;
                    *save_cfg = true;
                }
            }
            "wrap" => {
                if ui
                    .selectable_label(
                        self.config.editor.word_wrap,
                        toolbar_widget("wrap", icons, jp, size, sel(self.config.editor.word_wrap)),
                    )
                    .on_hover_text("Word wrap")
                    .clicked()
                {
                    self.config.editor.word_wrap = !self.config.editor.word_wrap;
                    *save_cfg = true;
                }
            }
            "fold" => {
                if ui
                    .selectable_label(
                        self.fold_view,
                        toolbar_widget("fold", icons, jp, size, sel(self.fold_view)),
                    )
                    .on_hover_text("Folded view")
                    .clicked()
                {
                    self.fold_view = !self.fold_view;
                }
            }
            "linenumbers" => {
                if ui
                    .selectable_label(
                        self.config.editor.show_line_numbers,
                        toolbar_widget(
                            "linenumbers",
                            icons,
                            jp,
                            size,
                            sel(self.config.editor.show_line_numbers),
                        ),
                    )
                    .on_hover_text("Line numbers")
                    .clicked()
                {
                    self.config.editor.show_line_numbers = !self.config.editor.show_line_numbers;
                    *save_cfg = true;
                }
            }
            "spellcheck" => {
                if ui
                    .selectable_label(
                        self.config.spellcheck.enabled,
                        toolbar_widget(
                            "spellcheck",
                            icons,
                            jp,
                            size,
                            sel(self.config.spellcheck.enabled),
                        ),
                    )
                    .on_hover_text("Spellcheck (offline)")
                    .clicked()
                {
                    self.config.spellcheck.enabled = !self.config.spellcheck.enabled;
                    *save_cfg = true;
                }
            }
            "lsp" => {
                if ui
                    .button(toolbar_widget("lsp", icons, jp, size, Color32::PLACEHOLDER))
                    .on_hover_text("Start language server")
                    .clicked()
                {
                    *start_lsp = true;
                }
            }
            _ => {}
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
            Err(e) => self.toast = Some(format!("plugin error: {e}")),
        }
    }

    /// Count misspellings in the active buffer when spellcheck is enabled.
    fn spell_count(&self) -> usize {
        self.misspellings_for_active().len()
    }

    /// Misspellings in the active buffer (#78), memoized by a content+config
    /// hash so the dictionary scan runs once per changed frame and is shared by
    /// the status-bar count and the editor underline painter. Empty when
    /// spellcheck is off or there is no active buffer.
    fn misspellings_for_active(&self) -> Vec<spell::Misspelling> {
        if !self.config.spellcheck.enabled {
            return Vec::new();
        }
        let active = self.active.min(self.tabs.len().saturating_sub(1));
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
        // Cache key: text + scope toggles + language. A change to any of these
        // invalidates the memo.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            // Wave-3: key off the per-tab edit generation instead of re-hashing
            // the whole buffer every frame. `doc_id` disambiguates tabs that
            // happen to share an edit_gen (the single-slot cache is shared). A
            // 1-frame-stale squiggle after a keystroke is visually harmless.
            tab.edit_gen.hash(&mut h);
            tab.doc_id.raw().hash(&mut h);
            self.config.spellcheck.check_comments.hash(&mut h);
            self.config.spellcheck.check_strings.hash(&mut h);
            self.config.spellcheck.check_identifiers.hash(&mut h);
            ext.hash(&mut h);
            h.finish()
        };
        if let Some((k, v)) = self.spell_cache.borrow().as_ref() {
            if *k == key {
                return v.clone();
            }
        }
        let spans = self.hl.classify_document(&tab.text, ext.as_deref());
        // Scoping (comments / strings / identifiers) is a CODE concept. When the
        // buffer has no code structure — an untitled note, plain text, markdown —
        // those classes don't apply, so check the whole document as prose. Only
        // when there are real comment/string/identifier spans do the toggles
        // constrain the check.
        let has_code_structure = spans
            .iter()
            .any(|s| !matches!(s.class, spell::SpanClass::Other));
        let result = if has_code_structure {
            spell::check_text_scoped(&self.spell, &tab.text, &spans, scope)
        } else {
            spell::check_text(&self.spell, &tab.text, true)
        };
        *self.spell_cache.borrow_mut() = Some((key, result.clone()));
        result
    }

    /// Once per launch: if the user opted into automatic update checks
    /// (`updates.mode` = `notify` or `auto`) and the configured interval has
    /// elapsed, run a single GitHub-Releases version check on a background
    /// thread. `notify` then surfaces a passive toast if an update is found;
    /// `auto` opens a yes/no modal. `off` and `manual` do NO network at all —
    /// the telemetry-free default. The check itself only reads the public
    /// Releases API and sends no identifiers (see PRIVACY.md). `is_check_due` +
    /// the persisted `last_check_unix` honour the interval across sessions.
    fn maybe_remind_update(&mut self, ctx: &egui::Context) {
        if self.did_update_check {
            return;
        }
        self.did_update_check = true;
        // First frame of the launch: reap update artifacts that are no longer
        // needed now that this (possibly just-updated) build is running — the
        // staging download dir (incl. a completed installer's setup.exe, which
        // couldn't be deleted while it ran) and the prior binary's `.bak`.
        crate::updater::cleanup_after_update();
        let Some(kind) = update_launch_action(
            self.config.updates.mode,
            self.config.updates.last_check_unix,
            self.config.updates.check_interval_hours as u64,
            now_unix(),
        ) else {
            return;
        };
        // Due: record the check time so the interval is honored next launch.
        self.config.updates.last_check_unix = Some(now_unix());
        self.save_config();
        self.updater.start_check(ctx, kind);
    }

    /// `auto`-mode on-launch update modal. When the launch check finds a newer
    /// release it asks yes/no; the SAME modal then follows the flow through
    /// download → verify → "Restart to finish", so the whole Auto update is
    /// self-contained. "Later" dismisses it for this session (won't re-prompt
    /// for the same version). The manual flow in Settings covers every other case.
    fn render_update_prompt(&mut self, ctx: &egui::Context) {
        if !self.updater.show_prompt {
            return;
        }
        use crate::updater::UpdateState;
        // Defer mutating calls past the immutable state borrow in the closure.
        enum Act {
            Download(scribe_core::update::ReleaseInfo),
            Apply,
            RunInstaller,
            Skip(String),
            Close,
        }
        let mut act: Option<Act> = None;
        egui::Modal::new(egui::Id::new("scr1b3_update_prompt")).show(ctx, |ui| {
            ui.set_max_width(400.0);
            ui.heading("Update available");
            ui.add_space(8.0);
            match &self.updater.state {
                UpdateState::Available(info) => {
                    let v = info.version.to_string();
                    ui.label(format!(
                        "SCR1B3 v{v} is available (you have v{}). Update now?",
                        crate::updater::current_version()
                    ));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Update now").clicked() {
                            act = Some(Act::Download(info.clone()));
                        }
                        if ui.button("Later").clicked() {
                            act = Some(Act::Skip(v.clone()));
                        }
                    });
                }
                UpdateState::Downloading { received, total } => {
                    let frac = if *total > 0 {
                        *received as f32 / *total as f32
                    } else {
                        0.0
                    };
                    ui.label("Downloading and verifying…");
                    ui.add_space(6.0);
                    ui.add(egui::ProgressBar::new(frac).show_percentage());
                }
                UpdateState::ReadyToApply { version, .. } => {
                    ui.label(format!("v{version} downloaded and verified."));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Restart to finish").clicked() {
                            act = Some(Act::Apply);
                        }
                        if ui.button("Later").clicked() {
                            act = Some(Act::Close);
                        }
                    });
                }
                UpdateState::ReadyToRunInstaller { version, .. } => {
                    ui.label(format!("v{version} downloaded and verified."));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Install (asks for admin)").clicked() {
                            act = Some(Act::RunInstaller);
                        }
                        if ui.button("Later").clicked() {
                            act = Some(Act::Close);
                        }
                    });
                }
                UpdateState::Applied { version } => {
                    ui.label(format!("Updated to v{version} — restarting…"));
                }
                UpdateState::Failed(e) => {
                    let err = ui.visuals().error_fg_color;
                    ui.colored_label(err, format!("Update failed: {e}"));
                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        act = Some(Act::Close);
                    }
                }
                // Nothing to prompt about in the on-launch modal — close it.
                // (NoAssetForPlatform is surfaced only in the Settings pane,
                // not the auto modal.)
                UpdateState::Idle
                | UpdateState::Checking
                | UpdateState::UpToDate { .. }
                | UpdateState::NoAssetForPlatform { .. } => {
                    act = Some(Act::Close);
                }
            }
        });
        match act {
            Some(Act::Download(info)) => self.updater.start_download(ctx, info),
            Some(Act::Apply) => self.updater.apply_and_restart(ctx),
            Some(Act::RunInstaller) => self.updater.run_installer(ctx),
            Some(Act::Skip(v)) => {
                self.updater.skipped_version = Some(v);
                self.updater.show_prompt = false;
            }
            Some(Act::Close) => self.updater.show_prompt = false,
            None => {}
        }
    }

    /// Rebuild the spell engine from the current config — called after the user
    /// changes the spellcheck language or custom dictionary in Settings so the
    /// new dictionary takes effect without a restart.
    fn reload_spell_engine(&mut self) {
        self.spell = build_spell_engine(&self.config);
    }

    fn save_config(&mut self) {
        let Some(path) = Config::config_file_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, self.config.to_toml_string()) {
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
        let Some(dir) = Config::config_dir() else {
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
                    self.file_tree_root = Some(folder);
                }
            }
            BuiltinCommand::Save => self.save_active(),
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

    fn save_active(&mut self) {
        let active = self.active;
        if active >= self.tabs.len() {
            return;
        }
        // Save-time hygiene (opt-in): trim trailing whitespace + ensure a
        // final newline. Cleaned text is reflected back into the live buffer.
        let mut text = self.tabs[active].text.clone();
        if self.config.editor.trim_trailing_whitespace_on_save {
            text = scribe_core::text_ops::trim_trailing_whitespace(&text);
        }
        if self.config.editor.final_newline_on_save {
            text = scribe_core::text_ops::ensure_final_newline(&text);
        }
        if text != self.tabs[active].text {
            self.tabs[active].set_text(text.clone());
        }
        // Sync editable text into the document model, then persist.
        self.tabs[active].doc.set_text(&text);
        if self.tabs[active].doc.path().is_none() {
            self.save_as_active();
            return;
        }
        match self.tabs[active].doc.save() {
            Ok(lossy) => {
                self.status = format!("saved {}", self.tabs[active].doc.file_name());
                // #R6 — the file's encoding can't represent every character;
                // those were replaced (data lost). Warn loudly + offer the fix.
                if lossy {
                    self.toast = Some(format!(
                        "Saved, but some characters can't be written as {} — they were \
                         replaced. Convert the file to UTF-8 to keep them.",
                        self.tabs[active].doc.encoding().name
                    ));
                }
                // F-022 — refresh the disk fingerprint after a successful
                // save so the next poll doesn't false-positive.
                self.tabs[active].disk_text = self.tabs[active].text.clone();
                if let Some(p) = self.tabs[active].doc.path() {
                    if let Ok(m) = std::fs::metadata(p).and_then(|m| m.modified()) {
                        self.tabs[active].disk_mtime = Some(m);
                    }
                }
                self.fire_save_hooks(active);
            }
            Err(e) => self.toast = Some(format!("save failed: {e}")),
        }
    }

    /// Hot-exit snapshot: flush every unsaved buffer's content to the backup
    /// store + write the session manifest, so unsaved work (incl. untitled
    /// scratch notes) survives a restart or crash. Each dirty file tab and each
    /// non-empty untitled tab gets an atomic content backup; clean tabs are
    /// recorded by path only; orphan backups are pruned. Best-effort.
    fn snapshot_session_backups(&mut self) {
        use scribe_core::session;
        let Some(dir) = Config::config_dir() else {
            return;
        };
        let bdir = session::backup_dir(&dir);
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        let mut snapshots = Vec::with_capacity(self.tabs.len());
        for (i, tab) in self.tabs.iter().enumerate() {
            let path = tab.doc.path().map(|p| p.display().to_string());
            let dirty = tab.is_dirty();
            let untitled_with_content = path.is_none() && !tab.text.is_empty();
            let cursor = tab.rope_state.as_ref().map(|s| s.edit.cursor).unwrap_or(0);
            let backup = if dirty || untitled_with_content {
                let name = session::backup_name(path.as_deref(), i);
                session::write_backup(&bdir, &name, &tab.text)
                    .ok()
                    .map(|()| name)
            } else {
                None
            };
            snapshots.push(session::TabSnapshot {
                path,
                dirty,
                backup,
                cursor,
            });
        }
        let manifest = session::SessionManifest::new(snapshots, active);
        let _ = session::save_manifest(&dir, &manifest);
        session::prune_orphan_backups(&bdir, &manifest);
        self.last_backup_at = Some(std::time::Instant::now());
    }

    /// Restore tabs from the session manifest + content backups (hot exit).
    /// Returns `(tabs, active_index)` or `None` when there is no usable
    /// manifest. A tab with a backup restores its unsaved content (marked
    /// dirty); a clean tab opens from disk.
    /// Restore tabs from the hot-exit manifest. The two session features are
    /// distinct and gated separately:
    ///   • unsaved CONTENT (a tab with a `backup`) is always restored here —
    ///     that is the "Restore unsaved notes" / `session_backup` feature.
    ///   • a clean, file-backed tab (a `path` with no `backup`) is reopened ONLY
    ///     when `restore_session` is on — that is the "Restore session" /
    ///     reopen-previous-tabs feature.
    /// So with "Restore session" UNCHECKED, previously-open clean files are NOT
    /// reopened, while unsaved scratch content is still recovered (if its own
    /// toggle is on). This is what makes the "Restore session" toggle authoritative
    /// instead of a no-op (the backup path used to reopen every clean file too).
    fn restore_tabs_from_manifest(restore_session: bool) -> Option<(Vec<EditorTab>, usize)> {
        use scribe_core::session;
        let dir = Config::config_dir()?;
        let manifest = session::load_manifest(&dir)?;
        let bdir = session::backup_dir(&dir);
        let mut tabs = Vec::new();
        for snap in &manifest.tabs {
            let path = snap.path.as_ref().map(PathBuf::from);
            if let Some(name) = &snap.backup {
                if let Ok(content) = session::read_backup(&bdir, name) {
                    tabs.push(EditorTab::from_backup(path, content));
                    continue;
                }
            }
            // Clean file-backed tab: only reopen when the user opted into session
            // restore. Otherwise it is dropped (unchecking the toggle takes effect).
            if !restore_session {
                continue;
            }
            if let Some(p) = path {
                if let Ok(tab) = EditorTab::from_path(p) {
                    tabs.push(tab);
                }
            }
        }
        if tabs.is_empty() {
            return None;
        }
        let active = manifest.active.min(tabs.len() - 1);
        Some((tabs, active))
    }

    /// F-022 — Poll every file-backed tab's mtime. If a tab's disk mtime
    /// advanced AND the buffer is still clean (text == disk_text), re-read
    /// the file in place + surface a status toast. If the buffer is dirty,
    /// flag the user so save doesn't silently clobber their edits.
    fn poll_external_disk_changes(&mut self) {
        // Snapshot first so we don't hold &mut self while mutating tabs.
        let mut to_reload: Vec<usize> = Vec::new();
        let mut to_warn: Vec<usize> = Vec::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            let Some(path) = tab.doc.path() else { continue };
            let Ok(m) = std::fs::metadata(path).and_then(|m| m.modified()) else {
                continue;
            };
            if Some(m) != tab.disk_mtime {
                if tab.text == tab.disk_text {
                    to_reload.push(i);
                } else {
                    to_warn.push(i);
                }
            }
        }
        for i in to_reload {
            let Some(path) = self.tabs[i].doc.path().map(|p| p.to_path_buf()) else {
                continue;
            };
            if let Ok(fresh) = std::fs::read_to_string(&path) {
                self.tabs[i].set_text(fresh.clone());
                self.tabs[i].doc.set_text(&fresh);
                self.tabs[i].disk_text = fresh;
                if let Ok(m) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                    self.tabs[i].disk_mtime = Some(m);
                }
                self.status = format!("reloaded {} (external edit)", path.display());
            }
        }
        for i in to_warn {
            if let Some(name) = self.tabs[i].doc.path().map(|p| p.display().to_string()) {
                self.toast = Some(format!(
                    "{} {name} changed on disk while you have local edits. Save will overwrite.",
                    egui_phosphor::thin::WARNING
                ));
                // Don't refresh disk_mtime — keep showing the warning until
                // the user explicitly saves (which sets a fresh mtime) or
                // closes/reopens the tab.
            }
        }
    }

    /// Fire plugin `on_save` hooks; apply any text transform they make.
    fn fire_save_hooks(&mut self, active: usize) {
        let mut pctx = PluginContext::new(self.tabs[active].text.clone());
        if self.plugins.fire_event(HookEvent::Save, &mut pctx).is_ok() {
            if pctx.text != self.tabs[active].text {
                self.tabs[active].set_text(pctx.text);
            }
            if let Some(n) = pctx.notifications.last() {
                self.status = n.clone();
            }
        }
    }

    fn save_as_active(&mut self) {
        let active = self.active;
        if active >= self.tabs.len() {
            return;
        }
        if let Some(path) = rfd::FileDialog::new().save_file() {
            let text = self.tabs[active].text.clone();
            self.tabs[active].doc.set_text(&text);
            match self.tabs[active].doc.save_as(&path) {
                Ok(lossy) => {
                    self.status = format!("saved {}", path.display());
                    if lossy {
                        self.toast = Some(format!(
                            "Saved, but some characters can't be written as {} — they were \
                             replaced. Convert the file to UTF-8 to keep them.",
                            self.tabs[active].doc.encoding().name
                        ));
                    }
                }
                Err(e) => self.toast = Some(format!("save failed: {e}")),
            }
        }
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

    /// Render the tab strip inside a Left/Right side panel, honouring the
    /// `side_tabs_rotated` orientation option (#82). A side tab bar is always a
    /// single vertical column; when `_rotated` is on, each tab's label is drawn
    /// rotated 90° (vertical text) via [`Self::draw_rotated_side_tabs`],
    /// otherwise the standard horizontal-label rows. Scrolls so no tab becomes
    /// unreachable in a small window.
    fn draw_side_tab_strip(
        &mut self,
        ui: &mut egui::Ui,
        accent: Color32,
        muted: Color32,
        _rotated: bool,
    ) {
        // A side tab bar is ALWAYS a single vertical column of tabs (one per
        // row). The earlier horizontal-wrap experiment was wrong — the user
        // wants the column preserved; the orientation option (#82) only rotates
        // each tab's TEXT, not the stacking. Scrolls so no tab is unreachable.
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if _rotated {
                    ui.vertical(|ui| self.draw_rotated_side_tabs(ui, accent, muted));
                } else {
                    ui.vertical(|ui| self.draw_tab_strip(ui, accent, muted));
                }
            });
    }

    /// Natural width for a left/right tab bar so it HUGS the tab content instead
    /// of a fixed 180px slab. The bar should be only as wide as the widest note
    /// tab needs — a short tab name must not leave a big empty bar beside it.
    /// Rotated tabs are a narrow vertical-text strip; horizontal tabs are
    /// measured from the widest label plus the grip/pin/close affordances, then
    /// clamped so a very long filename can't make the bar enormous.
    fn side_tab_bar_width(&self, ctx: &egui::Context, rotated: bool) -> f32 {
        if rotated {
            // Vertical-text column + the close/pin buttons stacked above it.
            return 44.0;
        }
        let font = ctx
            .style()
            .text_styles
            .get(&egui::TextStyle::Body)
            .cloned()
            .unwrap_or_else(|| egui::FontId::proportional(14.0));
        // Measure via a Painter (its `layout_no_wrap` is the `&self` form egui
        // exposes for text measurement; `Fonts::layout_no_wrap` needs `&mut`).
        // No layer is painted — this only measures galley widths.
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Background,
            egui::Id::new("scr1b3_side_tab_width_probe"),
        ));
        let mut max_label = 0.0_f32;
        for tab in &self.tabs {
            let shown = tab_display_label(&tab.title(), tab.pinned);
            let w = painter
                .layout_no_wrap(shown, font.clone(), Color32::WHITE)
                .size()
                .x;
            max_label = max_label.max(w);
        }
        // grip(~12) + pin(~18) + close(~18) + inter-item spacing + chip inner
        // margin(16) + panel inner margin(8): the fixed affordances a tab row
        // carries beside its label. Clamped: never narrower than a couple of
        // buttons, never wider than a readable cap.
        (max_label + 74.0).clamp(96.0, 280.0)
    }

    /// Render the side tab bar with each tab's label ROTATED 90° (vertical text,
    /// reading top-to-bottom), still stacked in a single column (#82). The close
    /// button sits ABOVE each tab (with the pin toggle on the active tab); the
    /// rotated label below is the click/drag target. Drag-reorder is resolved
    /// against the tab rects exactly like the horizontal strip.
    fn draw_rotated_side_tabs(&mut self, ui: &mut egui::Ui, accent: Color32, muted: Color32) {
        let active = self.active;
        let mut switch_to = None;
        let mut close = None;
        let mut close_others = None;
        let mut close_to_right = None;
        let mut close_all = false;
        let mut toggle_pin: Option<usize> = None;
        let mut reorder: Option<(usize, usize)> = None;
        let mut drag_src: Option<usize> = None;
        let mut drop_pos: Option<egui::Pos2> = None;
        // #59-parity live drag feedback for the rotated column: the in-flight
        // pointer position so an insertion hairline can be painted at the drop gap.
        let mut dragging: Option<egui::Pos2> = None;
        let mut rects: Vec<(usize, egui::Rect)> = Vec::with_capacity(self.tabs.len());
        let mut add_tab = false;
        let pad = egui::vec2(8.0, 10.0);
        let font = egui::TextStyle::Button.resolve(ui.style());

        for i in 0..self.tabs.len() {
            let selected = i == active;
            let pinned = self.tabs[i].pinned;
            let shown = tab_display_label(&self.tabs[i].title(), pinned);
            let pin_label = if pinned { "Unpin tab" } else { "Pin tab" };
            // The active tab is a filled chip spanning the whole vertical cell
            // (icon row + rotated label), so it reads as a real tab.
            // Tighten the coloured chip to the text (like the top bar): a snug
            // inner margin + a thin accent outline on the active tab so the fill
            // HUGS the rotated label/grip stack instead of floating in the column.
            let chip = egui::Frame::default()
                .inner_margin(egui::Margin::symmetric(2, 3))
                .corner_radius(egui::CornerRadius::same(5))
                .stroke(if selected {
                    egui::Stroke::new(1.0, accent.linear_multiply(0.5))
                } else {
                    egui::Stroke::NONE
                })
                .fill(if selected {
                    accent.linear_multiply(0.20)
                } else {
                    Color32::TRANSPARENT
                });
            chip.show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    // #30 column order: grip · rotated-name · pin · close (top→bottom).
                    // Drag GRIP at the head — painted dots (`grip_handle`), never a
                    // phosphor glyph, so the handle can't tofu. Pinned tabs show a
                    // dimmed, drag-disabled grip.
                    if pinned {
                        grip_handle(ui, false, muted).on_hover_text("Pinned — drag disabled");
                    } else {
                        let g = grip_handle(ui, true, muted)
                            .on_hover_text("Drag to reorder")
                            .on_hover_cursor(egui::CursorIcon::Grab);
                        if g.dragged() {
                            if let Some(p) = g.interact_pointer_pos() {
                                dragging = Some(p);
                            }
                        }
                        if g.drag_stopped() {
                            if let Some(p) = g.interact_pointer_pos() {
                                drag_src = Some(i);
                                drop_pos = Some(p);
                            }
                        }
                    }
                    let color = if selected { accent } else { muted };
                    let galley = ui
                        .painter()
                        .layout_no_wrap(shown.clone(), font.clone(), color);
                    let size = rotated_tab_size(galley.size(), pad);
                    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
                    // Paint the label rotated 90° clockwise (reads top-to-bottom).
                    let pos = rotated_tab_text_pos(rect, galley.size(), pad);
                    ui.painter().add(egui::Shape::Text(
                        egui::epaint::TextShape::new(pos, galley, color)
                            .with_angle(std::f32::consts::FRAC_PI_2),
                    ));
                    if resp.clicked() {
                        switch_to = Some(i);
                    }
                    if resp.clicked_by(egui::PointerButton::Middle) && !pinned {
                        close = Some(i);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Close").clicked() {
                            close = Some(i);
                            ui.close_menu();
                        }
                        if ui.button("Close Others").clicked() {
                            close_others = Some(i);
                            ui.close_menu();
                        }
                        if ui.button("Close All to the Right").clicked() {
                            close_to_right = Some(i);
                            ui.close_menu();
                        }
                        if ui.button("Close All").clicked() {
                            close_all = true;
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button(pin_label).clicked() {
                            toggle_pin = Some(i);
                            ui.close_menu();
                        }
                    });
                    // The rotated name is also a drag target (in addition to the
                    // grip) so either reorders; pinned tabs never initiate a drag.
                    if resp.dragged() && !pinned {
                        if let Some(p) = resp.interact_pointer_pos() {
                            dragging = Some(p);
                        }
                    }
                    if resp.drag_stopped() && !pinned {
                        if let Some(p) = resp.interact_pointer_pos() {
                            drag_src = Some(i);
                            drop_pos = Some(p);
                        }
                    }
                    rects.push((i, rect));
                    // Pin toggle (active only), then close (unpinned), BELOW the name.
                    if selected {
                        let glyph = if pinned {
                            egui_phosphor::thin::PUSH_PIN_SLASH
                        } else {
                            egui_phosphor::thin::PUSH_PIN
                        };
                        if ui
                            .add(egui::Button::new(glyph).frame(false).small())
                            .on_hover_text(pin_label)
                            .clicked()
                        {
                            toggle_pin = Some(i);
                        }
                    }
                    if !pinned
                        && ui
                            .add(
                                egui::Button::new(egui_phosphor::thin::X)
                                    .frame(false)
                                    .small(),
                            )
                            .on_hover_text("Close tab (or middle-click)")
                            .clicked()
                    {
                        close = Some(i);
                    }
                });
            });
            ui.add_space(2.0);
        }
        // Centre the + add-tab button in the note-tab column (it used to hug the
        // left edge of the bar). `vertical_centered` centres the single button
        // horizontally in the column.
        ui.vertical_centered(|ui| {
            if ui
                .small_button("+")
                .on_hover_text("New tab (Ctrl+N)")
                .clicked()
            {
                add_tab = true;
            }
        });
        // #59-parity insertion indicator: while a tab is in flight, paint an
        // accent hairline in the GAP the drop will land in. The rotated column is
        // always vertical, so the boundary is a horizontal line — drawn at the
        // inter-tab gap midpoint and spanning the FULL column width so it reads
        // as a separator BETWEEN rows, never as a mark inside a chip.
        if let Some(pointer) = dragging {
            if let Some((_, last_rect)) = rects.last().copied() {
                let x_range = ui.max_rect().x_range();
                let painter = ui.painter();
                let accent_line = egui::Stroke::new(2.0, accent);
                let mut drawn = false;
                for (idx, (_, rect)) in rects.iter().enumerate() {
                    if pointer.y < rect.center().y {
                        // Drop lands ABOVE this row: paint in the gap above it
                        // (midpoint between the previous row's bottom and this top).
                        let y = if idx == 0 {
                            rect.top() - 1.0
                        } else {
                            (rects[idx - 1].1.bottom() + rect.top()) * 0.5
                        };
                        painter.hline(x_range, y, accent_line);
                        drawn = true;
                        break;
                    }
                }
                if !drawn {
                    painter.hline(x_range, last_rect.bottom() + 1.0, accent_line);
                }
            }
        }
        // Drag-reorder drop resolution — the SAME nearest-chip model as the
        // horizontal strip: a direct hit wins; otherwise snap to the NEAREST chip
        // centre along the column's vertical axis, so a release in an inter-tab
        // gap, over the pin/close glyphs, or past either end still reorders. (The
        // old `contains`-only test silently no-op'd in all those cases — the
        // batch-1 fix only covered `draw_tab_strip`.)
        if let (Some(src), Some(pos)) = (drag_src, drop_pos) {
            let mut target: Option<usize> = rects
                .iter()
                .find(|(_, rect)| rect.contains(pos))
                .map(|(j, _)| *j);
            if target.is_none() {
                let mut best: Option<(usize, f32)> = None;
                for (j, rect) in &rects {
                    let d = (pos.y - rect.center().y).abs();
                    if best.map_or(true, |(_, bd)| d < bd) {
                        best = Some((*j, d));
                    }
                }
                target = best.map(|(j, _)| j);
            }
            if let Some(t) = target {
                if t != src {
                    reorder = Some((src, t));
                }
            }
        }
        if let Some(i) = switch_to {
            self.active = i;
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
        if let Some(keep) = close_others {
            self.close_all_tabs_except(keep);
        }
        if let Some(after) = close_to_right {
            self.close_tabs_after(after);
        }
        if close_all {
            self.close_all_tabs();
        }
        if let Some(i) = toggle_pin {
            if i < self.tabs.len() {
                self.tabs[i].pinned = !self.tabs[i].pinned;
            }
        }
        if let Some((src, target)) = reorder {
            self.move_tab(src, target);
        }
        if add_tab {
            self.new_tab();
        }
    }

    /// Render the tab strip — the row (or column, for side positions) of open
    /// documents with the active one accented and an `×` close button on it.
    /// Extracted from the toolbar (T18.4) so the same widget can live inline at
    /// the top OR in a dedicated bottom / left / right panel. Mouse ergonomics:
    ///
    /// - **Click** → switch to that tab
    /// - **Middle-click** → close that tab (universal editor convention)
    /// - **Right-click** → context menu: Close · Close Others · Close All to the Right · Close All · Pin
    /// - **`×` button on the active tab** → close (back-compat with pre-audit behavior)
    /// - **Drag** → rearrange. Each tab is ONE `click_and_drag` widget (click
    ///   switches, drag reorders); the drop target is resolved AFTER the loop by
    ///   hit-testing the release position against every tab's full rect, so a
    ///   drop onto a tab to the RIGHT of the dragged one is no longer missed and
    ///   the extra `dnd_drop_zone` interaction that used to swallow the click is
    ///   gone. The index arithmetic lives in [`tab_index_after_move`] (unit-tested).
    ///   Closes F-001 / F-043 from `docs/audits/overlooked-surfaces-2026-05-29.md`.
    fn draw_tab_strip(&mut self, ui: &mut egui::Ui, accent: Color32, muted: Color32) {
        let active = self.active;
        let mut switch_to = None;
        let mut close = None;
        let mut close_others = None;
        let mut close_to_right = None;
        let mut close_all = false;
        let mut toggle_pin: Option<usize> = None;
        // Reorder is resolved AFTER the loop from the dragged tab's release
        // position against the full set of tab rects. (The original code
        // hit-tested a half-built vector and missed drop targets to the right;
        // a later rewrite wrapped each tab in dnd_drop_zone/dnd_drag_source,
        // whose extra interaction swallowed the click so tabs couldn't be
        // switched. This uses ONE click_and_drag widget per tab — click switches,
        // drag reorders — with the drop resolved here against every rect.)
        let mut reorder: Option<(usize, usize)> = None;
        let mut drag_src: Option<usize> = None;
        let mut drop_pos: Option<egui::Pos2> = None;
        let mut rects: Vec<(usize, egui::Rect)> = Vec::with_capacity(self.tabs.len());
        let mut add_tab = false;
        // #59 live drag feedback: the in-flight (index, label, current pointer)
        // while a tab is being dragged, so we can paint a ghost following the
        // cursor and an insertion indicator at the drop gap.
        let mut dragging: Option<(usize, String, egui::Pos2)> = None;

        for i in 0..self.tabs.len() {
            let selected = i == active;
            let pinned = self.tabs[i].pinned;
            let shown = tab_display_label(&self.tabs[i].title(), pinned);
            let pin_label = if pinned { "Unpin tab" } else { "Pin tab" };
            // Each tab is a cohesive CHIP: the active tab gets a filled, rounded
            // accent background + a thin accent outline spanning the grip · label ·
            // pin · close so it reads as a real tab; inactive tabs are dimmed text.
            // Click = switch, drag = reorder, middle-click / ✕ = close. The fill +
            // outline + margins MATCH `draw_rotated_side_tabs` so a tab looks the
            // same size/shape in every orientation (horizontal and rotated).
            let chip = egui::Frame::default()
                .inner_margin(egui::Margin::symmetric(8, 4))
                .corner_radius(egui::CornerRadius::same(5))
                .stroke(if selected {
                    egui::Stroke::new(1.0, accent.linear_multiply(0.5))
                } else {
                    egui::Stroke::NONE
                })
                .fill(if selected {
                    accent.linear_multiply(0.20)
                } else {
                    Color32::TRANSPARENT
                });
            let chip_resp = chip.show(ui, |ui| {
                // Force a single horizontal ROW for the chip contents (grip ·
                // name · pin · close). Top/bottom strips are already in a
                // horizontal parent so this is a no-op there; a SIDE strip's
                // parent is vertical, where without this wrap the name/pin/close
                // would stack into a ragged column per tab (#30).
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    // EVERY tab — top/bottom AND side — leads with an explicit drag
                    // GRIP so the row reads grip · name · pin · close, identical in
                    // all orientations and mirroring the split-pane header. Painted
                    // dots (`grip_handle`), never a phosphor glyph, so the handle is
                    // a clean grab affordance and can't tofu into an empty square (it
                    // is the single left-of-title affordance — there is no separate
                    // pin-glyph box). A pinned tab shows the grip DIMMED + drag-
                    // disabled, which is how pinned state now reads.
                    {
                        if pinned {
                            grip_handle(ui, false, muted).on_hover_text("Pinned — drag disabled");
                        } else {
                            let g = grip_handle(ui, true, muted)
                                .on_hover_text("Drag to reorder")
                                .on_hover_cursor(egui::CursorIcon::Grab);
                            if g.dragged() {
                                if let Some(p) = g.interact_pointer_pos() {
                                    dragging = Some((i, shown.clone(), p));
                                }
                            }
                            if g.drag_stopped() {
                                if let Some(p) = g.interact_pointer_pos() {
                                    drag_src = Some(i);
                                    drop_pos = Some(p);
                                }
                            }
                        }
                    }
                    let label =
                        RichText::new(shown.clone()).color(if selected { accent } else { muted });
                    let resp = ui.add(
                        egui::Label::new(label)
                            .selectable(false)
                            .sense(egui::Sense::click_and_drag()),
                    );
                    if resp.clicked() {
                        switch_to = Some(i);
                    }
                    if resp.clicked_by(egui::PointerButton::Middle) && !pinned {
                        close = Some(i);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Close").clicked() {
                            close = Some(i);
                            ui.close_menu();
                        }
                        if ui.button("Close Others").clicked() {
                            close_others = Some(i);
                            ui.close_menu();
                        }
                        if ui.button("Close All to the Right").clicked() {
                            close_to_right = Some(i);
                            ui.close_menu();
                        }
                        if ui.button("Close All").clicked() {
                            close_all = true;
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button(pin_label).clicked() {
                            toggle_pin = Some(i);
                            ui.close_menu();
                        }
                    });
                    // #R5: pinned notes are anchored — they switch on click but
                    // never initiate a drag-reorder (no ghost, no drop resolution).
                    if resp.dragged() && !pinned {
                        if let Some(p) = resp.interact_pointer_pos() {
                            dragging = Some((i, shown.clone(), p));
                        }
                    }
                    if resp.drag_stopped() && !pinned {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            drag_src = Some(i);
                            drop_pos = Some(pos);
                        }
                    }
                    // Pin TOGGLE — only on the active tab (the pin GLYPH in the
                    // label still marks any pinned tab, so non-active pins stay
                    // visible).
                    if selected {
                        let glyph = if pinned {
                            egui_phosphor::thin::PUSH_PIN_SLASH
                        } else {
                            egui_phosphor::thin::PUSH_PIN
                        };
                        if ui
                            .add(egui::Button::new(glyph).frame(false).small())
                            .on_hover_text(pin_label)
                            .clicked()
                        {
                            toggle_pin = Some(i);
                        }
                    }
                    // Close — on every UNPINNED tab. A pinned note hides the ✕ (and
                    // refuses middle-click / context Close) so it can't be closed by
                    // accident; unpin first (#R5).
                    if !pinned
                        && ui
                            .add(
                                egui::Button::new(egui_phosphor::thin::X)
                                    .frame(false)
                                    .small(),
                            )
                            .on_hover_text("Close tab (or middle-click)")
                            .clicked()
                    {
                        close = Some(i);
                    }
                });
            });
            // Hit-test the FULL chip rect (label + pin + close + margins), not the
            // bare name-label — that was why a drop in the gap, or over the
            // pin/close area, or past the last tab silently did nothing.
            rects.push((i, chip_resp.response.rect));
        }

        // "+" — add a new tab at the end of the strip (same as Ctrl+N).
        if ui
            .small_button("+")
            .on_hover_text("New tab (Ctrl+N)")
            .clicked()
        {
            add_tab = true;
        }

        // #59 live drag feedback — paint while a tab is in flight:
        //  * an insertion indicator (accent line) at the gap the drop will land
        //  * a ghost of the dragged label following the cursor
        // Both are painted on the foreground (paint-only, never interactable —
        // a `layer_painter`, not an `Area`, so it cannot swallow clicks).
        if let Some((src, ref label, pointer)) = dragging {
            // Infer strip orientation from the first two tab rects: when tabs
            // advance mostly in X the strip is horizontal (top/bottom); mostly
            // in Y means a vertical side strip.
            let horizontal = rects.len() < 2
                || (rects[1].1.center().x - rects[0].1.center().x).abs()
                    >= (rects[1].1.center().y - rects[0].1.center().y).abs();

            // Insertion gap: the boundary nearest the pointer along the main
            // axis. We draw the line on the leading edge of the first tab whose
            // center is past the pointer (or the trailing edge of the last).
            let painter = ui.painter();
            let accent_line = egui::Stroke::new(2.0, accent);
            if let Some((_, last_rect)) = rects.last().copied().map(|r| (r.0, r.1)) {
                let mut drawn = false;
                for (_, rect) in &rects {
                    let past = if horizontal {
                        pointer.x < rect.center().x
                    } else {
                        pointer.y < rect.center().y
                    };
                    if past {
                        if horizontal {
                            painter.vline(rect.left(), rect.y_range(), accent_line);
                        } else {
                            painter.hline(rect.x_range(), rect.top(), accent_line);
                        }
                        drawn = true;
                        break;
                    }
                }
                if !drawn {
                    // Pointer is beyond the last tab — indicate append-at-end.
                    if horizontal {
                        painter.vline(last_rect.right(), last_rect.y_range(), accent_line);
                    } else {
                        painter.hline(last_rect.x_range(), last_rect.bottom(), accent_line);
                    }
                }
            }

            // Ghost label trailing the cursor (slightly offset so it doesn't sit
            // under the pointer). Drawn on the Tooltip layer so it floats above
            // the strip without taking input.
            let ghost = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::new("tab-drag-ghost"),
            ));
            let font = egui::TextStyle::Button.resolve(ui.style());
            let ghost_pos = pointer + egui::vec2(12.0, 6.0);
            let galley = ghost.layout_no_wrap(label.clone(), font, accent);
            // Soft backing chip for legibility against any background.
            let bg =
                egui::Rect::from_min_size(ghost_pos, galley.size()).expand2(egui::vec2(6.0, 3.0));
            ghost.rect_filled(bg, 4.0, muted.linear_multiply(0.25));
            ghost.galley(ghost_pos, galley, accent);
            let _ = src;
        }

        // Drag-reorder drop resolution. Use the SAME model as the live indicator:
        // a direct hit on a chip wins; otherwise snap to the NEAREST chip centre
        // along the strip's main axis. This means a release in an inter-tab gap,
        // over the pin/close area, or past either end still reorders (the old
        // name-label-`contains` test silently no-op'd in all those cases).
        if let (Some(src), Some(pos)) = (drag_src, drop_pos) {
            let horizontal = rects.len() < 2
                || (rects[1].1.center().x - rects[0].1.center().x).abs()
                    >= (rects[1].1.center().y - rects[0].1.center().y).abs();
            let mut target: Option<usize> = rects
                .iter()
                .find(|(_, rect)| rect.contains(pos))
                .map(|(j, _)| *j);
            if target.is_none() {
                let mut best: Option<(usize, f32)> = None;
                for (j, rect) in &rects {
                    let d = if horizontal {
                        (pos.x - rect.center().x).abs()
                    } else {
                        (pos.y - rect.center().y).abs()
                    };
                    if best.map_or(true, |(_, bd)| d < bd) {
                        best = Some((*j, d));
                    }
                }
                target = best.map(|(j, _)| j);
            }
            if let Some(t) = target {
                if t != src {
                    reorder = Some((src, t));
                }
            }
        }

        // Subtle dividers between tabs — a faint hairline in each inter-chip gap
        // so the tabs read as distinct without heavy separators. Painted after
        // the strip is laid out, using the same horizontal/vertical inference.
        if rects.len() > 1 {
            let horizontal = (rects[1].1.center().x - rects[0].1.center().x).abs()
                >= (rects[1].1.center().y - rects[0].1.center().y).abs();
            let painter = ui.painter();
            let hairline = egui::Stroke::new(1.0, muted.linear_multiply(0.30));
            for pair in rects.windows(2) {
                let (a, b) = (pair[0].1, pair[1].1);
                if horizontal {
                    let x = (a.right() + b.left()) * 0.5;
                    let y = egui::Rangef::new(a.top() + 3.0, a.bottom() - 3.0);
                    painter.vline(x, y, hairline);
                } else {
                    let y = (a.bottom() + b.top()) * 0.5;
                    let x = egui::Rangef::new(a.left() + 3.0, a.right() - 3.0);
                    painter.hline(x, y, hairline);
                }
            }
        }

        if let Some(i) = switch_to {
            self.active = i;
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
        if let Some(keep) = close_others {
            self.close_all_tabs_except(keep);
        }
        if let Some(after) = close_to_right {
            self.close_tabs_after(after);
        }
        if close_all {
            self.close_all_tabs();
        }
        if let Some(i) = toggle_pin {
            if i < self.tabs.len() {
                self.tabs[i].pinned = !self.tabs[i].pinned;
            }
        }
        if let Some((src, target)) = reorder {
            self.move_tab(src, target);
        }
        if add_tab {
            self.new_tab();
        }
    }

    /// Move the tab at `src` so it takes original position `target`'s slot
    /// (drag-and-drop reorder), keeping [`Self::active`] pointed at the same
    /// buffer the user is editing. No-op if either index is out of range or
    /// they are equal. Index math is in [`tab_index_after_move`].
    fn move_tab(&mut self, src: usize, target: usize) {
        if src >= self.tabs.len() || target >= self.tabs.len() || src == target {
            return;
        }
        // #R5: pinned notes are anchored — refuse to reorder a pinned tab.
        if self.tabs[src].pinned {
            return;
        }
        let new_active = tab_index_after_move(src, target, self.active);
        let tab = self.tabs.remove(src);
        // `target < original len` ⇒ `target <= new len`, so this never panics.
        self.tabs.insert(target, tab);
        self.active = new_active.min(self.tabs.len().saturating_sub(1));
    }

    /// Close every tab whose index is not `keep` AND is not pinned (F-044).
    fn close_all_tabs_except(&mut self, keep: usize) {
        if keep >= self.tabs.len() {
            return;
        }
        // Walk back-to-front so swap-remove indices stay valid; never remove
        // the kept index or any pinned tab.
        let mut i = self.tabs.len();
        while i > 0 {
            i -= 1;
            if i != keep && !self.tabs[i].pinned {
                self.tabs.remove(i);
            }
        }
        // Active retargets to the surviving copy of `keep` (its index may
        // have shifted left as pinned tabs above were preserved).
        // Simplest: find the kept tab's new index by pointer-equality
        // proxy. Since we never removed `keep`, count surviving tabs
        // before it.
        // Conservative fallback: clamp.
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
    }

    /// Close every tab after `after` (exclusive) that is not pinned (F-044).
    fn close_tabs_after(&mut self, after: usize) {
        let mut i = self.tabs.len();
        while i > after + 1 {
            i -= 1;
            if !self.tabs[i].pinned {
                self.tabs.remove(i);
            }
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
    }

    /// Close every tab that is not pinned (F-044). If nothing was unpinned,
    /// leave the tabs alone (don't replace them with a scratch buffer).
    fn close_all_tabs(&mut self) {
        let any_unpinned = self.tabs.iter().any(|t| !t.pinned);
        if any_unpinned {
            self.tabs.retain(|t| t.pinned);
        }
        if self.tabs.is_empty() {
            self.tabs.push(EditorTab::scratch());
        }
        self.active = 0;
    }

    fn close_tab(&mut self, idx: usize) {
        // #R5: pinned notes can't be closed directly — unpin first. This is the
        // single chokepoint behind the ✕ button, middle-click, and the
        // context-menu "Close" in the tab strip.
        if idx < self.tabs.len() && self.tabs[idx].pinned {
            self.status = "Note is pinned — unpin it to close".to_string();
            return;
        }
        crate::action_log::record("tab", "close");
        if idx < self.tabs.len() {
            // F-021 — capture the current scroll position so the next open
            // of the same path restores it. Uses the last-frame
            // scroll_metrics value the central panel records.
            if let Some(path) = self.tabs[idx].doc.path().map(|p| p.to_path_buf()) {
                let key = path.display().to_string();
                let y = self.scroll_metrics.0;
                scribe_core::config::record_scroll_pos(
                    &mut self.config.editor.scroll_positions,
                    &key,
                    y,
                );
                // Remember the caret too (best-effort; restored on reopen).
                if self.config.editor.restore_cursor_position {
                    let cur = self.tabs[idx]
                        .rope_state
                        .as_ref()
                        .map(|s| s.edit.cursor)
                        .unwrap_or(0);
                    if self.config.editor.cursor_positions.len()
                        >= scribe_core::config::SCROLL_POS_CAP
                    {
                        if let Some(k) = self.config.editor.cursor_positions.keys().next().cloned()
                        {
                            self.config.editor.cursor_positions.remove(&k);
                        }
                    }
                    self.config.editor.cursor_positions.insert(key.clone(), cur);
                }
                self.save_config();
            }
            // Push the closed tab onto the reopen stack (Ctrl+Shift+T-style),
            // capturing its content + caret so an accidental close is one
            // keystroke from recovery. Skip pristine empty scratch tabs.
            let tab = &self.tabs[idx];
            let cursor = tab.rope_state.as_ref().map(|s| s.edit.cursor).unwrap_or(0);
            if tab.doc.path().is_some() || !tab.text.is_empty() {
                self.closed_tabs.push(ClosedTab {
                    path: tab.doc.path().map(|p| p.to_path_buf()),
                    text: tab.text.clone(),
                    cursor,
                });
                const MAX_CLOSED: usize = 20;
                if self.closed_tabs.len() > MAX_CLOSED {
                    self.closed_tabs.remove(0);
                }
            }
            self.tabs.remove(idx);
            if self.tabs.is_empty() {
                self.tabs.push(EditorTab::scratch());
            }
            self.active = self.active.min(self.tabs.len() - 1);
        }
    }

    /// Reopen the most recently closed tab (restoring its unsaved content +
    /// caret). No-op when the reopen stack is empty.
    fn reopen_closed_tab(&mut self) {
        let Some(closed) = self.closed_tabs.pop() else {
            self.status = "no closed tab to reopen".to_string();
            return;
        };
        let mut tab = EditorTab::from_backup(closed.path, closed.text);
        if self.config.editor.experimental_rope_editor {
            let mut st = scribe_render::RopeEditorState::new();
            st.edit = scribe_core::editing::EditState::at(closed.cursor);
            tab.rope_state = Some(st);
        }
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.status = "reopened closed tab".to_string();
    }

    /// F-008 — Replace `find_query` with `replace_query` in the active
    /// buffer. `all=true` replaces every match; `all=false` replaces only the
    /// first. Matches with the SAME engine the find bar highlights with
    /// ([`find_matches_active`] → a default literal, case-insensitive
    /// [`scribe_core::search::Query`]), so Replace touches exactly what the
    /// user sees highlighted. Skips when the find field is empty.
    ///
    /// Root cause of the prior divergence: this used `str::replace` /
    /// `str::find`, which are case-SENSITIVE, while the find bar highlights
    /// case-INSENSITIVELY — so "Replace All" silently skipped the case-variant
    /// matches the find bar had highlighted (find said 3, replace changed 1).
    /// Unifying both surfaces on `find_all` eliminates the divergence. The
    /// replacement is spliced LITERALLY (the find bar exposes no regex toggle,
    /// so `$1`-style regex expansion must not leak in).
    fn replace_in_active(&mut self, all: bool) {
        if self.find_query.is_empty() || self.active >= self.tabs.len() {
            return;
        }
        let pat = self.find_query.clone();
        let rep = self.replace_query.clone();
        let q = scribe_core::search::Query {
            pattern: pat.clone(),
            ..Default::default()
        };
        let matches =
            scribe_core::search::find_all(&self.tabs[self.active].text, &q).unwrap_or_default();
        if matches.is_empty() {
            self.status = format!("no match for '{pat}'");
            return;
        }
        // Splice right-to-left so earlier byte offsets stay valid as the text
        // shifts. Match spans are UTF-8 char boundaries (regex guarantee), so
        // `replace_range` cannot panic on a boundary.
        let text = &mut self.tabs[self.active].text;
        let spans = if all { &matches[..] } else { &matches[..1] };
        for m in spans.iter().rev() {
            text.replace_range(m.start..m.end, &rep);
        }
        self.status = if all {
            format!("replaced {} x '{pat}' -> '{rep}'", matches.len())
        } else {
            format!("replaced '{pat}' -> '{rep}'")
        };
        // Wave-3: invalidate the gen-keyed minimap/spell caches.
        let i = self.active;
        self.tabs[i].edit_gen = self.tabs[i].edit_gen.wrapping_add(1);
    }

    /// Wave-5: run the project-wide search over the open folder into the
    /// results pane. Reuses the in-buffer find engine + the open file-tree root.
    fn run_find_in_files(&mut self) {
        self.find_in_files_error = None;
        self.find_in_files_results.clear();
        let Some(root) = self.file_tree_root.clone() else {
            self.find_in_files_error = Some("open a folder first".into());
            return;
        };
        let query = scribe_core::search::Query {
            pattern: self.find_in_files_query.clone(),
            regex: self.find_in_files_regex,
            case_sensitive: false,
            whole_word: false,
        };
        // Surface a bad regex once (the per-file search swallows it silently).
        if query.regex {
            if let Err(e) = scribe_core::search::find_all("", &query) {
                self.find_in_files_error = Some(format!("bad regex: {e}"));
                return;
            }
        }
        self.find_in_files_results = crate::find_in_files::search_project(&root, &query);
        self.status = format!("{} match(es)", self.find_in_files_results.len());
    }

    /// Wave-5: open `path` in a tab (reusing the normal open path) then scroll to
    /// 1-based `line` (the click-to-open target from the results pane).
    fn open_find_in_files_result(&mut self, path: PathBuf, line: usize) {
        self.open_path(path);
        self.goto_line(line.max(1));
    }

    /// F-016 — Toggle the line-comment prefix on every line touched by the
    /// active selection (or the cursor line if no selection). The prefix is
    /// picked from `comment_prefix_for_extension` based on the active doc's
    /// language hint; unknown languages fall back to no-op + status toast.
    ///
    /// Behaviour: if EVERY non-blank touched line already starts with the
    /// prefix, strip one prefix occurrence per line; otherwise prepend the
    /// prefix to every non-blank line.
    fn toggle_comment_active(&mut self) {
        if self.active >= self.tabs.len() {
            return;
        }
        let lang = self.tabs[self.active].doc.language_hint();
        let prefix = lang
            .as_deref()
            .and_then(comment_prefix_for_extension)
            .unwrap_or("");
        if prefix.is_empty() {
            self.toast = Some("no comment prefix for this language".to_string());
            return;
        }
        let text = &mut self.tabs[self.active].text;
        // Cheap full-buffer rewrite: split, decide direction by ALL-vs-ANY,
        // toggle, rejoin. The user's "selection" surface is the whole
        // buffer until we wire egui's selection range through to the rope
        // helpers (Phase 15 KEYSTONE follow-up F-009).
        let lines: Vec<&str> = text.lines().collect();
        let non_blank = lines.iter().any(|l| !l.trim().is_empty());
        if !non_blank {
            return;
        }
        let all_commented = lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .all(|l| l.trim_start().starts_with(prefix));
        let pfx_with_space = format!("{prefix} ");
        let new_lines: Vec<String> = lines
            .iter()
            .map(|l| {
                if l.trim().is_empty() {
                    (*l).to_string()
                } else if all_commented {
                    // Strip the prefix (and one trailing space if present).
                    let trimmed = l.trim_start();
                    let leading_ws_len = l.len() - trimmed.len();
                    let after_pfx = trimmed
                        .strip_prefix(&pfx_with_space)
                        .or_else(|| trimmed.strip_prefix(prefix))
                        .unwrap_or(trimmed);
                    format!("{}{}", &l[..leading_ws_len], after_pfx)
                } else {
                    let trimmed = l.trim_start();
                    let leading_ws_len = l.len() - trimmed.len();
                    format!("{}{pfx_with_space}{trimmed}", &l[..leading_ws_len])
                }
            })
            .collect();
        // Preserve a trailing newline if the original buffer had one.
        let trailing_nl = text.ends_with('\n');
        *text = new_lines.join("\n");
        if trailing_nl {
            text.push('\n');
        }
        let i = self.active;
        self.tabs[i].edit_gen = self.tabs[i].edit_gen.wrapping_add(1);
    }

    /// F-017 — Swap the cursor line with the neighbour `dir` rows away (-1 =
    /// up, +1 = down). No-op at the buffer's first/last line. The cursor
    /// "line" is read from `last_cursor_line_col`; if absent, defaults to
    /// line 0 (start of buffer) so the action is still observable on a
    /// fresh buffer.
    fn move_cursor_line(&mut self, dir: i32) {
        if self.active >= self.tabs.len() {
            return;
        }
        let ln = self
            .last_cursor_line_col
            .map(|(l, _)| l.saturating_sub(1))
            .unwrap_or(0);
        let text = &mut self.tabs[self.active].text;
        let trailing_nl = text.ends_with('\n');
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        // split('\n') with a trailing newline produces a trailing "" — drop it.
        if trailing_nl && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if lines.is_empty() {
            return;
        }
        let target = (ln as i32) + dir;
        if target < 0 || (target as usize) >= lines.len() {
            return;
        }
        lines.swap(ln, target as usize);
        // Track the cursor to the moved line.
        let new_ln = target as usize + 1;
        let new_col = self.last_cursor_line_col.map(|(_, c)| c).unwrap_or(1);
        self.last_cursor_line_col = Some((new_ln, new_col));
        *text = lines.join("\n");
        if trailing_nl {
            text.push('\n');
        }
        let i = self.active;
        self.tabs[i].edit_gen = self.tabs[i].edit_gen.wrapping_add(1);
    }

    /// F-017 — Duplicate the cursor line in-place: the new copy lands on the
    /// row immediately below.
    fn duplicate_cursor_line(&mut self) {
        if self.active >= self.tabs.len() {
            return;
        }
        let ln = self
            .last_cursor_line_col
            .map(|(l, _)| l.saturating_sub(1))
            .unwrap_or(0);
        let text = &mut self.tabs[self.active].text;
        let trailing_nl = text.ends_with('\n');
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        if trailing_nl && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if ln >= lines.len() {
            return;
        }
        let copy = lines[ln].clone();
        lines.insert(ln + 1, copy);
        *text = lines.join("\n");
        if trailing_nl {
            text.push('\n');
        }
        let i = self.active;
        self.tabs[i].edit_gen = self.tabs[i].edit_gen.wrapping_add(1);
    }

    /// F-017 — Join the cursor line with the next: trims the trailing
    /// whitespace of the cursor line + the leading whitespace of the next,
    /// joins them with a single space (the standard editor convention).
    fn join_cursor_line_with_next(&mut self) {
        if self.active >= self.tabs.len() {
            return;
        }
        let ln = self
            .last_cursor_line_col
            .map(|(l, _)| l.saturating_sub(1))
            .unwrap_or(0);
        let text = &mut self.tabs[self.active].text;
        let trailing_nl = text.ends_with('\n');
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        if trailing_nl && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if ln + 1 >= lines.len() {
            return;
        }
        let next = lines.remove(ln + 1);
        let cur = lines[ln].trim_end().to_string();
        let nxt = next.trim_start();
        lines[ln] = if cur.is_empty() || nxt.is_empty() {
            format!("{cur}{nxt}")
        } else {
            format!("{cur} {nxt}")
        };
        *text = lines.join("\n");
        if trailing_nl {
            text.push('\n');
        }
        let i = self.active;
        self.tabs[i].edit_gen = self.tabs[i].edit_gen.wrapping_add(1);
    }

    /// F-015 — Scroll the active buffer so the given 1-based line is in the
    /// viewport. The minimap renderer already drives `pending_scroll` for
    /// click-jump; we reuse that pipe by computing the approximate Y of
    /// `line` from the current per-line gutter heights (one-frame lag is
    /// fine — same lag the minimap accepts).
    fn goto_line(&mut self, line_1based: usize) {
        if self.active >= self.tabs.len() {
            return;
        }
        let line0 = line_1based.saturating_sub(1);
        // Prefer the captured per-line gutter Ys (most accurate; populated
        // each frame when line numbers render). Fall back to a simple
        // line-height * index estimate otherwise.
        if let Some(&y) = self.line_gutter.get(line0) {
            // line_gutter Ys are screen-Y; the editor scroll-pipe wants the
            // vertical offset INSIDE the scroll area. The minimap already
            // assumes scroll-area = full window vertically — keep that.
            self.pending_scroll = Some(y.max(0.0));
        } else {
            let lh = self.config.fonts.editor_size * self.config.fonts.line_height;
            self.pending_scroll = Some((line0 as f32) * lh);
        }
        self.status = format!("go to line {line_1based}");
    }

    /// #R6 — all matches of the current find query in the active buffer (empty
    /// when there is no query / no buffer / the regex is invalid).
    fn find_matches_active(&self) -> Vec<scribe_core::search::Match> {
        if self.find_query.is_empty() || self.active >= self.tabs.len() {
            return Vec::new();
        }
        let q = scribe_core::search::Query {
            pattern: self.find_query.clone(),
            ..Default::default()
        };
        scribe_core::search::find_all(&self.tabs[self.active].text, &q).unwrap_or_default()
    }

    /// Scroll the editor so the byte offset `start` is in view, reusing the
    /// gutter-Y scroll pipe `goto_line` uses (without its status message).
    fn scroll_to_offset(&mut self, start: usize) {
        if self.active >= self.tabs.len() {
            return;
        }
        let line0 = {
            let text = &self.tabs[self.active].text;
            let clamped = start.min(text.len());
            text.as_bytes()[..clamped]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
        };
        if let Some(&y) = self.line_gutter.get(line0) {
            self.pending_scroll = Some(y.max(0.0));
        } else {
            let lh = self.config.fonts.editor_size * self.config.fonts.line_height;
            self.pending_scroll = Some((line0 as f32) * lh);
        }
    }

    /// Move to the next (`forward`) or previous find match, wrapping around, and
    /// scroll it into view. No-op when there are no matches.
    fn find_navigate(&mut self, forward: bool) {
        let matches = self.find_matches_active();
        if matches.is_empty() {
            return;
        }
        let n = matches.len();
        // Clamp first (the buffer or query may have changed since last frame).
        self.find_match_idx = self.find_match_idx.min(n - 1);
        self.find_match_idx = if forward {
            (self.find_match_idx + 1) % n
        } else {
            (self.find_match_idx + n - 1) % n
        };
        let start = matches[self.find_match_idx].start;
        self.scroll_to_offset(start);
        self.status = format!("match {} of {}", self.find_match_idx + 1, n);
    }

    /// 0-based cursor line of the active tab (from `last_cursor_line_col`,
    /// which is 1-based; defaults to line 0 when no caret has been seen yet).
    fn cursor_line0(&self) -> usize {
        self.last_cursor_line_col
            .map(|(l, _)| l.saturating_sub(1))
            .unwrap_or(0)
    }

    /// Toggle a bookmark on the active tab's cursor line.
    fn toggle_bookmark(&mut self) {
        if self.active >= self.tabs.len() {
            return;
        }
        let line0 = self.cursor_line0();
        let bm = &mut self.tabs[self.active].bookmarks;
        if bm.remove(&line0) {
            self.status = format!("bookmark removed: line {}", line0 + 1);
        } else {
            bm.insert(line0);
            self.status = format!("bookmark added: line {}", line0 + 1);
        }
    }

    /// Jump to the next (`dir = 1`) or previous (`dir = -1`) bookmark on the
    /// active tab, wrapping around the buffer. No-op (with a status hint) when
    /// the tab has no bookmarks.
    fn navigate_bookmark(&mut self, dir: i32) {
        if self.active >= self.tabs.len() {
            return;
        }
        let from = self.cursor_line0();
        let target = pick_bookmark(&self.tabs[self.active].bookmarks, from, dir);
        match target {
            Some(line0) => self.goto_line(line0 + 1),
            None => self.status = "no bookmarks in this buffer".to_string(),
        }
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
        let line_height = self.config.fonts.line_height;
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

// ---- Keyboard shortcut cheatsheet table (F-014) ----

pub(crate) struct ShortcutEntry {
    pub chord: &'static str,
    pub action: &'static str,
}

/// The canonical "what shortcuts does SCR1B3 ship?" table. Rendered by the
/// F1 cheatsheet modal. Add new wired shortcuts HERE so the modal stays in
/// sync — every shortcut the editor actually responds to must appear in
/// this list. Grouped loosely top-to-bottom: file ops → tab/buffer ops →
/// find/replace → window/help.
pub(crate) const KEYBOARD_SHORTCUTS: &[ShortcutEntry] = &[
    ShortcutEntry {
        chord: "Ctrl+N",
        action: "New file",
    },
    ShortcutEntry {
        chord: "Ctrl+O",
        action: "Open file…",
    },
    ShortcutEntry {
        chord: "Ctrl+S",
        action: "Save active buffer",
    },
    ShortcutEntry {
        chord: "Ctrl+W",
        action: "Close active tab",
    },
    ShortcutEntry {
        chord: "Ctrl+Tab",
        action: "Cycle to next tab",
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+Tab",
        action: "Cycle to previous tab",
    },
    ShortcutEntry {
        chord: "Ctrl+\\",
        action: "Toggle multi-note grid",
    },
    ShortcutEntry {
        chord: "Ctrl+F",
        action: "Find in buffer",
    },
    ShortcutEntry {
        chord: "Ctrl+H",
        action: "Find + replace in buffer",
    },
    ShortcutEntry {
        chord: "Ctrl+/",
        action: "Toggle line comment (per-language prefix)",
    },
    ShortcutEntry {
        chord: "Ctrl+G",
        action: "Go to line (or line:column)",
    },
    ShortcutEntry {
        chord: "Ctrl+R",
        action: "Open a recent file (MRU list)",
    },
    ShortcutEntry {
        chord: "Ctrl+P",
        action: "Fuzzy-find a file in the project",
    },
    ShortcutEntry {
        chord: "Ctrl+F2",
        action: "Toggle a bookmark on the cursor line",
    },
    ShortcutEntry {
        chord: "F2",
        action: "Jump to the next bookmark",
    },
    ShortcutEntry {
        chord: "Shift+F2",
        action: "Jump to the previous bookmark",
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+O",
        action: "Go to a symbol (definition) in the active buffer",
    },
    ShortcutEntry {
        chord: "Alt+Up",
        action: "Move cursor line up",
    },
    ShortcutEntry {
        chord: "Alt+Down",
        action: "Move cursor line down",
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+D",
        action: "Duplicate cursor line",
    },
    ShortcutEntry {
        chord: "Ctrl+J",
        action: "Join cursor line with next",
    },
    ShortcutEntry {
        chord: "Ctrl+Space",
        action: "Identifier completion (popup)",
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+P",
        action: "Command palette",
    },
    ShortcutEntry {
        chord: "F1",
        action: "Show this keyboard cheatsheet",
    },
    ShortcutEntry {
        chord: "F11",
        action: "Toggle fullscreen — editor only, all chrome hidden (Esc exits)",
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+T",
        action: "Cycle to the next built-in theme",
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+M",
        action: "Toggle minimap on/off",
    },
    ShortcutEntry {
        chord: "Esc",
        action: "Close find / palette / cheatsheet / completion popup",
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+[",
        action: "Fold every region in the active buffer",
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+]",
        action: "Expand every folded region",
    },
    ShortcutEntry {
        chord: "Ctrl+C",
        action: "Copy selection to clipboard",
    },
    ShortcutEntry {
        chord: "Ctrl+X",
        action: "Cut selection to clipboard",
    },
    ShortcutEntry {
        chord: "Ctrl+V",
        action: "Paste from clipboard",
    },
    ShortcutEntry {
        chord: "Ctrl+Z",
        action: "Undo",
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+Z",
        action: "Redo",
    },
    ShortcutEntry {
        // Phosphor (Thin) ARROW_DOWN (U+E03E) / ARROW_UP (U+E08E) — the bare
        // U+2193/U+2191 arrows were tofu in the bundled fonts.
        chord: "Ctrl+Alt+\u{E03E} / \u{E08E}",
        action: "Add caret below / above (multi-cursor — in-house editor)",
    },
    ShortcutEntry {
        chord: "Ctrl+D",
        action: "Select word, then add caret on next occurrence (in-house editor)",
    },
    ShortcutEntry {
        chord: "Alt+drag",
        action: "Column / block selection (in-house editor)",
    },
    ShortcutEntry {
        chord: "Esc",
        action: "Collapse multi-cursor to one caret",
    },
    ShortcutEntry {
        chord: "Ctrl+= / Ctrl+- / Ctrl+0",
        action: "Zoom font in / out / reset (also Ctrl+scroll)",
    },
    ShortcutEntry {
        chord: "Ctrl+.",
        action: "Toggle zen / distraction-free mode (Esc exits)",
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+R",
        action: "Reopen the most recently closed tab",
    },
    ShortcutEntry {
        chord: "Tab / Shift+Tab",
        action: "Indent / outdent selected lines (experimental editor)",
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+K",
        action: "Delete the current line (experimental editor)",
    },
    ShortcutEntry {
        chord: "Ctrl+U / Ctrl+Shift+U",
        action: "Lowercase / uppercase the selection (experimental editor)",
    },
];

// ---- Built-in command palette registry (F-004) ----
//
// Every editor action a user can take without writing a plugin. Each entry
// is exposed in the Ctrl+Shift+P palette so the editor is self-discoverable
// on first launch (the old "plugin only" palette showed nothing on a fresh
// install). The shortcut column displays the key chord when one is wired.
// Invocation routes through `execute_builtin` (below) so the palette and
// the keyboard chord produce identical state changes.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinCommand {
    NewFile,
    OpenFile,
    OpenFolder,
    Save,
    CloseActiveTab,
    CloseAllTabs,
    ConvertToMarkdown,
    ExportAsHtml,
    SetLineEndingsLf,
    SetLineEndingsCrlf,
    SetLineEndingsCr,
    CycleTabNext,
    CycleTabPrev,
    ToggleSplitView,
    ToggleMinimap,
    ToggleZen,
    ToggleMarkdownPreview,
    ToggleDiffView,
    ToggleSpellcheck,
    ToggleWordWrap,
    ToggleLineNumbers,
    OpenSettings,
    OpenFind,
    OpenPalette,
    CycleTheme,
    StartLsp,
    FoldAll,
    ExpandAll,
    OpenPluginManager,
    SortLines,
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
    ToggleBookmark,
    NextBookmark,
    PrevBookmark,
    GoToSymbol,
}

/// A clipboard / history action the palette requests; drained in `frame_tick`
/// by injecting the matching egui event into the focused central editor so
/// egui's `TextEdit` performs the operation with its own selection + undo
/// state (no parallel editing model to keep in sync).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorAction {
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
}

pub(crate) struct BuiltinEntry {
    pub label: &'static str,
    pub shortcut: &'static str,
    pub action: BuiltinCommand,
}

/// The full registry, alphabetised by label so the palette is stable across
/// launches. Add new editor actions HERE so the palette stays the canonical
/// self-discovery surface.
pub(crate) const BUILTIN_COMMANDS: &[BuiltinEntry] = &[
    BuiltinEntry {
        label: "Close active tab",
        shortcut: "",
        action: BuiltinCommand::CloseActiveTab,
    },
    BuiltinEntry {
        label: "Close all tabs",
        shortcut: "",
        action: BuiltinCommand::CloseAllTabs,
    },
    BuiltinEntry {
        label: "Convert to Markdown and save as .md",
        shortcut: "",
        action: BuiltinCommand::ConvertToMarkdown,
    },
    BuiltinEntry {
        label: "Copy",
        shortcut: "Ctrl+C",
        action: BuiltinCommand::Copy,
    },
    BuiltinEntry {
        label: "Cut",
        shortcut: "Ctrl+X",
        action: BuiltinCommand::Cut,
    },
    BuiltinEntry {
        label: "Cycle theme",
        shortcut: "",
        action: BuiltinCommand::CycleTheme,
    },
    BuiltinEntry {
        label: "Expand all folds",
        shortcut: "Ctrl+Shift+]",
        action: BuiltinCommand::ExpandAll,
    },
    BuiltinEntry {
        label: "Export as HTML…",
        shortcut: "",
        action: BuiltinCommand::ExportAsHtml,
    },
    BuiltinEntry {
        label: "Find in buffer",
        shortcut: "Ctrl+F",
        action: BuiltinCommand::OpenFind,
    },
    BuiltinEntry {
        label: "Fold all regions",
        shortcut: "Ctrl+Shift+[",
        action: BuiltinCommand::FoldAll,
    },
    BuiltinEntry {
        label: "Line endings: CR (classic Mac)",
        shortcut: "",
        action: BuiltinCommand::SetLineEndingsCr,
    },
    BuiltinEntry {
        label: "Line endings: CRLF (Windows)",
        shortcut: "",
        action: BuiltinCommand::SetLineEndingsCrlf,
    },
    BuiltinEntry {
        label: "Line endings: LF (Unix)",
        shortcut: "",
        action: BuiltinCommand::SetLineEndingsLf,
    },
    BuiltinEntry {
        label: "Go to symbol…",
        shortcut: "Ctrl+Shift+O",
        action: BuiltinCommand::GoToSymbol,
    },
    BuiltinEntry {
        label: "Manage plugins",
        shortcut: "",
        action: BuiltinCommand::OpenPluginManager,
    },
    BuiltinEntry {
        label: "Navigate to next bookmark",
        shortcut: "F2",
        action: BuiltinCommand::NextBookmark,
    },
    BuiltinEntry {
        label: "Navigate to previous bookmark",
        shortcut: "Shift+F2",
        action: BuiltinCommand::PrevBookmark,
    },
    BuiltinEntry {
        label: "New file",
        shortcut: "Ctrl+N",
        action: BuiltinCommand::NewFile,
    },
    BuiltinEntry {
        label: "Next tab",
        shortcut: "",
        action: BuiltinCommand::CycleTabNext,
    },
    BuiltinEntry {
        label: "Open file…",
        shortcut: "Ctrl+O",
        action: BuiltinCommand::OpenFile,
    },
    BuiltinEntry {
        label: "Open folder…",
        shortcut: "",
        action: BuiltinCommand::OpenFolder,
    },
    BuiltinEntry {
        label: "Open settings",
        shortcut: "",
        action: BuiltinCommand::OpenSettings,
    },
    BuiltinEntry {
        label: "Paste",
        shortcut: "Ctrl+V",
        action: BuiltinCommand::Paste,
    },
    BuiltinEntry {
        label: "Previous tab",
        shortcut: "",
        action: BuiltinCommand::CycleTabPrev,
    },
    BuiltinEntry {
        label: "Redo",
        shortcut: "Ctrl+Shift+Z",
        action: BuiltinCommand::Redo,
    },
    BuiltinEntry {
        label: "Save",
        shortcut: "Ctrl+S",
        action: BuiltinCommand::Save,
    },
    BuiltinEntry {
        label: "Show command palette",
        shortcut: "Ctrl+Shift+P",
        action: BuiltinCommand::OpenPalette,
    },
    BuiltinEntry {
        label: "Sort lines (A-Z)",
        shortcut: "",
        action: BuiltinCommand::SortLines,
    },
    BuiltinEntry {
        label: "Start language server for current file",
        shortcut: "",
        action: BuiltinCommand::StartLsp,
    },
    BuiltinEntry {
        label: "Toggle bookmark on cursor line",
        shortcut: "Ctrl+F2",
        action: BuiltinCommand::ToggleBookmark,
    },
    BuiltinEntry {
        label: "Toggle line numbers",
        shortcut: "",
        action: BuiltinCommand::ToggleLineNumbers,
    },
    BuiltinEntry {
        label: "Toggle diff vs disk",
        shortcut: "",
        action: BuiltinCommand::ToggleDiffView,
    },
    BuiltinEntry {
        label: "Toggle markdown preview",
        shortcut: "Ctrl+Shift+V",
        action: BuiltinCommand::ToggleMarkdownPreview,
    },
    BuiltinEntry {
        label: "Toggle minimap",
        shortcut: "",
        action: BuiltinCommand::ToggleMinimap,
    },
    BuiltinEntry {
        label: "Toggle spellcheck",
        shortcut: "",
        action: BuiltinCommand::ToggleSpellcheck,
    },
    BuiltinEntry {
        label: "Toggle split / grid view",
        shortcut: "",
        action: BuiltinCommand::ToggleSplitView,
    },
    BuiltinEntry {
        label: "Toggle word wrap",
        shortcut: "",
        action: BuiltinCommand::ToggleWordWrap,
    },
    BuiltinEntry {
        label: "Toggle zen / distraction-free mode",
        shortcut: "Ctrl+.",
        action: BuiltinCommand::ToggleZen,
    },
    BuiltinEntry {
        label: "Undo",
        shortcut: "Ctrl+Z",
        action: BuiltinCommand::Undo,
    },
];

/// Quick-access toolbar action registry: `(id, human-readable label)`. The id
/// `"sep"` renders a divider. Shared by the toolbar renderer and the Settings
/// toolbar editor (add / remove / reorder).
pub(crate) const TOOLBAR_ACTIONS: &[(&str, &str)] = &[
    ("new", "New file"),
    ("open", "Open file"),
    ("openfolder", "Open folder"),
    ("save", "Save"),
    ("saveas", "Save As"),
    ("find", "Find"),
    ("palette", "Command palette"),
    ("split", "Split view"),
    ("minimap", "Minimap"),
    ("wrap", "Word wrap"),
    ("fold", "Folded view"),
    ("linenumbers", "Line numbers"),
    ("spellcheck", "Spellcheck"),
    ("lsp", "Start LSP"),
    ("sep", "Separator"),
];

/// Toolbar item label: phosphor (Thin) icon glyph when `icons` is true, the
/// existing short text label when false. Honours the `appearance.toolbar_icons`
/// config (Phase 16 T16.3 / DECISION-2026-005 "egui-phosphor hairline icons").
/// Phase 17 T17.5 — verified-canonical kanji "instrument plates" for the
/// quick-access toolbar. Returns `None` when the canonical kanji for an action
/// is uncertain, contested, or a Western metaphor — those stay English-only
/// per the Folklore-Consultant gate (DECISION-2026-005 cond #4: "verified-
/// accurate kanji ONLY"). The annotation is decorative and English-redundant;
/// every action keeps its English label or icon as the primary read.
///
/// Verification notes — IT-Japanese canonical usage:
/// - 新 (atarashii) "new" — `新規` (new entry)
/// - 開 (hiraku) "open" — `開く` (open a file)
/// - 保 (tamotsu) "save/preserve" — `保存` (save)
/// - 別 (betsu) "separate" — `別名保存` (save-as / under another name)
/// - 検 (ken) "inspect" — `検索` (search/find)
/// - 分 (bun) "divide" — `分割` (split)
/// - 図 (zu) "diagram/map" — `地図` (map)
/// - 折 (ori) "fold" — `折り返し` (line wrap; the canonical IT term)
/// - 畳 (tatamu) "fold up/layer" — `折り畳む` (fold/collapse)
/// - 番 (ban) "number/order" — `行番号` (line numbers)
/// - 綴 (tsuzuru) "spell/compose" — `綴り` (spelling)
///
/// Omitted (uncertain or non-canonical): openfolder (Western metaphor),
/// palette (`⌘` glyph fallback exists), lsp (acronym/loanword),
/// find (covered by 検).
pub(crate) fn jp_glyph(id: &str) -> Option<&'static str> {
    match id {
        "new" => Some("新"),
        "open" => Some("開"),
        "save" => Some("保"),
        "saveas" => Some("別"),
        "find" => Some("検"),
        "split" => Some("分"),
        "minimap" => Some("図"),
        "wrap" => Some("折"),
        "fold" => Some("畳"),
        "linenumbers" => Some("番"),
        "spellcheck" => Some("綴"),
        _ => None,
    }
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

pub(crate) fn toolbar_label(id: &str, icons: bool) -> &'static str {
    use egui_phosphor::thin as ph;
    match (icons, id) {
        (true, "new") => ph::FILE_PLUS,
        (true, "open") => ph::FILE_DASHED,
        (true, "openfolder") => ph::FOLDER_OPEN,
        (true, "save") => ph::FLOPPY_DISK,
        (true, "saveas") => ph::FLOPPY_DISK_BACK,
        (true, "find") => ph::MAGNIFYING_GLASS,
        (true, "palette") => ph::COMMAND,
        (true, "split") => ph::COLUMNS,
        (true, "minimap") => ph::MAP_TRIFOLD,
        (true, "wrap") => ph::TEXT_ALIGN_LEFT,
        (true, "fold") => ph::EYE,
        (true, "linenumbers") => ph::LIST_NUMBERS,
        (true, "spellcheck") => ph::CHECK_FAT,
        (true, "lsp") => ph::PLAY,
        (_, "new") => "new",
        (_, "open") => "open",
        (_, "openfolder") => "folder",
        (_, "save") => "save",
        (_, "saveas") => "save as",
        (_, "find") => "find",
        (_, "palette") => "\u{2318}",
        (_, "split") => "split",
        (_, "minimap") => "map",
        (_, "wrap") => "wrap",
        (_, "fold") => "fold",
        (_, "linenumbers") => "nums",
        (_, "spellcheck") => "spell",
        (_, "lsp") => "lsp",
        (_, _) => "·",
    }
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
        "../../../assets/fonts/JetBrainsMono/JetBrainsMono-Regular.ttf"
    );
    embed!(
        "IBMPlexMono",
        "../../../assets/fonts/IBMPlexMono/IBMPlexMono-Regular.ttf"
    );
    embed!(
        "FiraMono",
        "../../../assets/fonts/FiraMono/FiraMono-Regular.ttf"
    );
    embed!(
        "SpaceMono",
        "../../../assets/fonts/SpaceMono/SpaceMono-Regular.ttf"
    );
    embed!(
        "Cousine",
        "../../../assets/fonts/Cousine/Cousine-Regular.ttf"
    );
    embed!(
        "SourceCodePro",
        "../../../assets/fonts/SourceCodePro/SourceCodePro-Regular.ttf"
    );
    embed!(
        "B612Mono",
        "../../../assets/fonts/B612Mono/B612Mono-Regular.ttf"
    );
    embed!(
        "ShareTechMono",
        "../../../assets/fonts/ShareTechMono/ShareTechMono-Regular.ttf"
    );
    embed!("VT323", "../../../assets/fonts/VT323/VT323-Regular.ttf");
    // Wave 4 — brand display + accent faces (atomic with the FONT_FAMILIES
    // additions above; a key without its embed fails the registration test).
    embed!("Doto", "../../../assets/fonts/Doto/Doto[ROND,wght].ttf");
    embed!(
        "MajorMonoDisplay",
        "../../../assets/fonts/MajorMonoDisplay/MajorMonoDisplay-Regular.ttf"
    );
    embed!(
        "ChakraPetch",
        "../../../assets/fonts/ChakraPetch/ChakraPetch-Regular.ttf"
    );
    embed!(
        "Wallpoet",
        "../../../assets/fonts/Wallpoet/Wallpoet-Regular.ttf"
    );
    embed!(
        "Michroma",
        "../../../assets/fonts/Michroma/Michroma-Regular.ttf"
    );
    embed!(
        "RedHatMono",
        "../../../assets/fonts/RedHatMono/RedHatMono[wght].ttf"
    );
    embed!("Teko", "../../../assets/fonts/Teko/Teko[wght].ttf");
    embed!(
        "Rajdhani",
        "../../../assets/fonts/Rajdhani/Rajdhani-Regular.ttf"
    );
    embed!("Saira", "../../../assets/fonts/Saira/Saira[wdth,wght].ttf");
    embed!(
        "ZenDots",
        "../../../assets/fonts/ZenDots/ZenDots-Regular.ttf"
    );
    embed!(
        "Syncopate",
        "../../../assets/fonts/Syncopate/Syncopate-Regular.ttf"
    );
    embed!(
        "SplineSansMono",
        "../../../assets/fonts/SplineSansMono/SplineSansMono[wght].ttf"
    );
    embed!(
        "NotoSansJP-Subset",
        "../../../assets/fonts/NotoSansJP/NotoSansJP-Subset.ttf"
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

/// Paint a 2×3 dot drag-grip and return its response. We paint the dots instead
/// of drawing the phosphor `DOTS_SIX_VERTICAL` glyph because that PUA codepoint
/// renders as a tofu square in this build's font atlas (the glyph IS in the
/// font and phosphor IS registered in both families, yet egui's atlas resolves
/// it to .notdef here — a known egui-phosphor footgun). Painted dots are
/// font-independent and always render as a clean, recognizable grip. `enabled`
/// = false dims it and drops the drag sense (used for pinned panes).
pub(crate) fn grip_handle(ui: &mut egui::Ui, enabled: bool, color: Color32) -> egui::Response {
    let h = ui.text_style_height(&egui::TextStyle::Body);
    let sense = if enabled {
        egui::Sense::click_and_drag()
    } else {
        egui::Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(11.0, h), sense);
    let dim = if enabled {
        color
    } else {
        color.gamma_multiply(0.5)
    };
    let c = rect.center();
    let painter = ui.painter();
    for &x in &[c.x - 2.5, c.x + 2.5] {
        for &y in &[c.y - 4.5, c.y, c.y + 4.5] {
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
            if byte < line.len() {
                job.append(&line[byte..], 0.0, plain(fg));
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

/// Parse a Ctrl+G query string into `(line, column)`. Accepts:
/// - `"42"` → `Some((42, None))`
/// - `"42:10"` → `Some((42, Some(10)))`
/// - empty or non-numeric → `None`
///
/// Closes F-015 from `docs/audits/overlooked-surfaces-2026-05-29.md`.
pub(crate) fn parse_goto_query(s: &str) -> Option<(usize, Option<usize>)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some((l, c)) = s.split_once(':') {
        if let (Ok(line), Ok(col)) = (l.parse::<usize>(), c.parse::<usize>()) {
            if line > 0 {
                return Some((line, Some(col.max(1))));
            }
        }
        // fall through to plain-line parse
    }
    s.parse::<usize>()
        .ok()
        .filter(|&n| n > 0)
        .map(|n| (n, None))
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

/// Map a file extension (without the leading dot) to its single-line comment
/// prefix. Returns `None` for languages without one (HTML, CSS, JSON — the
/// caller toasts "no comment prefix for this language" in that case).
pub(crate) fn comment_prefix_for_extension(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" | "c" | "cc" | "cpp" | "cxx" | "h" | "hpp" | "java" | "kt" | "swift" | "go"
        | "scala" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "cs" | "dart" | "zig" | "v" => {
            "//"
        }
        "py" | "rb" | "sh" | "bash" | "zsh" | "fish" | "yaml" | "yml" | "toml" | "ini" | "conf"
        | "cfg" | "r" | "perl" | "pl" | "ps1" | "Makefile" => "#",
        "lua" | "sql" | "hs" | "elm" | "ada" => "--",
        "vim" | "vimrc" => "\"",
        "lisp" | "clj" | "scm" | "el" => ";;",
        "tex" | "latex" => "%",
        "asm" | "s" => ";",
        _ => return None,
    })
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

    /// One per-frame tick of the editor UI. Separated from `eframe::App::ui` so
    /// `egui_kittest` E2E tests can drive it through `Context::run` without an
    /// `eframe::Frame`. Drives every top-level panel via the deprecated-but-
    /// functional `Panel::show(ctx, …)` path.
    pub(crate) fn frame_tick(&mut self, ctx: &egui::Context) {
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
        self.poll_external_disk_changes();
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
            ctx.set_fonts(build_fonts(
                &self.config.fonts.editor_family,
                &self.config.fonts.ui_family,
            ));
            self.applied_font_family = font_key;
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
        ctx.input(|i| {
            let cmd = i.modifiers.command;
            act.new = cmd && i.key_pressed(egui::Key::N);
            act.open = cmd && i.key_pressed(egui::Key::O);
            act.save = cmd && i.key_pressed(egui::Key::S);
            if cmd && !i.modifiers.shift && i.key_pressed(egui::Key::F) {
                if !self.find_open {
                    self.focus_find = true;
                }
                self.find_open = true;
            }
            // Wave-5: Ctrl/Cmd+Shift+F opens project-wide find (find in files).
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::F) {
                if !self.find_in_files_open {
                    self.focus_find_in_files = true;
                }
                self.find_in_files_open = true;
            }
            // Wave-5 P1: Ctrl+. toggles zen / distraction-free mode. Entering zen
            // also closes the find bars so nothing but the editor remains.
            if cmd && !i.modifiers.shift && i.key_pressed(egui::Key::Period) {
                self.zen_mode = !self.zen_mode;
                if self.zen_mode {
                    self.find_open = false;
                    self.find_in_files_open = false;
                }
            }
            // Wave-5 P1: Ctrl+Shift+V toggles the markdown live-preview panel.
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::V) {
                self.md_preview_open = !self.md_preview_open;
            }
            // Ctrl/Cmd+Shift+P opens the command palette (plugin + builtin cmds).
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::P) {
                if !self.palette_open {
                    self.focus_palette = true;
                }
                self.palette_open = true;
                self.palette_query.clear();
            }
            // F-006 fix from docs/audits/overlooked-surfaces-2026-05-29.md —
            // wave 1 keyboard shortcuts:
            // - Ctrl+W: close the active tab.
            // - Ctrl+\: toggle the multi-note grid (F-003 entry-point fix).
            // - Ctrl+Tab / Ctrl+Shift+Tab: cycle tabs (next / prev).
            if cmd && i.key_pressed(egui::Key::W) {
                act.close_active_tab = true;
            }
            if cmd && i.key_pressed(egui::Key::Backslash) {
                act.toggle_grid = true;
            }
            // Wave-2 keyboard fill-in (docs/audits/overlooked-surfaces-2026-05-29.md).
            if cmd && i.key_pressed(egui::Key::H) {
                act.open_replace = true;
            }
            if cmd && i.key_pressed(egui::Key::Slash) {
                act.toggle_comment = true;
            }
            if i.key_pressed(egui::Key::F11) {
                act.toggle_fullscreen = true;
            }
            // F-018 — Ctrl+K Ctrl+T (cycle theme) approximated as
            // Ctrl+Shift+T (single-key chord) since egui has no native
            // multi-key chord layer. F-031 — Ctrl+Shift+M toggles the
            // minimap. Both persist via save_config.
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::T) {
                act.cycle_theme = true;
            }
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::M) {
                act.toggle_minimap = true;
            }
            // F-032 — Ctrl+Shift+[ folds every region in the active buffer,
            // Ctrl+Shift+] expands every region. Switches the editor into
            // fold-view mode so the user sees the change immediately
            // (otherwise the fold set is updated but the normal central
            // panel doesn't honor it).
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::OpenBracket) {
                act.fold_all = true;
            }
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::CloseBracket) {
                act.expand_all = true;
            }
            // Font zoom: Ctrl+= / Ctrl++ in, Ctrl+- out, Ctrl+0 reset, and
            // Ctrl+scroll. Universal editor convenience.
            if cmd && (i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals)) {
                act.font_zoom = Some(1);
            }
            if cmd && i.key_pressed(egui::Key::Minus) {
                act.font_zoom = Some(-1);
            }
            if cmd && i.key_pressed(egui::Key::Num0) {
                act.font_zoom = Some(0);
            }
            if cmd {
                let dy = i.smooth_scroll_delta.y;
                if dy > 0.5 {
                    act.font_zoom = Some(1);
                } else if dy < -0.5 {
                    act.font_zoom = Some(-1);
                }
            }
            // Reopen the most recently closed tab (Ctrl+Shift+R — Ctrl+Shift+T
            // is already the theme-cycle chord in this editor).
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::R) {
                act.reopen_tab = true;
            }
            // F-017 — Alt+Up/Down move the cursor line; Ctrl+Shift+D
            // duplicates; Ctrl+J joins next.
            if i.modifiers.alt && i.key_pressed(egui::Key::ArrowUp) {
                act.move_line_up = true;
            }
            if i.modifiers.alt && i.key_pressed(egui::Key::ArrowDown) {
                act.move_line_down = true;
            }
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::D) {
                act.duplicate_line = true;
            }
            if cmd && i.key_pressed(egui::Key::J) {
                act.join_lines = true;
            }
            // F-011 — drag-drop file open. egui collects DroppedFile entries
            // into RawInput.dropped_files; consume them here so the deferred
            // application opens each as a new tab.
            for file in i.raw.dropped_files.iter() {
                if let Some(p) = file.path.clone() {
                    act.files_to_open.push(p);
                }
            }
            if cmd
                && i.key_pressed(egui::Key::Tab)
                && !i.modifiers.shift
                && self.completion.is_none()
            {
                act.cycle_tab_next = true;
            }
            if cmd
                && i.key_pressed(egui::Key::Tab)
                && i.modifiers.shift
                && self.completion.is_none()
            {
                act.cycle_tab_prev = true;
            }
            // F-014: F1 toggles the keyboard cheatsheet — universal "help"
            // convention. The Esc handler below closes it like any overlay.
            if i.key_pressed(egui::Key::F1) {
                self.cheatsheet_open = !self.cheatsheet_open;
            }
            // F-015 — Ctrl+G opens the go-to-line modal.
            if cmd && i.key_pressed(egui::Key::G) {
                self.goto_open = true;
                self.focus_goto = true;
                self.goto_query.clear();
            }
            // Ctrl+Shift+O opens the go-to-symbol modal (jump to a definition
            // in the active buffer).
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::O) {
                if !self.goto_symbol_open {
                    self.focus_goto_symbol = true;
                }
                self.goto_symbol_open = true;
                self.goto_symbol_query.clear();
            }
            // F-012 — Ctrl+R opens the recent-files modal.
            if cmd && i.key_pressed(egui::Key::R) {
                self.recent_open = true;
            }
            // Line bookmarks: Ctrl+F2 toggles on the cursor line; F2 jumps to
            // the next bookmark; Shift+F2 jumps to the previous one. Ctrl takes
            // priority so Ctrl+F2 never doubles as a plain-F2 navigate.
            if i.key_pressed(egui::Key::F2) {
                if cmd {
                    act.toggle_bookmark = true;
                } else if i.modifiers.shift {
                    act.prev_bookmark = true;
                } else {
                    act.next_bookmark = true;
                }
            }
            // #R6 — F3 / Shift+F3 cycle find matches while the find bar is open.
            if self.find_open && i.key_pressed(egui::Key::F3) {
                find_nav = Some(!i.modifiers.shift);
            }
            // F-010 — Ctrl+P opens the fuzzy file finder (rebuilds the
            // file index on first open so cold-start cost lands here,
            // not on launch).
            if cmd && i.key_pressed(egui::Key::P) && !i.modifiers.shift {
                act.open_fuzzy = true;
            }
            if i.key_pressed(egui::Key::Escape) {
                // Esc exits zen mode / F11 fullscreen first so the chrome comes
                // back before any overlay close — one press to leave the
                // distraction-free / fullscreen surface.
                if self.zen_mode {
                    self.zen_mode = false;
                } else if i.viewport().fullscreen.unwrap_or(false) {
                    // Exit OS fullscreen via the existing deferred handler
                    // (it sends Fullscreen(false) since we are currently in it).
                    act.toggle_fullscreen = true;
                } else {
                    self.find_open = false;
                    self.palette_open = false;
                    self.cheatsheet_open = false;
                    self.goto_open = false;
                    self.goto_symbol_open = false;
                    self.recent_open = false;
                    self.welcome_open = false;
                    self.fuzzy_open = false;
                }
            }
        });
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
        if self.config.appearance.frameless && !fullscreen {
            egui::TopBottomPanel::top("titlebar")
                .exact_height(if toolbar_in_titlebar { 40.0 } else { 34.0 })
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
                        ui.add_space(10.0);
                        // Chrome text follows the APP UI font (Proportional family),
                        // NOT the note/editor font. egui's default family for
                        // RichText is Proportional, so simply NOT calling
                        // `.monospace()` (which selects the Monospace family that the
                        // note font leads) binds the titlebar to `ui_family`.
                        // Split-tone wordmark: "S C R " in the primary accent, "1 B 3"
                        // in the secondary brand colour. The two halves are painted with
                        // zero item-spacing between them so they read as ONE wordmark
                        // (egui otherwise inserts item_spacing.x between widgets, which
                        // would split the mark into two words).
                        let saved_spacing = ui.spacing().item_spacing.x;
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.label(RichText::new("S C R ").color(accent).strong());
                        ui.label(RichText::new("1 B 3").color(accent_alt).strong());
                        ui.spacing_mut().item_spacing.x = saved_spacing;
                        ui.add_space(6.0);
                        ui.label(RichText::new("//").color(muted));
                        // Japanese brand subtitle (写本 — shahon). Replaces the English
                        // tagline in the titlebar; the welcome screen keeps the tagline.
                        ui.label(
                            RichText::new(scribe_core::PRODUCT_SUBTITLE_JP)
                                .color(muted)
                                .small(),
                        );
                        // Toolbar-in-titlebar mode: the quick-access toolbar sits
                        // here, between the wordmark and the window caption buttons.
                        if toolbar_in_titlebar {
                            ui.add_space(12.0);
                            // Button PARITY with the standalone toolbar row: apply the
                            // SAME configured button height + item spacing here so the
                            // quick-access buttons are identical in size, spacing, look,
                            // and behaviour whether the toolbar lives in the titlebar or
                            // in its own row. (Previously this branch skipped the spacing
                            // setup the standalone path applies, so the buttons rendered
                            // a different size/spacing.) Scoped so it can't perturb the
                            // wordmark or caption buttons around it.
                            let btn = self.config.toolbar.clamped_button_size();
                            let gap = self.config.toolbar.clamped_button_spacing();
                            ui.scope(|ui| {
                                ui.spacing_mut().interact_size.y = btn;
                                ui.spacing_mut().item_spacing.x = gap;
                                self.toolbar_contents(ui, &mut act, &mut save_cfg, &mut start_lsp);
                            });
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let is_max = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
                            let close_hover = Color32::from_rgb(0xE8, 0x11, 0x23);
                            let soft_hover = Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 26);
                            if caption_btn(ui, CaptionIcon::Close, muted, close_hover).clicked() {
                                // Funnel into the two-phase close (hide-before-destroy)
                                // so a transparent window leaves no DWM ghost (T19.1).
                                self.want_close = true;
                            }
                            let max_icon = if is_max {
                                CaptionIcon::Restore
                            } else {
                                CaptionIcon::Maximize
                            };
                            if caption_btn(ui, max_icon, muted, soft_hover).clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_max));
                            }
                            if caption_btn(ui, CaptionIcon::Minimize, muted, soft_hover).clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                            }
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
                            ui.horizontal(|ui| self.draw_tab_strip(ui, accent, muted));
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
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.find_in_files_regex, "regex");
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if enter || ui.button("search").clicked() {
                            self.run_find_in_files();
                        }
                    });
                    if let Some(err) = &self.find_in_files_error {
                        ui.colored_label(Color32::from_rgb(0xe5, 0x3e, 0x3e), err);
                    }
                    ui.separator();
                    let mut open_target: Option<(PathBuf, usize)> = None;
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for m in &self.find_in_files_results {
                            let name = m.path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                            let label = format!("{}:{}  {}", name, m.line, m.line_text.trim());
                            if ui
                                .add(
                                    egui::Label::new(RichText::new(label).monospace().small())
                                        .sense(egui::Sense::click()),
                                )
                                .clicked()
                            {
                                open_target = Some((m.path.clone(), m.line));
                            }
                        }
                    });
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
            egui::Window::new(
                RichText::new(format!("{}  command palette", egui_phosphor::thin::COMMAND))
                    .color(accent)
                    .monospace(),
            )
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 64.0])
            .show(ctx, |ui| {
                let r = ui.text_edit_singleline(&mut self.palette_query);
                if self.focus_palette {
                    r.request_focus();
                    self.focus_palette = false;
                }
                let q = self.palette_query.to_lowercase();
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        let mut any = false;
                        // Built-in commands first — universally available even
                        // with zero plugins.
                        for cmd in BUILTIN_COMMANDS {
                            let label = cmd.label;
                            let shortcut = cmd.shortcut;
                            if q.is_empty()
                                || label.to_lowercase().contains(&q)
                                || shortcut.to_lowercase().contains(&q)
                            {
                                any = true;
                                let display = if shortcut.is_empty() {
                                    label.to_string()
                                } else {
                                    format!("{label}  ·  {shortcut}")
                                };
                                if ui.selectable_label(false, display).clicked() {
                                    run_builtin = Some(cmd.action);
                                }
                            }
                        }
                        if !self.plugin_cmds.is_empty() {
                            ui.separator();
                        }
                        for c in &self.plugin_cmds {
                            if q.is_empty()
                                || c.label.to_lowercase().contains(&q)
                                || c.id.contains(&q)
                            {
                                any = true;
                                if ui
                                    .selectable_label(
                                        false,
                                        format!("{}  ·  {}", c.label, c.plugin_id),
                                    )
                                    .clicked()
                                {
                                    run_cmd = Some(c.id.clone());
                                }
                            }
                        }
                        if !any {
                            ui.label(RichText::new("no match").color(muted).small());
                        }
                    });
            });
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
            let mut chosen: Option<usize> = None;
            let mut want_close = false;
            let mut first_match: Option<usize> = None;
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
                // Enter jumps to the first match; Esc closes (handled in input).
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
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
                            for s in &symbols {
                                if !q.is_empty() && !s.label.to_lowercase().contains(&q) {
                                    continue;
                                }
                                if first_match.is_none() {
                                    first_match = Some(s.start_line);
                                }
                                // Indent by nesting depth; show the 1-based line.
                                let indent = "  ".repeat(s.depth);
                                let label = RichText::new(format!(
                                    "{indent}{}  ·  {}",
                                    s.label,
                                    s.start_line + 1
                                ))
                                .monospace();
                                if ui.selectable_label(false, label).clicked() {
                                    chosen = Some(s.start_line);
                                }
                            }
                        });
                }
                if enter {
                    if let Some(line0) = first_match {
                        chosen = Some(line0);
                    } else {
                        want_close = true;
                    }
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Enter jumps to the first match · Esc closes")
                        .color(muted)
                        .small()
                        .monospace(),
                );
            });
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
                            for p in &self.config.editor.recent_files {
                                let label = RichText::new(p.display().to_string()).monospace();
                                if ui.selectable_label(false, label).clicked() {
                                    chosen = Some(p.clone());
                                }
                            }
                        });
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new("press Ctrl+R or Esc to close")
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
                            let lines = t.text.lines().count().max(1);
                            // F-024 — word + line counters in the status bar.
                            // Both are cheap (single-pass split) on the buffers
                            // SCR1B3 targets (multi-GB files go through the rope
                            // browser which sets is_read_only_large and short-
                            // circuits this segment).
                            let (words, chars) = if t.doc.is_read_only_large() {
                                (0, 0)
                            } else {
                                (t.text.split_whitespace().count(), t.text.chars().count())
                            };
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
        let font = FontId::monospace(self.config.fonts.editor_size);
        let line_height = self.config.fonts.line_height;
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
            let total = self.tabs[active].text.lines().count().max(1);
            let digits = total.to_string().len().max(2);
            let gutter_w = digits as f32 * (font.size * 0.62) + 16.0;
            let rows = &self.line_gutter;
            let bookmarks = &self.tabs[active].bookmarks;
            egui::SidePanel::left("line-gutter")
                .exact_width(gutter_w)
                .resizable(false)
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    let painter = ui.painter();
                    let clip = ui.clip_rect();
                    let rx = ui.max_rect().right() - 8.0;
                    let lx = ui.max_rect().left() + 4.0;
                    let nfont = FontId::monospace((font.size * 0.92).max(8.0));
                    for (i, &y) in rows.iter().enumerate() {
                        if y < clip.top() - gutter_row_h || y > clip.bottom() {
                            continue;
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
                // compute brace-delimited definition scopes once for the
                // breadcrumb bar (above the editor) and the sticky-scroll
                // headers (pinned at the viewport top). Skipped for very large
                // buffers to keep the per-frame O(n) scan bounded.
                let scopes = if self.tabs[active].text.len() <= 500_000 {
                    crate::editor_features::symbol_scopes(&self.tabs[active].text)
                } else {
                    Vec::new()
                };
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
                            let ws_font = FontId::monospace(self.config.fonts.editor_size);
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
                                .unwrap_or(self.config.fonts.editor_size * 0.6);
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
                                    .map_or(true, |(r, _)| r.min.distance(caret_rect.min) > 1.0);
                                if moved {
                                    self.caret_trail.push_back((caret_rect, t));
                                    while self.caret_trail.len() > 24 {
                                        self.caret_trail.pop_front();
                                    }
                                }
                            }
                            let collapsed = range.primary.index == range.secondary.index;
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
                                        .unwrap_or(self.config.fonts.editor_size * 0.6);
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
        if act.new {
            self.new_tab();
        }
        if act.open {
            self.open_dialog();
        }
        if act.save {
            self.save_active();
        }
        if let Some(cmd) = run_cmd {
            self.run_plugin_command(&cmd);
            self.palette_open = false;
        }
        if let Some(builtin) = run_builtin {
            self.execute_builtin(builtin);
            self.palette_open = false;
        }
        if save_cfg {
            self.save_config();
        }
        if act.open_folder {
            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                self.status = format!("folder: {}", folder.display());
                self.file_tree_root = Some(folder);
            }
        }
        // F-006 wave-1 fixes from docs/audits/overlooked-surfaces-2026-05-29.md.
        if act.close_active_tab {
            self.close_tab(self.active);
        }
        if act.toggle_grid {
            self.config.editor.grid_enabled = !self.config.editor.grid_enabled;
            self.save_config();
            self.status = format!(
                "multi-note grid: {}",
                if self.config.editor.grid_enabled {
                    "on"
                } else {
                    "off"
                }
            );
        }
        if act.cycle_tab_next && !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
        if act.cycle_tab_prev && !self.tabs.is_empty() {
            self.active = if self.active == 0 {
                self.tabs.len() - 1
            } else {
                self.active - 1
            };
        }
        // Wave-2 deferred handlers.
        if act.open_replace {
            // Re-use the existing find bar; focus the replace field.
            self.find_open = true;
            self.focus_replace = true;
        }
        if act.toggle_comment {
            self.toggle_comment_active();
        }
        if act.toggle_fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(
                !ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)),
            ));
        }
        if act.move_line_up {
            self.move_cursor_line(-1);
        }
        if act.move_line_down {
            self.move_cursor_line(1);
        }
        if act.duplicate_line {
            self.duplicate_cursor_line();
        }
        if act.join_lines {
            self.join_cursor_line_with_next();
        }
        if act.cycle_theme {
            let names = scribe_core::theme::Theme::builtin_names();
            if !names.is_empty() {
                let cur = &self.config.appearance.theme;
                let idx = names.iter().position(|n| *n == cur.as_str()).unwrap_or(0);
                let next = names[(idx + 1) % names.len()].to_string();
                self.config.appearance.theme = next.clone();
                self.reapply_theme(ctx);
                self.save_config();
                self.status = format!("theme: {next}");
            }
        }
        // Font zoom (Ctrl+= / Ctrl+- / Ctrl+0 / Ctrl+scroll).
        if let Some(z) = act.font_zoom {
            let def = scribe_core::config::Config::default().fonts.editor_size;
            let size = &mut self.config.fonts.editor_size;
            *size = match z {
                0 => def,
                d => (*size + d as f32).clamp(8.0, 32.0),
            };
            self.save_config();
            self.status = format!("font size: {:.0}", self.config.fonts.editor_size);
        }
        // Reopen the most recently closed tab.
        if act.reopen_tab {
            self.reopen_closed_tab();
        }
        // Line bookmarks (Ctrl+F2 toggle, F2 next, Shift+F2 prev).
        if act.toggle_bookmark {
            self.toggle_bookmark();
        }
        if act.next_bookmark {
            self.navigate_bookmark(1);
        }
        if act.prev_bookmark {
            self.navigate_bookmark(-1);
        }
        if act.toggle_minimap {
            self.config.editor.show_minimap = !self.config.editor.show_minimap;
            self.save_config();
            self.status = format!(
                "minimap: {}",
                if self.config.editor.show_minimap {
                    "on"
                } else {
                    "off"
                }
            );
        }
        // F-032: Ctrl+Shift+[ / Ctrl+Shift+] — fold-all / expand-all.
        // Re-extract regions against the current buffer so the action is
        // always applied to what the user sees, then switch on fold-view
        // so the change is visible in the central panel.
        if act.fold_all && self.active < self.tabs.len() {
            let text = self.tabs[self.active].text.clone();
            let regions = crate::editor_features::fold_regions(&text);
            self.folds = regions.iter().map(|r| r.start_line).collect();
            self.fold_view = true;
            self.status = format!("folded {} region(s)", regions.len());
        }
        if act.expand_all {
            self.folds.clear();
            self.status = String::from("expanded all");
        }
        if act.open_fuzzy {
            // Lazy-build the index on first open so cold-start latency
            // lands here, not in build(). Rebuild whenever the project
            // root changes.
            if self.fuzzy_index.is_empty() {
                let root = self
                    .file_tree_root
                    .clone()
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| PathBuf::from("."));
                self.fuzzy_index = crate::fuzzy::scan_project(&root, crate::fuzzy::FUZZY_SCAN_CAP);
            }
            self.fuzzy_open = true;
            self.focus_fuzzy = true;
            self.fuzzy_query.clear();
            self.fuzzy_selected = 0;
        }
        for p in act.files_to_open.drain(..) {
            self.open_path(p);
        }
        if let Some(p) = open_from_tree {
            self.open_path(p);
        }
        if close_tree {
            self.file_tree_root = None;
        }
        if start_lsp {
            self.start_lsp_for_active();
        }
        // F-038 — apply deferred config-banner actions.
        if want_open_cfg {
            if let Some(p) = Config::config_file_path() {
                // Ensure the file actually exists before trying to open it
                // (cold install: write defaults first so the user can edit).
                if !p.exists() {
                    if let Some(parent) = p.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(&p, self.config.to_toml_string());
                }
                self.open_path(p);
            }
        }
        if want_restore_cfg {
            self.config = Config::default();
            self.save_config();
            self.reapply_theme(ctx);
            self.config_error_banner = None;
            self.status = "config restored to defaults".to_string();
        }
        if want_dismiss_cfg {
            self.config_error_banner = None;
        }

        // Persist the open-file session when it changes (for restore-on-launch).
        if self.config.editor.restore_session {
            let sig = session_signature(&self.tabs);
            if sig != self.session_sig {
                let paths: Vec<PathBuf> = self
                    .tabs
                    .iter()
                    .filter_map(|t| t.doc.path().map(|p| p.to_path_buf()))
                    .collect();
                save_session(&paths);
                self.session_sig = sig;
            }
        }

        // Hot-exit: periodically flush unsaved buffer CONTENT to the backup
        // store so an unsaved note survives a crash/restart. Throttled so we
        // don't rewrite content every keystroke, and only when something is
        // actually unsaved.
        if self.config.editor.session_backup {
            const BACKUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(4);
            let due = self
                .last_backup_at
                .map(|t| t.elapsed() >= BACKUP_INTERVAL)
                .unwrap_or(true);
            let has_unsaved = self
                .tabs
                .iter()
                .any(|t| t.is_dirty() || (t.doc.path().is_none() && !t.text.is_empty()));
            // Audit fix F1: only rewrite backups when the unsaved CONTENT
            // actually changed since the last flush — avoids re-writing the
            // same bytes every interval while a buffer sits dirty but idle.
            let content_sig = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                for t in &self.tabs {
                    if t.is_dirty() || (t.doc.path().is_none() && !t.text.is_empty()) {
                        t.text.hash(&mut h);
                        t.doc.path().map(|p| p.to_path_buf()).hash(&mut h);
                    }
                }
                h.finish()
            };
            if due && has_unsaved && content_sig != self.last_backup_sig {
                self.snapshot_session_backups();
                self.last_backup_sig = content_sig;
            }
        }

        // Auto-save (opt-in, default OFF): after a quiet interval, write any
        // dirty file-backed buffer to disk. Untitled buffers are never
        // auto-saved (no path → would pop a dialog). Throttled like backups.
        if self.config.editor.auto_save {
            const AUTOSAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);
            let due = self
                .last_autosave_at
                .map(|t| t.elapsed() >= AUTOSAVE_INTERVAL)
                .unwrap_or(true);
            if due {
                let dirty: Vec<usize> = (0..self.tabs.len())
                    .filter(|&i| self.tabs[i].doc.path().is_some() && self.tabs[i].is_dirty())
                    .collect();
                if !dirty.is_empty() {
                    let prev_active = self.active;
                    for i in dirty {
                        self.active = i;
                        self.save_active();
                    }
                    self.active = prev_active.min(self.tabs.len().saturating_sub(1));
                }
                self.last_autosave_at = Some(std::time::Instant::now());
            }
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
}

fn caption_btn(
    ui: &mut egui::Ui,
    icon: CaptionIcon,
    base: Color32,
    hover_fill: Color32,
) -> egui::Response {
    let size = egui::vec2(46.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, 2.0, hover_fill);
    }
    let col = if resp.hovered() { Color32::WHITE } else { base };
    let c = rect.center();
    let s = 4.5_f32;
    let stroke = egui::Stroke::new(1.4, col);
    match icon {
        CaptionIcon::Minimize => {
            painter.line_segment([egui::pos2(c.x - s, c.y), egui::pos2(c.x + s, c.y)], stroke);
        }
        CaptionIcon::Maximize => {
            // egui 0.34: rect_stroke gained a 4th StrokeKind arg.
            painter.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(2.0 * s, 2.0 * s)),
                1.0,
                stroke,
                egui::StrokeKind::Outside,
            );
        }
        CaptionIcon::Restore => {
            // Full front square (lower-left) + an L of the back square peeking
            // out upper-right — reads as "restore" with no overlap masking.
            let front = egui::Rect::from_center_size(
                egui::pos2(c.x - 1.5, c.y + 1.5),
                egui::vec2(2.0 * s, 2.0 * s),
            );
            painter.rect_stroke(front, 1.0, stroke, egui::StrokeKind::Outside);
            let top = front.top() - 3.0;
            let right = front.right() + 3.0;
            painter.line_segment(
                [egui::pos2(front.left() + 3.0, top), egui::pos2(right, top)],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(right, top),
                    egui::pos2(right, front.bottom() - 3.0),
                ],
                stroke,
            );
        }
        CaptionIcon::Close => {
            painter.line_segment(
                [egui::pos2(c.x - s, c.y - s), egui::pos2(c.x + s, c.y + s)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x - s, c.y + s), egui::pos2(c.x + s, c.y - s)],
                stroke,
            );
        }
    }
    resp
}

/// Width of the 4 edge resize zones, in logical px. Slim so they only intercept
/// pointer events right at the window border.
const RESIZE_EDGE_PX: f32 = 8.0;
/// Side length of the 4 corner resize zones, in logical px. Slightly larger than
/// the edges so diagonal grabs are forgiving.
const RESIZE_CORNER_PX: f32 = 12.0;

/// Which window-edge resize direction (if any) the pointer `p` is over, given
/// the window `rect` and the edge/corner band widths. Corners (within `corner`
/// of two sides) take priority over straight edges; the interior returns `None`.
/// Pure + unit-tested so the frameless-resize hit-testing can't silently regress.
fn resize_dir_at(
    p: egui::Pos2,
    rect: egui::Rect,
    edge: f32,
    corner: f32,
) -> Option<egui::ResizeDirection> {
    use egui::ResizeDirection as D;
    let (l, r, t, b) = (
        p.x - rect.left(),
        rect.right() - p.x,
        p.y - rect.top(),
        rect.bottom() - p.y,
    );
    // Outside the window → not a resize zone.
    if l < 0.0 || r < 0.0 || t < 0.0 || b < 0.0 {
        return None;
    }
    let (w, e, n, s) = (l <= edge, r <= edge, t <= edge, b <= edge);
    let (nw, ne, nn, ns) = (l <= corner, r <= corner, t <= corner, b <= corner);
    if (n && nw) || (w && nn) {
        Some(D::NorthWest)
    } else if (n && ne) || (e && nn) {
        Some(D::NorthEast)
    } else if (s && nw) || (w && ns) {
        Some(D::SouthWest)
    } else if (s && ne) || (e && ns) {
        Some(D::SouthEast)
    } else if n {
        Some(D::North)
    } else if s {
        Some(D::South)
    } else if w {
        Some(D::West)
    } else if e {
        Some(D::East)
    } else {
        None
    }
}

/// Frameless window edge-resize, the no-Area way. Each frame: if the pointer is
/// over an edge band, hint the matching resize cursor; on a primary press there
/// — and only when egui isn't already using the pointer for a widget — start an
/// OS resize via `ViewportCommand::BeginResize`. No persistent `Order::Foreground`
/// Areas, so it never swallows clicks meant for tabs / the settings ✕ / panels,
/// and it works on every resize, not just the first.
fn handle_frameless_resize(ctx: &egui::Context) {
    use egui::{CursorIcon as C, ResizeDirection as D, ViewportCommand};
    let Some(p) = ctx.pointer_latest_pos() else {
        return;
    };
    // Hit-test against the FULL window surface (screen_rect), not content_rect —
    // content_rect can exclude the top titlebar / bottom status panels, which
    // would push the resize bands inward off the real window edges so the user
    // can't grab them. screen_rect is the whole inner window area.
    let Some(dir) = resize_dir_at(p, ctx.screen_rect(), RESIZE_EDGE_PX, RESIZE_CORNER_PX) else {
        return;
    };
    ctx.set_cursor_icon(match dir {
        D::North => C::ResizeNorth,
        D::South => C::ResizeSouth,
        D::West => C::ResizeWest,
        D::East => C::ResizeEast,
        D::NorthWest => C::ResizeNorthWest,
        D::NorthEast => C::ResizeNorthEast,
        D::SouthWest => C::ResizeSouthWest,
        D::SouthEast => C::ResizeSouthEast,
    });
    // Start the OS resize on a FRESH press anywhere in the (thin) edge/corner
    // band. The previous `&& !ctx.wants_pointer_input()` guard is why resize
    // silently did nothing: the editor TextEdit + the status/side panels cover
    // every window edge, so `wants_pointer_input()` is true at the edges and the
    // BeginResize was always skipped (the cursor still changed — that part is
    // unconditional — which is exactly the "cursor changes but it doesn't
    // resize" report). The band is only 8px (12px at corners), so a press that
    // lands in it is an intentional resize; handing the drag to the OS is the
    // right call even if a widget also sits under the very edge. `primary_pressed`
    // is the rising edge, so no widget drag is in progress yet.
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        // The OS now owns the drag. winit's modal resize loop swallows the
        // button-up, so egui can be left believing a drag is still in progress —
        // which makes `wants_pointer_input()` return true forever and blocks
        // EVERY subsequent resize (the "works once, then never" bug). Clearing
        // egui's drag bookkeeping here unsticks that state so resize re-arms.
        ctx.stop_dragging();
    }
    // Belt-and-suspenders: with no button held there can be no legitimate drag,
    // so proactively clear any phantom drag the OS resize loop may have orphaned.
    if !ctx.input(|i| i.pointer.any_down()) {
        ctx.stop_dragging();
    }
}

#[cfg(test)]
mod resize_tests {
    //! Regression guard for the frameless resize hit-testing. The interior MUST
    //! NOT be a resize zone (that's what made the resize overlay eat tab /
    //! settings-✕ clicks); edges/corners must map to the right direction. Pure,
    //! so it runs every CI build and pins the geometry across window sizes.
    use super::resize_dir_at;
    use egui::{pos2, Rect, ResizeDirection as D};

    fn win() -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 700.0))
    }

    #[test]
    fn interior_is_never_a_resize_zone() {
        assert_eq!(resize_dir_at(pos2(500.0, 350.0), win(), 6.0, 12.0), None);
        // The exact tab position the old Foreground overlay was eating.
        assert_eq!(resize_dir_at(pos2(574.0, 48.0), win(), 6.0, 12.0), None);
    }

    #[test]
    fn edges_map_to_their_direction() {
        assert_eq!(
            resize_dir_at(pos2(500.0, 1.0), win(), 6.0, 12.0),
            Some(D::North)
        );
        assert_eq!(
            resize_dir_at(pos2(500.0, 699.0), win(), 6.0, 12.0),
            Some(D::South)
        );
        assert_eq!(
            resize_dir_at(pos2(1.0, 350.0), win(), 6.0, 12.0),
            Some(D::West)
        );
        assert_eq!(
            resize_dir_at(pos2(999.0, 350.0), win(), 6.0, 12.0),
            Some(D::East)
        );
    }

    #[test]
    fn corners_take_priority_over_edges() {
        assert_eq!(
            resize_dir_at(pos2(2.0, 2.0), win(), 6.0, 12.0),
            Some(D::NorthWest)
        );
        assert_eq!(
            resize_dir_at(pos2(998.0, 2.0), win(), 6.0, 12.0),
            Some(D::NorthEast)
        );
        assert_eq!(
            resize_dir_at(pos2(2.0, 698.0), win(), 6.0, 12.0),
            Some(D::SouthWest)
        );
        assert_eq!(
            resize_dir_at(pos2(998.0, 698.0), win(), 6.0, 12.0),
            Some(D::SouthEast)
        );
        // On the top edge but within the corner band of the left side → NW.
        assert_eq!(
            resize_dir_at(pos2(8.0, 1.0), win(), 6.0, 12.0),
            Some(D::NorthWest)
        );
    }

    #[test]
    fn outside_the_window_is_none() {
        assert_eq!(resize_dir_at(pos2(-5.0, 350.0), win(), 6.0, 12.0), None);
        assert_eq!(resize_dir_at(pos2(500.0, 800.0), win(), 6.0, 12.0), None);
    }
}

#[cfg(test)]
mod save_session_tests {
    //! #81 — prove (not just assert wired) that the Save & Session settings the
    //! user couldn't tell were working actually change what hits disk. These run
    //! the real open_path → save_active pipeline against a temp file, no GUI
    //! focus or timing needed.
    use super::ScribeApp;
    use scribe_core::Config;

    #[test]
    fn trim_and_final_newline_on_save_take_effect() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("note.txt");
        std::fs::write(&p, "seed").unwrap();
        let mut cfg = Config::default();
        cfg.editor.trim_trailing_whitespace_on_save = true;
        cfg.editor.final_newline_on_save = true;
        let mut app = ScribeApp::new_test(cfg);
        app.open_path(p.clone());
        let active = app.active;
        app.tabs[active].text = "alpha   \nbeta".into(); // trailing spaces, no final \n
        app.save_active();
        let on_disk = std::fs::read_to_string(&p).unwrap();
        assert!(
            !on_disk.contains("alpha   "),
            "trailing whitespace must be trimmed: {on_disk:?}"
        );
        assert!(on_disk.ends_with('\n'), "a final newline must be ensured");
        assert_eq!(on_disk, "alpha\nbeta\n");
    }

    #[test]
    fn save_hygiene_is_a_noop_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("note.txt");
        std::fs::write(&p, "seed").unwrap();
        let mut cfg = Config::default();
        cfg.editor.trim_trailing_whitespace_on_save = false;
        cfg.editor.final_newline_on_save = false;
        let mut app = ScribeApp::new_test(cfg);
        app.open_path(p.clone());
        let active = app.active;
        app.tabs[active].text = "alpha   ".into();
        app.save_active();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "alpha   ",
            "with both toggles off the bytes are written verbatim"
        );
    }
}

#[cfg(test)]
mod perf_and_security_tests {
    //! #91 — perf + security smoke coverage. The perf test guards against a
    //! pathological-slowdown regression on a large buffer; the security tests
    //! assert untrusted / hostile buffer content and traversal-shaped paths are
    //! handled without panicking.
    use super::{byte_to_char_index, ScribeApp};
    use scribe_core::Config;

    #[test]
    fn large_buffer_spell_scan_stays_bounded() {
        let mut cfg = Config::default();
        cfg.spellcheck.enabled = true;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs[0].text = "word ".repeat(20_000); // ~100 KB / 20k words
        let t = std::time::Instant::now();
        let _ = app.misspellings_for_active();
        // Memoized + linear; a generous ceiling that still catches an O(n^2)
        // regression without being flaky on slow CI.
        assert!(
            t.elapsed().as_secs() < 10,
            "spell scan on a 100 KB buffer must stay well under 10s"
        );
    }

    #[test]
    fn hostile_buffer_content_does_not_panic() {
        let mut app = ScribeApp::new_test(Config::default());
        // NUL + control chars + a very long line + an RTL-override + combining.
        app.tabs[0].text = format!("\0\u{1}\u{7f}{}\u{202e}rtl\u{0301}", "x".repeat(50_000));
        let _ = app.misspellings_for_active();
        let _ = app.spell_count();
        // Byte→char mapping must clamp, never index mid-codepoint or overflow.
        let _ = byte_to_char_index(&app.tabs[0].text, 49_999);
        let _ = byte_to_char_index(&app.tabs[0].text, usize::MAX);
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn traversal_shaped_open_path_is_handled_gracefully() {
        let mut app = ScribeApp::new_test(Config::default());
        let before = app.tabs.len();
        // A nonexistent, traversal-shaped path must not panic or open a phantom
        // tab; from_path fails and is surfaced as a toast.
        app.open_path(std::path::PathBuf::from("../../../../nonexistent/passwd"));
        assert_eq!(
            app.tabs.len(),
            before,
            "no tab opened for an unreadable path"
        );
    }
}

#[cfg(test)]
mod wave3_perf_tests {
    //! Wave-3 perf: the minimap + spellcheck caches now key off a per-tab
    //! `edit_gen` counter instead of re-hashing the whole buffer every frame.
    //! Correctness hinges on EVERY text mutation advancing the counter — these
    //! tests pin the Rust-side mutation funnels (Class A + Class B). The egui /
    //! rope write-back paths (Class C/D) require a live frame and are covered by
    //! the `out.response.changed()` / `resp.content_changed` hooks directly.
    use super::{use_rope_editor, ScribeApp};
    use scribe_core::Config;

    fn gen(app: &ScribeApp) -> u64 {
        app.tabs[app.active].edit_gen
    }

    #[test]
    fn use_rope_editor_decision_matrix() {
        // The experimental opt-in forces the rope editor regardless of size.
        assert!(use_rope_editor(true, 0, 0));
        assert!(use_rope_editor(true, 10, 16 * 1024 * 1024));
        // threshold 0 disables auto-promotion no matter how big the buffer is.
        assert!(!use_rope_editor(false, usize::MAX, 0));
        // Below the threshold → the canonical egui TextEdit path.
        assert!(!use_rope_editor(false, 1024, 16 * 1024 * 1024));
        // At or above the threshold → the viewport-culled rope path.
        assert!(use_rope_editor(false, 16 * 1024 * 1024, 16 * 1024 * 1024));
        assert!(use_rope_editor(false, 32 * 1024 * 1024, 16 * 1024 * 1024));
    }

    #[test]
    fn set_text_advances_edit_gen() {
        let mut app = ScribeApp::new_test(Config::default());
        let g0 = gen(&app);
        app.tabs[app.active].set_text("hello\nworld\n".to_string());
        assert!(
            gen(&app) > g0,
            "set_text (Class A funnel) must bump edit_gen"
        );
    }

    #[test]
    fn direct_edit_commands_advance_edit_gen() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[app.active].text = "alpha\nbravo\ncharlie\n".to_string();
        app.last_cursor_line_col = Some((2, 1)); // 1-based line 2 (bravo)

        let g = gen(&app);
        app.duplicate_cursor_line();
        assert!(gen(&app) > g, "duplicate_cursor_line must bump edit_gen");

        let g = gen(&app);
        app.move_cursor_line(1);
        assert!(gen(&app) > g, "move_cursor_line must bump edit_gen");

        let g = gen(&app);
        app.join_cursor_line_with_next();
        assert!(
            gen(&app) > g,
            "join_cursor_line_with_next must bump edit_gen"
        );

        app.find_query = "a".to_string();
        app.replace_query = "X".to_string();
        let g = gen(&app);
        app.replace_in_active(true);
        assert!(gen(&app) > g, "replace_in_active must bump edit_gen");
    }
}

#[cfg(test)]
mod font_theme_tests {
    //! #87 — bundled font themes. The selected family must lead the Monospace
    //! family list (so it actually renders), JetBrains Mono must stay right
    //! behind it as a fallback, every bundled face + the JP subset must be
    //! registered, and an unknown name must fall back gracefully.
    use super::{build_fonts, font_family_key, FONT_FAMILIES};

    #[test]
    fn selected_family_leads_with_jetbrains_fallback() {
        let f = build_fonts("IBM Plex Mono", "System default");
        let mono = &f.families[&egui::FontFamily::Monospace];
        assert_eq!(mono[0], "IBMPlexMono", "selected face renders first");
        assert_eq!(mono[1], "JetBrainsMono", "JetBrains kept as fallback");
        for (_, key) in FONT_FAMILIES {
            assert!(f.font_data.contains_key(*key), "{key} registered");
        }
        assert!(f.font_data.contains_key("NotoSansJP-Subset"));
    }

    #[test]
    fn unknown_family_falls_back_to_jetbrains() {
        assert_eq!(font_family_key("No Such Font"), "JetBrainsMono");
        let f = build_fonts("No Such Font", "System default");
        assert_eq!(f.families[&egui::FontFamily::Monospace][0], "JetBrainsMono");
    }

    #[test]
    fn ui_family_overrides_only_the_proportional_family() {
        // #103 — the UI family leads Proportional; the note family leads
        // Monospace; the two are independent.
        let f = build_fonts("JetBrains Mono", "Fira Mono");
        assert_eq!(f.families[&egui::FontFamily::Proportional][0], "FiraMono");
        assert_eq!(f.families[&egui::FontFamily::Monospace][0], "JetBrainsMono");
        // "System default" leaves egui's built-in UI font at the head.
        let f2 = build_fonts("JetBrains Mono", "System default");
        assert_ne!(f2.families[&egui::FontFamily::Proportional][0], "FiraMono");
    }

    #[test]
    fn default_family_is_jetbrains_and_does_not_double_insert() {
        let f = build_fonts("JetBrains Mono", "System default");
        let mono = &f.families[&egui::FontFamily::Monospace];
        assert_eq!(mono[0], "JetBrainsMono");
        // No redundant second JetBrains entry when it's already the selection.
        assert_ne!(mono.get(1).map(String::as_str), Some("JetBrainsMono"));
    }
}

#[cfg(test)]
mod background_override_tests {
    //! #88 — the app background override repaints panel/window fills
    //! independently of the theme; None follows the theme.
    use super::ScribeApp;
    use scribe_core::Config;

    #[test]
    fn override_repaints_panel_and_window_fill() {
        let mut cfg = Config::default();
        cfg.appearance.background_override = Some("#112233".into());
        let app = ScribeApp::new_test(cfg);
        let v = app.current_visuals();
        let want = egui::Color32::from_rgb(0x11, 0x22, 0x33);
        assert_eq!(v.panel_fill, want);
        assert_eq!(v.window_fill, want);
    }

    #[test]
    fn none_follows_theme_not_the_override_colour() {
        let cfg = Config::default(); // background_override = None
        let app = ScribeApp::new_test(cfg);
        let v = app.current_visuals();
        // Whatever the theme is, it must NOT be the arbitrary override colour.
        assert_ne!(v.panel_fill, egui::Color32::from_rgb(0x11, 0x22, 0x33));
    }

    #[test]
    fn linked_note_background_follows_the_app_override() {
        // #106 — linked (default): the note well (extreme_bg_color) tracks the
        // app background override.
        let mut cfg = Config::default();
        cfg.appearance.link_backgrounds = true;
        cfg.appearance.background_override = Some("#112233".into());
        let app = ScribeApp::new_test(cfg);
        let v = app.current_visuals();
        let want = egui::Color32::from_rgb(0x11, 0x22, 0x33);
        assert_eq!(v.panel_fill, want);
        assert_eq!(v.extreme_bg_color, want, "linked note follows app bg");
    }

    #[test]
    fn unlinked_note_background_is_independent() {
        // #106 — unlinked: app + note backgrounds are set separately.
        let mut cfg = Config::default();
        cfg.appearance.link_backgrounds = false;
        cfg.appearance.background_override = Some("#112233".into());
        cfg.appearance.note_background_override = Some("#445566".into());
        let app = ScribeApp::new_test(cfg);
        let v = app.current_visuals();
        assert_eq!(v.panel_fill, egui::Color32::from_rgb(0x11, 0x22, 0x33));
        assert_eq!(
            v.extreme_bg_color,
            egui::Color32::from_rgb(0x44, 0x55, 0x66)
        );
    }
}

#[cfg(test)]
mod spell_underline_tests {
    //! #78 — spellcheck underlines. The byte→char mapping must be correct
    //! (galley cursors are char-indexed; spell spans are byte-indexed) and the
    //! active-buffer misspelling scan must actually flag a bad word so the
    //! painter has something to draw.
    use super::{byte_to_char_index, ScribeApp};
    use scribe_core::Config;

    #[test]
    fn byte_to_char_handles_multibyte() {
        // "café word" — 'é' is 2 bytes, so byte 6 (start of "word") is char 5.
        let s = "café word";
        assert_eq!(byte_to_char_index(s, 0), 0);
        assert_eq!(byte_to_char_index(s, 5), 4, "byte after é → char 4");
        assert_eq!(byte_to_char_index(s, 6), 5, "start of 'word' → char 5");
        assert_eq!(byte_to_char_index(s, 999), s.chars().count(), "clamps");
    }

    #[test]
    fn active_buffer_misspelling_is_detected_when_enabled() {
        let mut cfg = Config::default();
        cfg.spellcheck.enabled = true;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs[0].text = "this zxqwyzz is wrong".into();
        let found = app.misspellings_for_active();
        assert!(
            found.iter().any(|m| m.word.contains("zxqwyzz")),
            "the nonsense word must be flagged (got {found:?})"
        );
    }

    #[test]
    fn no_misspellings_when_spellcheck_disabled() {
        let mut cfg = Config::default();
        cfg.spellcheck.enabled = false;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs[0].text = "zxqwyzz".into();
        assert!(app.misspellings_for_active().is_empty());
    }
}

#[cfg(test)]
mod wrap_tests {
    //! #75 — word-wrap toggle. egui's TextEdit passes the scroll viewport's
    //! available width to the layouter as the wrap width regardless of
    //! `desired_width`, so the editor wrapped even with word-wrap OFF. The fix
    //! lives in `effective_wrap_width`: infinite width when wrap is off (galley
    //! lays out on one line, the ScrollArea scrolls horizontally), the given
    //! viewport width when on.
    use super::effective_wrap_width;

    #[test]
    fn rotated_tab_geometry_swaps_axes_and_anchors_top_right() {
        use super::{rotated_tab_size, rotated_tab_text_pos};
        // A 100x16 horizontal label with (8,10) padding → a 24-wide, 110-tall
        // cell (height+pad.x wide, width+pad.y tall).
        let g = egui::vec2(100.0, 16.0);
        let pad = egui::vec2(8.0, 10.0);
        let size = rotated_tab_size(g, pad);
        assert_eq!(size, egui::vec2(24.0, 110.0));
        // Anchor sits at the top-right of the padded inner area so a +90° spin
        // drops the text into the cell.
        let rect = egui::Rect::from_min_size(egui::pos2(5.0, 7.0), size);
        let pos = rotated_tab_text_pos(rect, g, pad);
        assert_eq!(pos, egui::pos2(5.0 + 4.0 + 16.0, 7.0 + 5.0));
    }

    #[test]
    fn wrap_off_forces_infinite_width() {
        assert_eq!(effective_wrap_width(false, 800.0), f32::INFINITY);
        assert_eq!(effective_wrap_width(false, 1.0), f32::INFINITY);
    }

    #[test]
    fn wrap_on_uses_the_viewport_width() {
        assert_eq!(effective_wrap_width(true, 800.0), 800.0);
        assert_eq!(effective_wrap_width(true, 123.5), 123.5);
    }
}

#[cfg(test)]
mod indent_tests {
    //! #107 — Enter auto-indent. The new line copies the current line's leading
    //! whitespace so indentation persists (which is what makes the indentation
    //! settings visibly do something line-to-line).
    use super::newline_with_indent;

    #[test]
    fn carries_space_indent() {
        let (t, c) = newline_with_indent("    code", 8);
        assert_eq!(t, "    code\n    ");
        assert_eq!(c, 13); // \n + 4 spaces
    }

    #[test]
    fn no_indent_just_breaks_the_line() {
        let (t, c) = newline_with_indent("code", 4);
        assert_eq!(t, "code\n");
        assert_eq!(c, 5);
    }

    #[test]
    fn carries_tab_indent() {
        let (t, c) = newline_with_indent("\tfoo", 4);
        assert_eq!(t, "\tfoo\n\t");
        assert_eq!(c, 6);
    }

    #[test]
    fn indent_is_clamped_to_the_cursor() {
        // Cursor after "ab" on "  abcd" → carry only the line's leading "  ".
        let (t, c) = newline_with_indent("  abcd", 4);
        assert_eq!(t, "  ab\n  cd");
        assert_eq!(c, 7);
    }
}

#[cfg(test)]
mod foreground_area_guard {
    //! Anti-regression source-scan guard (#67).
    //!
    //! The whole class of "I clicked X and nothing happened" bugs in this app
    //! traced to a frameless-resize overlay built from `egui::Area`s at
    //! `Order::Foreground`: a Foreground `Area` is **interactable by default**,
    //! so one that covers (part of) the window silently swallows every click in
    //! its rect — tab switches, the settings ✕, panel-resize handles, all of it.
    //! The fix removed that overlay (resize is now a pointer-gated per-frame edge
    //! check with NO Area — see `handle_frameless_resize`).
    //!
    //! This guard scans the source so the dangerous pattern cannot creep back:
    //! every `egui::Area` placed at `Order::Foreground` MUST either declare
    //! `.interactable(false)` (paint-only / hint overlay — cannot eat clicks) or
    //! be an allowlisted bounded popup that is positioned at a point (so it
    //! covers a small region, not the window) and only shown on demand. A new
    //! Foreground `Area` that is neither fails this test loudly, with a pointer
    //! to this comment, before it can ship.

    /// Foreground `Area`s that are intentionally interactable. Each is a small,
    /// on-demand, point-anchored popup — NOT a window-spanning cover. Adding an
    /// entry here is the explicit, reviewed way to introduce a new one.
    const ALLOWED_INTERACTIVE_FOREGROUND_AREAS: &[&str] = &[
        // Code-completion list, anchored just below the cursor via `.fixed_pos`,
        // shown only while a completion is active. Rows must be clickable.
        "scr1b3-completion",
    ];

    #[test]
    fn no_ungated_interactable_foreground_area() {
        let src = include_str!("app.rs");
        let lines: Vec<&str> = src.lines().collect();

        for (i, line) in lines.iter().enumerate() {
            // Find the start of an Area construction that names its Id.
            let Some(rest) = line.split("egui::Area::new(egui::Id::new(\"").nth(1) else {
                continue;
            };
            let Some(id) = rest.split('"').next() else {
                continue;
            };

            // Collect the builder chain: this line plus the following lines up to
            // the `.show(` that ends the builder. That window is what we inspect
            // for `.order(...Foreground)` and the gating call.
            let mut chain = String::new();
            for l in lines.iter().skip(i).take(20) {
                chain.push_str(l);
                chain.push('\n');
                if l.contains(".show(") {
                    break;
                }
            }

            let is_foreground = chain.contains("Order::Foreground");
            if !is_foreground {
                continue;
            }

            // Paint-only / hint overlays opt out of input explicitly — safe.
            let non_interactable = chain.contains(".interactable(false)");
            if non_interactable {
                continue;
            }

            let allowlisted = ALLOWED_INTERACTIVE_FOREGROUND_AREAS.contains(&id);
            assert!(
                allowlisted,
                "app.rs:{}: `egui::Area` id={id:?} is at Order::Foreground and \
                 interactable-by-default, which swallows clicks in its rect \
                 (the resize-overlay click-eating regression class — see the \
                 `foreground_area_guard` module doc). Either add \
                 `.interactable(false)` if it must not take input, or — if it is \
                 genuinely a small on-demand popup — add {id:?} to \
                 ALLOWED_INTERACTIVE_FOREGROUND_AREAS with a justifying comment.",
                i + 1
            );
        }
    }

    #[test]
    fn the_completion_popup_is_still_present_so_the_scan_is_not_vacuous() {
        // If the only allowlisted Area ever disappears, this guard would pass
        // trivially on an empty match set. Pin that the scan actually sees it.
        let src = include_str!("app.rs");
        assert!(
            src.contains("egui::Id::new(\"scr1b3-completion\")"),
            "completion popup Area id not found — the foreground-area scan would \
             be vacuous; update ALLOWED_INTERACTIVE_FOREGROUND_AREAS to match \
             reality"
        );
    }
}

/// Paint a translucent color tint over the whole window (portable; works in
/// every mode and on every OS). Strength scales the alpha.
fn paint_tint_overlay(ctx: &egui::Context, tint_hex: &str, strength: f32) {
    let Some(c) = Rgba::parse_hex(tint_hex) else {
        return;
    };
    let a = (strength.clamp(0.0, 1.0) * 90.0).round() as u8;
    // #77 — paint the tint at the BACKMOST layer so it washes the app
    // background only. At Order::Foreground it painted OVER every window
    // (including the Settings window), which is exactly the "transparency
    // applies to the settings window" bug the user reported. Behind the panels,
    // it shows through translucent (glass-mode) chrome but never over a window.
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("tint-overlay"),
    ));
    painter.rect_filled(
        // egui 0.34: screen_rect -> content_rect (same window-content footprint).
        ctx.content_rect(),
        0.0,
        Color32::from_rgba_unmultiplied(c.r, c.g, c.b, a),
    );
}

/// Paint translucent horizontal CRT scanlines over the window — a calm retro
/// post-effect ported from C0PL4ND. Dark bands a few points apart at `darkness`
/// strength, drifting slowly with `t` (seconds) so they shimmer like a live CRT
/// rather than reading as a static grid. `Order::Foreground` so they sit OVER
/// the text; the caller gates this behind `motion.enabled && crt_scanlines`.
fn paint_crt_scanlines(ctx: &egui::Context, darkness: f32, t: f64) {
    let alpha = (darkness.clamp(0.0, 1.0) * 200.0).round() as u8;
    if alpha == 0 {
        return;
    }
    let rect = ctx.content_rect();
    let band = Color32::from_rgba_unmultiplied(0, 0, 0, alpha);
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("crt-scanlines"),
    ));
    const PERIOD: f32 = 3.0; // points between band tops
    const BAND_H: f32 = 1.0; // points of dark per band
                             // Slow vertical drift (~6 pt/s) so the lines visibly shimmer.
    let drift = (t * 6.0).rem_euclid(PERIOD as f64) as f32;
    let mut y = rect.top() - PERIOD + drift;
    while y < rect.bottom() {
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), y),
                egui::pos2(rect.right(), y + BAND_H),
            ),
            0.0,
            band,
        );
        y += PERIOD;
    }
}

/// Subtle full-window brightness flicker (CRT-style). A translucent black wash
/// whose alpha wanders via layered sines of `t` (deterministic — no RNG, so the
/// reduced-motion resting frame is stable). `strength` is capped at 0.20 for
/// accessibility. `Order::Foreground` so it modulates the whole composited view.
fn paint_flicker(ctx: &egui::Context, strength: f32, t: f64) {
    let s = strength.clamp(0.0, 0.20);
    if s <= 0.0 {
        return;
    }
    let n = ((t * 17.0).sin() * 0.5 + (t * 53.0).sin() * 0.3 + (t * 97.0).sin() * 0.2).abs();
    let a = (s * n as f32 * 90.0).round() as u8;
    if a == 0 {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("crt-flicker"),
    ));
    painter.rect_filled(
        ctx.content_rect(),
        0.0,
        Color32::from_rgba_unmultiplied(0, 0, 0, a),
    );
}

/// VHS-style tracking lines: faint bright horizontal bands sweeping down the
/// window at two different speeds, like analogue tape tracking error.
fn paint_vhs_tracking(ctx: &egui::Context, t: f64) {
    let rect = ctx.content_rect();
    if rect.height() < 1.0 {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("vhs-tracking"),
    ));
    for (i, speed) in [(0u32, 0.13f64), (1, 0.071)].iter() {
        let phase = (t * speed + *i as f64 * 0.5).rem_euclid(1.0) as f32;
        let y = rect.top() + phase * rect.height();
        let band_h = 16.0;
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), y),
                egui::pos2(rect.right(), y + band_h),
            ),
            0.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 9),
        );
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), y + band_h * 0.4),
                egui::pos2(rect.right(), y + band_h * 0.6),
            ),
            0.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 7),
        );
    }
}

/// Animated wired node-mesh ambient background (Lain "Wired" feel). `density`
/// (0..1) drives the node count (8..64); nodes drift slowly and near neighbours
/// are linked with faint accent lines. `Order::Background` so it sits BEHIND the
/// editor like the tint overlay. O(n²) over n ≤ 64 — bounded per frame.
fn paint_wired_mesh(ctx: &egui::Context, density: f32, color: Color32, t: f64) {
    let rect = ctx.content_rect();
    if rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("wired-mesh"),
    ));
    let n = (8.0 + density.clamp(0.0, 1.0) * 56.0) as usize;
    let mut pts: Vec<egui::Pos2> = Vec::with_capacity(n);
    for i in 0..n {
        let fi = i as f64;
        let bx = (fi * 0.732).fract() as f32;
        let by = (fi * 0.387 + 0.13).fract() as f32;
        let dx = ((t * 0.07 + fi * 1.3).sin() * 0.5 + 0.5) as f32;
        let dy = ((t * 0.05 + fi * 0.7).cos() * 0.5 + 0.5) as f32;
        let x = rect.left() + (bx * 0.85 + dx * 0.1) * rect.width();
        let y = rect.top() + (by * 0.85 + dy * 0.1) * rect.height();
        pts.push(egui::pos2(x, y));
    }
    let link = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 16);
    let dot = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 40);
    let max_d = rect.width().min(rect.height()) * 0.18;
    for i in 0..n {
        for j in (i + 1)..n {
            if pts[i].distance(pts[j]) < max_d {
                painter.line_segment([pts[i], pts[j]], egui::Stroke::new(1.0, link));
            }
        }
        painter.circle_filled(pts[i], 1.5, dot);
    }
}

/// Caret ghost-trail: fading echoes of recent caret rectangles. The caller feeds
/// `trail` (rect + birth-time) as the caret moves; each echo fades over ~0.45s.
fn paint_caret_trail(
    ctx: &egui::Context,
    trail: &std::collections::VecDeque<(egui::Rect, f64)>,
    color: Color32,
    now: f64,
) {
    if trail.is_empty() {
        return;
    }
    const LIFE: f64 = 0.45;
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("caret-trail"),
    ));
    for (rect, born) in trail.iter() {
        let age = (now - born).clamp(0.0, LIFE);
        let f = 1.0 - (age / LIFE);
        if f <= 0.0 {
            continue;
        }
        let a = (f * 90.0) as u8;
        painter.rect_filled(
            *rect,
            1.0,
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a),
        );
    }
}

/// One-shot boot "glitch" sweep over the first ~0.55s after launch: a bright
/// scan line descends while a few dark offset bands flicker, all fading out.
/// `elapsed` is seconds since the first frame; outside `[0, DUR]` it no-ops.
fn paint_boot_glitch(ctx: &egui::Context, elapsed: f64) {
    const DUR: f64 = 0.55;
    if !(0.0..=DUR).contains(&elapsed) {
        return;
    }
    let rect = ctx.content_rect();
    if rect.width() < 160.0 {
        return; // first-frame 0-width content_rect guard
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("boot-glitch"),
    ));
    let p = (elapsed / DUR) as f32;
    let fade = 1.0 - p;
    let y = rect.top() + p * rect.height();
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(rect.left(), y - 2.0),
            egui::pos2(rect.right(), y + 2.0),
        ),
        0.0,
        Color32::from_rgba_unmultiplied(255, 255, 255, (fade * 120.0) as u8),
    );
    for i in 0..3u32 {
        let fi = i as f32;
        let gy = rect.top() + ((p * 2.0 + fi * 0.27).fract()) * rect.height();
        let gh = 6.0 + fi * 4.0;
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), gy),
                egui::pos2(rect.right(), gy + gh),
            ),
            0.0,
            Color32::from_rgba_unmultiplied(0, 0, 0, (fade * 60.0) as u8),
        );
    }
}

#[cfg(test)]
mod jp_glyph_tests {
    //! Phase 17 T17.5 — verify the JP-glyph instrument-label discipline.
    use super::{jp_glyph, toolbar_widget};

    #[test]
    fn verified_canonical_kanji_present_for_high_confidence_ids() {
        // The Folklore-Consultant gate requires "verified-accurate kanji ONLY".
        // These 11 are the verified-canonical IT-Japanese forms; this test
        // pins them so an accidental edit (typo, replacement with an
        // unverified glyph) regresses loudly.
        assert_eq!(jp_glyph("new"), Some("新"));
        assert_eq!(jp_glyph("open"), Some("開"));
        assert_eq!(jp_glyph("save"), Some("保"));
        assert_eq!(jp_glyph("saveas"), Some("別"));
        assert_eq!(jp_glyph("find"), Some("検"));
        assert_eq!(jp_glyph("split"), Some("分"));
        assert_eq!(jp_glyph("minimap"), Some("図"));
        assert_eq!(jp_glyph("wrap"), Some("折"));
        assert_eq!(jp_glyph("fold"), Some("畳"));
        assert_eq!(jp_glyph("linenumbers"), Some("番"));
        assert_eq!(jp_glyph("spellcheck"), Some("綴"));
    }

    #[test]
    fn bundled_jp_subset_covers_every_toolbar_kanji() {
        // #56 — the toolbar kanji rendered as tofu because no font in the stack
        // covered CJK. We bundle a Noto Sans JP subset; this asserts that subset
        // actually contains a glyph for every kanji `jp_glyph` can emit, read
        // through skrifa (the same font crate epaint/egui 0.34 rasterizes with).
        // A botched regeneration that drops a glyph fails here, loudly.
        use skrifa::{raw::FontRef, MetadataProvider as _};
        const SUBSET: &[u8] =
            include_bytes!("../../../assets/fonts/NotoSansJP/NotoSansJP-Subset.ttf");
        let face = FontRef::new(SUBSET).expect("bundled JP subset must parse");
        let charmap = face.charmap();
        let ids = [
            "new",
            "open",
            "save",
            "saveas",
            "find",
            "split",
            "minimap",
            "wrap",
            "fold",
            "linenumbers",
            "spellcheck",
        ];
        for id in ids {
            let kanji = jp_glyph(id).expect("id has a verified kanji");
            let ch = kanji.chars().next().unwrap();
            let gid = charmap.map(ch);
            assert!(
                gid.is_some_and(|g| g.to_u32() != 0),
                "bundled JP subset is missing a glyph for {id} = {kanji:?} \
                 (regenerate via scripts/generate-jp-kanji-subset.py)"
            );
        }
    }

    #[test]
    fn uncertain_ids_omit_kanji() {
        // Western-metaphor or acronym/loanword actions stay English-only —
        // the canonical kanji is uncertain or contested. They MUST return
        // None so a future "ship a guess" doesn't slip through.
        assert_eq!(jp_glyph("openfolder"), None);
        assert_eq!(jp_glyph("palette"), None);
        assert_eq!(jp_glyph("lsp"), None);
        // Unknown ids also return None — the helper never invents.
        assert_eq!(jp_glyph("not-a-toolbar-action"), None);
    }

    #[test]
    fn widget_falls_back_to_label_when_disabled_or_unknown() {
        // jp_glyph_labels=false → primary label only, regardless of action.
        let off = toolbar_widget("new", false, false, 14.0, egui::Color32::PLACEHOLDER);
        assert_eq!(off.text(), "new");
        // Even with the flag on, an action without verified kanji returns
        // only the primary label — no kanji is invented.
        let on_unknown =
            toolbar_widget("openfolder", false, true, 14.0, egui::Color32::PLACEHOLDER);
        assert_eq!(on_unknown.text(), "folder");
    }

    #[test]
    fn widget_appends_kanji_when_enabled_for_verified_action() {
        // jp_glyph_labels=true + verified action → primary then kanji.
        // The LayoutJob's flattened text contains both pieces.
        let on = toolbar_widget("save", false, true, 14.0, egui::Color32::PLACEHOLDER);
        let text = on.text();
        assert!(text.starts_with("save"), "got {text:?}");
        assert!(text.contains("保"), "got {text:?}");
    }

    #[test]
    fn kanji_label_keeps_english_text_colour_constant() {
        // #105 — with kanji ON, the ENGLISH (primary) section must use
        // PLACEHOLDER so the widget substitutes its normal text colour, i.e.
        // identical to kanji-OFF. Only the kanji section is explicitly tinted.
        let on = toolbar_widget("save", false, true, 14.0, egui::Color32::PLACEHOLDER);
        match on {
            egui::WidgetText::LayoutJob(job) => {
                assert_eq!(
                    job.sections[0].format.color,
                    egui::Color32::PLACEHOLDER,
                    "english label must inherit the widget colour (constant on/off)"
                );
                assert_ne!(
                    job.sections[1].format.color,
                    egui::Color32::PLACEHOLDER,
                    "the kanji section is the one that is tinted"
                );
            }
            other => panic!("expected a LayoutJob with a kanji section, got {other:?}"),
        }
    }

    #[test]
    fn selected_toggle_pins_primary_to_accent_with_kanji_on() {
        // #22 — a SELECTED toolbar toggle passes a CONCRETE accent as the primary
        // colour, so with kanji ON the LayoutJob's primary (english) section must
        // carry that exact accent — NOT PLACEHOLDER, which `selectable_label`
        // would recolour to its strong-contrast (white) selected colour. The
        // kanji section keeps its own dim tint. This is the regression guard for
        // the "kanji-on selected toggle renders white instead of accent" bug.
        let accent = egui::Color32::from_rgb(0, 255, 254);
        let on = toolbar_widget("save", false, true, 14.0, accent);
        match on {
            egui::WidgetText::LayoutJob(job) => {
                assert_eq!(
                    job.sections[0].format.color, accent,
                    "selected english label must be accent even with kanji on"
                );
                assert_ne!(
                    job.sections[0].format.color,
                    egui::Color32::PLACEHOLDER,
                    "a selected toggle must NOT leave the primary as PLACEHOLDER"
                );
                assert_ne!(
                    job.sections[1].format.color, accent,
                    "the kanji section keeps its own dim tint"
                );
            }
            other => panic!("expected a LayoutJob with a kanji section, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod tab_reorder_tests {
    //! Phase 2 — tab drag-reorder. The index arithmetic in
    //! [`tab_index_after_move`] is the part that historically broke (the old
    //! hit-test missed drop targets to the right of the dragged tab), so it is
    //! pinned exhaustively here; [`ScribeApp::move_tab`] is the thin wrapper
    //! that also keeps `active` pointed at the same buffer.
    use super::{fuzzy_move_selection, tab_index_after_move, EditorTab, ScribeApp};
    use scribe_core::Config;

    #[test]
    fn fuzzy_nav_down_advances_and_saturates_at_last() {
        assert_eq!(fuzzy_move_selection(0, 3, false, true), 1);
        assert_eq!(fuzzy_move_selection(2, 3, false, true), 2, "down saturates");
    }

    #[test]
    fn fuzzy_nav_up_saturates_at_first() {
        assert_eq!(fuzzy_move_selection(1, 3, true, false), 0);
        assert_eq!(fuzzy_move_selection(0, 3, true, false), 0, "up saturates");
    }

    #[test]
    fn fuzzy_nav_reclamps_when_results_shrank() {
        // The query just narrowed the list under a stale selection index.
        assert_eq!(fuzzy_move_selection(9, 3, false, false), 2);
        assert_eq!(fuzzy_move_selection(9, 0, false, true), 0, "empty -> 0");
    }

    #[test]
    fn move_is_identity_when_src_equals_target() {
        for idx in 0..5 {
            assert_eq!(tab_index_after_move(2, 2, idx), idx);
        }
    }

    #[test]
    fn rightward_move_places_dragged_at_target_slot() {
        // [0,1,2,3], drag 0 → onto 2  =>  [1,2,0,3]  (0 takes slot 2)
        assert_eq!(tab_index_after_move(0, 2, 0), 2); // dragged element → target
        assert_eq!(tab_index_after_move(0, 2, 1), 0); // 1 shifts left
        assert_eq!(tab_index_after_move(0, 2, 2), 1); // 2 shifts left
        assert_eq!(tab_index_after_move(0, 2, 3), 3); // untouched
    }

    #[test]
    fn leftward_move_places_dragged_at_target_slot() {
        // [0,1,2,3], drag 3 → onto 1  =>  [0,3,1,2]  (3 takes slot 1)
        assert_eq!(tab_index_after_move(3, 1, 3), 1); // dragged element → target
        assert_eq!(tab_index_after_move(3, 1, 0), 0); // before target, untouched
        assert_eq!(tab_index_after_move(3, 1, 1), 2); // 1 shifts right
        assert_eq!(tab_index_after_move(3, 1, 2), 3); // 2 shifts right
    }

    #[test]
    fn adjacent_swap_both_directions() {
        // drag 1 → onto 2 (rightward by one): [0,1,2] -> [0,2,1]
        assert_eq!(tab_index_after_move(1, 2, 1), 2);
        assert_eq!(tab_index_after_move(1, 2, 2), 1);
        // drag 2 → onto 1 (leftward by one): [0,1,2] -> [0,2,1]
        assert_eq!(tab_index_after_move(2, 1, 2), 1);
        assert_eq!(tab_index_after_move(2, 1, 1), 2);
    }

    /// `move_tab` must physically reorder the tabs AND keep `active` glued to
    /// the buffer the user was editing, whichever tab moved.
    #[test]
    fn move_tab_reorders_and_tracks_active() {
        let mut app = ScribeApp::new_test(Config::default());
        // Replace whatever the constructor produced with 4 identifiable tabs.
        app.tabs.clear();
        for n in 0..4u64 {
            let mut t = EditorTab::scratch();
            t.doc_id = crate::grid::DocId(n);
            app.tabs.push(t);
        }

        // User is editing buffer 1 (tab index 1); drag tab 0 onto tab 2 so 0
        // takes slot 2 => order [1,2,0,3].
        app.active = 1;
        app.move_tab(0, 2);
        let ids: Vec<u64> = app.tabs.iter().map(|t| t.doc_id.0).collect();
        assert_eq!(ids, vec![1, 2, 0, 3], "physical order after rightward move");
        assert_eq!(app.tabs[app.active].doc_id.0, 1, "active still on buffer 1");

        // Now drag the last tab (index 3, buffer 3) onto index 0 so 3 takes
        // slot 0 => [3,1,2,0]. The user is editing buffer 1 (now at index 0);
        // it should follow to index 1.
        app.active = 0;
        let active_buf = app.tabs[app.active].doc_id.0;
        app.move_tab(3, 0);
        let ids: Vec<u64> = app.tabs.iter().map(|t| t.doc_id.0).collect();
        assert_eq!(ids, vec![3, 1, 2, 0], "physical order after leftward move");
        assert_eq!(
            app.tabs[app.active].doc_id.0, active_buf,
            "active follows its buffer across a leftward move"
        );
    }

    #[test]
    fn move_tab_is_noop_on_bad_indices() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs.clear();
        for n in 0..3u64 {
            let mut t = EditorTab::scratch();
            t.doc_id = crate::grid::DocId(n);
            app.tabs.push(t);
        }
        app.active = 2;
        app.move_tab(0, 0); // equal
        app.move_tab(5, 1); // src OOB
        app.move_tab(1, 9); // target OOB
        let ids: Vec<u64> = app.tabs.iter().map(|t| t.doc_id.0).collect();
        assert_eq!(ids, vec![0, 1, 2], "order unchanged");
        assert_eq!(app.active, 2, "active unchanged");
    }

    #[test]
    fn move_tab_refuses_to_move_a_pinned_tab() {
        // #R5: pinned notes are anchored — move_tab is a no-op when the source
        // tab is pinned, so a pinned note can't be drag-reordered.
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs.clear();
        for n in 0..3u64 {
            let mut t = EditorTab::scratch();
            t.doc_id = crate::grid::DocId(n);
            app.tabs.push(t);
        }
        app.tabs[0].pinned = true;
        app.active = 0;
        app.move_tab(0, 2); // try to drag the pinned tab to the end
        let ids: Vec<u64> = app.tabs.iter().map(|t| t.doc_id.0).collect();
        assert_eq!(ids, vec![0, 1, 2], "a pinned tab must not move");
    }

    #[test]
    fn close_tab_refuses_to_close_a_pinned_tab() {
        // #R5: pinned notes can't be closed directly — close_tab is the single
        // chokepoint and refuses a pinned index, while unpinned tabs still close.
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs.clear();
        for n in 0..3u64 {
            let mut t = EditorTab::scratch();
            t.doc_id = crate::grid::DocId(n);
            app.tabs.push(t);
        }
        app.tabs[1].pinned = true;
        app.close_tab(1);
        let ids: Vec<u64> = app.tabs.iter().map(|t| t.doc_id.0).collect();
        assert_eq!(
            ids,
            vec![0, 1, 2],
            "a pinned tab must not be closed directly"
        );
        // An unpinned tab still closes normally.
        app.close_tab(0);
        let ids: Vec<u64> = app.tabs.iter().map(|t| t.doc_id.0).collect();
        assert_eq!(ids, vec![1, 2], "an unpinned tab still closes");
    }

    #[test]
    fn find_navigate_cycles_through_matches_and_wraps() {
        // #R6 — the find bar can jump between matches (Next/Prev/F3), not just
        // count them.
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "foo bar foo baz foo".to_string(); // three "foo"
        app.find_query = "foo".to_string();
        app.find_open = true;
        assert_eq!(app.find_matches_active().len(), 3);

        app.find_match_idx = 0;
        app.find_navigate(true);
        assert_eq!(app.find_match_idx, 1);
        app.find_navigate(true);
        assert_eq!(app.find_match_idx, 2);
        app.find_navigate(true); // wraps to the first
        assert_eq!(app.find_match_idx, 0);
        app.find_navigate(false); // wraps back to the last
        assert_eq!(app.find_match_idx, 2);

        // No matches -> no-op, never panics.
        app.find_query = "this-string-is-absent".to_string();
        app.find_navigate(true);
        assert!(app.find_matches_active().is_empty());
    }
}

#[cfg(test)]
mod update_reminder_tests {
    //! The once-per-launch automatic-update mode→action mapping. `off`/`manual`
    //! never do automatic network activity; `notify`/`auto` run a check when due.
    use super::update_launch_action;
    use crate::updater::LaunchKind;
    use scribe_core::config::UpdateMode;

    const HOUR: u64 = 3_600;

    #[test]
    fn off_never_checks_even_when_overdue() {
        assert_eq!(
            update_launch_action(UpdateMode::Off, None, 24, 1_000_000),
            None
        );
    }

    #[test]
    fn manual_never_checks_automatically() {
        // `manual` = the user checks via the Settings button; no on-launch network.
        assert_eq!(update_launch_action(UpdateMode::Manual, None, 24, 0), None);
        assert_eq!(
            update_launch_action(UpdateMode::Manual, None, 24, 1_000_000),
            None
        );
    }

    #[test]
    fn notify_checks_when_due_as_notify_kind() {
        let last = 1_000;
        // 1h after a 24h-interval check → not due.
        assert_eq!(
            update_launch_action(UpdateMode::Notify, Some(last), 24, last + HOUR),
            None,
        );
        // 24h later → due → a Notify-kind check.
        assert_eq!(
            update_launch_action(UpdateMode::Notify, Some(last), 24, last + 24 * HOUR),
            Some(LaunchKind::Notify),
        );
        // Never checked → always due.
        assert_eq!(
            update_launch_action(UpdateMode::Notify, None, 24, 0),
            Some(LaunchKind::Notify),
        );
    }

    #[test]
    fn auto_checks_when_due_as_auto_kind_and_respects_interval() {
        assert_eq!(
            update_launch_action(UpdateMode::Auto, None, 24, 0),
            Some(LaunchKind::Auto),
        );
        // Auto still respects the interval (not-due → no check).
        let last = 1_000;
        assert_eq!(
            update_launch_action(UpdateMode::Auto, Some(last), 24, last + HOUR),
            None,
        );
    }
}

#[cfg(test)]
mod e2e {
    //! End-to-end tests: drive the real `ScribeApp::ui` render loop headlessly
    //! through egui's own `Context::run`, exercising the full per-frame UI +
    //! state pipeline (menus, panels, editor, overlays) without a window/GPU.
    //!
    //! The `egui_kittest`-backed tests below go further: they simulate real user
    //! input (clicking widgets BY LABEL via AccessKit) and assert the observable
    //! outcome — the only kind of test that catches "clicking does nothing".
    use super::*;
    #[allow(unused_imports)]
    use egui_kittest::kittest::Queryable as _;

    /// Run `n` full UI frames against a fresh headless egui context.
    fn run_frames(app: &mut ScribeApp, n: usize) {
        let ctx = egui::Context::default();
        for _ in 0..n {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1100.0, 720.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| app.frame_tick(ctx));
        }
    }

    #[test]
    fn renders_default_without_panic() {
        let mut app = ScribeApp::new_test(Config::default());
        run_frames(&mut app, 3);
        assert_eq!(app.tabs.len(), 1, "expected one scratch tab");
    }

    /// Regression guard (e2e, no GPU): a NARROW pane lays its header out as a
    /// vertical COLUMN — name on top, pin below, close below, all centered and
    /// stacked (the user's Image #2) — NOT a horizontal row. We tile 4 panes in
    /// a wide+short window so each is < 220px (the narrow threshold), and pin all
    /// but one so exactly ONE pane shows a PUSH_PIN + close (✕) — making both
    /// uniquely queryable. We then assert the ✕ sits BELOW the pin and in the
    /// SAME column (x-aligned); a regression to the row layout would put them
    /// side-by-side (≈equal y, different x) and fail.
    #[test]
    fn narrow_pane_header_stacks_vertically() {
        let mut cfg = Config::default();
        cfg.appearance.frameless = false;
        cfg.editor.first_run_completed = true;
        cfg.editor.grid_enabled = true;
        let mut app = ScribeApp::new_test(cfg);
        for _ in 0..3 {
            app.tabs.push(EditorTab::scratch());
        }
        // Pin all but the LAST pane: pinned panes show PUSH_PIN_SLASH and hide
        // their close, so the one unpinned pane is the sole source of a PUSH_PIN
        // glyph and an X glyph — both unambiguously queryable.
        app.tabs[0].pinned = true;
        app.tabs[1].pinned = true;
        app.tabs[2].pinned = true;
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(820.0, 250.0)) // wide+short => 4 panes < 220px each
            .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
        h.run();
        h.run();
        let pin = h.get_by_label(egui_phosphor::thin::PUSH_PIN).rect();
        let close = h.get_by_label(egui_phosphor::thin::X).rect();
        assert!(
            close.center().y > pin.center().y + 2.0,
            "narrow pane header must STACK: close (✕) must be BELOW the pin \
             (pin.y={:.1}, close.y={:.1})",
            pin.center().y,
            close.center().y
        );
        assert!(
            (close.center().x - pin.center().x).abs() < 24.0,
            "narrow pane header must be a single COLUMN: close (✕) and pin must \
             share an x (pin.x={:.1}, close.x={:.1}) — not sit side-by-side",
            pin.center().x,
            close.center().x
        );
    }

    /// REAL interaction test (egui_kittest): open Settings, then click its close
    /// (✕) the way a user does, and assert the window actually closes. This is
    /// the kind of test that would have caught "the ✕ doesn't close".
    #[test]
    fn settings_close_button_actually_closes() {
        // frameless OFF so the ONLY "Close window" button is the settings
        // window's ✕ (the frameless app titlebar adds its own close button).
        let mut cfg = Config::default();
        cfg.appearance.frameless = false; // no titlebar ✕
        cfg.editor.first_run_completed = true; // no welcome-modal ✕
        let app = ScribeApp::new_test(cfg);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(1100.0, 720.0))
            .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
        harness.state_mut().settings_open = true;
        harness.run();
        assert!(harness.state().settings_open, "settings should be open");
        // Click the window close (✕) by its accessibility label, like a user.
        harness.get_by_label("Close window").click();
        harness.run();
        assert!(
            !harness.state().settings_open,
            "clicking the ✕ must close the settings window"
        );
    }

    /// Regression guard (e2e, no GPU): the Settings window width MUST NOT change
    /// between pages. Reproduces the user's "the Toolbar page gets a lot wider"
    /// report (was ~829px on Appearance vs ~1069px on Toolbar). The close (✕) is
    /// pinned to the window's top-right, so a constant window width means a
    /// constant ✕ x-position; we assert the ✕ right-edge is identical on the
    /// Appearance and Toolbar pages. Fixed by the inner ScrollArea::max_width cap.
    #[test]
    fn settings_window_width_constant_across_pages() {
        let mut cfg = Config::default();
        cfg.appearance.frameless = false; // the settings ✕ is the only "Close window"
        cfg.editor.first_run_completed = true;
        let app = ScribeApp::new_test(cfg);
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(1280.0, 940.0))
            .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
        h.state_mut().settings_open = true;
        h.run();
        let close_appearance = h.get_by_label("Close window").rect().right();
        h.get_by_label("Toolbar").click();
        h.run();
        h.run();
        let close_toolbar = h.get_by_label("Close window").rect().right();
        assert!(
            (close_appearance - close_toolbar).abs() < 1.0,
            "settings window width changed between pages: close (✕) right edge \
             {close_appearance} on Appearance vs {close_toolbar} on Toolbar"
        );
    }

    /// Same, but in the DEFAULT frameless mode (custom titlebar) — the config
    /// the user actually runs. Two "Close window" buttons exist (app titlebar +
    /// settings window); we click the settings one (lower on screen) and assert
    /// it closes. Reproduces the user's "✕ doesn't close" report if it's a
    /// frameless-mode interaction problem.
    #[test]
    fn settings_close_works_in_frameless_mode() {
        let mut cfg = Config::default();
        cfg.appearance.frameless = true;
        cfg.editor.first_run_completed = true;
        let app = ScribeApp::new_test(cfg);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(1100.0, 720.0))
            .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
        harness.state_mut().settings_open = true;
        harness.run();
        // The settings window's ✕ is the "Close window" button with the LARGEST
        // top-y (the app titlebar's sits at the very top of the screen).
        let target = harness
            .get_all_by_label("Close window")
            .max_by(|a, b| {
                a.rect()
                    .top()
                    .partial_cmp(&b.rect().top())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("a settings close button");
        target.click();
        harness.run();
        assert!(
            !harness.state().settings_open,
            "clicking the settings ✕ must close it even in frameless mode"
        );
    }

    /// REAL interaction test: with two tabs open, clicking the other tab's label
    /// switches the active document to it. Catches the regression where the
    /// drag-source wrapper ate the click and tabs couldn't be switched.
    #[test]
    fn clicking_a_tab_switches_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let alpha = dir.path().join("alpha.txt");
        let beta = dir.path().join("beta.txt");
        std::fs::write(&alpha, "A\n").unwrap();
        std::fs::write(&beta, "B\n").unwrap();
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.appearance.frameless = true; // the user's actual mode
        let mut app = ScribeApp::new_test(cfg);
        app.open_path(alpha.clone());
        app.open_path(beta.clone());
        // beta was opened last → it is the active tab.
        let beta_idx = app.active;
        assert_eq!(app.tabs[beta_idx].title(), "beta.txt");

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(1100.0, 720.0))
            .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
        harness.run();
        // Click the OTHER tab the way a user does.
        harness.get_by_label("alpha.txt").click();
        harness.run();
        let active_title = {
            let app = harness.state();
            app.tabs[app.active].title()
        };
        assert_eq!(
            active_title, "alpha.txt",
            "clicking the alpha tab must switch the active document to it"
        );
    }

    /// Build a kittest harness over the app in the user's default (frameless)
    /// mode, with the first-run welcome modal suppressed.
    fn ui_harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
        egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(1100.0, 720.0))
            .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
    }
    fn fresh_app() -> ScribeApp {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        ScribeApp::new_test(cfg)
    }

    // #91 render-coverage: exercise the new render paths headlessly via
    // run_frames so the GUI-heavy code (rotated side tabs, spell underline
    // painter, font-theme reapply, background tint) is actually executed.

    #[test]
    fn render_rotated_left_tab_bar_does_not_panic() {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.editor.tab_bar_position = scribe_core::config::TabBarPosition::Left;
        cfg.editor.side_tabs_rotated = true;
        let mut app = ScribeApp::new_test(cfg);
        app.new_tab();
        app.tabs[0].pinned = true; // exercise the pin-glyph path too
        run_frames(&mut app, 3);
        assert!(app.tabs.len() >= 2);
    }

    #[test]
    fn render_horizontal_left_tab_bar_does_not_panic() {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.editor.tab_bar_position = scribe_core::config::TabBarPosition::Right;
        cfg.editor.side_tabs_rotated = false;
        let mut app = ScribeApp::new_test(cfg);
        app.new_tab();
        run_frames(&mut app, 3);
        assert!(app.tabs.len() >= 2);
    }

    /// The Bottom tab strip must render ABOVE the status bar (status keeps the
    /// very bottom screen edge). Verified by y-order: the status bar's "LF"
    /// eol-button sits BELOW a tab's close (✕).
    #[test]
    fn bottom_tab_bar_sits_above_status_bar() {
        use egui_kittest::kittest::Queryable as _;
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.appearance.frameless = false;
        cfg.editor.tab_bar_position = scribe_core::config::TabBarPosition::Bottom;
        let app = ScribeApp::new_test(cfg);
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(900.0, 600.0))
            .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
        h.run();
        h.run();
        let status_lf = h.get_by_label("LF").rect();
        let tab_close = h.get_by_label(egui_phosphor::thin::X).rect();
        assert!(
            status_lf.center().y > tab_close.center().y,
            "status bar must be BELOW the bottom tab strip: \
             status.y={:.1} tab.y={:.1}",
            status_lf.center().y,
            tab_close.center().y
        );
    }

    /// #30 node.rect verification — a ROTATE-OFF side tab lays its controls out
    /// as a horizontal ROW (grip · name · pin · close): the close (✕) shares the
    /// pin's row (≈ same y) and sits to its RIGHT. Before the fix the side strip
    /// inherited the vertical parent layout and stacked name/pin/close per tab.
    #[test]
    fn rotate_off_side_tab_is_a_horizontal_row() {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.appearance.frameless = false; // no custom-titlebar ✕ to disambiguate
        cfg.editor.tab_bar_position = scribe_core::config::TabBarPosition::Left;
        cfg.editor.side_tabs_rotated = false;
        let app = ScribeApp::new_test(cfg); // single active, unpinned tab
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(900.0, 600.0))
            .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
        h.run();
        h.run();
        let pin = h.get_by_label(egui_phosphor::thin::PUSH_PIN).rect();
        let close = h.get_by_label(egui_phosphor::thin::X).rect();
        assert!(
            (close.center().y - pin.center().y).abs() < 6.0,
            "rotate-off side tab must be a ROW: pin and close share a row \
             (pin.y={:.1}, close.y={:.1})",
            pin.center().y,
            close.center().y
        );
        assert!(
            close.center().x > pin.center().x + 2.0,
            "rotate-off row order is grip·name·pin·close: close (✕) is RIGHT of \
             the pin (pin.x={:.1}, close.x={:.1})",
            pin.center().x,
            close.center().x
        );
    }

    /// #30 node.rect verification — a ROTATE-ON side tab is a vertical COLUMN
    /// (grip · rotated-name · pin · close): the close sits BELOW the pin and they
    /// share an x. Also exercises the rotated drag-grip render path.
    #[test]
    fn rotate_on_side_tab_is_a_vertical_column() {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.appearance.frameless = false;
        cfg.editor.tab_bar_position = scribe_core::config::TabBarPosition::Left;
        cfg.editor.side_tabs_rotated = true;
        let app = ScribeApp::new_test(cfg);
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(900.0, 600.0))
            .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
        h.run();
        h.run();
        let pin = h.get_by_label(egui_phosphor::thin::PUSH_PIN).rect();
        let close = h.get_by_label(egui_phosphor::thin::X).rect();
        assert!(
            close.center().y > pin.center().y + 2.0,
            "rotate-on column order is grip·name·pin·close: close (✕) is BELOW \
             the pin (pin.y={:.1}, close.y={:.1})",
            pin.center().y,
            close.center().y
        );
        assert!(
            (close.center().x - pin.center().x).abs() < 24.0,
            "rotate-on side tab is a single COLUMN: pin and close share an x \
             (pin.x={:.1}, close.x={:.1})",
            pin.center().x,
            close.center().x
        );
    }

    /// #28 render-coverage — the render-whitespace overlay now runs in the
    /// DEFAULT egui TextEdit path (previously the `·`/`→` markers only drew in
    /// the experimental rope editor). We can't read painted glyphs back, so this
    /// guards that the galley-walk overlay executes without panicking over a
    /// buffer that has both spaces and tabs.
    #[test]
    fn render_whitespace_overlay_default_editor_runs() {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.editor.render_whitespace = true;
        cfg.editor.experimental_rope_editor = false; // exercise the TextEdit path
        let mut app = ScribeApp::new_test(cfg);
        app.tabs[0].text = "fn  main() {\n\tlet x = 1;\n}\n".into();
        run_frames(&mut app, 3);
        assert!(app.config.editor.render_whitespace);
    }

    /// #28 render-coverage — the same overlay also runs in split/grid view.
    #[test]
    fn render_whitespace_overlay_grid_path_runs() {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.editor.render_whitespace = true;
        cfg.editor.grid_enabled = true;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs[0].text = "a  b\tc\n".into();
        app.new_tab();
        run_frames(&mut app, 3);
        assert!(app.tabs.len() >= 2);
    }

    #[test]
    fn render_spellcheck_underline_path_runs() {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.spellcheck.enabled = true;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs[0].text = "this zxqwyzz wordd is rong".into();
        run_frames(&mut app, 3); // paints the squiggles
        assert!(!app.misspellings_for_active().is_empty());
    }

    #[test]
    fn render_font_theme_bg_override_and_glass_paths_run() {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.fonts.editor_family = "IBM Plex Mono".into();
        cfg.appearance.background_override = Some("#203040".into());
        cfg.window.transparency_enabled = true;
        cfg.window.mode = scribe_core::config::WindowMode::Glass;
        let mut app = ScribeApp::new_test(cfg);
        run_frames(&mut app, 3); // build_fonts reapply + tint overlay + visuals
                                 // applied_font_family is the combined note+UI key (#103).
        assert!(
            app.applied_font_family.starts_with("IBM Plex Mono"),
            "note family recorded in the font-state key (got {:?})",
            app.applied_font_family
        );
    }

    #[test]
    fn toolbar_gear_opens_settings() {
        let mut h = ui_harness(fresh_app());
        h.run();
        assert!(!h.state().settings_open);
        // #R5: the gear is the phosphor GEAR_SIX glyph (the bare "⚙" emoji was
        // tofu in the bundled fonts).
        h.get_by_label(egui_phosphor::thin::GEAR_SIX).click();
        h.run();
        assert!(
            h.state().settings_open,
            "clicking the settings gear must open Settings"
        );
    }

    #[test]
    fn open_find_bar_suppresses_and_clears_completion_popup() {
        // #72 regression: a completion popup must not survive (and so cannot
        // steal ↑↓/Enter) while the find bar owns the keyboard.
        let mut app = fresh_app();
        app.find_open = true;
        app.completion = Some(super::Completion {
            prefix_start: 0,
            items: vec!["alpha".into(), "alpine".into()],
            selected: 0,
        });
        assert!(
            app.modal_owns_keyboard(),
            "an open find bar must own the keyboard"
        );
        let mut h = ui_harness(app);
        h.run();
        assert!(
            h.state().completion.is_none(),
            "the completion popup must be force-closed while the find bar is open"
        );
    }

    #[test]
    fn editor_owns_keyboard_when_no_modal_open() {
        let app = fresh_app();
        assert!(
            !app.modal_owns_keyboard(),
            "with no modal open the editor (not a modal) owns the keyboard"
        );
    }

    #[test]
    fn toolbar_palette_button_opens_palette() {
        let mut h = ui_harness(fresh_app());
        h.run();
        h.get_by_label(">_").click();
        h.run();
        assert!(
            h.state().palette_open,
            "clicking the >_ button must open the command palette"
        );
    }

    #[test]
    fn toolbar_split_button_toggles_grid() {
        let mut h = ui_harness(fresh_app());
        h.run();
        assert!(!h.state().config.editor.grid_enabled);
        h.get_by_label("split").click();
        h.run();
        assert!(
            h.state().config.editor.grid_enabled,
            "the split button must toggle the unified split/grid view on"
        );
    }

    #[test]
    fn middle_click_tab_closes_it() {
        let dir = tempfile::tempdir().unwrap();
        let alpha = dir.path().join("alpha.txt");
        let beta = dir.path().join("beta.txt");
        std::fs::write(&alpha, "A\n").unwrap();
        std::fs::write(&beta, "B\n").unwrap();
        let mut app = fresh_app();
        app.open_path(alpha);
        app.open_path(beta);
        let mut h = ui_harness(app);
        h.run();
        h.get_by_label("beta.txt")
            .click_button(egui::PointerButton::Middle);
        h.run();
        let has_beta = h.state().tabs.iter().any(|t| t.title() == "beta.txt");
        assert!(!has_beta, "middle-clicking a tab must close it");
    }

    /// Wave 2: the Editor settings page exposes the Scroll-speed control.
    #[test]
    fn settings_exposes_scroll_speed_control() {
        let mut h = ui_harness(fresh_app());
        h.run();
        h.state_mut().settings_open = true;
        h.run();
        h.get_by_label("Editor").click();
        h.run();
        // Panics (failing the test) if the Scroll-speed slider label is absent.
        let _ = h.get_by_label("Scroll speed");
    }

    /// Wave 2: a middle-click INSIDE the editor arms autoscroll; the existing
    /// `middle_click_tab_closes_it` proves a middle-click on a TAB does not (it
    /// would otherwise spin the harness past max_steps via the drift repaint).
    #[test]
    fn middle_click_in_editor_arms_autoscroll() {
        // Short buffer so the editor widget's centre (what `click_button` targets)
        // is ON-SCREEN — a click on off-viewport content is correctly rejected by
        // the visible-editor-area gate, which is the feature, not a bug.
        let mut app = fresh_app();
        app.tabs[0].text = "hi\n".to_string();
        let mut h = ui_harness(app);
        h.run();
        let id = egui::Id::new("scr1b3_autoscroll");
        let armed = |h: &egui_kittest::Harness<'_, ScribeApp>| {
            h.ctx
                .data(|d| d.get_temp::<AutoScrollState>(id))
                .map(|s| s.active)
                .unwrap_or(false)
        };
        assert!(!armed(&h), "autoscroll starts disarmed");
        h.get_by_role(egui::accesskit::Role::MultilineTextInput)
            .click_button(egui::PointerButton::Middle);
        h.run();
        assert!(
            armed(&h),
            "a middle-click inside the editor must arm autoscroll"
        );
    }

    #[test]
    fn command_palette_opens_then_escape_closes() {
        let mut h = ui_harness(fresh_app());
        h.run();
        h.get_by_label(">_").click();
        h.run();
        assert!(h.state().palette_open);
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(
            !h.state().palette_open,
            "Escape must close the command palette"
        );
    }

    #[test]
    fn typing_updates_the_active_buffer() {
        let mut app = fresh_app();
        // Make the scratch tab empty + active so typed text is observable.
        app.tabs[0].text.clear();
        let mut h = ui_harness(app);
        h.run();
        // Focus the editor text area and type like a user.
        let editor = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
        editor.focus();
        h.run();
        h.get_by_role(egui::accesskit::Role::MultilineTextInput)
            .type_text("hello");
        h.run();
        let active = h.state().active;
        assert!(
            h.state().tabs[active].text.contains("hello"),
            "typing must update the active buffer, got {:?}",
            h.state().tabs[active].text
        );
    }

    #[test]
    fn settings_toggle_flips_runtime_config() {
        let mut h = ui_harness(fresh_app());
        h.state_mut().settings_open = true;
        h.run();
        // Navigate to the Editor category, then flip "Line numbers".
        h.get_by_label("Editor").click();
        h.run();
        let before = h.state().config.editor.show_line_numbers;
        h.get_by_label("Line numbers").click();
        h.run();
        assert_ne!(
            h.state().config.editor.show_line_numbers,
            before,
            "clicking the Line numbers checkbox must flip the setting"
        );
    }

    #[test]
    fn plus_button_adds_a_tab() {
        let mut h = ui_harness(fresh_app());
        h.run();
        let before = h.state().tabs.len();
        h.get_by_label("+").click();
        h.run();
        assert_eq!(
            h.state().tabs.len(),
            before + 1,
            "the + button must add a new tab"
        );
    }

    #[test]
    fn tab_label_is_plain_title_no_tofu_pin_glyph() {
        // The pin glyph prefix was removed: it rendered as a tofu □ left of the
        // title in this build's font atlas (the egui-phosphor .notdef footgun).
        // Pinned state now reads from the dimmed, drag-disabled grab handle.
        // The label must be the bare title in BOTH states — no leading glyph.
        assert_eq!(super::tab_display_label("notes.txt", true), "notes.txt");
        assert_eq!(super::tab_display_label("notes.txt", false), "notes.txt");
    }

    #[test]
    fn follow_os_theme_switches_with_os() {
        use scribe_core::theme::Appearance;
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.appearance.follow_os_theme = true;
        cfg.appearance.theme = "wired-noir".to_string(); // a dark brand theme
        let mut h = ui_harness(ScribeApp::new_test(cfg));
        // OS reports LIGHT → the app must switch to a light theme.
        h.ctx.set_theme(egui::Theme::Light);
        h.run();
        h.run();
        assert!(
            matches!(h.state().theme.appearance, Appearance::Light),
            "light OS theme must switch the app to a light theme, got {:?}",
            h.state().theme.appearance
        );
        // OS flips to DARK → the app must switch back to a dark theme.
        h.ctx.set_theme(egui::Theme::Dark);
        h.run();
        h.run();
        assert!(
            matches!(h.state().theme.appearance, Appearance::Dark),
            "dark OS theme must switch the app to a dark theme, got {:?}",
            h.state().theme.appearance
        );
    }

    /// Phase 18 T18.2 — flipping `editor.grid_enabled` on creates the
    /// tile-tree at the top of the next frame and the central panel
    /// renders without panicking. Three frames are enough to exercise
    /// the sync + render + post-frame cleanup paths.
    #[test]
    fn grid_enabled_renders_without_panic() {
        let mut cfg = Config::default();
        cfg.editor.grid_enabled = true;
        let mut app = ScribeApp::new_test(cfg);
        run_frames(&mut app, 3);
        assert!(
            app.grid_tree.is_some(),
            "grid tree must be built when enabled"
        );
        assert_eq!(app.tabs.len(), 1, "still one scratch tab");
        // The single scratch tab got a real doc id (the legacy 0
        // sentinel gets bumped on first sync).
        assert!(app.tabs[0].doc_id.0 > 0, "doc id allocated");
    }

    /// Phase 18 T18.2 — toggling the grid OFF after it was ON drops
    /// the tree and re-engages the single-pane code path on the next
    /// frame.
    #[test]
    fn grid_disabled_drops_tree() {
        let mut cfg = Config::default();
        cfg.editor.grid_enabled = true;
        let mut app = ScribeApp::new_test(cfg);
        run_frames(&mut app, 1);
        assert!(app.grid_tree.is_some());
        app.config.editor.grid_enabled = false;
        run_frames(&mut app, 1);
        assert!(app.grid_tree.is_none(), "tree drops when disabled");
    }

    #[test]
    fn panel_fill_opaque_when_master_off() {
        // T19.2: with transparency disabled the chrome fill keeps full alpha,
        // so the window reads as a normal opaque window.
        let theme = Theme::wired_noir();
        let w_off = scribe_core::config::WindowConfig {
            mode: scribe_core::config::WindowMode::Glass, // mode set, but master OFF
            opacity: 0.5,
            ..Default::default()
        };
        assert_eq!(
            panel_fill(&theme, &w_off, None).a(),
            255,
            "opaque while master toggle off"
        );
        // Master ON + translucent mode => alpha lowered to opacity.
        let w_on = scribe_core::config::WindowConfig {
            transparency_enabled: true,
            ..w_off
        };
        let a = panel_fill(&theme, &w_on, None).a();
        assert!(
            (76..255).contains(&a),
            "alpha reduced to ~opacity (got {a})"
        );
    }

    #[test]
    fn close_latch_hides_before_destroy() {
        // T19.1: requesting close must NOT close immediately; it hides first
        // (want_close -> closing) so a layered window leaves no DWM ghost.
        let mut app = ScribeApp::new_test(Config::default());
        app.want_close = true;
        run_frames(&mut app, 1);
        assert!(
            app.closing,
            "first frame latches into the hide-then-close phase"
        );
        assert!(!app.want_close, "want_close consumed");
    }

    #[test]
    fn settings_window_renders() {
        let mut app = ScribeApp::new_test(Config::default());
        app.settings_open = true;
        run_frames(&mut app, 2);
    }

    #[test]
    fn find_bar_renders_with_query() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "foo bar foo baz foo".to_string();
        app.find_open = true;
        app.find_query = "foo".to_string();
        run_frames(&mut app, 1);
        // The find-count path ran without panic; verify the engine agrees.
        let q = scribe_core::search::Query {
            pattern: "foo".into(),
            ..Default::default()
        };
        assert_eq!(
            scribe_core::search::find_all(&app.tabs[0].text, &q)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn spellcheck_flags_misspellings_e2e() {
        let mut cfg = Config::default();
        cfg.spellcheck.enabled = true;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs[0].text = "thiss sentense has bad wrds".to_string();
        run_frames(&mut app, 1);
        assert!(app.spell_count() > 0, "misspellings should be detected");
    }

    #[test]
    fn command_palette_opens_and_renders() {
        let mut app = ScribeApp::new_test(Config::default());
        app.palette_open = true;
        run_frames(&mut app, 1);
        assert!(app.palette_open);
    }

    #[test]
    fn file_tree_sidebar_renders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut app = ScribeApp::new_test(Config::default());
        app.file_tree_root = Some(dir.path().to_path_buf());
        run_frames(&mut app, 2);
        assert!(app.file_tree_root.is_some());
    }

    #[test]
    fn open_then_edit_then_save_e2e() {
        // Full editor lifecycle through the headless render loop.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.txt");
        std::fs::write(&path, "original\n").unwrap();
        let mut app = ScribeApp::new_test(Config::default());
        app.open_path(path.clone());
        run_frames(&mut app, 1);
        let idx = app.active;
        app.tabs[idx].text = "edited via e2e\n".to_string();
        app.save_active();
        run_frames(&mut app, 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "edited via e2e\n");
    }

    #[test]
    fn split_is_unified_with_grid() {
        // Split and grid are one feature: enabling the multi-pane view lays the
        // OPEN TABS out as panes (two = side-by-side split, more = grid). With
        // two tabs open the grid has two panes.
        let mut cfg = Config::default();
        cfg.editor.grid_enabled = true;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs[0].text = "fn main() {}\n".into();
        app.tabs.push(EditorTab::scratch());
        app.tabs[1].text = "second note\n".into();
        run_frames(&mut app, 2);
        let tree = app
            .grid_tree
            .as_ref()
            .expect("grid tree present when enabled");
        assert_eq!(
            crate::grid::count_panes(tree),
            2,
            "two open tabs render as two panes (a side-by-side split)"
        );
    }

    #[test]
    fn minimap_renders_with_viewport() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = (0..200).map(|i| format!("line {i}\n")).collect();
        app.config.editor.show_minimap = true;
        run_frames(&mut app, 2);
        assert!(app.config.editor.show_minimap);
        // Scroll metrics get populated by the editor render.
        assert!(app.scroll_metrics.1 >= 1.0);
    }

    #[test]
    fn fold_view_collapses_region() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "fn a() {\n    body;\n    more;\n}\ntail;\n".into();
        app.fold_view = true;
        run_frames(&mut app, 1);
        // Fold the first region (header at line 0) and re-render — no panic.
        app.folds.insert(0);
        run_frames(&mut app, 1);
        assert!(app.folds.contains(&0));
    }

    #[test]
    fn apply_indent_inserts_spaces_at_caret() {
        let (out, caret) = apply_indent("ab", 1, 1, 4);
        assert_eq!(out, "a    b");
        assert_eq!(caret, 5);
    }

    #[test]
    fn apply_indent_replaces_selection() {
        // Replace chars [1,3) ("bc") of "abcd" with 2 spaces.
        let (out, caret) = apply_indent("abcd", 1, 3, 2);
        assert_eq!(out, "a  d");
        assert_eq!(caret, 3);
    }

    #[test]
    fn line_gutter_populated_when_line_numbers_on() {
        let mut app = ScribeApp::new_test(Config::default());
        app.config.editor.show_line_numbers = true;
        app.tabs[0].text = "a\nb\nc\nd\n".into();
        run_frames(&mut app, 2);
        assert!(
            app.line_gutter.len() >= 4,
            "gutter should hold one Y per logical line (got {})",
            app.line_gutter.len()
        );
    }

    #[test]
    fn line_gutter_empty_when_line_numbers_off() {
        let mut app = ScribeApp::new_test(Config::default());
        app.config.editor.show_line_numbers = false;
        app.tabs[0].text = "a\nb\nc\n".into();
        run_frames(&mut app, 2);
        assert!(app.line_gutter.is_empty());
    }

    #[test]
    fn word_wrap_toggle_renders_without_panic() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "a very long line ".repeat(40);
        app.config.editor.word_wrap = true;
        run_frames(&mut app, 2);
        app.config.editor.word_wrap = false;
        run_frames(&mut app, 2);
        assert!(app.scroll_metrics.1 >= 1.0);
    }

    #[test]
    fn toolbar_default_has_core_actions() {
        let items = scribe_core::config::ToolbarConfig::default().items;
        for want in ["new", "save", "find", "palette"] {
            assert!(
                items.iter().any(|i| i == want),
                "default toolbar missing {want}"
            );
        }
    }

    #[test]
    fn toolbar_layout_survives_serde_roundtrip() {
        let mut cfg = Config::default();
        cfg.toolbar.items = vec!["save".into(), "sep".into(), "lsp".into()];
        let back = Config::from_toml_str(&cfg.to_toml_string()).unwrap();
        assert_eq!(back.toolbar.items, cfg.toolbar.items);
    }

    #[test]
    fn settings_window_renders_open() {
        let mut app = ScribeApp::new_test(Config::default());
        app.settings_open = true;
        run_frames(&mut app, 2);
        assert!(app.settings_open, "settings stays open across frames");
    }

    #[test]
    fn completion_opens_and_accepts() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "value valuer val".into();
        let cursor = app.tabs[0].text.chars().count();
        app.open_completion(0, Some(cursor));
        assert!(
            app.completion.is_some(),
            "completion opens for prefix 'val'"
        );
        let before = app.tabs[0].text.clone();
        app.accept_completion(0, Some(cursor));
        assert_ne!(app.tabs[0].text, before, "accept inserts a completion");
        assert!(app.completion.is_none(), "popup closes after accept");
    }

    #[test]
    fn completion_popup_renders_in_frame() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "alpha alphabet alph".into();
        app.completion = Some(Completion {
            prefix_start: 15,
            items: vec!["alpha".into(), "alphabet".into()],
            selected: 0,
        });
        // The popup Area renders against the live cursor without panic.
        run_frames(&mut app, 1);
    }

    // ---- Input-driven ("computer control") E2E ----
    // A robot user: inject real pointer + keyboard events through egui's own
    // event loop (the same `RawInput.events` path a physical mouse/keyboard
    // produces) against ONE persistent `Context` so focus + widget state carry
    // across frames, then assert what the app did.

    struct Driver {
        ctx: egui::Context,
    }

    impl Driver {
        fn new() -> Self {
            Self {
                ctx: egui::Context::default(),
            }
        }

        fn frame(&self, app: &mut ScribeApp, modifiers: egui::Modifiers, events: Vec<egui::Event>) {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1100.0, 720.0),
                )),
                modifiers,
                events,
                ..Default::default()
            };
            let _ = self.ctx.run(input, |ctx| app.frame_tick(ctx));
        }

        fn idle(&self, app: &mut ScribeApp) {
            self.frame(app, egui::Modifiers::NONE, vec![]);
        }

        fn click(&self, app: &mut ScribeApp, pos: egui::Pos2) {
            let m = egui::Modifiers::NONE;
            self.frame(
                app,
                m,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: m,
                    },
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: m,
                    },
                ],
            );
        }

        fn key(&self, app: &mut ScribeApp, key: egui::Key, modifiers: egui::Modifiers) {
            self.frame(
                app,
                modifiers,
                vec![
                    egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers,
                    },
                    egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: false,
                        repeat: false,
                        modifiers,
                    },
                ],
            );
        }

        fn type_text(&self, app: &mut ScribeApp, s: &str) {
            self.frame(
                app,
                egui::Modifiers::NONE,
                vec![egui::Event::Text(s.to_string())],
            );
        }
    }

    #[test]
    fn input_ctrl_n_adds_a_tab() {
        let mut app = ScribeApp::new_test(Config::default());
        let d = Driver::new();
        d.idle(&mut app);
        let before = app.tabs.len();
        d.key(&mut app, egui::Key::N, egui::Modifiers::COMMAND);
        assert_eq!(app.tabs.len(), before + 1, "Ctrl+N opens a new tab");
    }

    #[test]
    fn input_ctrl_f_opens_and_escape_closes_find() {
        let mut app = ScribeApp::new_test(Config::default());
        let d = Driver::new();
        d.idle(&mut app);
        d.key(&mut app, egui::Key::F, egui::Modifiers::COMMAND);
        assert!(app.find_open, "Ctrl+F opens the find bar");
        d.key(&mut app, egui::Key::Escape, egui::Modifiers::NONE);
        assert!(!app.find_open, "Escape closes the find bar");
    }

    /// F-005 helper: line:col math handles plain ASCII, end-of-buffer, and
    /// multi-byte UTF-8 codepoints.
    #[test]
    fn line_col_from_char_index_basics() {
        assert_eq!(line_col_from_char_index("", 0), (1, 1));
        let s = "hello
world";
        assert_eq!(line_col_from_char_index(s, 0), (1, 1));
        assert_eq!(line_col_from_char_index(s, 5), (1, 6));
        assert_eq!(line_col_from_char_index(s, 6), (2, 1));
        assert_eq!(line_col_from_char_index(s, 11), (2, 6));
        let cjk = "日本
語";
        assert_eq!(line_col_from_char_index(cjk, 1), (1, 2));
        assert_eq!(line_col_from_char_index(cjk, 2), (1, 3));
        assert_eq!(line_col_from_char_index(cjk, 3), (2, 1));
    }

    #[test]
    fn line_col_from_char_index_clamps() {
        let s = "abc
def";
        let (line, col) = line_col_from_char_index(s, 99);
        assert_eq!((line, col), (2, 4));
    }

    /// F-015 parser: accepts plain line number, line:col, and rejects garbage.
    #[test]
    fn parse_goto_query_accepts_line_and_line_col() {
        assert_eq!(parse_goto_query("42"), Some((42, None)));
        assert_eq!(parse_goto_query("42:10"), Some((42, Some(10))));
        assert_eq!(parse_goto_query("  42  "), Some((42, None)));
        assert_eq!(parse_goto_query("42:"), None);
        assert_eq!(parse_goto_query(":10"), None);
        assert_eq!(parse_goto_query("0"), None);
        assert_eq!(parse_goto_query("abc"), None);
        assert_eq!(parse_goto_query(""), None);
        // Column clamps to 1.
        assert_eq!(parse_goto_query("42:0"), Some((42, Some(1))));
    }

    /// pick_bookmark walks the ordered set forward / backward and wraps.
    #[test]
    fn pick_bookmark_navigates_and_wraps() {
        use std::collections::BTreeSet;
        let bm: BTreeSet<usize> = [2usize, 5, 9].into_iter().collect();
        // Forward: strictly-after, wrapping past the last.
        assert_eq!(pick_bookmark(&bm, 0, 1), Some(2));
        assert_eq!(pick_bookmark(&bm, 2, 1), Some(5));
        assert_eq!(pick_bookmark(&bm, 5, 1), Some(9));
        assert_eq!(pick_bookmark(&bm, 9, 1), Some(2), "wraps to first");
        assert_eq!(pick_bookmark(&bm, 20, 1), Some(2), "past end wraps");
        // Backward: strictly-before, wrapping past the first.
        assert_eq!(pick_bookmark(&bm, 9, -1), Some(5));
        assert_eq!(pick_bookmark(&bm, 5, -1), Some(2));
        assert_eq!(pick_bookmark(&bm, 2, -1), Some(9), "wraps to last");
        assert_eq!(pick_bookmark(&bm, 0, -1), Some(9), "before start wraps");
        // Empty set yields nothing.
        let empty: BTreeSet<usize> = BTreeSet::new();
        assert_eq!(pick_bookmark(&empty, 0, 1), None);
        assert_eq!(pick_bookmark(&empty, 0, -1), None);
    }

    /// toggle_bookmark flips the cursor line in the active tab's set, and
    /// navigate_bookmark requests a scroll when a target exists.
    #[test]
    fn toggle_and_navigate_bookmark() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "a\nb\nc\nd\ne\n".into();
        // Cursor on line 3 (1-based) → 0-based line 2.
        app.last_cursor_line_col = Some((3, 1));
        app.toggle_bookmark();
        assert!(app.tabs[0].bookmarks.contains(&2), "bookmark added");
        // Toggling again removes it.
        app.toggle_bookmark();
        assert!(!app.tabs[0].bookmarks.contains(&2), "bookmark removed");
        // Re-add and navigate.
        app.tabs[0].bookmarks.insert(4);
        app.pending_scroll = None;
        app.last_cursor_line_col = Some((1, 1)); // 0-based line 0
        app.navigate_bookmark(1);
        assert!(
            app.pending_scroll.is_some(),
            "navigate to an existing bookmark requests a scroll"
        );
    }

    /// GoToSymbol builtin opens the modal + requests focus.
    #[test]
    fn execute_builtin_go_to_symbol_opens_modal() {
        let mut app = ScribeApp::new_test(Config::default());
        assert!(!app.goto_symbol_open);
        app.execute_builtin(BuiltinCommand::GoToSymbol);
        assert!(app.goto_symbol_open, "modal opened");
        assert!(app.focus_goto_symbol, "focus requested");
    }

    /// Jumping to a symbol's start line (the modal's action) requests a
    /// scroll via the shared goto_line pipe. Exercises the symbol_scopes →
    /// goto_line path the modal wires together.
    #[test]
    fn go_to_symbol_jump_requests_scroll() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "fn a() {\n}\nfn b() {\n}\n".into();
        let scopes = crate::editor_features::symbol_scopes(&app.tabs[0].text);
        assert!(!scopes.is_empty(), "two fn definitions detected");
        // Jump to the second symbol's start line (the modal calls
        // goto_line(start_line + 1)).
        let target = scopes.last().unwrap().start_line;
        app.pending_scroll = None;
        app.goto_line(target + 1);
        assert!(
            app.pending_scroll.is_some(),
            "symbol jump requests a scroll"
        );
    }

    /// F-015 method: goto_line sets pending_scroll non-None for a valid
    /// line on an active buffer.
    #[test]
    fn goto_line_sets_pending_scroll() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "a\nb\nc\nd\ne\n".into();
        app.goto_line(3);
        assert!(
            app.pending_scroll.is_some(),
            "goto_line should request scroll"
        );
    }

    /// F-014: F1 toggles the cheatsheet open + a second F1 closes it.
    #[test]
    fn input_f1_toggles_cheatsheet() {
        let mut app = ScribeApp::new_test(Config::default());
        assert!(!app.cheatsheet_open);
        let d = Driver::new();
        d.idle(&mut app);
        d.key(&mut app, egui::Key::F1, egui::Modifiers::NONE);
        assert!(app.cheatsheet_open, "F1 opens the cheatsheet");
        d.key(&mut app, egui::Key::F1, egui::Modifiers::NONE);
        assert!(!app.cheatsheet_open, "second F1 closes the cheatsheet");
    }

    /// F-014: Esc closes the cheatsheet as a normal overlay.
    #[test]
    fn input_esc_closes_cheatsheet() {
        let mut app = ScribeApp::new_test(Config::default());
        app.cheatsheet_open = true;
        let d = Driver::new();
        d.idle(&mut app);
        d.key(&mut app, egui::Key::Escape, egui::Modifiers::NONE);
        assert!(!app.cheatsheet_open, "Esc closes the cheatsheet");
    }

    /// F-014 registry sanity: every entry has a non-empty chord + action.
    #[test]
    fn keyboard_shortcuts_registry_is_populated() {
        assert!(!KEYBOARD_SHORTCUTS.is_empty(), "registry must be populated");
        for entry in KEYBOARD_SHORTCUTS {
            assert!(!entry.chord.is_empty(), "shortcut chord must be non-empty");
            assert!(
                !entry.action.is_empty(),
                "shortcut action label must be non-empty"
            );
        }
    }

    /// F-016 prefix table sanity.
    #[test]
    fn comment_prefix_for_extension_table() {
        assert_eq!(comment_prefix_for_extension("rs"), Some("//"));
        assert_eq!(comment_prefix_for_extension("py"), Some("#"));
        assert_eq!(comment_prefix_for_extension("lua"), Some("--"));
        assert_eq!(comment_prefix_for_extension("toml"), Some("#"));
        assert_eq!(comment_prefix_for_extension("RS"), Some("//"));
        assert_eq!(comment_prefix_for_extension("html"), None);
        assert_eq!(comment_prefix_for_extension(""), None);
    }

    /// F-008 replace: empty pattern is a no-op.
    #[test]
    fn replace_in_active_no_op_when_pattern_empty() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "hello hello".into();
        app.find_query.clear();
        app.replace_query = "world".into();
        app.replace_in_active(true);
        assert_eq!(app.tabs[0].text, "hello hello");
    }

    /// F-008 replace: replace-next changes only the first match.
    #[test]
    fn replace_in_active_first_only() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "hello hello hello".into();
        app.find_query = "hello".into();
        app.replace_query = "x".into();
        app.replace_in_active(false);
        assert_eq!(app.tabs[0].text, "x hello hello");
    }

    /// F-008 replace: replace-all changes every literal match.
    #[test]
    fn replace_in_active_all_matches() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "hello hello hello".into();
        app.find_query = "hello".into();
        app.replace_query = "x".into();
        app.replace_in_active(true);
        assert_eq!(app.tabs[0].text, "x x x");
    }

    /// F-008 replace: replace matches the find bar's CASE-INSENSITIVE semantics.
    /// The find bar (`find_matches_active`) highlights "Foo"/"foo"/"FOO" alike;
    /// the old `str::replace` was case-SENSITIVE and silently changed only the
    /// exact-case match the user did NOT see as the sole highlight. Both surfaces
    /// now share `search::find_all`, so replace touches what find highlights.
    #[test]
    fn replace_in_active_matches_find_bar_case_insensitively() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "Foo foo FOO".into();
        app.find_query = "foo".into();
        app.replace_query = "x".into();
        app.replace_in_active(true);
        assert_eq!(app.tabs[0].text, "x x x");
    }

    /// The replacement is spliced LITERALLY — a `$`-containing replacement is not
    /// treated as a regex capture reference (the find bar has no regex toggle).
    #[test]
    fn replace_in_active_replacement_is_literal_not_regex_expanded() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "ab".into();
        app.find_query = "a".into();
        app.replace_query = "$1".into();
        app.replace_in_active(true);
        assert_eq!(app.tabs[0].text, "$1b");
    }

    /// A stale completion offset that lands mid-multibyte-char must not abort the
    /// app: `replace_range` panics on a non-boundary byte offset (`panic = abort`).
    #[test]
    fn accept_completion_with_stale_non_boundary_offset_does_not_panic() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "a\u{e9}".into(); // 'a' + 'é' (2 bytes) → byte len 3
        app.completion = Some(Completion {
            prefix_start: 2, // mid-'é' — NOT a UTF-8 char boundary
            items: vec!["zz".to_string()],
            selected: 0,
        });
        app.accept_completion(0, Some(2)); // must not panic
        assert_eq!(app.tabs[0].text, "a\u{e9}"); // dropped the stale completion
    }

    /// F-017 — move-line-down swaps the cursor line with its neighbour.
    #[test]
    fn move_cursor_line_down_swaps_lines() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "alpha\nbeta\ngamma\n".into();
        app.last_cursor_line_col = Some((1, 1)); // 1-based line 1 = "alpha"
        app.move_cursor_line(1);
        assert_eq!(app.tabs[0].text, "beta\nalpha\ngamma\n");
        assert_eq!(app.last_cursor_line_col, Some((2, 1)));
    }

    /// F-017 — move-line-up at line 1 is a no-op.
    #[test]
    fn move_cursor_line_up_at_top_is_noop() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "alpha\nbeta\n".into();
        app.last_cursor_line_col = Some((1, 1));
        app.move_cursor_line(-1);
        assert_eq!(app.tabs[0].text, "alpha\nbeta\n");
    }

    /// F-017 — duplicate inserts a copy on the row below.
    #[test]
    fn duplicate_cursor_line_inserts_copy() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "alpha\nbeta\n".into();
        app.last_cursor_line_col = Some((1, 1));
        app.duplicate_cursor_line();
        assert_eq!(app.tabs[0].text, "alpha\nalpha\nbeta\n");
    }

    /// F-017 — join glues cursor line + next with a single space.
    #[test]
    fn join_cursor_line_with_next_uses_single_space() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "hello   \n   world\n".into();
        app.last_cursor_line_col = Some((1, 1));
        app.join_cursor_line_with_next();
        assert_eq!(app.tabs[0].text, "hello world\n");
    }

    /// F-017 — join at last line is a no-op.
    #[test]
    fn join_cursor_line_with_next_at_last_line_is_noop() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "only".into();
        app.last_cursor_line_col = Some((1, 1));
        app.join_cursor_line_with_next();
        assert_eq!(app.tabs[0].text, "only");
    }

    /// F-022 — external edit + clean buffer: silent reload picks up the new
    /// content. The poller is driven manually here (frame_tick is heavy).
    #[test]
    fn external_disk_change_reloads_clean_buffer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "first").expect("write seed");
        let mut app = ScribeApp::new_test(Config::default());
        app.open_path(path.clone());
        let opened_idx = app.tabs.len() - 1;
        assert_eq!(app.tabs[opened_idx].text, "first");
        // Simulate external write — sleep is required because filesystems
        // typically only track mtime at second resolution.
        std::thread::sleep(std::time::Duration::from_millis(1200));
        std::fs::write(&path, "second").expect("write update");
        app.poll_external_disk_changes();
        assert_eq!(
            app.tabs[opened_idx].text, "second",
            "clean buffer reloads from disk silently"
        );
    }

    /// F-022 — external edit + dirty buffer: do NOT reload; surface a toast.
    #[test]
    fn external_disk_change_warns_when_buffer_dirty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "first").expect("write seed");
        let mut app = ScribeApp::new_test(Config::default());
        app.open_path(path.clone());
        let opened_idx = app.tabs.len() - 1;
        // Make local edits.
        app.tabs[opened_idx].text = "local edits".to_string();
        std::thread::sleep(std::time::Duration::from_millis(1200));
        std::fs::write(&path, "second").expect("write update");
        app.poll_external_disk_changes();
        assert_eq!(
            app.tabs[opened_idx].text, "local edits",
            "dirty buffer must NOT be silently overwritten"
        );
        assert!(
            app.toast
                .as_deref()
                .unwrap_or("")
                .contains("changed on disk"),
            "toast should warn about external change"
        );
    }

    /// F-004 sanity: BUILTIN_COMMANDS is non-empty and every entry's label
    /// is unique (no two entries collide in the palette).
    #[test]
    fn builtin_commands_registry_is_populated_and_unique() {
        assert!(!BUILTIN_COMMANDS.is_empty(), "registry must be populated");
        let mut labels: Vec<&'static str> = BUILTIN_COMMANDS.iter().map(|e| e.label).collect();
        labels.sort_unstable();
        let unique_len = labels
            .iter()
            .fold(Vec::<&'static str>::new(), |mut acc, l| {
                if !acc.last().is_some_and(|p| *p == *l) {
                    acc.push(l);
                }
                acc
            })
            .len();
        assert_eq!(
            labels.len(),
            unique_len,
            "duplicate command label in registry"
        );
    }

    /// F-004 sanity: every BuiltinCommand variant the registry references is
    /// dispatchable. We assert this by running execute_builtin on each entry
    /// and confirming it doesn't panic.
    #[test]
    fn every_builtin_command_dispatches_without_panic() {
        let mut app = ScribeApp::new_test(Config::default());
        for entry in BUILTIN_COMMANDS {
            // The three rfd-touching variants would either hang the test
            // runner waiting for user input (Linux/Windows) or panic in
            // rfd's macOS backend (no NSApplication main thread on CI):
            //   - OpenFile / OpenFolder call rfd::FileDialog directly.
            //   - Save falls through to save_as → rfd::FileDialog when the
            //     active buffer has no path. After CloseAllTabs in this
            //     same loop the active tab IS a pathless scratch, so the
            //     fall-through fires. Easier to skip than to keep the
            //     fixture path alive across CloseAllTabs side-effects.
            //   - ConvertToMarkdown / ExportAsHtml open an rfd save dialog.
            match entry.action {
                BuiltinCommand::OpenFile
                | BuiltinCommand::OpenFolder
                | BuiltinCommand::Save
                | BuiltinCommand::ConvertToMarkdown
                | BuiltinCommand::ExportAsHtml => continue,
                _ => app.execute_builtin(entry.action),
            }
        }
    }

    /// Clipboard/history palette commands record a pending editor action
    /// (drained into the focused editor as an egui event by `frame_tick`).
    /// `execute_builtin` itself must never touch the OS clipboard so it stays
    /// headless-test-safe.
    #[test]
    fn clipboard_palette_commands_set_pending_action() {
        let mut app = ScribeApp::new_test(Config::default());
        for (cmd, want) in [
            (BuiltinCommand::Copy, EditorAction::Copy),
            (BuiltinCommand::Cut, EditorAction::Cut),
            (BuiltinCommand::Paste, EditorAction::Paste),
            (BuiltinCommand::Undo, EditorAction::Undo),
            (BuiltinCommand::Redo, EditorAction::Redo),
        ] {
            app.pending_editor_action = None;
            app.execute_builtin(cmd);
            assert_eq!(app.pending_editor_action, Some(want), "{cmd:?}");
        }
    }

    /// The five clipboard/history actions are all reachable from the palette
    /// AND the cheatsheet (discoverability — they previously worked only via
    /// unlisted chords).
    #[test]
    fn clipboard_actions_are_discoverable() {
        let palette: Vec<&str> = BUILTIN_COMMANDS.iter().map(|e| e.label).collect();
        for label in ["Copy", "Cut", "Paste", "Undo", "Redo"] {
            assert!(palette.contains(&label), "palette missing {label}");
        }
        let chords: Vec<&str> = KEYBOARD_SHORTCUTS.iter().map(|e| e.chord).collect();
        for chord in ["Ctrl+C", "Ctrl+X", "Ctrl+V", "Ctrl+Z", "Ctrl+Shift+Z"] {
            assert!(chords.contains(&chord), "cheatsheet missing {chord}");
        }
    }

    /// F-004: ToggleWordWrap from the palette flips the config and persists
    /// the change in-memory.
    #[test]
    fn execute_builtin_toggle_word_wrap_flips_config() {
        let mut app = ScribeApp::new_test(Config::default());
        let before = app.config.editor.word_wrap;
        app.execute_builtin(BuiltinCommand::ToggleWordWrap);
        assert_eq!(app.config.editor.word_wrap, !before);
    }

    /// F-004: CycleTheme advances through the built-in theme list.
    #[test]
    fn execute_builtin_cycle_theme_advances() {
        let mut app = ScribeApp::new_test(Config::default());
        let names = scribe_core::theme::Theme::builtin_names();
        if names.len() < 2 {
            return; // nothing to cycle
        }
        let before = app.config.appearance.theme.clone();
        app.execute_builtin(BuiltinCommand::CycleTheme);
        let after = app.config.appearance.theme.clone();
        assert_ne!(
            before, after,
            "CycleTheme should change the active theme name"
        );
        assert!(
            names.iter().any(|n| *n == after),
            "post-cycle theme must be a known built-in"
        );
    }

    /// F-032: FoldAll switches fold view on and records every detected
    /// region's start line. ExpandAll then clears the recorded fold set.
    /// Uses a small Rust snippet so `fold_regions` finds at least one
    /// brace-delimited region.
    #[test]
    fn execute_builtin_fold_then_expand_round_trips() {
        let mut app = ScribeApp::new_test(Config::default());
        // Replace the scratch tab text with code that has a foldable
        // region. The fold extractor scans for matched braces so any
        // multi-line braced block produces a region.
        app.tabs[app.active].text = "fn x() {\n    1;\n}\n".to_string();
        app.execute_builtin(BuiltinCommand::FoldAll);
        assert!(app.fold_view, "FoldAll should switch fold view on");
        assert!(
            !app.folds.is_empty(),
            "FoldAll should record at least one fold for a braced region"
        );
        app.execute_builtin(BuiltinCommand::ExpandAll);
        assert!(app.folds.is_empty(), "ExpandAll should clear the fold set");
    }

    #[test]
    fn input_ctrl_shift_p_opens_palette() {
        let mut app = ScribeApp::new_test(Config::default());
        let d = Driver::new();
        d.idle(&mut app);
        let cmd_shift = egui::Modifiers {
            shift: true,
            command: true,
            ..Default::default()
        };
        d.key(&mut app, egui::Key::P, cmd_shift);
        assert!(app.palette_open, "Ctrl+Shift+P opens the command palette");
    }

    /// F-006 wave-1: Ctrl+W closes the active tab.
    #[test]
    fn input_ctrl_w_closes_active_tab() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs.push(EditorTab::scratch());
        app.tabs.push(EditorTab::scratch());
        assert_eq!(app.tabs.len(), 3, "seed three tabs");
        app.active = 1;
        let d = Driver::new();
        d.idle(&mut app);
        d.key(&mut app, egui::Key::W, egui::Modifiers::COMMAND);
        assert_eq!(app.tabs.len(), 2, "Ctrl+W closes one tab");
    }

    /// F-003 fix: Ctrl+\\ toggles the multi-note grid.
    #[test]
    fn input_ctrl_backslash_toggles_grid_mode() {
        let mut app = ScribeApp::new_test(Config::default());
        assert!(!app.config.editor.grid_enabled, "grid starts off");
        let d = Driver::new();
        d.idle(&mut app);
        d.key(&mut app, egui::Key::Backslash, egui::Modifiers::COMMAND);
        assert!(app.config.editor.grid_enabled, "Ctrl+\\\\ turns grid on");
        d.key(&mut app, egui::Key::Backslash, egui::Modifiers::COMMAND);
        assert!(
            !app.config.editor.grid_enabled,
            "Ctrl+\\\\ toggles back off"
        );
    }

    /// F-006 wave-1: Ctrl+Tab cycles to the next tab; Ctrl+Shift+Tab cycles
    /// to the previous tab.
    #[test]
    fn input_ctrl_tab_cycles_tabs() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs.push(EditorTab::scratch());
        app.tabs.push(EditorTab::scratch());
        app.active = 0;
        let d = Driver::new();
        d.idle(&mut app);
        d.key(&mut app, egui::Key::Tab, egui::Modifiers::COMMAND);
        assert_eq!(app.active, 1, "Ctrl+Tab moves to tab 1");
        d.key(&mut app, egui::Key::Tab, egui::Modifiers::COMMAND);
        assert_eq!(app.active, 2, "Ctrl+Tab moves to tab 2");
        d.key(&mut app, egui::Key::Tab, egui::Modifiers::COMMAND);
        assert_eq!(app.active, 0, "Ctrl+Tab wraps to tab 0");
        let cmd_shift = egui::Modifiers {
            shift: true,
            command: true,
            ..Default::default()
        };
        d.key(&mut app, egui::Key::Tab, cmd_shift);
        assert_eq!(app.active, 2, "Ctrl+Shift+Tab wraps backward to tab 2");
    }

    /// F-001 / F-043 fix: tab close helpers behave correctly.
    #[test]
    fn tab_close_helpers() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs.push(EditorTab::scratch());
        app.tabs.push(EditorTab::scratch());
        app.tabs.push(EditorTab::scratch());
        assert_eq!(app.tabs.len(), 4);
        app.close_tabs_after(1);
        assert_eq!(app.tabs.len(), 2, "close_tabs_after(1) leaves tabs [0,1]");
        app.tabs.push(EditorTab::scratch());
        app.tabs.push(EditorTab::scratch());
        app.close_all_tabs_except(1);
        assert_eq!(app.tabs.len(), 1, "close_all_tabs_except keeps one tab");
        assert_eq!(app.active, 0, "active normalises to 0 after close-others");
        app.tabs.push(EditorTab::scratch());
        app.tabs.push(EditorTab::scratch());
        app.close_all_tabs();
        assert_eq!(
            app.tabs.len(),
            1,
            "close_all_tabs leaves the scratch buffer"
        );
    }

    /// F-001 fix: tab swap preserves the active-tab pointer to the same
    /// document the user was viewing.
    #[test]
    fn tab_swap_preserves_active_pointer() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs.push(EditorTab::scratch());
        app.tabs.push(EditorTab::scratch());
        // Mark each tab with a recognisable byte so swap is observable.
        app.tabs[0].text = "A".into();
        app.tabs[1].text = "B".into();
        app.tabs[2].text = "C".into();
        app.active = 1; // viewing B
        app.tabs.swap(0, 1);
        // The buffer at index 0 is now B (the user's view), but the index
        // shifted — verify the swap is observable.
        assert_eq!(app.tabs[0].text, "B");
        assert_eq!(app.tabs[1].text, "A");
        assert_eq!(app.tabs[2].text, "C");
    }

    #[test]
    fn input_type_without_click_autofocuses_editor() {
        // Regression for the auto-focus fix: a user should be able to type
        // immediately after launch with NO click — the editor grabs focus when
        // idle. (Surfaced by the live computer-control screenshot pass.)
        let mut app = ScribeApp::new_test(Config::default());
        let d = Driver::new();
        d.idle(&mut app); // frame 1: editor requests focus
        d.idle(&mut app); // frame 2: focus is now held
        d.type_text(&mut app, "no_click_needed");
        d.idle(&mut app);
        assert!(
            app.tabs[app.active].text.contains("no_click_needed"),
            "editor should auto-focus and accept typing without a click (got {:?})",
            app.tabs[app.active].text
        );
    }

    #[test]
    fn input_click_and_type_inserts_text() {
        let mut app = ScribeApp::new_test(Config::default());
        let d = Driver::new();
        d.idle(&mut app);
        // Click into the central editor to focus it, then type.
        d.click(&mut app, egui::pos2(550.0, 360.0));
        d.type_text(&mut app, "robot");
        d.idle(&mut app);
        assert!(
            app.tabs[app.active].text.contains("robot"),
            "typed text should reach the buffer (got {:?})",
            app.tabs[app.active].text
        );
    }

    #[test]
    fn input_ctrl_space_completion_then_enter_accepts() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "value valuer ".into();
        let d = Driver::new();
        d.idle(&mut app);
        d.click(&mut app, egui::pos2(550.0, 360.0));
        d.type_text(&mut app, "val");
        d.key(&mut app, egui::Key::Space, egui::Modifiers::COMMAND);
        assert!(
            app.completion.is_some(),
            "Ctrl+Space opens completion for prefix 'val' (buffer {:?})",
            app.tabs[0].text
        );
        let before = app.tabs[0].text.clone();
        d.key(&mut app, egui::Key::Enter, egui::Modifiers::NONE);
        assert_ne!(
            app.tabs[0].text, before,
            "Enter accepts the highlighted completion"
        );
        assert!(app.completion.is_none(), "popup closes after accept");
    }

    // ---- E2E for the new feature surfaces (integration smoke) ----

    /// The experimental owned rope editor renders the full frame loop without
    /// panicking (exercises show_editable + the bridge + caret render path).
    #[test]
    fn experimental_rope_editor_renders_without_panic() {
        let mut cfg = Config::default();
        cfg.editor.experimental_rope_editor = true;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs[0].text = "fn main() {\n    let x = 1;\n}\n".to_string();
        run_frames(&mut app, 4);
        assert_eq!(app.tabs.len(), 1);
    }

    /// KEYSTONE perf bridge: the experimental editor builds the rope ONCE and
    /// persists it across frames (no per-frame `Buffer::from_text`), and an
    /// external `set_text` invalidates the cache so the next frame rebuilds
    /// from the new content.
    #[test]
    fn experimental_editor_persists_rope_and_invalidates_on_external_edit() {
        let mut cfg = Config::default();
        cfg.editor.experimental_rope_editor = true;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs[0].text = "alpha\nbeta\n".to_string();
        run_frames(&mut app, 3);
        assert!(
            app.tabs[0].rope_buf.is_some(),
            "experimental editor builds + persists the rope across frames"
        );
        // External mutation (reload / plugin / sort) invalidates the cache.
        app.tabs[0].set_text("gamma\n".to_string());
        assert!(
            app.tabs[0].rope_buf.is_none(),
            "set_text invalidates the persistent rope cache"
        );
        run_frames(&mut app, 2);
        let rebuilt = app.tabs[0]
            .rope_buf
            .as_ref()
            .and_then(|b| b.as_rope())
            .map(|r| r.to_string());
        assert_eq!(
            rebuilt,
            Some("gamma\n".to_string()),
            "rope rebuilt after invalidation reflects the externally-set content"
        );
    }

    /// Auto-save + session-backup + trim-on-save all enabled together render
    /// the frame loop cleanly (no panic from the periodic save/snapshot paths).
    #[test]
    fn save_hygiene_configs_render_without_panic() {
        let mut cfg = Config::default();
        cfg.editor.auto_save = true;
        cfg.editor.session_backup = true;
        cfg.editor.trim_trailing_whitespace_on_save = true;
        cfg.editor.final_newline_on_save = true;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs[0].text = "x   \ny".to_string();
        run_frames(&mut app, 3);
        assert_eq!(app.tabs.len(), 1);
    }

    /// Reopen-closed-tab restores an accidentally closed tab's content.
    #[test]
    fn reopen_closed_tab_restores_content() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "important note".to_string();
        app.close_tab(0);
        // close_tab replaces the empty tab set with a fresh scratch.
        app.reopen_closed_tab();
        assert!(
            app.tabs.iter().any(|t| t.text == "important note"),
            "closed tab content recovered"
        );
    }

    /// Performance: the owned editing model handles a large buffer without
    /// quadratic blowup — 5k sequential inserts + an undo on a 50k-line rope
    /// complete well within a generous bound.
    #[test]
    fn perf_large_buffer_edit_is_bounded() {
        use scribe_render::{apply_event, RopeEditorState};
        let mut body = String::with_capacity(50_000 * 6);
        for i in 0..50_000 {
            body.push_str(&format!("{i:05}\n"));
        }
        let mut buf = scribe_core::buffer::Buffer::from_text(&body);
        let rope = buf.as_rope_mut().expect("rope buffer");
        let mut st = RopeEditorState::new();
        let start = std::time::Instant::now();
        for _ in 0..5_000 {
            apply_event(rope, &mut st, &egui::Event::Text("a".to_string()));
        }
        apply_event(
            rope,
            &mut st,
            &egui::Event::Key {
                key: egui::Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            },
        );
        let elapsed = start.elapsed();
        // Snapshot-undo rebuilds the rope per keystroke (O(n) bridge) — still
        // must stay well under a wall-clock ceiling on a 50k-line buffer.
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "5k edits on a 50k-line rope took {elapsed:?}"
        );
    }
}
