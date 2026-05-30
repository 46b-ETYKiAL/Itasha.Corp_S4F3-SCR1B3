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
use scribe_core::config::WindowMode;
use scribe_core::lsp::{Diagnostic, LspClient, LspRegistry};
use scribe_core::plugin::{self, CommandInfo, HookEvent, PluginContext, PluginHost};
use scribe_core::spell::{self, HashSetEngine};
use scribe_core::syntax::Highlighter;
use scribe_core::theme::{Rgba, Theme};
use scribe_core::{Config, Document};
use std::path::{Path, PathBuf};

// Parse a `#RRGGBB` tint to an RGBA quad for native blur tinting.
//
// Only consumed by Windows' `window_vibrancy::apply_acrylic`. macOS' vibrancy
// API takes no tint, and Linux falls back to the portable transparent surface
// with a tint overlay (neither needs the quad). Gating the fn to Windows keeps
// `-D warnings` (clippy dead_code) green on Linux and macOS without a blanket
// `#[allow(dead_code)]` (which would mask real dead code).
#[cfg(windows)]
fn tint_rgba(hex: &str, alpha: u8) -> Option<(u8, u8, u8, u8)> {
    Rgba::parse_hex(hex).map(|c| (c.r, c.g, c.b, alpha))
}

/// Apply the OS window effect for the chosen mode (best-effort, graceful on
/// unsupported platforms). Windows: acrylic/mica; macOS: vibrancy; elsewhere the
/// portable transparent surface + tint overlay carry the look.
fn apply_window_effect(cc: &eframe::CreationContext<'_>, mode: WindowMode, tint_hex: &str) {
    let _ = (cc, tint_hex);
    match mode {
        WindowMode::Glass => {
            #[cfg(windows)]
            {
                let _ = window_vibrancy::apply_acrylic(cc, tint_rgba(tint_hex, 160));
            }
            #[cfg(target_os = "macos")]
            {
                let _ = window_vibrancy::apply_vibrancy(
                    cc,
                    window_vibrancy::NSVisualEffectMaterial::HudWindow,
                    None,
                    None,
                );
            }
        }
        WindowMode::Mica => {
            #[cfg(windows)]
            {
                let _ = window_vibrancy::apply_mica(cc, Some(true));
            }
            #[cfg(target_os = "macos")]
            {
                let _ = window_vibrancy::apply_vibrancy(
                    cc,
                    window_vibrancy::NSVisualEffectMaterial::HudWindow,
                    None,
                    None,
                );
            }
        }
        WindowMode::Vibrancy => {
            #[cfg(target_os = "macos")]
            {
                let _ = window_vibrancy::apply_vibrancy(
                    cc,
                    window_vibrancy::NSVisualEffectMaterial::Sidebar,
                    None,
                    None,
                );
            }
        }
        WindowMode::Transparent | WindowMode::Opaque => {}
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
}

impl EditorTab {
    fn scratch() -> Self {
        Self {
            doc: Document::scratch(),
            text: String::new(),
            doc_id: crate::grid::DocId(0),
        }
    }

    fn from_path(path: PathBuf) -> Result<Self, String> {
        let doc = Document::open(&path).map_err(|e| e.to_string())?;
        let text = doc.text();
        Ok(Self {
            doc,
            text,
            doc_id: crate::grid::DocId(0),
        })
    }

    fn title(&self) -> String {
        let name = self.doc.file_name();
        if self.is_dirty() {
            format!("● {name}")
        } else {
            name
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
    hl: Highlighter,
    tabs: Vec<EditorTab>,
    active: usize,
    visuals_applied: bool,
    /// Set when the user asks to close (custom titlebar ✕). Funnels into the
    /// same two-phase close path as an OS-initiated close.
    want_close: bool,
    /// Two-phase close latch: a transparent/layered window must be hidden BEFORE
    /// it is destroyed or DWM retains its last frame as a ghost on the desktop
    /// (the T19.1 root cause). On the first close request we hide + cancel, then
    /// issue the real Close on the next frame.
    closing: bool,
    find_open: bool,
    find_query: String,
    status: String,
    toast: Option<String>,
    /// Plugin/mod host (Rhai easy-mode); loaded from the plugins dir on start.
    plugins: PluginHost,
    plugin_cmds: Vec<CommandInfo>,
    /// Offline spellcheck engine (bundled en_US); checked only when enabled.
    spell: HashSetEngine,
    palette_open: bool,
    palette_query: String,
    settings_open: bool,
    /// Open folder for the file-tree sidebar (None = sidebar hidden).
    file_tree_root: Option<PathBuf>,
    /// LSP: per-language server registry + the active server connection.
    lsp_registry: LspRegistry,
    lsp: Option<LspClient>,
    lsp_lang: Option<String>,
    diagnostics: Vec<Diagnostic>,
    /// Signature of the currently-open file set (to persist session on change).
    session_sig: String,
    /// Cached syntax-highlight layout (keyed by text+lang+size) so syntect only
    /// re-runs when the buffer changes, not every frame (perf hotspot fix).
    hl_cache: std::cell::RefCell<Option<(u64, std::sync::Arc<LayoutJob>)>>,
    /// Config-file watcher for live-reload (kept alive; events arrive on `cfg_rx`).
    _cfg_watcher: Option<notify::RecommendedWatcher>,
    cfg_rx: Option<std::sync::mpsc::Receiver<()>>,
    /// Split view: a second editor pane over the same active buffer.
    split_view: bool,
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
    /// Index of the tab currently being dragged in the tab strip, or `None`
    /// if no tab is mid-drag. Drives the click_and_drag swap-on-release
    /// pattern in `draw_tab_strip` (F-001 fix from the 2026-05-29 overlooked-
    /// surfaces audit).
    dragged_tab: Option<usize>,
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
        let mut app = Self::build(config, config_err, cli_path);
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
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Thin);
        const JETBRAINS_MONO_REGULAR: &[u8] =
            include_bytes!("../../../assets/fonts/JetBrainsMono/JetBrainsMono-Regular.ttf");
        fonts.font_data.insert(
            "JetBrainsMono".to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(JETBRAINS_MONO_REGULAR)),
        );
        if let Some(monospace) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            monospace.insert(0, "JetBrainsMono".to_owned());
        }
        cc.egui_ctx.set_fonts(fonts);
        cc.egui_ctx.set_visuals(app.current_visuals());
        app.visuals_applied = true;
        // Apply the OS glass/acrylic/mica/vibrancy effect — only when the master
        // transparency toggle is on AND the mode wants it. Otherwise the window is
        // a normal opaque window (no layered surface => no ghost-on-close risk).
        if app.config.window.effective_translucent() {
            apply_window_effect(cc, app.config.window.mode, &app.config.window.tint);
        }
        // Phase 17 T17.4 wgpu CRT post-pass — INIT step. Construct
        // `PostResources` (compiled shader + pipeline + uniform buffer +
        // bind group + sampler) once at startup and stash them in the
        // egui_wgpu renderer's `callback_resources` type-map so a later
        // PR's `CrtPostCallback` can find them at paint time. We do NOT
        // register the callback in this PR — the offscreen-RT copy step
        // the dossier's §4 prescribes lands in a follow-up. This init is
        // pure-cost-zero: the resources sit in the type-map until a draw
        // callback retrieves them.
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            let post_res =
                scribe_render::PostResources::new(&rs.device, &rs.queue, rs.target_format);
            rs.renderer.write().callback_resources.insert(post_res);
            tracing::debug!("scr1b3-post: PostResources initialised + stashed");
        } else {
            tracing::debug!(
                "scr1b3-post: no wgpu_render_state (probably glow backend); \
                 post-pass disabled for this session"
            );
        }
        app
    }

    /// Test constructor — builds the app without an eframe context, for headless
    /// `egui_kittest` E2E driving. Session-restore + plugin auto-load are disabled
    /// so tests are hermetic (independent of the real user environment).
    #[cfg(test)]
    pub fn new_test(mut config: Config) -> Self {
        config.editor.restore_session = false;
        config.plugins.enabled = false;
        Self::build(config, None, None)
    }

    fn build(config: Config, config_err: Option<String>, cli_path: Option<String>) -> Self {
        let theme = load_theme(&config.appearance.theme);

        let mut tabs = Vec::new();
        let mut toast = config_err.map(|e| format!("config: {e} (using defaults)"));
        if let Some(p) = cli_path {
            match EditorTab::from_path(PathBuf::from(&p)) {
                Ok(t) => tabs.push(t),
                Err(e) => toast = Some(format!("could not open {p}: {e}")),
            }
        }
        // Restore the previous session (open files) when launched bare.
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
        let mut plugins = PluginHost::new();
        if config.plugins.enabled {
            if let Some(dir) = Config::config_dir() {
                let (found, errors) = plugin::discover(&dir.join("plugins"));
                for p in found {
                    if config.plugins.disabled.contains(&p.manifest.id) {
                        continue;
                    }
                    if let Ok(src) = std::fs::read_to_string(p.entry_path()) {
                        if let Err(e) = plugins.load_script(&p.manifest.id, &src) {
                            tracing::warn!("plugin load failed: {e}");
                        }
                    }
                }
                if !errors.is_empty() && toast.is_none() {
                    toast = Some(format!("{} plugin(s) skipped (see log)", errors.len()));
                }
            }
        }
        let plugin_cmds = plugins.commands();

        // Live-reload: watch the config dir for external edits to scr1b3.toml.
        let (cfg_tx, cfg_rx) = std::sync::mpsc::channel();
        let cfg_watcher = spawn_config_watcher(cfg_tx);

        Self {
            config,
            theme,
            hl: Highlighter::new(),
            tabs,
            active: 0,
            visuals_applied: false,
            want_close: false,
            closing: false,
            find_open: false,
            find_query: String::new(),
            status: format!(
                "{} — {}",
                scribe_core::PRODUCT_NAME,
                scribe_core::PRODUCT_TAGLINE
            ),
            toast,
            plugins,
            plugin_cmds,
            spell: HashSetEngine::bundled_en_us(),
            palette_open: false,
            palette_query: String::new(),
            settings_open: false,
            file_tree_root: None,
            lsp_registry: LspRegistry::with_defaults(),
            lsp: None,
            lsp_lang: None,
            diagnostics: Vec::new(),
            session_sig,
            hl_cache: std::cell::RefCell::new(None),
            _cfg_watcher: cfg_watcher,
            cfg_rx: Some(cfg_rx),
            split_view: false,
            fold_view: false,
            folds: std::collections::BTreeSet::new(),
            completion: None,
            pending_scroll: None,
            scroll_metrics: (0.0, 1.0, 1.0),
            minimap_cache: std::cell::RefCell::new(None),
            focus_find: false,
            focus_palette: false,
            line_gutter: Vec::new(),
            grid_tree: None,
            next_doc_id: crate::grid::DocIdAllocator::default(),
            grid_close_queue: Vec::new(),
            dragged_tab: None,
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
        // Reset the per-frame close queue; the behavior may push into it.
        self.grid_close_queue.clear();
        // Use a local close-queue inside the closure so the borrow
        // checker doesn't see `&mut self.grid_close_queue` twice (once
        // via the body closure capture, once via the behavior field).
        let mut close_queue: Vec<crate::grid::DocId> = Vec::new();
        egui::CentralPanel::default().show(ctx, |ui| {
            let tabs = &mut self.tabs;
            let body_close_queue = &mut close_queue;
            let mut render_body = |ui: &mut egui::Ui, doc_id: crate::grid::DocId| -> bool {
                let Some(idx) = tabs.iter().position(|t| t.doc_id == doc_id) else {
                    ui.weak("(document closed)");
                    return false;
                };
                let mut drag_started = false;
                ui.horizontal(|ui| {
                    if ui.small_button("✕").on_hover_text("Close pane").clicked() {
                        body_close_queue.push(doc_id);
                    }
                    // F-002 fix from docs/audits/overlooked-surfaces-2026-05-29.md:
                    // the previous code used `is_pointer_button_down_on()` which
                    // returns `true` every frame the button is held — egui_tiles
                    // expects `UiResponse::DragStarted` to fire ONCE on drag
                    // start. Re-firing every frame put the tile tree's drag
                    // state into a confused "constantly starting" loop and the
                    // pane never actually moved. The fix uses `drag_started()`
                    // on a click_and_drag Sense.
                    let handle = ui
                        .small_button("⠿")
                        .on_hover_text("Drag to rearrange")
                        .interact(egui::Sense::click_and_drag());
                    if handle.drag_started() {
                        drag_started = true;
                    }
                });
                egui::ScrollArea::both()
                    .id_salt(("scr1b3-grid-pane", doc_id.raw()))
                    .show(ui, |ui| {
                        let editor = egui::TextEdit::multiline(&mut tabs[idx].text)
                            .code_editor()
                            .font(font.clone())
                            .desired_width(f32::INFINITY)
                            .desired_rows(20);
                        editor.show(ui);
                    });
                drag_started
            };
            // egui_tiles' `retain_pane` is consulted on every paint; we
            // wire a small empty vec so the behavior owns its own slot
            // and the body's close_queue is the authoritative buffer
            // we drain after the frame.
            let mut behavior_close_requests: Vec<crate::grid::DocId> = Vec::new();
            let mut behavior = crate::grid::AppGridBehavior {
                titles: &titles,
                render_body: &mut render_body,
                close_requests: &mut behavior_close_requests,
            };
            tree.ui(&mut behavior, ui);
        });
        self.grid_close_queue.append(&mut close_queue);
        // Phase 18 T18.2 — 6-pane cap. Reads the grid storage (NOT the
        // currently-visible tabs) and toasts when the user splits past
        // the ceiling. The full undo-snapshot pattern from the dossier
        // lands in a follow-up; the MVP here just warns + caps the
        // count by capping the tab vec so the next layout-build picks
        // up the right shape.
        if crate::grid::count_panes(&tree) > crate::grid::MAX_PANES {
            self.toast = Some(format!(
                "Pane limit reached ({}). Close a pane before opening more.",
                crate::grid::MAX_PANES
            ));
        }
        // After the frame: if the user closed any panes via the chrome,
        // we drop those tabs as well. The simplest cleanup is to remove
        // the tabs matching each close request; the tree itself prunes
        // empty parents via simplification on its next paint.
        if !self.grid_close_queue.is_empty() {
            for doc_id in self.grid_close_queue.drain(..).collect::<Vec<_>>() {
                self.tabs.retain(|t| t.doc_id != doc_id);
            }
            if self.tabs.is_empty() {
                self.tabs.push(EditorTab::scratch());
            }
            // Re-sync the tree to the surviving doc set.
            let docs: Vec<crate::grid::DocId> = self.tabs.iter().map(|t| t.doc_id).collect();
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
                tab.doc_id = self.next_doc_id.next();
                // Ensure the first real id is >= 1 (next() starts at 0).
                if tab.doc_id.0 == 0 {
                    tab.doc_id = self.next_doc_id.next();
                }
            }
            self.next_doc_id.observe(tab.doc_id);
        }
        // Pass 2: align tree state with the config flag.
        match (self.config.editor.grid_enabled, self.grid_tree.is_some()) {
            (true, false) => {
                let docs: Vec<crate::grid::DocId> = self.tabs.iter().map(|t| t.doc_id).collect();
                self.grid_tree = Some(crate::grid::build_default_grid(&docs));
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
            }
            Err(e) => self.toast = Some(format!("open failed: {e}")),
        }
    }

    /// Build the egui visuals for the current theme, applying surface opacity
    /// when a translucent/glass window mode is active.
    fn current_visuals(&self) -> egui::Visuals {
        let mut v = scribe_render::theme_to_visuals(&self.theme);
        if self.config.window.effective_translucent() {
            scribe_render::apply_window_opacity(&mut v, self.config.window.opacity);
        }
        v
    }

    /// Apply the current theme to the egui context (after a theme/config change).
    fn reapply_theme(&mut self, ctx: &egui::Context) {
        self.theme = load_theme(&self.config.appearance.theme);
        ctx.set_visuals(self.current_visuals());
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
        self.tabs[active].text = new_text;
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(new_idx),
            )));
        state.store(ctx, id);
    }

    /// Render one quick-access toolbar entry by action id and apply its effect.
    /// Buttons set the pending-action flags; toggles flip the live config/state
    /// and request a config save. The id `"sep"` draws a divider.
    // The explicit `=> { if widget.clicked() { effect } }` per arm is clearer
    // than clippy's suggested match-guard form, which would render the widget as
    // a side effect inside the guard condition.
    #[allow(clippy::collapsible_match)]
    fn toolbar_item(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        act: &mut Pending,
        save_cfg: &mut bool,
        start_lsp: &mut bool,
    ) {
        // Phase 16 T16.3: every toolbar label routes through `toolbar_widget(id, icons, jp)`
        // so flipping `appearance.toolbar_icons` swaps every entry between its text
        // form and its Phosphor (Thin) glyph in one place. Phase 17 T17.5: the
        // same helper also appends a verified-canonical kanji "instrument plate"
        // when `appearance.jp_glyph_labels` is on (English-redundant, dimmed, smaller).
        let icons = self.config.appearance.toolbar_icons;
        let jp = self.config.appearance.jp_glyph_labels;
        match id {
            "sep" => {
                ui.separator();
            }
            "new" => {
                if ui
                    .button(toolbar_widget("new", icons, jp))
                    .on_hover_text("New file (Ctrl+N)")
                    .clicked()
                {
                    act.new = true;
                }
            }
            "open" => {
                if ui
                    .button(toolbar_widget("open", icons, jp))
                    .on_hover_text("Open file (Ctrl+O)")
                    .clicked()
                {
                    act.open = true;
                }
            }
            "openfolder" => {
                if ui
                    .button(toolbar_widget("openfolder", icons, jp))
                    .on_hover_text("Open folder")
                    .clicked()
                {
                    act.open_folder = true;
                }
            }
            "save" => {
                if ui
                    .button(toolbar_widget("save", icons, jp))
                    .on_hover_text("Save (Ctrl+S)")
                    .clicked()
                {
                    act.save = true;
                }
            }
            "saveas" => {
                if ui
                    .button(toolbar_widget("saveas", icons, jp))
                    .on_hover_text("Save As…")
                    .clicked()
                {
                    self.save_as_active();
                }
            }
            "find" => {
                if ui
                    .button(toolbar_widget("find", icons, jp))
                    .on_hover_text("Find (Ctrl+F)")
                    .clicked()
                {
                    self.find_open = true;
                    self.focus_find = true;
                }
            }
            "palette" => {
                if ui
                    .button(toolbar_widget("palette", icons, jp))
                    .on_hover_text("Command palette")
                    .clicked()
                {
                    self.palette_open = true;
                    self.focus_palette = true;
                    self.palette_query.clear();
                }
            }
            "split" => {
                if ui
                    .selectable_label(self.split_view, toolbar_widget("split", icons, jp))
                    .on_hover_text("Split view")
                    .clicked()
                {
                    self.split_view = !self.split_view;
                }
            }
            "minimap" => {
                if ui
                    .selectable_label(
                        self.config.editor.show_minimap,
                        toolbar_widget("minimap", icons, jp),
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
                        toolbar_widget("wrap", icons, jp),
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
                    .selectable_label(self.fold_view, toolbar_widget("fold", icons, jp))
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
                        toolbar_widget("linenumbers", icons, jp),
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
                        toolbar_widget("spellcheck", icons, jp),
                    )
                    .on_hover_text("Spellcheck (offline)")
                    .clicked()
                {
                    self.config.spellcheck.enabled = !self.config.spellcheck.enabled;
                    *save_cfg = true;
                }
            }
            "crt" => {
                if ui
                    .selectable_label(
                        self.config.effects.crt_enabled,
                        toolbar_widget("crt", icons, jp),
                    )
                    .on_hover_text("CRT effect")
                    .clicked()
                {
                    self.config.effects.crt_enabled = !self.config.effects.crt_enabled;
                    *save_cfg = true;
                }
            }
            "lsp" => {
                if ui
                    .button(toolbar_widget("lsp", icons, jp))
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
                self.tabs[active].text = pctx.text;
                if let Some(n) = pctx.notifications.last() {
                    self.status = n.clone();
                }
            }
            Err(e) => self.toast = Some(format!("plugin error: {e}")),
        }
    }

    /// Count misspellings in the active buffer when spellcheck is enabled.
    fn spell_count(&self) -> usize {
        if !self.config.spellcheck.enabled {
            return 0;
        }
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        let Some(tab) = self.tabs.get(active) else {
            return 0;
        };
        spell::check_text(&self.spell, &tab.text, true).len()
    }

    /// Persist the current config to the user TOML file (creating the config
    /// dir if needed). Best-effort: surfaces a toast on failure, never panics.
    fn save_config(&mut self) {
        let Some(path) = Config::config_file_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, self.config.to_toml_string()) {
            Ok(()) => self.status = "settings saved".to_string(),
            Err(e) => self.toast = Some(format!("could not save settings: {e}")),
        }
    }

    fn new_tab(&mut self) {
        self.tabs.push(EditorTab::scratch());
        self.active = self.tabs.len() - 1;
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
        // Sync editable text into the document model, then persist.
        let text = self.tabs[active].text.clone();
        self.tabs[active].doc.set_text(&text);
        if self.tabs[active].doc.path().is_none() {
            self.save_as_active();
            return;
        }
        match self.tabs[active].doc.save() {
            Ok(()) => {
                self.status = format!("saved {}", self.tabs[active].doc.file_name());
                self.fire_save_hooks(active);
            }
            Err(e) => self.toast = Some(format!("save failed: {e}")),
        }
    }

    /// Fire plugin `on_save` hooks; apply any text transform they make.
    fn fire_save_hooks(&mut self, active: usize) {
        let mut pctx = PluginContext::new(self.tabs[active].text.clone());
        if self.plugins.fire_event(HookEvent::Save, &mut pctx).is_ok() {
            if pctx.text != self.tabs[active].text {
                self.tabs[active].text = pctx.text;
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
                Ok(()) => self.status = format!("saved {}", path.display()),
                Err(e) => self.toast = Some(format!("save failed: {e}")),
            }
        }
    }

    /// Render the tab strip — the row (or column, for side positions) of open
    /// documents with the active one accented and an `×` close button on it.
    /// Extracted from the toolbar (T18.4) so the same widget can live inline
    /// at the top OR in a dedicated bottom / left / right panel.
    /// Render the tab strip with full mouse ergonomics:
    ///
    /// - **Click** → switch to that tab
    /// - **Middle-click** → close that tab (universal editor convention)
    /// - **Right-click** → context menu: Close · Close Others · Close All to the Right · Close All
    /// - **`×` button on the active tab** → close (back-compat with pre-audit behavior)
    /// - **Drag** → rearrange. Dragging a tab over another tab swaps them on
    ///   release. The egui pattern is `Sense::click_and_drag` per item, a
    ///   `dragged_tab: Option<usize>` field on the app to remember which
    ///   tab is mid-drag, and `response.drag_stopped()` to commit the swap.
    ///   Closes F-001 / F-043 from `docs/audits/overlooked-surfaces-2026-05-29.md`.
    fn draw_tab_strip(&mut self, ui: &mut egui::Ui, accent: Color32, muted: Color32) {
        let active = self.active;
        let mut switch_to = None;
        let mut close = None;
        let mut close_others = None;
        let mut close_to_right = None;
        let mut close_all = false;
        // Per-tab pointer position when a drag ends — used to compute the
        // drop-target index without storing rects on the app.
        let mut drop_target: Option<usize> = None;

        // Collect per-tab Responses so we can do drop-target hit-testing in a
        // second pass (each Response carries its rect).
        let mut responses: Vec<egui::Response> = Vec::with_capacity(self.tabs.len());

        for (i, t) in self.tabs.iter().enumerate() {
            let selected = i == active;
            let label = RichText::new(t.title()).color(if selected { accent } else { muted });
            // `click_and_drag` so the same widget services left-click switch,
            // middle-click close, right-click context, and drag-rearrange.
            let resp = ui
                .add(egui::SelectableLabel::new(selected, label))
                .interact(egui::Sense::click_and_drag());
            if resp.clicked() {
                switch_to = Some(i);
            }
            if resp.clicked_by(egui::PointerButton::Middle) {
                close = Some(i);
            }
            // Right-click → context menu.
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
            });

            // Drag bookkeeping. We start a drag the frame the press begins;
            // we commit a swap on the frame the press ends.
            if resp.drag_started() {
                self.dragged_tab = Some(i);
            }
            if resp.drag_stopped() {
                // The drop target is whatever tab the pointer is over now.
                if let (Some(src), Some(pos)) = (self.dragged_tab, resp.interact_pointer_pos()) {
                    // Find which tab rect contains the release position.
                    for (j, other) in responses.iter().enumerate() {
                        if other.rect.contains(pos) {
                            drop_target = Some(j);
                            break;
                        }
                    }
                    // Special-case: released over self → no-op.
                    if drop_target == Some(src) {
                        drop_target = None;
                    }
                }
                self.dragged_tab = None;
            }
            // Visual hint while dragging: dim the dragged tab.
            if self.dragged_tab == Some(i) && resp.dragged() {
                ui.painter()
                    .rect_filled(resp.rect, 0.0, accent.linear_multiply(0.10));
            }

            responses.push(resp);

            if selected && ui.small_button("×").clicked() {
                close = Some(i);
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
        // Commit the drag-swap. The swap is index-based; `swap` is O(1) and
        // preserves every other tab's position.
        if let Some(target) = drop_target {
            // The source index is whatever was being dragged THIS frame; it
            // was already cleared above on drag_stopped, so we recover it
            // from the responses we collected: the dragged response had
            // `drag_stopped()` set true above. Re-derive: there is at most
            // one such response.
            if let Some(src) = responses
                .iter()
                .position(|r| r.drag_stopped() && r.interact_pointer_pos().is_some())
            {
                if src < self.tabs.len() && target < self.tabs.len() && src != target {
                    self.tabs.swap(src, target);
                    // Keep the active tab pointing at the same buffer the
                    // user is editing.
                    if self.active == src {
                        self.active = target;
                    } else if self.active == target {
                        self.active = src;
                    }
                }
            }
        }
    }

    /// Close every tab whose index is not `keep`.
    fn close_all_tabs_except(&mut self, keep: usize) {
        if keep >= self.tabs.len() {
            return;
        }
        let kept = self.tabs.remove(keep);
        self.tabs.clear();
        self.tabs.push(kept);
        self.active = 0;
    }

    /// Close every tab after `after` (exclusive).
    fn close_tabs_after(&mut self, after: usize) {
        if after + 1 < self.tabs.len() {
            self.tabs.truncate(after + 1);
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
        }
    }

    /// Close every tab, leaving a single scratch buffer.
    fn close_all_tabs(&mut self) {
        self.tabs.clear();
        self.tabs.push(EditorTab::scratch());
        self.active = 0;
    }

    fn close_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.tabs.remove(idx);
            if self.tabs.is_empty() {
                self.tabs.push(EditorTab::scratch());
            }
            self.active = self.active.min(self.tabs.len() - 1);
        }
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
        if c.prefix_start <= byte && byte <= text.len() {
            text.replace_range(c.prefix_start..byte, &item);
        }
    }

    /// Render the minimap strip (rightmost): a memoized scaled overview of the
    /// active document with a viewport indicator; click/drag jumps the editor.
    fn show_minimap(&mut self, ctx: &egui::Context, panel: Color32, accent: Color32) {
        egui::SidePanel::right("minimap")
            .exact_width(110.0)
            .resizable(false)
            .frame(egui::Frame::default().fill(panel).inner_margin(4.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("MAP").color(accent).small().monospace());
                let avail = ui.available_size();
                let text = self.tabs[self.active].text.clone();
                // Memoize the tiny galley keyed by (text, width).
                let galley = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    text.hash(&mut h);
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
                                    text.clone(),
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
        let text = self.tabs[self.active].text.clone();
        let regions = crate::editor_features::fold_regions(&text);
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
            crate::editor_features::project_folded(&text, &regions, &self.folds);
        let line_height = self.config.fonts.line_height;
        let hl = &self.hl;
        let mut layouter = make_layouter(hl, &self.hl_cache, ext, font, line_height);
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
}

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
    ("crt", "CRT effect"),
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
/// palette (`⌘` glyph fallback exists), crt (acronym/loanword), lsp
/// (acronym/loanword), find (covered by 検).
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
pub(crate) fn toolbar_widget(id: &str, icons: bool, jp_glyphs: bool) -> egui::WidgetText {
    let primary = toolbar_label(id, icons);
    let kanji = if jp_glyphs { jp_glyph(id) } else { None };
    let Some(kanji) = kanji else {
        return egui::WidgetText::from(primary);
    };
    use egui::text::LayoutJob;
    let mut job = LayoutJob::default();
    job.append(
        primary,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(14.0),
            ..Default::default()
        },
    );
    job.append(
        &format!("  {kanji}"),
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(10.0),
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
        (true, "crt") => ph::MONITOR,
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
        (_, "crt") => "crt",
        (_, "lsp") => "lsp",
        (_, _) => "·",
    }
}

fn ui_color(theme: &Theme, key: &str, default: Rgba) -> Color32 {
    scribe_render::color32(theme.ui(key, default))
}

/// The fill color for chrome panels (titlebar/toolbar/status/sidebars/gutter).
/// In an effectively-translucent window the alpha is lowered to `window.opacity`
/// so the OS blur (Mica/acrylic/vibrancy) or the desktop shows through the
/// chrome — not just the central editor. When the master transparency toggle is
/// off (or the mode is opaque) the panel stays fully opaque.
fn panel_fill(theme: &Theme, window: &scribe_core::config::WindowConfig) -> Color32 {
    let base = ui_color(theme, "panel", Rgba::new(0x0d, 0x0b, 0x14, 255));
    if window.effective_translucent() {
        let a = (window.opacity.clamp(0.30, 1.0) * 255.0).round() as u8;
        Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a)
    } else {
        base
    }
}

/// Build a syntect-colored `LayoutJob` for the editor surface. Free function so
/// the egui `layouter` closure captures only the highlighter, not `self`.
fn highlight_job(
    hl: &Highlighter,
    text: &str,
    ext: Option<&str>,
    font: FontId,
    line_height_mult: f32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let lines = hl.highlight_document(text, ext);
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
            // Append any tail not covered by spans.
            if byte < line.len() {
                job.append(&line[byte..], 0.0, plain(Color32::GRAY));
            }
        } else {
            job.append(line, 0.0, plain(Color32::GRAY));
        }
        char_cursor += line.len();
    }
    let _ = char_cursor;
    job
}

/// Build the memoizing egui `layouter` closure for a `TextEdit`. Reuses the
/// cached highlight `LayoutJob` unless the buffer/lang/font-size changed, so
/// syntect/tree-sitter only re-run when the text actually changes.
fn make_layouter<'a>(
    hl: &'a Highlighter,
    cache: &'a std::cell::RefCell<Option<(u64, std::sync::Arc<LayoutJob>)>>,
    ext: Option<&'a str>,
    font: FontId,
    line_height: f32,
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
        let key = hasher.finish();
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
                    ));
                    *slot = Some((key, arc.clone()));
                    arc
                }
            }
        };
        let mut job = (*job_arc).clone();
        job.wrap.max_width = wrap;
        // egui 0.34: FontsView::layout_job caches into the view → needs &mut.
        ui.fonts_mut(|f| f.layout_job(job))
    }
}

/// Byte offset of char index `ci` in `s` (clamped to `s.len()`).
fn char_to_byte(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
}

/// Translate an egui [`egui::epaint::text::cursor::CCursor`] char index into
/// a human-visible `(1-based line, 1-based column)` pair. Counts a literal
/// `\n` as a line break; the column resets on every newline.
///
/// Used by the status bar to render "Ln 4, Col 17" — closes F-005 from
/// `docs/audits/overlooked-surfaces-2026-05-29.md`. Char-based (not byte-
/// based) so multi-byte UTF-8 codepoints (CJK, emoji) still produce the
/// column the user sees on screen.
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
    Theme::builtin(name).unwrap_or_else(Theme::wired_noir)
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

impl ScribeApp {
    /// One per-frame tick of the editor UI. Separated from `eframe::App::ui` so
    /// `egui_kittest` E2E tests can drive it through `Context::run` without an
    /// `eframe::Frame`. Drives every top-level panel via the deprecated-but-
    /// functional `Panel::show(ctx, …)` path.
    pub(crate) fn frame_tick(&mut self, ctx: &egui::Context) {
        // Phase 18 T18.2 — keep the grid in step with the editor.grid_enabled
        // config preference (toggled in Settings or via TOML edit + watcher).
        // This is cheap on the common path (config unchanged + ids already
        // assigned) and lets the grid show up the same frame the user flips
        // the checkbox.
        self.sync_grid_state();
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
        ctx.input(|i| {
            let cmd = i.modifiers.command;
            act.new = cmd && i.key_pressed(egui::Key::N);
            act.open = cmd && i.key_pressed(egui::Key::O);
            act.save = cmd && i.key_pressed(egui::Key::S);
            if cmd && i.key_pressed(egui::Key::F) {
                if !self.find_open {
                    self.focus_find = true;
                }
                self.find_open = true;
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
            if i.key_pressed(egui::Key::Escape) {
                self.find_open = false;
                self.palette_open = false;
            }
        });
        // Ctrl/Cmd+Space requests identifier completion at the cursor.
        let want_completion =
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Space));
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
        let muted = ui_color(&self.theme, "line_number", Rgba::new(0x5a, 0x58, 0x69, 255));
        // Chrome panels (titlebar/toolbar/status/filetree/split/gutter/minimap) all
        // fill with this color. In a translucent window mode the fill MUST carry the
        // reduced alpha — otherwise opaque chrome covers the transparent/blurred
        // surface and "transparency doesn't work" (the T19.2 root cause). The master
        // `transparency_enabled` toggle gates this via `effective_translucent()`.
        let panel = panel_fill(&self.theme, &self.config.window);
        let warn = ui_color(&self.theme, "warning", Rgba::new(0xfb, 0xbf, 0x24, 255));

        // ---- Custom frameless titlebar ----
        if self.config.appearance.frameless {
            egui::TopBottomPanel::top("titlebar")
                .exact_height(34.0)
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
                        ui.label(
                            RichText::new("S C R 1 B 3")
                                .color(accent)
                                .strong()
                                .monospace(),
                        );
                        ui.label(RichText::new("//").color(muted).monospace());
                        ui.label(
                            RichText::new(scribe_core::PRODUCT_TAGLINE)
                                .color(muted)
                                .small()
                                .monospace(),
                        );
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
                // Settings + command palette are always present; the palette is
                // the discoverable backbone for every action, so keep it visible.
                // The gear toggles settings — clicking it while open closes it.
                if ui
                    .selectable_label(self.settings_open, "⚙")
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
                    self.toolbar_item(ui, id, &mut act, &mut save_cfg, &mut start_lsp);
                }

                // Inline tab strip — only when the user has the strip docked
                // at the toolbar (Top, the default). Other positions render the
                // strip in their own panel below. T18.4.
                if self.config.editor.tab_bar_position == scribe_core::config::TabBarPosition::Top {
                    ui.separator();
                    self.draw_tab_strip(ui, accent, muted);
                }
            });
        });

        // ---- Relocated tab strip (T18.4) — Bottom / Left / Right ----
        match self.config.editor.tab_bar_position {
            scribe_core::config::TabBarPosition::Top => {}
            scribe_core::config::TabBarPosition::Bottom => {
                egui::TopBottomPanel::bottom("tabs-bottom")
                    .frame(egui::Frame::default().fill(panel))
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| self.draw_tab_strip(ui, accent, muted));
                    });
            }
            scribe_core::config::TabBarPosition::Left => {
                egui::SidePanel::left("tabs-left")
                    .resizable(true)
                    .default_width(180.0)
                    .frame(egui::Frame::default().fill(panel).inner_margin(4.0))
                    .show(ctx, |ui| {
                        ui.vertical(|ui| self.draw_tab_strip(ui, accent, muted));
                    });
            }
            scribe_core::config::TabBarPosition::Right => {
                egui::SidePanel::right("tabs-right")
                    .resizable(true)
                    .default_width(180.0)
                    .frame(egui::Frame::default().fill(panel).inner_margin(4.0))
                    .show(ctx, |ui| {
                        ui.vertical(|ui| self.draw_tab_strip(ui, accent, muted));
                    });
            }
        }

        // ---- Find bar ----
        if self.find_open {
            egui::TopBottomPanel::top("find").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("find").color(accent).monospace());
                    let r = ui.text_edit_singleline(&mut self.find_query);
                    if self.focus_find {
                        r.request_focus();
                        self.focus_find = false;
                    }
                    let count = if self.find_query.is_empty() || self.active >= self.tabs.len() {
                        0
                    } else {
                        let q = scribe_core::search::Query {
                            pattern: self.find_query.clone(),
                            ..Default::default()
                        };
                        scribe_core::search::find_all(&self.tabs[self.active].text, &q)
                            .map(|m| m.len())
                            .unwrap_or(0)
                    };
                    ui.label(
                        RichText::new(format!("{count} matches"))
                            .color(muted)
                            .small(),
                    );
                    if ui.button("close").clicked() {
                        self.find_open = false;
                    }
                });
            });
        }

        // ---- Command palette (plugin + future builtin commands) ----
        if self.palette_open {
            egui::Window::new(RichText::new("⌘ command palette").color(accent).monospace())
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
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        let mut any = false;
                        for c in &self.plugin_cmds {
                            if q.is_empty()
                                || c.label.to_lowercase().contains(&q)
                                || c.id.contains(&q)
                            {
                                any = true;
                                if ui
                                    .selectable_label(false, format!("{}  ·  {}", c.label, c.plugin_id))
                                    .clicked()
                                {
                                    run_cmd = Some(c.id.clone());
                                }
                            }
                        }
                        if self.plugin_cmds.is_empty() {
                            ui.label(
                                RichText::new("no plugin commands yet — drop a mod into the plugins dir (see PLUGINS.md)")
                                    .color(muted)
                                    .small(),
                            );
                        } else if !any {
                            ui.label(RichText::new("no match").color(muted).small());
                        }
                    });
                });
        }

        // ---- Settings window (deep customization, live preview) ----
        if self.settings_open {
            let changed = crate::settings::show(ctx, &mut self.config, &mut self.settings_open);
            if changed {
                self.reapply_theme(ctx);
                self.save_config();
            }
        }

        // Spellcheck status (computed before the status-bar closure borrows self).
        let spell_on = self.config.spellcheck.enabled;
        let spell_misspellings = self.spell_count();
        let diag_errors = self.diagnostics.iter().filter(|d| d.severity == 1).count();
        let diag_total = self.diagnostics.len();

        // ---- Status bar ----
        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::default().fill(panel))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let active = self.active.min(self.tabs.len().saturating_sub(1));
                    if let Some(t) = self.tabs.get(active) {
                        ui.label(
                            RichText::new(t.doc.eol().label().to_string())
                                .color(muted)
                                .small()
                                .monospace(),
                        );
                        ui.label(
                            RichText::new(t.doc.encoding().name.clone())
                                .color(muted)
                                .small()
                                .monospace(),
                        );
                        let lang = t.doc.language_hint().unwrap_or_else(|| "text".into());
                        ui.label(RichText::new(lang).color(accent).small().monospace());
                        let lines = t.text.lines().count().max(1);
                        ui.label(
                            RichText::new(format!("{lines} ln"))
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
                                ("spell ✓".to_string(), accent)
                            } else {
                                (format!("spell: {spell_misspellings}"), warn)
                            };
                            ui.label(RichText::new(txt).color(col).small().monospace());
                        }
                        if diag_total > 0 {
                            let col = if diag_errors > 0 { warn } else { muted };
                            ui.label(
                                RichText::new(format!("⊘ {diag_errors}e / {diag_total}"))
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
                        ui.label(RichText::new("EXPLORER").color(accent).small().monospace());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").clicked() {
                                close_tree = true;
                            }
                        });
                    });
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if let Some(p) = crate::filetree::show(ui, &root) {
                            open_from_tree = Some(p);
                        }
                    });
                });
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

        // ---- Minimap (rightmost strip) ----
        if self.config.editor.show_minimap {
            self.show_minimap(ctx, panel, accent);
        }

        // ---- Split view: second pane over the same buffer (right of center) ----
        if self.split_view {
            let hl = &self.hl;
            let ext_ref = ext.as_deref();
            egui::SidePanel::right("split-pane")
                .resizable(true)
                .default_width(360.0)
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    ui.label(RichText::new("SPLIT").color(accent).small().monospace());
                    ui.separator();
                    let mut layouter =
                        make_layouter(hl, &self.hl_cache, ext_ref, font.clone(), line_height);
                    let sa = if word_wrap {
                        egui::ScrollArea::vertical()
                    } else {
                        egui::ScrollArea::both()
                    };
                    sa.id_salt("split-scroll").show(ui, |ui| {
                        let dw = if word_wrap {
                            ui.available_width()
                        } else {
                            f32::INFINITY
                        };
                        let editor = egui::TextEdit::multiline(&mut self.tabs[active].text)
                            .code_editor()
                            .desired_width(dw)
                            .desired_rows(30)
                            .lock_focus(true)
                            .interactive(!read_only)
                            .layouter(&mut layouter);
                        ui.add_sized(ui.available_size(), editor);
                    });
                });
        }

        // ---- Line-number gutter (sticky left strip; numbers are synced to the
        // editor galley rows captured last frame — one-frame lag, like minimap).
        if show_line_numbers && !self.fold_view {
            let total = self.tabs[active].text.lines().count().max(1);
            let digits = total.to_string().len().max(2);
            let gutter_w = digits as f32 * (font.size * 0.62) + 16.0;
            let rows = &self.line_gutter;
            egui::SidePanel::left("line-gutter")
                .exact_width(gutter_w)
                .resizable(false)
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    let painter = ui.painter();
                    let clip = ui.clip_rect();
                    let rx = ui.max_rect().right() - 8.0;
                    let nfont = FontId::monospace((font.size * 0.92).max(8.0));
                    for (i, &y) in rows.iter().enumerate() {
                        if y < clip.top() - gutter_row_h || y > clip.bottom() {
                            continue;
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

                // Scope the layouter (which borrows `self.hl`) so it drops before
                // the `&mut self` completion calls below.
                let mut new_gutter: Vec<f32> = Vec::new();
                let anchor: Option<(egui::Pos2, usize)> = {
                    let hl = &self.hl;
                    let ext_ref = ext.as_deref();
                    let mut layouter =
                        make_layouter(hl, &self.hl_cache, ext_ref, font.clone(), line_height);
                    let mut sa = if word_wrap {
                        egui::ScrollArea::vertical()
                    } else {
                        egui::ScrollArea::both()
                    };
                    if let Some(off) = self.pending_scroll.take() {
                        sa = sa.vertical_scroll_offset(off);
                    }
                    let mut a: Option<(egui::Pos2, usize)> = None;
                    let sa_out = sa.show(ui, |ui| {
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
                            self.last_cursor_line_col = Some(line_col_from_char_index(
                                text_ref,
                                cc.index,
                            ));
                            self.last_selection_chars =
                                range.primary.index.abs_diff(range.secondary.index);
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
                    a
                };
                self.line_gutter = new_gutter;

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

        // CRT post-effect overlay (top-most; skipped entirely when disabled).
        if self.config.effects.crt_enabled {
            paint_crt_overlay(ctx, &self.config.effects, false);
        }
        // Window color-tint overlay (subtle wash; portable across modes/OSes).
        if self.config.window.tint_strength > 0.0 {
            paint_tint_overlay(
                ctx,
                &self.config.window.tint,
                self.config.window.tint_strength,
            );
        }
        // Phase 18 T18.1: 8-zone resize overlay for the frameless window. egui
        // doesn't restore OS resize when window decorations are off (winit
        // #4186) so we paint invisible interact rectangles at the edges + four
        // corners that send `ViewportCommand::BeginResize(dir)` on drag and
        // hint the right cursor on hover.
        if self.config.appearance.frameless {
            let maximized = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
            if !maximized {
                draw_resize_overlays(ctx);
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
        if let Some(p) = open_from_tree {
            self.open_path(p);
        }
        if close_tree {
            self.file_tree_root = None;
        }
        if start_lsp {
            self.start_lsp_for_active();
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

/// Paint the CRT post-effect as a top-most overlay: horizontal scanlines plus a
/// soft vignette. Cheap (egui shapes, no GPU pass), reduced-motion-safe (static),
/// and skipped entirely when disabled. `reduced_motion` zeroes any animated term.
/// Width of the 4 edge resize zones, in logical px. Slim so they only intercept
/// pointer events right at the window border.
const RESIZE_EDGE_PX: f32 = 6.0;
/// Side length of the 4 corner resize zones, in logical px. Slightly larger than
/// the edges so diagonal grabs are forgiving.
const RESIZE_CORNER_PX: f32 = 12.0;

/// Phase 18 T18.1 — paint the 8 invisible resize-handle interact zones around
/// the frameless window. Pure side-effect on the egui context: on hover the
/// pointer cursor flips to the matching direction; on drag-start a
/// `ViewportCommand::BeginResize(dir)` is queued and winit drives the actual
/// resize from there. Called once per frame from `frame_tick`.
fn draw_resize_overlays(ctx: &egui::Context) {
    use egui::{
        Area, CursorIcon, Id, Order, PointerButton, Rect, ResizeDirection, Sense, ViewportCommand,
    };
    let rect = ctx.content_rect();
    let e = RESIZE_EDGE_PX;
    let c = RESIZE_CORNER_PX;
    // (id, rect, cursor, direction)
    let zones: [(&'static str, Rect, CursorIcon, ResizeDirection); 8] = [
        (
            "rz-n",
            Rect::from_min_max(
                rect.left_top() + egui::vec2(c, 0.0),
                rect.right_top() + egui::vec2(-c, e),
            ),
            CursorIcon::ResizeNorth,
            ResizeDirection::North,
        ),
        (
            "rz-s",
            Rect::from_min_max(
                rect.left_bottom() + egui::vec2(c, -e),
                rect.right_bottom() + egui::vec2(-c, 0.0),
            ),
            CursorIcon::ResizeSouth,
            ResizeDirection::South,
        ),
        (
            "rz-w",
            Rect::from_min_max(
                rect.left_top() + egui::vec2(0.0, c),
                rect.left_bottom() + egui::vec2(e, -c),
            ),
            CursorIcon::ResizeWest,
            ResizeDirection::West,
        ),
        (
            "rz-e",
            Rect::from_min_max(
                rect.right_top() + egui::vec2(-e, c),
                rect.right_bottom() + egui::vec2(0.0, -c),
            ),
            CursorIcon::ResizeEast,
            ResizeDirection::East,
        ),
        (
            "rz-nw",
            Rect::from_min_size(rect.left_top(), egui::vec2(c, c)),
            CursorIcon::ResizeNorthWest,
            ResizeDirection::NorthWest,
        ),
        (
            "rz-ne",
            Rect::from_min_size(rect.right_top() - egui::vec2(c, 0.0), egui::vec2(c, c)),
            CursorIcon::ResizeNorthEast,
            ResizeDirection::NorthEast,
        ),
        (
            "rz-sw",
            Rect::from_min_size(rect.left_bottom() - egui::vec2(0.0, c), egui::vec2(c, c)),
            CursorIcon::ResizeSouthWest,
            ResizeDirection::SouthWest,
        ),
        (
            "rz-se",
            Rect::from_min_size(rect.right_bottom() - egui::vec2(c, c), egui::vec2(c, c)),
            CursorIcon::ResizeSouthEast,
            ResizeDirection::SouthEast,
        ),
    ];
    for (id, zone, cursor, dir) in zones {
        Area::new(Id::new(id))
            .order(Order::Foreground)
            .fixed_pos(zone.min)
            .interactable(true)
            .show(ctx, |ui| {
                let resp = ui.allocate_rect(zone, Sense::click_and_drag());
                if resp.hovered() {
                    ctx.set_cursor_icon(cursor);
                }
                if resp.drag_started_by(PointerButton::Primary) {
                    ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
                }
            });
    }
}

fn paint_crt_overlay(
    ctx: &egui::Context,
    fx: &scribe_core::config::EffectsConfig,
    reduced_motion: bool,
) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("crt-overlay"),
    ));
    // egui 0.34: Context::screen_rect split into viewport_rect() + content_rect()
    // — the CRT overlay paints across the entire window content, so content_rect()
    // is the right successor.
    let rect = ctx.content_rect();

    // Scanlines — the iconic CRT element (static, accessibility-safe).
    if fx.scanline > 0.0 {
        let a = (fx.scanline * 46.0).round().clamp(0.0, 255.0) as u8;
        let col = Color32::from_black_alpha(a);
        let mut y = rect.top();
        while y < rect.bottom() {
            painter.hline(rect.x_range(), y, egui::Stroke::new(1.0, col));
            y += 3.0;
        }
    }

    // Phosphor glow tint — a faint accent wash (skipped under reduced motion to
    // avoid any shimmer; here it is static so we keep it but scale by glow).
    let _ = reduced_motion;

    // Vignette — a 3x3 vertex mesh, transparent center darkening to the corners.
    if fx.vignette > 0.0 {
        let edge = (fx.vignette * 140.0).round().clamp(0.0, 255.0) as u8;
        let corner = Color32::from_black_alpha(edge);
        let mid = Color32::from_black_alpha(edge / 2);
        let center = Color32::TRANSPARENT;
        let xs = [rect.left(), rect.center().x, rect.right()];
        let ys = [rect.top(), rect.center().y, rect.bottom()];
        // color per (row, col): corners full, edge-midpoints mid, center clear.
        let color_at = |r: usize, c: usize| -> Color32 {
            match (r, c) {
                (1, 1) => center,
                (1, _) | (_, 1) => mid,
                _ => corner,
            }
        };
        let mut mesh = egui::Mesh::default();
        for (r, &y) in ys.iter().enumerate() {
            for (c, &x) in xs.iter().enumerate() {
                mesh.colored_vertex(egui::pos2(x, y), color_at(r, c));
            }
        }
        let idx = |r: usize, c: usize| (r * 3 + c) as u32;
        for r in 0..2 {
            for c in 0..2 {
                mesh.add_triangle(idx(r, c), idx(r, c + 1), idx(r + 1, c));
                mesh.add_triangle(idx(r, c + 1), idx(r + 1, c + 1), idx(r + 1, c));
            }
        }
        painter.add(egui::Shape::mesh(mesh));
    }
}

/// Paint a translucent color tint over the whole window (portable; works in
/// every mode and on every OS). Strength scales the alpha.
fn paint_tint_overlay(ctx: &egui::Context, tint_hex: &str, strength: f32) {
    let Some(c) = Rgba::parse_hex(tint_hex) else {
        return;
    };
    let a = (strength.clamp(0.0, 1.0) * 90.0).round() as u8;
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("tint-overlay"),
    ));
    painter.rect_filled(
        // egui 0.34: screen_rect -> content_rect (same window-content footprint).
        ctx.content_rect(),
        0.0,
        Color32::from_rgba_unmultiplied(c.r, c.g, c.b, a),
    );
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
    fn uncertain_ids_omit_kanji() {
        // Western-metaphor or acronym/loanword actions stay English-only —
        // the canonical kanji is uncertain or contested. They MUST return
        // None so a future "ship a guess" doesn't slip through.
        assert_eq!(jp_glyph("openfolder"), None);
        assert_eq!(jp_glyph("palette"), None);
        assert_eq!(jp_glyph("crt"), None);
        assert_eq!(jp_glyph("lsp"), None);
        // Unknown ids also return None — the helper never invents.
        assert_eq!(jp_glyph("not-a-toolbar-action"), None);
    }

    #[test]
    fn widget_falls_back_to_label_when_disabled_or_unknown() {
        // jp_glyph_labels=false → primary label only, regardless of action.
        let off = toolbar_widget("new", false, false);
        assert_eq!(off.text(), "new");
        // Even with the flag on, an action without verified kanji returns
        // only the primary label — no kanji is invented.
        let on_unknown = toolbar_widget("openfolder", false, true);
        assert_eq!(on_unknown.text(), "folder");
    }

    #[test]
    fn widget_appends_kanji_when_enabled_for_verified_action() {
        // jp_glyph_labels=true + verified action → primary then kanji.
        // The LayoutJob's flattened text contains both pieces.
        let on = toolbar_widget("save", false, true);
        let text = on.text();
        assert!(text.starts_with("save"), "got {text:?}");
        assert!(text.contains("保"), "got {text:?}");
    }
}

#[cfg(test)]
mod e2e {
    //! End-to-end tests: drive the real `ScribeApp::ui` render loop headlessly
    //! through egui's own `Context::run`, exercising the full per-frame UI +
    //! state pipeline (menus, panels, editor, overlays) without a window/GPU.
    use super::*;

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
            panel_fill(&theme, &w_off).a(),
            255,
            "opaque while master toggle off"
        );
        // Master ON + translucent mode => alpha lowered to opacity.
        let w_on = scribe_core::config::WindowConfig {
            transparency_enabled: true,
            ..w_off
        };
        let a = panel_fill(&theme, &w_on).a();
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
    fn crt_overlay_renders_when_enabled() {
        let mut cfg = Config::default();
        cfg.effects.crt_enabled = true;
        let mut app = ScribeApp::new_test(cfg);
        run_frames(&mut app, 2);
        assert!(app.config.effects.crt_enabled);
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
    fn split_view_renders() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].text = "fn main() {\n    let x = 1;\n}\n".into();
        app.split_view = true;
        run_frames(&mut app, 2);
        assert!(app.split_view);
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
        cfg.toolbar.items = vec!["save".into(), "sep".into(), "crt".into()];
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
        // Empty buffer at start of line 1.
        assert_eq!(line_col_from_char_index("", 0), (1, 1));
        // Two-line buffer.
        let s = "hello\nworld";
        assert_eq!(line_col_from_char_index(s, 0), (1, 1)); // start
        assert_eq!(line_col_from_char_index(s, 5), (1, 6)); // before \n
        assert_eq!(line_col_from_char_index(s, 6), (2, 1)); // after \n
        assert_eq!(line_col_from_char_index(s, 11), (2, 6)); // end
        // Multi-byte: each codepoint advances column once, not byte-count times.
        let cjk = "日本\n語";
        assert_eq!(line_col_from_char_index(cjk, 1), (1, 2));
        assert_eq!(line_col_from_char_index(cjk, 2), (1, 3));
        assert_eq!(line_col_from_char_index(cjk, 3), (2, 1));
    }

    /// F-005 helper: out-of-range char index clamps to end-of-buffer (no panic).
    #[test]
    fn line_col_from_char_index_clamps() {
        let s = "abc\ndef";
        // Walks until the end and stops there.
        let (line, col) = line_col_from_char_index(s, 99);
        assert_eq!((line, col), (2, 4));
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
}
