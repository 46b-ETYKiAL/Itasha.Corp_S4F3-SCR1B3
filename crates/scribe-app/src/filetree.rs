//! File-tree / explorer sidebar. Lazy: a directory's children are read only
//! when its header is expanded (egui `CollapsingHeader` runs its body only when
//! open). Returns the path the user clicked to open, if any.

use eframe::egui;
use std::fs;
use std::path::{Path, PathBuf};

/// Render the tree rooted at `root`. Returns `Some(path)` for a file the user
/// clicked this frame.
pub fn show(ui: &mut egui::Ui, root: &Path) -> Option<PathBuf> {
    let mut clicked = None;
    let root_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    egui::CollapsingHeader::new(root_name)
        .default_open(true)
        .show(ui, |ui| dir_children(ui, root, &mut clicked));
    clicked
}

fn dir_children(ui: &mut egui::Ui, dir: &Path, clicked: &mut Option<PathBuf>) {
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
        if path.is_dir() {
            egui::CollapsingHeader::new(name)
                .id_salt(&path)
                .show(ui, |ui| dir_children(ui, &path, clicked));
        } else if ui.selectable_label(false, name).clicked() {
            *clicked = Some(path);
        }
    }
}
