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
use std::fs;
use std::path::{Path, PathBuf};

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
        egui::CollapsingHeader::new(root_name)
            .default_open(true)
            .show(ui, |ui| {
                dir_children(
                    ui,
                    root,
                    &mut clicked,
                    &mut self.visible,
                    focused.as_deref(),
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

fn dir_children(
    ui: &mut egui::Ui,
    dir: &Path,
    clicked: &mut Option<PathBuf>,
    visible: &mut Vec<PathBuf>,
    focused: Option<&Path>,
) {
    let Ok(read) = fs::read_dir(dir) else {
        ui.label(egui::RichText::new("(unreadable)").weak().small());
        return;
    };
    let mut entries: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    // Dirs first, then files; each group alphabetical (case-insensitive).
    entries.sort_by(|a, b| {
        let ad = a.is_dir();
        let bd = b.is_dir();
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
        if path.is_dir() {
            egui::CollapsingHeader::new(name)
                .id_salt(&path)
                .show(ui, |ui| dir_children(ui, &path, clicked, visible, focused));
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
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            dir_children(ui, root, &mut clicked, &mut visible, None);
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
}
