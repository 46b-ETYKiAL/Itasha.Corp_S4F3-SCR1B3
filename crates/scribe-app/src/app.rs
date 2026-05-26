//! The SCR1B3 application shell: frameless brand titlebar, tab strip, syntect-
//! highlighted editor surface, find bar, and status bar. v1 keeps the shell in
//! one focused module; later phases split tabs/titlebar/chrome into submodules.

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

/// Parse a `#RRGGBB` tint to an RGBA quad for native blur tinting.
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
}

impl EditorTab {
    fn scratch() -> Self {
        Self {
            doc: Document::scratch(),
            text: String::new(),
        }
    }

    fn from_path(path: PathBuf) -> Result<Self, String> {
        let doc = Document::open(&path).map_err(|e| e.to_string())?;
        let text = doc.text();
        Ok(Self { doc, text })
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
    /// Minimap: a scaled side-strip overview of the active document.
    minimap: bool,
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
        cc.egui_ctx.set_visuals(app.current_visuals());
        app.visuals_applied = true;
        // Apply the OS glass/acrylic/mica/vibrancy effect for the chosen mode.
        apply_window_effect(cc, app.config.window.mode, &app.config.window.tint);
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
            minimap: false,
            fold_view: false,
            folds: std::collections::BTreeSet::new(),
            completion: None,
            pending_scroll: None,
            scroll_metrics: (0.0, 1.0, 1.0),
            minimap_cache: std::cell::RefCell::new(None),
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
        if self.config.window.mode.is_translucent() {
            scribe_render::apply_window_opacity(&mut v, self.config.window.opacity);
        }
        v
    }

    /// Apply the current theme to the egui context (after a theme/config change).
    fn reapply_theme(&mut self, ctx: &egui::Context) {
        self.theme = load_theme(&self.config.appearance.theme);
        ctx.set_visuals(self.current_visuals());
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
                            let g = ui.fonts(|f| {
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
        let hl = &self.hl;
        let mut layouter = make_layouter(hl, &self.hl_cache, ext, font);
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
#[derive(Default)]
struct Pending {
    new: bool,
    open: bool,
    open_folder: bool,
    save: bool,
}

fn ui_color(theme: &Theme, key: &str, default: Rgba) -> Color32 {
    scribe_render::color32(theme.ui(key, default))
}

/// Build a syntect-colored `LayoutJob` for the editor surface. Free function so
/// the egui `layouter` closure captures only the highlighter, not `self`.
fn highlight_job(hl: &Highlighter, text: &str, ext: Option<&str>, font: FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    let lines = hl.highlight_document(text, ext);
    let mut char_cursor = 0usize;
    // Reconstruct text with colored spans line by line.
    for (li, line) in text.split_inclusive('\n').enumerate() {
        if let Some(spans) = lines.get(li) {
            let mut byte = 0usize;
            for s in spans {
                let seg = &line.get(s.range.clone()).unwrap_or("");
                if !seg.is_empty() {
                    let mut fmt =
                        TextFormat::simple(font.clone(), scribe_render::syntax_color32(s.color));
                    if s.italic {
                        fmt.italics = true;
                    }
                    job.append(seg, 0.0, fmt);
                }
                byte = s.range.end;
            }
            // Append any tail not covered by spans.
            if byte < line.len() {
                job.append(
                    &line[byte..],
                    0.0,
                    TextFormat::simple(font.clone(), Color32::GRAY),
                );
            }
        } else {
            job.append(line, 0.0, TextFormat::simple(font.clone(), Color32::GRAY));
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
) -> impl FnMut(&egui::Ui, &str, f32) -> std::sync::Arc<egui::Galley> + 'a {
    move |ui: &egui::Ui, text: &str, wrap: f32| {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        ext.hash(&mut hasher);
        font.size.to_bits().hash(&mut hasher);
        let key = hasher.finish();
        let job_arc = {
            let mut slot = cache.borrow_mut();
            match slot.as_ref() {
                Some((k, j)) if *k == key => j.clone(),
                _ => {
                    let arc = std::sync::Arc::new(highlight_job(hl, text, ext, font.clone()));
                    *slot = Some((key, arc.clone()));
                    arc
                }
            }
        };
        let mut job = (*job_arc).clone();
        job.wrap.max_width = wrap;
        ui.fonts(|f| f.layout_job(job))
    }
}

/// Byte offset of char index `ci` in `s` (clamped to `s.len()`).
fn char_to_byte(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
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
    // Try a user theme file `<config_dir>/themes/<name>.toml`; fall back to brand.
    if let Some(dir) = Config::config_dir() {
        let p = dir.join("themes").join(format!("{name}.toml"));
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(t) = Theme::from_toml_str(&s) {
                return t;
            }
        }
    }
    Theme::itasha_void()
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

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui(ctx);
    }
}

impl ScribeApp {
    /// All per-frame UI + state logic. Separated from `eframe::App::update` (which
    /// only forwards here) so it can be driven headlessly by `egui_kittest` E2E
    /// tests without an `eframe::Frame`.
    pub(crate) fn ui(&mut self, ctx: &egui::Context) {
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
                self.find_open = true;
            }
            // Ctrl/Cmd+Shift+P opens the command palette (plugin + builtin cmds).
            if cmd && i.modifiers.shift && i.key_pressed(egui::Key::P) {
                self.palette_open = true;
                self.palette_query.clear();
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
        let panel = ui_color(&self.theme, "panel", Rgba::new(0x0d, 0x0b, 0x14, 255));
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
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
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
                            if window_btn(ui, "✕", accent).clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            if window_btn(ui, "▢", muted).clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
                            }
                            if window_btn(ui, "—", muted).clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                            }
                        });
                    });
                });
        }

        // ---- Menu / toolbar ----
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("New      Ctrl+N").clicked() {
                        act.new = true;
                        ui.close_menu();
                    }
                    if ui.button("Open…    Ctrl+O").clicked() {
                        act.open = true;
                        ui.close_menu();
                    }
                    if ui.button("Open Folder…").clicked() {
                        act.open_folder = true;
                        ui.close_menu();
                    }
                    if ui.button("Save     Ctrl+S").clicked() {
                        act.save = true;
                        ui.close_menu();
                    }
                    if ui.button("Save As…").clicked() {
                        self.save_as_active();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.button("Find          Ctrl+F").clicked() {
                        self.find_open = true;
                        ui.close_menu();
                    }
                    if ui.button("Command Palette  Ctrl+Shift+P").clicked() {
                        self.palette_open = true;
                        self.palette_query.clear();
                        ui.close_menu();
                    }
                });
                ui.menu_button("View", |ui| {
                    let mut sc = self.config.spellcheck.enabled;
                    if ui.checkbox(&mut sc, "Spellcheck (offline)").clicked() {
                        self.config.spellcheck.enabled = sc;
                        save_cfg = true;
                        ui.close_menu();
                    }
                    let mut crt = self.config.effects.crt_enabled;
                    if ui.checkbox(&mut crt, "CRT effect").clicked() {
                        self.config.effects.crt_enabled = crt;
                        save_cfg = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.checkbox(&mut self.split_view, "Split view").clicked() {
                        ui.close_menu();
                    }
                    if ui.checkbox(&mut self.minimap, "Minimap").clicked() {
                        ui.close_menu();
                    }
                    if ui.checkbox(&mut self.fold_view, "Folded view").clicked() {
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Settings…").clicked() {
                        self.settings_open = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button("Language", |ui| {
                    if ui.button("Start language server").clicked() {
                        start_lsp = true;
                        ui.close_menu();
                    }
                    ui.label(
                        RichText::new("uses your installed LSP server, if any")
                            .weak()
                            .small(),
                    );
                });
                ui.separator();
                // Tab strip
                let active = self.active;
                let mut switch_to = None;
                let mut close = None;
                for (i, t) in self.tabs.iter().enumerate() {
                    let selected = i == active;
                    let label =
                        RichText::new(t.title()).color(if selected { accent } else { muted });
                    if ui.selectable_label(selected, label).clicked() {
                        switch_to = Some(i);
                    }
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
            });
        });

        // ---- Find bar ----
        if self.find_open {
            egui::TopBottomPanel::top("find").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("find").color(accent).monospace());
                    ui.text_edit_singleline(&mut self.find_query);
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
                    ui.text_edit_singleline(&mut self.palette_query);
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
        let ext = self.tabs[active].doc.language_hint();
        let read_only = self.tabs[active].doc.is_read_only_large();

        // ---- Minimap (rightmost strip) ----
        if self.minimap {
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
                    let mut layouter = make_layouter(hl, &self.hl_cache, ext_ref, font.clone());
                    egui::ScrollArea::both()
                        .id_salt("split-scroll")
                        .show(ui, |ui| {
                            let editor = egui::TextEdit::multiline(&mut self.tabs[active].text)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .desired_rows(30)
                                .lock_focus(true)
                                .interactive(!read_only)
                                .layouter(&mut layouter);
                            ui.add_sized(ui.available_size(), editor);
                        });
                });
        }

        // ---- Central editor surface ----
        egui::CentralPanel::default().show(ctx, |ui| {
            // Folded read-only preview is a distinct surface (no live editing).
            if self.fold_view {
                self.show_fold_view(ui, font.clone(), ext.as_deref());
                return;
            }

            // Scope the layouter (which borrows `self.hl`) so it drops before
            // the `&mut self` completion calls below.
            let anchor: Option<(egui::Pos2, usize)> = {
                let hl = &self.hl;
                let ext_ref = ext.as_deref();
                let mut layouter = make_layouter(hl, &self.hl_cache, ext_ref, font.clone());
                let mut sa = egui::ScrollArea::both();
                if let Some(off) = self.pending_scroll.take() {
                    sa = sa.vertical_scroll_offset(off);
                }
                let mut a: Option<(egui::Pos2, usize)> = None;
                let sa_out = sa.show(ui, |ui| {
                    let editor = egui::TextEdit::multiline(&mut self.tabs[active].text)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(30)
                        .lock_focus(true)
                        .interactive(!read_only)
                        .layouter(&mut layouter);
                    let out = editor.show(ui);
                    if let Some(range) = out.cursor_range {
                        let cc = range.primary.ccursor;
                        let rect = out.galley.pos_from_ccursor(cc);
                        let pos = out.galley_pos + egui::vec2(rect.min.x, rect.max.y);
                        a = Some((pos, cc.index));
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

fn window_btn(ui: &mut egui::Ui, glyph: &str, color: Color32) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(glyph).color(color).monospace()).frame(false))
}

/// Paint the CRT post-effect as a top-most overlay: horizontal scanlines plus a
/// soft vignette. Cheap (egui shapes, no GPU pass), reduced-motion-safe (static),
/// and skipped entirely when disabled. `reduced_motion` zeroes any animated term.
fn paint_crt_overlay(
    ctx: &egui::Context,
    fx: &scribe_core::config::EffectsConfig,
    reduced_motion: bool,
) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("crt-overlay"),
    ));
    let rect = ctx.screen_rect();

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
        ctx.screen_rect(),
        0.0,
        Color32::from_rgba_unmultiplied(c.r, c.g, c.b, a),
    );
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
            let _ = ctx.run(input, |ctx| app.ui(ctx));
        }
    }

    #[test]
    fn renders_default_without_panic() {
        let mut app = ScribeApp::new_test(Config::default());
        run_frames(&mut app, 3);
        assert_eq!(app.tabs.len(), 1, "expected one scratch tab");
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
        app.minimap = true;
        run_frames(&mut app, 2);
        assert!(app.minimap);
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
}
