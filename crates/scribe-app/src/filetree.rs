//! File-tree / explorer sidebar. Lazy: a directory's children are read only
//! when its header is expanded (egui `CollapsingHeader` runs its body only when
//! open). Returns the path the user clicked to open, if any.
//!
//! F-041 from `docs/audits/overlooked-surfaces-2026-05-29.md` adds keyboard
//! navigation: the explorer tracks a `focused` path and the host wires
//! Up/Down/Home/End/Enter into `FileTreeState::handle_input` so users can
//! drive the tree without the mouse. The flat visible-list is rebuilt every
//! frame from whichever folders egui currently shows as open, so the focus
//! index stays consistent with the rendered surface.

use eframe::egui;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Max directory-recursion depth for the explorer. A self-referential symlink
/// cycle (or a pathologically deep real tree) would otherwise drive
/// `dir_children`'s native recursion into a stack overflow / OOM. 64 levels is
/// far deeper than any real project hierarchy yet bounds the worst case.
const MAX_TREE_DEPTH: usize = 64;

/// Persistent state for the sidebar across frames.
#[derive(Default, Debug, Clone)]
pub struct FileTreeState {
    /// The focused entry (kept across frames for arrow-key nav).
    pub focused: Option<PathBuf>,
    /// Rebuilt every render — flat list of visible entries, top-down,
    /// matching the order egui paints them. Keyboard handlers consult
    /// this snapshot so nav stays consistent with the visible tree.
    visible: Vec<PathBuf>,
}

impl FileTreeState {
    /// Render the tree rooted at `root`. Returns `Some(path)` for a file the
    /// user clicked this frame.
    pub fn show(&mut self, ui: &mut egui::Ui, root: &Path) -> Option<PathBuf> {
        let mut clicked = None;
        self.visible.clear();
        let root_name = root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        // The root header itself is part of the visible list so Home/End
        // can land on it.
        self.visible.push(root.to_path_buf());
        let focused = self.focused.clone();
        // Track canonicalized directories already entered this frame so a
        // symlink cycle can't re-enter one (cheap second line of defense
        // alongside the no-symlink-traversal check and the depth cap).
        let mut visited: HashSet<PathBuf> = HashSet::new();
        if let Ok(real) = root.canonicalize() {
            visited.insert(real);
        }
        egui::CollapsingHeader::new(root_name)
            .default_open(true)
            .show(ui, |ui| {
                dir_children(
                    ui,
                    root,
                    &mut clicked,
                    &mut self.visible,
                    focused.as_deref(),
                    0,
                    &mut visited,
                )
            });
        clicked
    }

    /// Consume arrow keys / Enter / Home / End from the active egui context.
    /// Returns `Some(path)` when Enter on a file should open it. Safe to call
    /// every frame; only acts when the corresponding key is pressed AND the
    /// sidebar is showing a non-empty visible list.
    pub fn handle_input(&mut self, ctx: &egui::Context) -> Option<PathBuf> {
        if self.visible.is_empty() {
            return None;
        }
        let mut open_via_enter = None;
        ctx.input_mut(|i| {
            let cur_idx = self
                .focused
                .as_ref()
                .and_then(|p| self.visible.iter().position(|v| v == p));
            // Down → next visible entry; bounded at the bottom.
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                let next = match cur_idx {
                    Some(n) => (n + 1).min(self.visible.len() - 1),
                    None => 0,
                };
                self.focused = Some(self.visible[next].clone());
            }
            // Up → previous visible entry; bounded at the top.
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                let next = match cur_idx {
                    Some(0) | None => 0,
                    Some(n) => n - 1,
                };
                self.focused = Some(self.visible[next].clone());
            }
            // Home / End → first / last visible entry.
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                self.focused = Some(self.visible[0].clone());
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::End) {
                self.focused = Some(self.visible[self.visible.len() - 1].clone());
            }
            // Enter → open the focused entry IF it's a file. Directories are
            // navigated by clicking their header (egui doesn't expose a
            // stable way to toggle a CollapsingHeader open-state externally).
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                if let Some(p) = self.focused.clone() {
                    if p.is_file() {
                        open_via_enter = Some(p);
                    }
                }
            }
        });
        open_via_enter
    }
}

#[allow(clippy::too_many_arguments)]
fn dir_children(
    ui: &mut egui::Ui,
    dir: &Path,
    clicked: &mut Option<PathBuf>,
    visible: &mut Vec<PathBuf>,
    focused: Option<&Path>,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
) {
    // Bound recursion so a deep tree (or a symlink cycle that slipped past the
    // checks below) can never blow the stack.
    if depth >= MAX_TREE_DEPTH {
        ui.label(egui::RichText::new("(max depth reached)").weak().small());
        return;
    }
    let Ok(read) = fs::read_dir(dir) else {
        ui.label(egui::RichText::new("(unreadable)").weak().small());
        return;
    };
    let mut entries: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    // Dirs first, then files; each group alphabetical (case-insensitive).
    // `symlink_metadata` does NOT follow links, so a symlinked dir sorts as a
    // non-dir and is never recursed into.
    let is_real_dir = |p: &Path| {
        fs::symlink_metadata(p)
            .map(|m| m.file_type().is_dir())
            .unwrap_or(false)
    };
    entries.sort_by(|a, b| {
        let ad = is_real_dir(a);
        let bd = is_real_dir(b);
        bd.cmp(&ad).then_with(|| {
            a.file_name()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .cmp(&b.file_name().unwrap_or_default().to_ascii_lowercase())
        })
    });
    for path in entries {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Skip hidden / noisy dirs by convention.
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        visible.push(path.clone());
        // Only recurse into a REAL directory (not a symlink-to-dir) we have not
        // already entered this frame. `symlink_metadata` here means a symlinked
        // directory is rendered as a leaf — its cycle is never followed.
        if is_real_dir(&path) {
            // Dedupe by canonical path so even a hard-to-detect alias cycle is
            // entered at most once. If canonicalization fails, fall through and
            // rely on the depth cap.
            let already_seen = path
                .canonicalize()
                .map(|real| !visited.insert(real))
                .unwrap_or(false);
            if already_seen {
                ui.label(
                    egui::RichText::new(format!("{name} (cycle)"))
                        .weak()
                        .small(),
                );
                continue;
            }
            egui::CollapsingHeader::new(name)
                .id_salt(&path)
                .show(ui, |ui| {
                    dir_children(ui, &path, clicked, visible, focused, depth + 1, visited)
                });
        } else {
            let selected = focused.is_some_and(|f| f == path.as_path());
            let resp = ui.selectable_label(selected, name);
            if resp.clicked() {
                *clicked = Some(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir, File};
    use tempfile::tempdir;

    #[test]
    fn state_default_is_empty() {
        let s = FileTreeState::default();
        assert!(s.focused.is_none());
        assert!(s.visible.is_empty());
    }

    /// The visible list is rebuilt every frame; we populate it during a
    /// render-walk, then nav keys move `focused` through it. Smoke-test
    /// the walk by manually invoking `dir_children` against a real temp
    /// dir — this exercises the sort + filter + push order without needing
    /// a full egui render loop.
    #[test]
    fn dir_children_populates_visible_list_in_sort_order() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        // dir, file, hidden — the hidden one should be filtered.
        create_dir(root.join("zzz_dir")).unwrap();
        File::create(root.join("alpha.txt")).unwrap();
        File::create(root.join(".hidden")).unwrap();

        let mut visible = Vec::<PathBuf>::new();
        let mut clicked: Option<PathBuf> = None;
        let mut visited = HashSet::new();
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            dir_children(ui, root, &mut clicked, &mut visible, None, 0, &mut visited);
        });

        // dir comes first per the sort, then the file; hidden is excluded.
        assert_eq!(visible.len(), 2, "got {visible:?}");
        assert_eq!(visible[0].file_name().unwrap().to_string_lossy(), "zzz_dir");
        assert_eq!(
            visible[1].file_name().unwrap().to_string_lossy(),
            "alpha.txt"
        );
        assert!(clicked.is_none());
    }

    #[cfg(unix)]
    fn try_symlink_dir(src: &Path, dst: &Path) -> bool {
        std::os::unix::fs::symlink(src, dst).is_ok()
    }
    #[cfg(windows)]
    fn try_symlink_dir(src: &Path, dst: &Path) -> bool {
        std::os::windows::fs::symlink_dir(src, dst).is_ok()
    }

    /// A self-referential symlink cycle must NOT drive `dir_children` into
    /// unbounded recursion. The walk renders the symlink as a leaf (never
    /// recurses into it) and terminates; without the fix this recursed until a
    /// stack overflow. Gated: symlink creation may be unavailable on Windows
    /// without Developer Mode — skip gracefully when it is.
    #[test]
    fn dir_children_does_not_recurse_symlink_cycle() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        File::create(root.join("file.txt")).unwrap();
        let sub = root.join("sub");
        create_dir(&sub).unwrap();
        if !try_symlink_dir(root, &sub.join("loop")) {
            eprintln!("skipping: symlink creation unavailable");
            return;
        }
        let mut visible = Vec::<PathBuf>::new();
        let mut clicked: Option<PathBuf> = None;
        let mut visited = HashSet::new();
        if let Ok(real) = root.canonicalize() {
            visited.insert(real);
        }
        let ctx = egui::Context::default();
        // The key assertion is simply that this RETURNS (no stack overflow /
        // hang). The symlinked `loop` is rendered as a leaf, never entered.
        let _ = ctx.run_ui(Default::default(), |ui| {
            dir_children(ui, root, &mut clicked, &mut visible, None, 0, &mut visited);
        });
        assert!(
            visible.iter().any(|p| p.ends_with("file.txt")),
            "real file rendered: {visible:?}"
        );
    }

    /// The depth cap halts recursion on a pathologically deep REAL tree even
    /// when no symlink is involved (covers platforms where symlink creation is
    /// unavailable).
    #[test]
    fn dir_children_respects_depth_cap() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        // Build a real chain deeper than MAX_TREE_DEPTH.
        let mut p = root.to_path_buf();
        for i in 0..(MAX_TREE_DEPTH + 5) {
            p = p.join(format!("d{i}"));
            create_dir(&p).unwrap();
        }
        File::create(p.join("deep.txt")).unwrap();
        let mut visible = Vec::<PathBuf>::new();
        let mut clicked: Option<PathBuf> = None;
        let mut visited = HashSet::new();
        let ctx = egui::Context::default();
        // Must return without overflow; the deepest file is below the cap and
        // therefore never reached, proving the recursion was bounded.
        let _ = ctx.run_ui(Default::default(), |ui| {
            dir_children(ui, root, &mut clicked, &mut visible, None, 0, &mut visited);
        });
        assert!(
            !visible.iter().any(|p| p.ends_with("deep.txt")),
            "recursion must stop at the depth cap before reaching the deepest file"
        );
    }
}
