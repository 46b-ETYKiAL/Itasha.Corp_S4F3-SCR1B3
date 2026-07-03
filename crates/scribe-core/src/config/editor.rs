//! Editor pane configuration: the [`EditorConfig`] struct + its caret /
//! scrollbar / tab-bar enums and the MRU + scroll-position helpers.

use super::default_true;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EditorConfig {
    pub tab_width: usize,
    pub insert_spaces: bool,
    pub show_line_numbers: bool,
    /// When true (default), draw a Notepad++-style change bar in the gutter:
    /// lines edited this session but unsaved get one colour, edited-then-saved
    /// lines another, untouched lines none.
    pub show_change_bar: bool,
    pub show_minimap: bool,
    pub word_wrap: bool,
    pub auto_save: bool,
    pub restore_session: bool,
    /// Where the open-tab strip lives: top (default, inline with the toolbar),
    /// bottom (status-side), left, or right. Phase 18 T18.4.
    pub tab_bar_position: TabBarPosition,
    /// When the tab bar is on the Left or Right, ROTATE each tab's label 90° so
    /// the text reads vertically (bottom-to-top), while the tabs stay stacked in
    /// a single column. `false` (default) keeps the labels horizontal — the
    /// familiar look. No effect for the Top/Bottom positions.
    #[serde(default, alias = "side_tabs_vertical")]
    pub side_tabs_rotated: bool,
    /// Note (editor) syntax colour theme — the text colour scheme for the note
    /// body, independent of the app chrome theme (#104). One of the bundled
    /// syntect themes; an unknown value falls back to the default.
    #[serde(default = "default_note_theme")]
    pub note_theme: String,
    /// #E P1 — when true, the active chrome theme's documented `[syntax]` map
    /// (incl. the `markup.*` keys) drives in-editor token colours, unifying the
    /// two colour systems so editing your theme TOML actually recolours the
    /// editor. Default OFF — the `note_theme` syntect preset stays authoritative
    /// unless the user opts in, so existing setups are unchanged.
    #[serde(default)]
    pub syntax_from_theme: bool,
    /// #D — detect bare `http(s)://` URLs in editor text and render them as a
    /// colored, underlined, Ctrl/Cmd-click-to-open link (scheme allow-listed to
    /// http/https). Default ON — links are a calm, low-risk affordance and the
    /// open is gated on an explicit modifier-click.
    #[serde(default = "default_true")]
    pub detect_links: bool,
    /// Master switch for the extra markdown/note token-colouring passes that
    /// colour tokens the grammar leaves plain: `----` & decorative dividers,
    /// `#tags`, `~~strikethrough~~`, task boxes `[ ]`/`[x]`, and table `|` cell
    /// separators. Default ON. Turn OFF to disable them all at once; the
    /// per-token switches below disable individual passes when the master is on.
    #[serde(default = "default_true")]
    pub md_rich_coloring: bool,
    /// Colour decorative divider lines (`----`, `====//====//`, `* * *`, setext
    /// underlines, box-drawing rules). Only active when `md_rich_coloring`.
    #[serde(default = "default_true")]
    pub md_color_dividers: bool,
    /// Colour `#tag` tokens in the editor. Only active when `md_rich_coloring`.
    #[serde(default = "default_true")]
    pub md_color_tags: bool,
    /// Colour `~~strikethrough~~` spans. Only active when `md_rich_coloring`.
    #[serde(default = "default_true")]
    pub md_color_strikethrough: bool,
    /// Colour task checkboxes `[ ]`/`[x]`. Only active when `md_rich_coloring`.
    #[serde(default = "default_true")]
    pub md_color_task_boxes: bool,
    /// Colour table `|` cell separators. Only active when `md_rich_coloring`.
    #[serde(default = "default_true")]
    pub md_color_table_pipes: bool,
    /// Phase 18 T18.2 — enable the multi-note grid. When ON, the central
    /// editor surface renders every open tab as a movable, resizable pane
    /// inside an egui_tiles tree (up to 6 panes). Default OFF — the
    /// existing single-pane render path is unchanged for users who don't
    /// opt in.
    #[serde(default)]
    pub grid_enabled: bool,
    /// #R6 — persisted multi-note grid layout (a JSON-serialised
    /// `egui_tiles::Tree<Pane>` from `grid::to_json`). Restored on launch when
    /// the grid is enabled and the persisted panes match the reopened doc set,
    /// so a split arrangement survives a restart. `None` until a grid layout has
    /// been used.
    #[serde(default)]
    pub grid_layout: Option<String>,
    /// F-012 from docs/audits/overlooked-surfaces-2026-05-29.md: MRU
    /// list of recently-opened file paths. Capped at
    /// [`RECENT_FILES_MAX`]; freshly opened paths push to the front and
    /// duplicates collapse to the front position.
    #[serde(default)]
    pub recent_files: Vec<PathBuf>,
    /// MRU of folders opened as the file-tree root. Same MRU discipline as
    /// `recent_files` (front-push, dedup, capped) via `record_recent_file`.
    #[serde(default)]
    pub recent_folders: Vec<PathBuf>,
    /// F-013 from docs/audits/overlooked-surfaces-2026-05-29.md: set true
    /// after the welcome modal is dismissed. Used to suppress the welcome
    /// modal on subsequent launches.
    #[serde(default)]
    pub first_run_completed: bool,
    /// F-021 from docs/audits/overlooked-surfaces-2026-05-29.md: per-file
    /// scroll-offset map (path string → vertical pixel offset). Captured
    /// on tab close + open, restored on next open of the same path. Capped
    /// at [`SCROLL_POS_CAP`].
    #[serde(default)]
    pub scroll_positions: std::collections::HashMap<String, f32>,
    /// KEYSTONE — opt into the in-house rope editor (own cursor / selection /
    /// undo) instead of egui's `TextEdit` for normal-size files. Default OFF:
    /// the egui path stays the default while the owned editor matures (it does
    /// not yet have IME / mouse-selection parity). Read-only huge files always
    /// use the rope browse path regardless of this flag.
    #[serde(default)]
    pub experimental_rope_editor: bool,
    /// Wave-3 perf: byte size above which an *editable* buffer is auto-routed
    /// through the viewport-culled rope editor even when `experimental_rope_editor`
    /// is off — so a multi-MiB file does not pay the per-frame O(n) egui `TextEdit`
    /// cost. The rope path trades away a few large-file niceties (breadcrumb bar,
    /// sticky-scroll headers — both already disabled past 500 KiB anyway — plus
    /// spellcheck squiggles and Tab→spaces) for O(viewport) rendering, which is
    /// the right call at this size. `0` disables auto-promotion entirely. Default
    /// 16 MiB (aligns with the core mmap threshold).
    #[serde(default = "default_rope_auto_threshold")]
    pub rope_editor_auto_threshold_bytes: usize,
    /// Persist UNSAVED buffer content (incl. untitled scratch notes) so it
    /// survives a restart or crash without an explicit save — the Notepad++
    /// "session snapshot" / VS Code "Hot Exit" behaviour. Backups live in
    /// `<config>/backup/`; deleted once the buffer is saved. Default ON.
    #[serde(default = "default_true")]
    pub session_backup: bool,
    /// Strip trailing spaces/tabs from every line on save. Default OFF.
    #[serde(default)]
    pub trim_trailing_whitespace_on_save: bool,
    /// Ensure the file ends with a single newline on save. Default OFF.
    #[serde(default)]
    pub final_newline_on_save: bool,
    /// Remember + restore the caret char index per file path (extends the
    /// scroll-position memory). Default ON.
    #[serde(default = "default_true")]
    pub restore_cursor_position: bool,
    /// Per-file caret char index, restored on reopen (companion to
    /// `scroll_positions`). Capped at [`SCROLL_POS_CAP`].
    #[serde(default)]
    pub cursor_positions: std::collections::HashMap<String, usize>,
    /// Render visible whitespace markers (a faint `·` per space, `→` per
    /// tab) in the OWNED rope editor. Default OFF — the markers are an
    /// opt-in overlay; the egui TextEdit path and the real buffer text are
    /// untouched whether on or off.
    #[serde(default)]
    pub render_whitespace: bool,
    /// Enable Tab-trigger snippet expansion in the in-house editor. A Tab
    /// pressed right after a known prefix from `<config>/snippets.toml` expands
    /// the snippet instead of indenting. Default ON (the feature is inert when
    /// no snippets file is present), and ON for configs written before the
    /// field existed.
    #[serde(default = "default_true")]
    pub snippets_enabled: bool,
    /// Highlight the line the caret is on with a faint full-width band. Default
    /// OFF (the calm-surface default; opt-in like the other overlays).
    #[serde(default)]
    pub current_line_highlight: bool,
    /// Caret shape drawn over egui's native caret. Default `Bar` = egui's own
    /// look (so the default is a visual no-op).
    #[serde(default)]
    pub caret_style: CaretStyle,
    /// Caret stroke width in points for the Bar/Underline styles (Block ignores
    /// it — it fills the cell). Clamped to [1.0, 4.0] at render time.
    #[serde(default = "default_caret_width")]
    pub caret_width: f32,
    /// Draw faint vertical indent-guide lines at each `tab_width` column.
    /// Default OFF.
    #[serde(default)]
    pub indent_guides: bool,
    /// Box-highlight the bracket matching the one next to the caret. Default OFF.
    #[serde(default)]
    pub bracket_match: bool,
    /// Faintly box every other occurrence of the current selection in the
    /// viewport (VS Code / Sublime style). Default ON.
    #[serde(default = "default_true")]
    pub highlight_selection_occurrences: bool,
    /// Tint trailing whitespace on each line a faint warn colour (distinct
    /// from `render_whitespace`, which shows ALL whitespace). Default OFF.
    #[serde(default)]
    pub highlight_trailing_whitespace: bool,
    /// Vertical guide rulers at these 1-based column positions (e.g. [80, 100]).
    /// Empty = no rulers. Default empty.
    #[serde(default)]
    pub rulers: Vec<usize>,
    /// Smooth (eased) wheel scrolling. Default ON — egui's native feel. Off makes
    /// the wheel jump in discrete notches (snappier, no glide).
    #[serde(default = "default_true")]
    pub smooth_scroll: bool,
    /// Scrollbar chrome style for the editor surface.
    #[serde(default)]
    pub scrollbar_style: ScrollbarStyle,
    /// Smart list continuation on Enter for note files (`.md`/`.txt`/`.markdown`):
    /// continue `-`/`*`/`+`/`N.` markers (and `- [ ]` task boxes), terminate the
    /// list on an empty item. Default ON. Code files are unaffected regardless.
    #[serde(default = "default_true")]
    pub smart_lists: bool,
    /// Paste a clipboard URL over a non-URL selection as a markdown link
    /// `[selection](url)` in note files. Default ON (mirrors VS Code's
    /// `pasteUrlAsFormattedLink` smartWithSelection).
    #[serde(default = "default_true")]
    pub paste_url_as_link: bool,
    /// Auto-pair brackets/quotes/backticks: wrap a selection on an opening char,
    /// insert the closing partner on an empty caret, and type over a closing
    /// char. Default OFF — auto-pair is contested, so it is strictly opt-in.
    #[serde(default)]
    pub auto_pair: bool,
    /// Typewriter scrolling preference (keep the caret line vertically centred).
    /// Default OFF. Config surface for the view nicety.
    #[serde(default)]
    pub typewriter_scroll: bool,
    /// Pinned / favourite file paths surfaced above the recent-files MRU.
    /// Independent of `recent_files`; a pin survives MRU eviction.
    #[serde(default)]
    pub pinned_files: Vec<PathBuf>,
}

/// Caret shape rendered over the editor's native caret. `Bar` reproduces egui's
/// default thin vertical caret (so it is a visual no-op when selected).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CaretStyle {
    #[default]
    Bar,
    /// Full-cell filled rectangle — the retro terminal look.
    Block,
    /// A thick underline at the caret's baseline.
    Underline,
}

/// Editor scrollbar chrome. `Auto` = egui default (shows on hover/scroll);
/// `Thin` = a slimmer bar; `Hidden` = no visible bar (scroll still works).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ScrollbarStyle {
    #[default]
    Auto,
    Thin,
    Hidden,
}

/// serde default for the caret stroke width.
fn default_caret_width() -> f32 {
    1.0
}

impl EditorConfig {
    /// Caret stroke width clamped to a sane band.
    pub fn clamped_caret_width(&self) -> f32 {
        self.caret_width.clamp(1.0, 4.0)
    }
}

/// serde default for the note syntax-colour theme (#104).
fn default_note_theme() -> String {
    "base16-eighties.dark".to_string()
}

/// Wave-3: default byte threshold (16 MiB) above which an editable buffer is
/// auto-promoted to the viewport-culled rope editor. Aligns with the core
/// `Buffer::MMAP_THRESHOLD`. `0` (user-set) disables auto-promotion.
fn default_rope_auto_threshold() -> usize {
    16 * 1024 * 1024
}

/// Cap on the scroll-position memory map (F-021). Older entries are evicted
/// in arbitrary order — the map is best-effort, not history.
pub const SCROLL_POS_CAP: usize = 200;

/// Insert / update `path`'s scroll offset, capping the map at
/// [`SCROLL_POS_CAP`] entries.
pub fn record_scroll_pos(map: &mut std::collections::HashMap<String, f32>, path: &str, y: f32) {
    if map.len() >= SCROLL_POS_CAP && !map.contains_key(path) {
        if let Some(first) = map.keys().next().cloned() {
            map.remove(&first);
        }
    }
    map.insert(path.to_string(), y);
}

/// Cap on the recent-files MRU list. 20 is the universal editor
/// convention (VSCode, Sublime, Notepad++).
pub const RECENT_FILES_MAX: usize = 20;

/// Push `path` to the front of `recent` (MRU semantics), dedup by host-FS
/// path identity (case-/separator-insensitive on Windows, case-sensitive on
/// POSIX — see [`crate::path_norm`]), and cap the list at [`RECENT_FILES_MAX`].
/// Pure helper so the open-path codepath stays testable without the egui shell.
pub fn record_recent_file(recent: &mut Vec<PathBuf>, path: PathBuf) {
    recent.retain(|p| !crate::path_norm::paths_equal_for_compare(p, &path));
    recent.insert(0, path);
    if recent.len() > RECENT_FILES_MAX {
        recent.truncate(RECENT_FILES_MAX);
    }
}

/// Tab-strip position relative to the editor surface. `Top` keeps the tab
/// strip inline with the toolbar (the v1 layout); the other three host the
/// strip in its own dedicated panel.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TabBarPosition {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

impl TabBarPosition {
    /// True when the strip should render as a vertical list of tabs (one tab
    /// per row) — used for the side positions.
    pub fn is_vertical(self) -> bool {
        matches!(self, TabBarPosition::Left | TabBarPosition::Right)
    }
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            tab_width: 4,
            insert_spaces: true,
            show_line_numbers: true,
            show_change_bar: true,
            show_minimap: true,
            word_wrap: true,
            auto_save: false,
            restore_session: true,
            tab_bar_position: TabBarPosition::Top,
            side_tabs_rotated: false,
            note_theme: default_note_theme(),
            grid_enabled: false,
            grid_layout: None,
            recent_files: Vec::new(),
            recent_folders: Vec::new(),
            first_run_completed: false,
            scroll_positions: std::collections::HashMap::new(),
            experimental_rope_editor: false,
            rope_editor_auto_threshold_bytes: default_rope_auto_threshold(),
            session_backup: true,
            trim_trailing_whitespace_on_save: false,
            final_newline_on_save: false,
            restore_cursor_position: true,
            cursor_positions: std::collections::HashMap::new(),
            render_whitespace: false,
            syntax_from_theme: false,
            detect_links: true,
            md_rich_coloring: true,
            md_color_dividers: true,
            md_color_tags: true,
            md_color_strikethrough: true,
            md_color_task_boxes: true,
            md_color_table_pipes: true,
            snippets_enabled: true,
            current_line_highlight: false,
            caret_style: CaretStyle::Bar,
            caret_width: default_caret_width(),
            indent_guides: false,
            bracket_match: false,
            highlight_selection_occurrences: true,
            highlight_trailing_whitespace: false,
            rulers: Vec::new(),
            smooth_scroll: true,
            scrollbar_style: ScrollbarStyle::Auto,
            smart_lists: true,
            paste_url_as_link: true,
            auto_pair: false,
            typewriter_scroll: false,
            pinned_files: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn side_tabs_rotated_round_trips_and_accepts_legacy_alias() {
        // Absent → default false.
        let older = "tab_width = 2\n";
        let cfg: EditorConfig = toml::from_str(older).unwrap();
        assert!(!cfg.side_tabs_rotated, "absent field defaults to false");
        // Explicit new name.
        let explicit = "tab_width = 2\nside_tabs_rotated = true\n";
        let cfg2: EditorConfig = toml::from_str(explicit).unwrap();
        assert!(cfg2.side_tabs_rotated);
        // The old `side_tabs_vertical` name is accepted via serde alias so
        // existing configs don't error.
        let legacy = "tab_width = 2\nside_tabs_vertical = true\n";
        let cfg3: EditorConfig = toml::from_str(legacy).unwrap();
        assert!(cfg3.side_tabs_rotated, "legacy alias maps to the new field");
    }

    /// F-012 helper: record_recent_file pushes to the front, dedups, caps.
    #[test]
    fn record_recent_file_mru_dedup_cap() {
        use super::{record_recent_file, RECENT_FILES_MAX};
        let mut r: Vec<PathBuf> = Vec::new();
        record_recent_file(&mut r, PathBuf::from("/a/b.txt"));
        record_recent_file(&mut r, PathBuf::from("/c/d.txt"));
        record_recent_file(&mut r, PathBuf::from("/a/b.txt")); // dedup → front
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], PathBuf::from("/a/b.txt"));
        assert_eq!(r[1], PathBuf::from("/c/d.txt"));
        // Cap test.
        for n in 0..(RECENT_FILES_MAX + 5) {
            record_recent_file(&mut r, PathBuf::from(format!("/fill/{n}.txt")));
        }
        assert_eq!(r.len(), RECENT_FILES_MAX);
    }

    /// A-07: on Windows the recent list dedups by host-FS identity — the SAME
    /// file reached via two casings must NOT produce two entries. On POSIX the
    /// list stays case-sensitive, so the two casings are genuinely distinct.
    #[test]
    fn record_recent_file_casefold_dedup_per_platform() {
        use super::record_recent_file;
        let mut r: Vec<PathBuf> = Vec::new();
        record_recent_file(&mut r, PathBuf::from(r"C:\Data\f.txt"));
        record_recent_file(&mut r, PathBuf::from(r"c:\data\F.TXT"));
        if cfg!(windows) {
            // Windows FS is case-insensitive → one entry, front-pushed to the
            // most-recent casing.
            assert_eq!(r.len(), 1, "windows must dedup the two casings");
            assert_eq!(r[0], PathBuf::from(r"c:\data\F.TXT"));
        } else {
            // POSIX is case-sensitive → two genuinely distinct files.
            assert_eq!(r.len(), 2, "posix must keep both casings");
        }
    }

    /// F-012: recent_files round-trips through TOML.
    #[test]
    fn recent_files_round_trip() {
        let mut c = Config::default();
        c.editor.recent_files = vec![PathBuf::from("/x/y.rs"), PathBuf::from("/p/q.py")];
        let s = c.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert_eq!(back.editor.recent_files.len(), 2);
        assert_eq!(back.editor.recent_files[0], PathBuf::from("/x/y.rs"));
    }

    #[test]
    fn recent_folders_round_trip_and_default_empty() {
        // Default is empty (a missing key must not error or invent entries).
        assert!(Config::default().editor.recent_folders.is_empty());
        let mut c = Config::default();
        c.editor.recent_folders = vec![PathBuf::from("/proj/a"), PathBuf::from("/proj/b")];
        let back: Config = toml::from_str(&c.to_toml_string()).expect("config TOML round-trip");
        assert_eq!(back.editor.recent_folders.len(), 2);
        assert_eq!(back.editor.recent_folders[0], PathBuf::from("/proj/a"));
        // MRU discipline is shared with recent_files via record_recent_file:
        // front-push + dedup.
        let mut list = vec![PathBuf::from("/x")];
        record_recent_file(&mut list, PathBuf::from("/y"));
        record_recent_file(&mut list, PathBuf::from("/x")); // re-touch -> front
        assert_eq!(list, vec![PathBuf::from("/x"), PathBuf::from("/y")]);
    }

    /// F-013: first_run_completed defaults false + round-trips.
    #[test]
    fn first_run_completed_default_false_and_round_trips() {
        let c = Config::default();
        assert!(!c.editor.first_run_completed);
        let mut c2 = c.clone();
        c2.editor.first_run_completed = true;
        let s = c2.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert!(back.editor.first_run_completed);
    }

    /// render_whitespace defaults OFF and round-trips through TOML.
    #[test]
    fn render_whitespace_default_off_and_round_trips() {
        let c = Config::default();
        assert!(!c.editor.render_whitespace);
        let mut c2 = c.clone();
        c2.editor.render_whitespace = true;
        let s = c2.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert!(back.editor.render_whitespace);
    }

    #[test]
    fn wave6_editor_customization_defaults() {
        let e = EditorConfig::default();
        assert!(!e.current_line_highlight && !e.indent_guides && !e.bracket_match);
        assert_eq!(e.caret_style, CaretStyle::Bar); // visual no-op default
        assert!(e.smooth_scroll); // ON by default
        assert_eq!(e.scrollbar_style, ScrollbarStyle::Auto);
        assert_eq!(e.clamped_caret_width(), 1.0);
        let wide = EditorConfig {
            caret_width: 99.0,
            ..EditorConfig::default()
        };
        assert_eq!(wide.clamped_caret_width(), 4.0);
    }

    #[test]
    fn wave6_customization_round_trips() {
        let mut c = Config::default();
        c.editor.caret_style = CaretStyle::Block;
        c.editor.scrollbar_style = ScrollbarStyle::Thin;
        c.editor.indent_guides = true;
        let s = c.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert_eq!(back.editor.caret_style, CaretStyle::Block);
        assert_eq!(back.editor.scrollbar_style, ScrollbarStyle::Thin);
        assert!(back.editor.indent_guides);
    }

    /// F-021 helper: record_scroll_pos inserts + caps at SCROLL_POS_CAP.
    #[test]
    fn record_scroll_pos_caps_and_round_trips() {
        use super::{record_scroll_pos, SCROLL_POS_CAP};
        let mut m = std::collections::HashMap::<String, f32>::new();
        record_scroll_pos(&mut m, "/a/b.rs", 100.0);
        record_scroll_pos(&mut m, "/c/d.rs", 200.0);
        record_scroll_pos(&mut m, "/a/b.rs", 150.0); // update in place
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("/a/b.rs").copied(), Some(150.0));
        for n in 0..(SCROLL_POS_CAP + 10) {
            record_scroll_pos(&mut m, &format!("/fill/{n}.rs"), n as f32);
        }
        assert_eq!(m.len(), SCROLL_POS_CAP);
        // Round-trip a small map.
        let mut c = Config::default();
        c.editor
            .scroll_positions
            .insert("/x/y.rs".to_string(), 250.0);
        let s = c.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert_eq!(
            back.editor.scroll_positions.get("/x/y.rs").copied(),
            Some(250.0)
        );
    }

    #[test]
    fn default_rope_auto_threshold_is_16_mib() {
        // Real behavioural constant: the byte size above which a buffer is
        // auto-promoted to the viewport-culled rope editor. It MUST equal
        // `Buffer::MMAP_THRESHOLD` (16 MiB). A mutated `0`/`1` would auto-promote
        // (almost) every buffer to the heavy rope path; the `16 * 1024 * 1024`
        // product degrading to `16 + 1024 + 1024` (≈2 KiB) would do the same.
        use super::default_rope_auto_threshold;
        assert_eq!(default_rope_auto_threshold(), 16 * 1024 * 1024);
        assert_eq!(default_rope_auto_threshold(), 16_777_216);
        // The default config wires this in.
        assert_eq!(
            EditorConfig::default().rope_editor_auto_threshold_bytes,
            16_777_216
        );
    }

    #[test]
    fn default_note_theme_is_a_nonempty_bundled_theme_name() {
        // The note syntax-colour theme name is a functional contract: it must
        // name a real bundled syntect theme, so it cannot be empty (an empty
        // name resolves to no highlighting). Guards the `String::new()` mutant.
        use super::default_note_theme;
        let name = default_note_theme();
        assert!(!name.is_empty(), "note theme name must be non-empty");
        assert_eq!(name, "base16-eighties.dark");
        assert_eq!(EditorConfig::default().note_theme, "base16-eighties.dark");
    }

    // ---- MUTANT-EQUIVALENT (config/editor.rs): documented, intentionally not tested ----
    //
    // These surviving mutants have no behaviourally-distinguishable effect on a
    // realistic input, so killing them would mean asserting an internal constant
    // for its own sake (test bloat) or is impossible (clamp masks the change):
    //
    // MUTANT-EQUIVALENT: config/editor.rs:187 — `default_caret_width -> 0.0` /
    //   `-> -1.0`. The default feeds straight into `clamped_caret_width()` whose
    //   floor is 1.0, so any value <= 1.0 (including 0.0 and -1.0) produces the
    //   SAME clamped result 1.0. No observable difference; the clamp test already
    //   pins the user-visible output. (The raw default is never read un-clamped.)
    // MUTANT-EQUIVALENT: config/editor.rs:235 — `record_recent_file` `>` -> `>=`
    //   in `recent.len() > RECENT_FILES_MAX`. `truncate(MAX)` is a no-op when
    //   `len == MAX`, so `>` and `>=` produce identical lists for every input
    //   (at len==MAX both leave MAX entries; at len>MAX both truncate to MAX).
    //   Behaviourally indistinguishable — the cap test already pins len==MAX.

    #[test]
    fn tab_bar_defaults_to_top_horizontal() {
        // T18.4: the v1 layout puts the tab strip inline with the toolbar at
        // the top. is_vertical() flips only for the side positions.
        assert_eq!(
            EditorConfig::default().tab_bar_position,
            TabBarPosition::Top
        );
        assert!(!TabBarPosition::Top.is_vertical());
        assert!(!TabBarPosition::Bottom.is_vertical());
        assert!(TabBarPosition::Left.is_vertical());
        assert!(TabBarPosition::Right.is_vertical());
        // Side-tab rotation defaults OFF — labels stay horizontal (the familiar
        // look) until the user opts into vertical text.
        assert!(!EditorConfig::default().side_tabs_rotated);
    }

    #[test]
    fn note_feature_flag_defaults_and_round_trip() {
        let e = EditorConfig::default();
        // Note-friendly defaults: smart lists + smart link-paste ON; auto-pair +
        // typewriter OFF (opt-in); no pinned files.
        assert!(e.smart_lists);
        assert!(e.paste_url_as_link);
        assert!(!e.auto_pair);
        assert!(!e.typewriter_scroll);
        assert!(e.pinned_files.is_empty());
        // Absent keys keep the defaults (older configs don't error).
        let older: EditorConfig = toml::from_str("tab_width = 2\n").unwrap();
        assert!(older.smart_lists && older.paste_url_as_link && !older.auto_pair);
        // Round-trip through the full config.
        let mut c = Config::default();
        c.editor.auto_pair = true;
        c.editor.smart_lists = false;
        c.editor.pinned_files = vec![PathBuf::from("/notes/today.md")];
        let back: Config = toml::from_str(&c.to_toml_string()).expect("round-trip");
        assert!(back.editor.auto_pair);
        assert!(!back.editor.smart_lists);
        assert_eq!(
            back.editor.pinned_files,
            vec![PathBuf::from("/notes/today.md")]
        );
    }

    #[test]
    fn reopen_last_session_defaults_on() {
        assert!(EditorConfig::default().restore_session);
        let cfg: EditorConfig = toml::from_str("").unwrap();
        assert!(
            cfg.restore_session,
            "missing restore_session must default ON"
        );
    }
}
