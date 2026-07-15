//! Command-palette, keyboard-shortcut, and toolbar registries plus their pure
//! lookup helpers — the self-discovery surface of the editor.
//!
//! Extracted from the `app` god-module (coverage WU-1) so the registries and
//! their pure routing/lookup logic live in one cohesive, directly unit-testable
//! module. The thin egui glue that renders these tables (the F1 cheatsheet, the
//! Ctrl+Shift+P palette, the quick-access toolbar) renders in app/frame_tick.rs
//! (cheatsheet + palette) and app/toolbar_render.rs (toolbar), each calling into
//! the pure functions here. Every item is re-exported from
//! `app/mod.rs` via `pub(crate) use commands::*;` so existing call sites
//! (`crate::app::BUILTIN_COMMANDS`, `super::*`, …) are unchanged.

use super::keymap::action;

// ---- Keyboard shortcut cheatsheet table (F-014) ----

pub(crate) struct ShortcutEntry {
    /// Fallback chord text, used only when `bindings` is empty (a shortcut that
    /// is not rebindable). For a rebindable row this is the DEFAULT chord and is
    /// not what gets rendered — the live keymap is.
    pub chord: &'static str,
    pub action: &'static str,
    /// The `[keybindings]` actions this row describes, or `&[]` when the shortcut
    /// is hard-wired.
    ///
    /// A rebindable row MUST list its actions: the cheatsheet renders the user's
    /// CURRENT chord from these, so a row left at `&[]` would keep showing the
    /// default after a rebind and quietly lie. Several actions render as one row
    /// (font zoom is in / out / reset).
    pub bindings: &'static [&'static str],
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
        bindings: &[action::NEW_FILE],
    },
    ShortcutEntry {
        chord: "Ctrl+O",
        action: "Open file…",
        bindings: &[action::OPEN_FILE],
    },
    ShortcutEntry {
        chord: "Ctrl+S",
        action: "Save active buffer",
        bindings: &[action::SAVE],
    },
    ShortcutEntry {
        chord: "Ctrl+W",
        action: "Close active tab",
        bindings: &[action::CLOSE_TAB],
    },
    ShortcutEntry {
        chord: "Ctrl+Tab",
        action: "Cycle to next tab",
        bindings: &[action::NEXT_TAB],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+Tab",
        action: "Cycle to previous tab",
        bindings: &[action::PREV_TAB],
    },
    ShortcutEntry {
        chord: "Ctrl+\\",
        action: "Toggle multi-note grid",
        bindings: &[action::TOGGLE_GRID],
    },
    ShortcutEntry {
        chord: "Ctrl+F",
        action: "Find in buffer",
        bindings: &[action::FIND],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+F",
        action: "Find in files (project-wide)",
        bindings: &[action::FIND_IN_FILES],
    },
    ShortcutEntry {
        chord: "Ctrl+H",
        action: "Find + replace in buffer",
        bindings: &[action::REPLACE],
    },
    ShortcutEntry {
        chord: "Ctrl+/",
        action: "Toggle line comment (per-language prefix)",
        bindings: &[action::TOGGLE_COMMENT],
    },
    ShortcutEntry {
        chord: "Ctrl+G",
        action: "Go to line (or line:column)",
        bindings: &[action::GOTO_LINE],
    },
    ShortcutEntry {
        chord: "Ctrl+R",
        action: "Open a recent file (MRU list)",
        bindings: &[action::RECENT_FILES],
    },
    ShortcutEntry {
        chord: "Ctrl+P",
        action: "Fuzzy-find a file in the project",
        bindings: &[action::FUZZY_FINDER],
    },
    ShortcutEntry {
        chord: "Ctrl+F2",
        action: "Toggle a bookmark on the cursor line",
        bindings: &[action::TOGGLE_BOOKMARK],
    },
    ShortcutEntry {
        chord: "F2",
        action: "Jump to the next bookmark",
        bindings: &[action::NEXT_BOOKMARK],
    },
    ShortcutEntry {
        chord: "Shift+F2",
        action: "Jump to the previous bookmark",
        bindings: &[action::PREV_BOOKMARK],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+O",
        action: "Go to a symbol (definition) in the active buffer",
        bindings: &[action::GOTO_SYMBOL],
    },
    ShortcutEntry {
        chord: "Alt+Up",
        action: "Move cursor line up",
        bindings: &[action::MOVE_LINE_UP],
    },
    ShortcutEntry {
        chord: "Alt+Down",
        action: "Move cursor line down",
        bindings: &[action::MOVE_LINE_DOWN],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+D",
        action: "Duplicate cursor line",
        bindings: &[action::DUPLICATE_LINE],
    },
    ShortcutEntry {
        chord: "Ctrl+J",
        action: "Join cursor line with next",
        bindings: &[action::JOIN_LINES],
    },
    ShortcutEntry {
        chord: "Ctrl+Space",
        action: "Identifier completion (popup)",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+P",
        action: "Command palette",
        bindings: &[action::COMMAND_PALETTE],
    },
    ShortcutEntry {
        chord: "F1",
        action: "Show this keyboard cheatsheet",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "F11",
        action: "Toggle fullscreen — editor only, all chrome hidden (Esc exits)",
        bindings: &[action::TOGGLE_FULLSCREEN],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+T",
        action: "Cycle to the next built-in theme",
        bindings: &[action::CYCLE_THEME],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+M",
        action: "Toggle minimap on/off",
        bindings: &[action::TOGGLE_MINIMAP],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+V",
        action: "Toggle the markdown live-preview panel",
        bindings: &[action::TOGGLE_MD_PREVIEW],
    },
    ShortcutEntry {
        chord: "Ctrl+M",
        action: "Jump to the matching bracket",
        bindings: &[action::JUMP_BRACKET],
    },
    ShortcutEntry {
        chord: "Esc",
        action: "Close find / palette / cheatsheet / completion popup",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+[",
        action: "Fold every region in the active buffer",
        bindings: &[action::FOLD_ALL],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+]",
        action: "Expand every folded region",
        bindings: &[action::EXPAND_ALL],
    },
    ShortcutEntry {
        chord: "Ctrl+C",
        action: "Copy selection to clipboard",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+X",
        action: "Cut selection to clipboard",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+V",
        action: "Paste from clipboard",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+Z",
        action: "Undo",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+Z",
        action: "Redo",
        bindings: &[],
    },
    ShortcutEntry {
        // Phosphor (Thin) ARROW_DOWN (U+E03E) / ARROW_UP (U+E08E) — the bare
        // U+2193/U+2191 arrows were tofu in the bundled fonts.
        chord: "Ctrl+Alt+\u{E03E} / \u{E08E}",
        action: "Add caret below / above (multi-cursor — in-house editor)",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+D",
        action: "Select word, then add caret on next occurrence (in-house editor)",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Alt+drag",
        action: "Column / block selection (in-house editor)",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Esc",
        action: "Collapse multi-cursor to one caret",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+= / Ctrl+- / Ctrl+0",
        action: "Zoom font in / out / reset (also Ctrl+scroll)",
        bindings: &[
            action::INCREASE_FONT,
            action::DECREASE_FONT,
            action::RESET_FONT,
        ],
    },
    ShortcutEntry {
        chord: "Ctrl+.",
        action: "Toggle zen / distraction-free mode (Esc exits)",
        bindings: &[action::TOGGLE_ZEN],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+R",
        action: "Reopen the most recently closed tab",
        bindings: &[action::REOPEN_TAB],
    },
    ShortcutEntry {
        chord: "Tab / Shift+Tab",
        action: "Indent / outdent selected lines (experimental editor)",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+K",
        action: "Delete the current line (experimental editor)",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+U / Ctrl+Shift+U",
        action: "Lowercase / uppercase the selection (experimental editor)",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+Enter",
        action: "Toggle the task checkbox on the caret / selected lines",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+B / Ctrl+I",
        action: "Toggle bold / italic on the selection",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+`",
        action: "Toggle inline code on the selection",
        bindings: &[],
    },
    ShortcutEntry {
        chord: "Ctrl+Shift+X",
        action: "Toggle strikethrough on the selection",
        bindings: &[],
    },
];

// ---- Built-in command palette registry (F-004) ----
//
// Every editor action a user can take without writing a plugin. Each entry
// is exposed in the Ctrl+Shift+P palette so the editor is self-discoverable
// on first launch (the old "plugin only" palette showed nothing on a fresh
// install). The shortcut column displays the key chord when one is wired.
// Invocation routes through `execute_builtin` (in app/builtins.rs) so the palette
// and the keyboard chord produce identical state changes.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinCommand {
    NewFile,
    OpenFile,
    OpenFolder,
    OpenRecentFolder,
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
    ToggleChangeBar,
    OpenSettings,
    ReportIssue,
    OpenFind,
    OpenPalette,
    CycleTheme,
    StartLsp,
    FoldAll,
    ExpandAll,
    OpenPluginManager,
    SortLines,
    SortLinesUnique,
    TrimTrailingWhitespace,
    EnsureFinalNewline,
    ConvertIndentToSpaces,
    ConvertIndentToTabs,
    RevealInExplorer,
    CopyFilePath,
    JumpMatchingBracket,
    InsertDateTime,
    DuplicateSelection,
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
    ToggleBookmark,
    NextBookmark,
    PrevBookmark,
    GoToSymbol,
    // ---- Note-usability actions (P0–P3) ----
    /// P0-1 — toggle / insert the GFM task checkbox on the caret / selection.
    ToggleTaskCheckbox,
    /// P0-4 — wrap-toggle the selection with an inline emphasis marker.
    ToggleBold,
    ToggleItalic,
    ToggleInlineCode,
    ToggleStrikethrough,
    /// P1-4 — case-convert the selection.
    UppercaseSelection,
    LowercaseSelection,
    TitlecaseSelection,
    /// P2-1 — format the markdown table under the caret.
    FormatTable,
    /// P3-3 — open a new note seeded from a built-in template.
    NewChecklistNote,
    NewMeetingNote,
    NewDailyNote,
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
    /// Fallback shortcut hint, used only when `bindings` is empty (a hard-wired
    /// shortcut, or no shortcut at all). For a rebindable command this is the
    /// DEFAULT chord and is not what the palette renders — the live keymap is.
    pub shortcut: &'static str,
    pub action: BuiltinCommand,
    /// The `[keybindings]` actions behind this command, or `&[]` when it has no
    /// rebindable chord. See [`ShortcutEntry::bindings`].
    pub bindings: &'static [&'static str],
}

/// The full registry, alphabetised by label so the palette is stable across
/// launches. Add new editor actions HERE so the palette stays the canonical
/// self-discovery surface.
pub(crate) const BUILTIN_COMMANDS: &[BuiltinEntry] = &[
    BuiltinEntry {
        label: "Close active tab",
        shortcut: "",
        action: BuiltinCommand::CloseActiveTab,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Close all tabs",
        shortcut: "",
        action: BuiltinCommand::CloseAllTabs,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Convert to Markdown and save as .md",
        shortcut: "",
        action: BuiltinCommand::ConvertToMarkdown,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Copy",
        shortcut: "Ctrl+C",
        action: BuiltinCommand::Copy,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Cut",
        shortcut: "Ctrl+X",
        action: BuiltinCommand::Cut,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Cycle theme",
        shortcut: "",
        action: BuiltinCommand::CycleTheme,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Expand all folds",
        shortcut: "Ctrl+Shift+]",
        action: BuiltinCommand::ExpandAll,
        bindings: &[action::EXPAND_ALL],
    },
    BuiltinEntry {
        label: "Export as HTML…",
        shortcut: "",
        action: BuiltinCommand::ExportAsHtml,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Find in buffer",
        shortcut: "Ctrl+F",
        action: BuiltinCommand::OpenFind,
        bindings: &[action::FIND],
    },
    BuiltinEntry {
        label: "Fold all regions",
        shortcut: "Ctrl+Shift+[",
        action: BuiltinCommand::FoldAll,
        bindings: &[action::FOLD_ALL],
    },
    BuiltinEntry {
        label: "Line endings: CR (classic Mac)",
        shortcut: "",
        action: BuiltinCommand::SetLineEndingsCr,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Line endings: CRLF (Windows)",
        shortcut: "",
        action: BuiltinCommand::SetLineEndingsCrlf,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Line endings: LF (Unix)",
        shortcut: "",
        action: BuiltinCommand::SetLineEndingsLf,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Go to symbol…",
        shortcut: "Ctrl+Shift+O",
        action: BuiltinCommand::GoToSymbol,
        bindings: &[action::GOTO_SYMBOL],
    },
    BuiltinEntry {
        label: "Manage plugins",
        shortcut: "",
        action: BuiltinCommand::OpenPluginManager,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Navigate to next bookmark",
        shortcut: "F2",
        action: BuiltinCommand::NextBookmark,
        bindings: &[action::NEXT_BOOKMARK],
    },
    BuiltinEntry {
        label: "Navigate to previous bookmark",
        shortcut: "Shift+F2",
        action: BuiltinCommand::PrevBookmark,
        bindings: &[action::PREV_BOOKMARK],
    },
    BuiltinEntry {
        label: "New file",
        shortcut: "Ctrl+N",
        action: BuiltinCommand::NewFile,
        bindings: &[action::NEW_FILE],
    },
    BuiltinEntry {
        label: "Next tab",
        shortcut: "",
        action: BuiltinCommand::CycleTabNext,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Open file…",
        shortcut: "Ctrl+O",
        action: BuiltinCommand::OpenFile,
        bindings: &[action::OPEN_FILE],
    },
    BuiltinEntry {
        label: "Open folder…",
        shortcut: "",
        action: BuiltinCommand::OpenFolder,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Open recent folder",
        shortcut: "",
        action: BuiltinCommand::OpenRecentFolder,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Open settings",
        shortcut: "",
        action: BuiltinCommand::OpenSettings,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Paste",
        shortcut: "Ctrl+V",
        action: BuiltinCommand::Paste,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Previous tab",
        shortcut: "",
        action: BuiltinCommand::CycleTabPrev,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Redo",
        shortcut: "Ctrl+Shift+Z",
        action: BuiltinCommand::Redo,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Report an issue…",
        shortcut: "",
        action: BuiltinCommand::ReportIssue,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Save",
        shortcut: "Ctrl+S",
        action: BuiltinCommand::Save,
        bindings: &[action::SAVE],
    },
    BuiltinEntry {
        label: "Show command palette",
        shortcut: "Ctrl+Shift+P",
        action: BuiltinCommand::OpenPalette,
        bindings: &[action::COMMAND_PALETTE],
    },
    BuiltinEntry {
        label: "Sort lines (A-Z)",
        shortcut: "",
        action: BuiltinCommand::SortLines,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Sort lines (A-Z, unique)",
        shortcut: "",
        action: BuiltinCommand::SortLinesUnique,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Trim trailing whitespace",
        shortcut: "",
        action: BuiltinCommand::TrimTrailingWhitespace,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Ensure final newline",
        shortcut: "",
        action: BuiltinCommand::EnsureFinalNewline,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Convert indentation to spaces",
        shortcut: "",
        action: BuiltinCommand::ConvertIndentToSpaces,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Convert indentation to tabs",
        shortcut: "",
        action: BuiltinCommand::ConvertIndentToTabs,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Reveal file in explorer",
        shortcut: "",
        action: BuiltinCommand::RevealInExplorer,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Copy file path",
        shortcut: "",
        action: BuiltinCommand::CopyFilePath,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Jump to matching bracket",
        shortcut: "Ctrl+M",
        action: BuiltinCommand::JumpMatchingBracket,
        bindings: &[action::JUMP_BRACKET],
    },
    BuiltinEntry {
        label: "Insert date/time (UTC, ISO-8601)",
        shortcut: "",
        action: BuiltinCommand::InsertDateTime,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Duplicate selection (or line)",
        shortcut: "",
        action: BuiltinCommand::DuplicateSelection,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Start language server for current file",
        shortcut: "",
        action: BuiltinCommand::StartLsp,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Toggle bookmark on cursor line",
        shortcut: "Ctrl+F2",
        action: BuiltinCommand::ToggleBookmark,
        bindings: &[action::TOGGLE_BOOKMARK],
    },
    BuiltinEntry {
        label: "Toggle line numbers",
        shortcut: "",
        action: BuiltinCommand::ToggleLineNumbers,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Toggle change bar (edited-line markers)",
        shortcut: "",
        action: BuiltinCommand::ToggleChangeBar,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Toggle diff vs disk",
        shortcut: "",
        action: BuiltinCommand::ToggleDiffView,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Toggle markdown preview",
        shortcut: "Ctrl+Shift+V",
        action: BuiltinCommand::ToggleMarkdownPreview,
        bindings: &[action::TOGGLE_MD_PREVIEW],
    },
    BuiltinEntry {
        label: "Toggle minimap",
        shortcut: "",
        action: BuiltinCommand::ToggleMinimap,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Toggle spellcheck",
        shortcut: "",
        action: BuiltinCommand::ToggleSpellcheck,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Toggle split / grid view",
        shortcut: "",
        action: BuiltinCommand::ToggleSplitView,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Toggle word wrap",
        shortcut: "",
        action: BuiltinCommand::ToggleWordWrap,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Toggle zen / distraction-free mode",
        shortcut: "Ctrl+.",
        action: BuiltinCommand::ToggleZen,
        bindings: &[action::TOGGLE_ZEN],
    },
    BuiltinEntry {
        label: "Undo",
        shortcut: "Ctrl+Z",
        action: BuiltinCommand::Undo,
        bindings: &[],
    },
    // ---- Note-usability actions (P0–P3) ----
    BuiltinEntry {
        label: "Toggle task checkbox",
        shortcut: "Ctrl+Enter",
        action: BuiltinCommand::ToggleTaskCheckbox,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Toggle bold",
        shortcut: "Ctrl+B",
        action: BuiltinCommand::ToggleBold,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Toggle italic",
        shortcut: "Ctrl+I",
        action: BuiltinCommand::ToggleItalic,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Toggle inline code",
        shortcut: "Ctrl+`",
        action: BuiltinCommand::ToggleInlineCode,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Toggle strikethrough",
        shortcut: "Ctrl+Shift+X",
        action: BuiltinCommand::ToggleStrikethrough,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Uppercase selection",
        shortcut: "",
        action: BuiltinCommand::UppercaseSelection,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Lowercase selection",
        shortcut: "",
        action: BuiltinCommand::LowercaseSelection,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Title Case selection",
        shortcut: "",
        action: BuiltinCommand::TitlecaseSelection,
        bindings: &[],
    },
    BuiltinEntry {
        label: "Format markdown table",
        shortcut: "",
        action: BuiltinCommand::FormatTable,
        bindings: &[],
    },
    BuiltinEntry {
        label: "New note from checklist template",
        shortcut: "",
        action: BuiltinCommand::NewChecklistNote,
        bindings: &[],
    },
    BuiltinEntry {
        label: "New note from meeting template",
        shortcut: "",
        action: BuiltinCommand::NewMeetingNote,
        bindings: &[],
    },
    BuiltinEntry {
        label: "New note from daily template",
        shortcut: "",
        action: BuiltinCommand::NewDailyNote,
        bindings: &[],
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

/// Toolbar "instrument plate" kanji for an action id (Phase 17 T17.5 /
/// DECISION-2026-005 cond #4 "verified-accurate kanji ONLY"). Returns `None`
/// when the canonical kanji for an action is uncertain, contested, or a Western
/// metaphor — those stay English-only. The annotation is decorative and
/// English-redundant; every action keeps its English label or icon as the
/// primary read.
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_goto_query ----

    #[test]
    fn goto_plain_line_number() {
        assert_eq!(parse_goto_query("42"), Some((42, None)));
        assert_eq!(parse_goto_query("1"), Some((1, None)));
    }

    #[test]
    fn goto_line_and_column() {
        assert_eq!(parse_goto_query("42:10"), Some((42, Some(10))));
        assert_eq!(parse_goto_query("7:3"), Some((7, Some(3))));
    }

    #[test]
    fn goto_trims_surrounding_whitespace() {
        assert_eq!(parse_goto_query("  42  "), Some((42, None)));
        assert_eq!(parse_goto_query("\t12:4\n"), Some((12, Some(4))));
    }

    #[test]
    fn goto_column_zero_clamps_to_one() {
        // A 0 column is meaningless (columns are 1-based); clamp up to 1.
        assert_eq!(parse_goto_query("42:0"), Some((42, Some(1))));
    }

    #[test]
    fn goto_rejects_empty_zero_and_garbage() {
        assert_eq!(parse_goto_query(""), None);
        assert_eq!(parse_goto_query("   "), None);
        assert_eq!(parse_goto_query("0"), None, "line 0 is invalid");
        assert_eq!(parse_goto_query("abc"), None);
        assert_eq!(parse_goto_query("42:"), None, "missing column");
        assert_eq!(parse_goto_query(":10"), None, "missing line");
        assert_eq!(parse_goto_query("0:5"), None, "line 0 with column");
    }

    #[test]
    fn goto_non_numeric_column_falls_through_to_plain_line() {
        // "42:foo" — the colon-parse fails, then the whole-string plain-line
        // parse also fails (it isn't a bare integer), so None.
        assert_eq!(parse_goto_query("42:foo"), None);
    }

    // ---- comment_prefix_for_extension ----

    #[test]
    fn comment_prefix_c_family_is_double_slash() {
        for ext in [
            "rs", "c", "cpp", "java", "kt", "go", "ts", "js", "cs", "zig",
        ] {
            assert_eq!(comment_prefix_for_extension(ext), Some("//"), "{ext}");
        }
    }

    #[test]
    fn comment_prefix_hash_family() {
        for ext in ["py", "rb", "sh", "yaml", "toml", "ps1", "r"] {
            assert_eq!(comment_prefix_for_extension(ext), Some("#"), "{ext}");
        }
    }

    #[test]
    fn comment_prefix_other_families() {
        assert_eq!(comment_prefix_for_extension("lua"), Some("--"));
        assert_eq!(comment_prefix_for_extension("sql"), Some("--"));
        assert_eq!(comment_prefix_for_extension("vim"), Some("\""));
        assert_eq!(comment_prefix_for_extension("clj"), Some(";;"));
        assert_eq!(comment_prefix_for_extension("tex"), Some("%"));
        assert_eq!(comment_prefix_for_extension("asm"), Some(";"));
    }

    #[test]
    fn comment_prefix_is_case_insensitive_for_lowercased_keys() {
        // The lookup lowercases the input, so an uppercased extension maps to
        // the same prefix as its lowercase form.
        assert_eq!(comment_prefix_for_extension("RS"), Some("//"));
        assert_eq!(comment_prefix_for_extension("Py"), Some("#"));
    }

    #[test]
    fn comment_prefix_none_for_prefixless_languages() {
        for ext in ["html", "css", "json", "md", "txt", "xml"] {
            assert_eq!(comment_prefix_for_extension(ext), None, "{ext}");
        }
    }

    // ---- toolbar_label ----

    #[test]
    fn toolbar_label_text_mode_returns_short_labels() {
        assert_eq!(toolbar_label("new", false), "new");
        assert_eq!(toolbar_label("save", false), "save");
        assert_eq!(toolbar_label("saveas", false), "save as");
        assert_eq!(toolbar_label("openfolder", false), "folder");
        assert_eq!(toolbar_label("linenumbers", false), "nums");
        assert_eq!(toolbar_label("spellcheck", false), "spell");
    }

    #[test]
    fn toolbar_label_unknown_id_falls_back_to_dot() {
        assert_eq!(toolbar_label("sep", false), "·");
        assert_eq!(toolbar_label("bogus", false), "·");
        assert_eq!(toolbar_label("sep", true), "·");
    }

    #[test]
    fn toolbar_label_icon_mode_differs_from_text_for_known_ids() {
        // Icon glyphs come from egui_phosphor and are non-empty + distinct from
        // the plain-text labels for every id that has an icon.
        for id in ["new", "open", "save", "find", "split", "lsp"] {
            let icon = toolbar_label(id, true);
            let text = toolbar_label(id, false);
            assert!(!icon.is_empty(), "{id} icon empty");
            assert_ne!(icon, text, "{id} icon should differ from text label");
        }
    }

    #[test]
    fn toolbar_label_palette_text_is_command_symbol() {
        assert_eq!(toolbar_label("palette", false), "\u{2318}");
    }

    // ---- jp_glyph ----

    #[test]
    fn jp_glyph_known_ids_return_verified_kanji() {
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
    fn jp_glyph_omitted_ids_return_none() {
        // Deliberately English-only per the Folklore-Consultant gate.
        for id in ["openfolder", "palette", "lsp", "sep", "unknown"] {
            assert_eq!(jp_glyph(id), None, "{id}");
        }
    }

    // ---- registry invariants ----

    #[test]
    fn builtin_command_labels_are_unique() {
        // The palette must not show two rows with the same label (one would
        // shadow the other in the fuzzy filter). Note: the registry is NOT
        // strictly alphabetised end-to-end — later editor actions were appended
        // after the original sorted block — so this asserts uniqueness, the
        // invariant that actually holds, rather than a stale "sorted" claim.
        let labels: Vec<&str> = BUILTIN_COMMANDS.iter().map(|e| e.label).collect();
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert_ne!(a, b, "duplicate label in registry: {a}");
            }
        }
    }

    #[test]
    fn builtin_command_actions_are_unique() {
        // No two registry entries route to the same BuiltinCommand — a dup
        // would be a confusing double palette entry. BuiltinCommand is Copy +
        // PartialEq (but not Hash), so compare pairwise.
        let actions: Vec<BuiltinCommand> = BUILTIN_COMMANDS.iter().map(|e| e.action).collect();
        for (i, a) in actions.iter().enumerate() {
            for b in &actions[i + 1..] {
                assert_ne!(a, b, "duplicate action in registry: {a:?}");
            }
        }
    }

    #[test]
    fn builtin_commands_have_nonempty_labels() {
        for entry in BUILTIN_COMMANDS {
            assert!(!entry.label.is_empty());
        }
    }

    #[test]
    fn keyboard_shortcuts_all_have_chord_and_action() {
        assert!(!KEYBOARD_SHORTCUTS.is_empty());
        for s in KEYBOARD_SHORTCUTS {
            assert!(!s.chord.is_empty(), "empty chord");
            assert!(!s.action.is_empty(), "empty action for chord {}", s.chord);
        }
    }

    /// The cheatsheet's own promise — "every shortcut the editor actually responds
    /// to must appear in this list" — checked rather than trusted.
    ///
    /// It was not true: `find_in_files`, `toggle_md_preview` and `jump_bracket`
    /// were all live shortcuts absent from the F1 modal. Nothing noticed, because
    /// nothing compared the table against the action list.
    #[test]
    fn every_rebindable_action_appears_in_the_cheatsheet() {
        let listed: Vec<&str> = KEYBOARD_SHORTCUTS
            .iter()
            .flat_map(|e| e.bindings.iter().copied())
            .collect();
        for name in super::super::keymap::action::ALL {
            assert!(
                listed.contains(name),
                "action '{name}' is a live shortcut but no cheatsheet row lists it — \
                 the F1 modal claims to show every shortcut",
            );
        }
    }

    /// Every `bindings` entry must name a real action.
    ///
    /// The consts make a typo a compile error, but a row could still cite an
    /// action that no longer exists after a rename; then the row would silently
    /// fall back to its stale static chord.
    #[test]
    fn cheatsheet_and_palette_bindings_name_real_actions() {
        let known = super::super::keymap::action::ALL;
        for e in KEYBOARD_SHORTCUTS {
            for b in e.bindings {
                assert!(
                    known.contains(b),
                    "cheatsheet row '{}' cites unknown action '{b}'",
                    e.action
                );
            }
        }
        for e in BUILTIN_COMMANDS {
            for b in e.bindings {
                assert!(
                    known.contains(b),
                    "palette command '{}' cites unknown action '{b}'",
                    e.label
                );
            }
        }
    }

    #[test]
    fn toolbar_actions_include_separator_and_known_ids() {
        let ids: Vec<&str> = TOOLBAR_ACTIONS.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&"sep"), "separator id present");
        assert!(ids.contains(&"new"));
        assert!(ids.contains(&"save"));
        // Every non-sep toolbar id has a human label.
        for (id, label) in TOOLBAR_ACTIONS {
            assert!(!id.is_empty());
            assert!(!label.is_empty(), "{id} has no label");
        }
    }

    #[test]
    fn every_jp_glyph_id_is_a_real_toolbar_action() {
        // The kanji plate must only annotate ids that actually exist in the
        // toolbar registry — a stale kanji key would never render.
        let ids: std::collections::HashSet<&str> =
            TOOLBAR_ACTIONS.iter().map(|(id, _)| *id).collect();
        // Every id that jp_glyph annotates must be a real toolbar action id.
        for id in [
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
        ] {
            assert!(
                ids.contains(id),
                "jp_glyph id {id} missing from TOOLBAR_ACTIONS"
            );
            assert!(jp_glyph(id).is_some(), "{id} should have a kanji");
        }
    }

    #[test]
    fn editor_action_is_copyable_and_comparable() {
        // Smoke-cover the derived traits the palette relies on.
        let a = EditorAction::Copy;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(EditorAction::Copy, EditorAction::Paste);
    }
}
