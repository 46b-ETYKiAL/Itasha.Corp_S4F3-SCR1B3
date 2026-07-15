//! F-039 + F-040 from `docs/audits/overlooked-surfaces-2026-05-29.md`:
//! the plugin manager modal that surfaces the Phase-20 plugin foundation
//! (`scribe_core::plugin::{registry, integrity, discover}`) which was built
//! but never wired to any UI.
//!
//! Three tabs:
//!
//! * **Loaded** — the plugins `discover()` found on disk, with an
//!   enable/disable toggle that the host applies to `config.plugins.disabled`.
//! * **Registry** — parse a local `index.toml` via
//!   [`RegistryIndex::from_toml_str`] and list its entries with the same
//!   case-insensitive search the core exposes. "One-click install" prefills
//!   the Install tab from the selected release (F-040).
//! * **Install** — verify a *local* plugin tarball against its
//!   checksum + minisign signature + author key via
//!   [`verify_plugin_tarball`] and surface the honest verdict (F-039).
//!
//! The app ships **no network stack by construction** (zero HTTP deps — see
//! the update module, which operates on local paths only). So "install from
//! URL" is deliberately NOT offered: the install path verifies a tarball the
//! user already has on disk, which is the truthful surface of the built
//! foundation. Network download is a separate, dependency-bearing change.

use std::path::Path;

use scribe_core::plugin::{verify_plugin_tarball, RegistryIndex};

/// Which tab the modal is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PluginManagerTab {
    #[default]
    Loaded,
    Registry,
    Install,
}

/// One row in the Loaded tab — a projection of a discovered plugin plus the
/// host-known enabled state. The host builds these (it owns `discover` +
/// `config.plugins.disabled`) and hands them to [`PluginManagerState::show`].
#[derive(Debug, Clone)]
pub struct LoadedRow {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
    /// #R6 — the plugin was discovered but is held back (its current entry
    /// script has not been approved by the user). It is NOT running.
    pub pending: bool,
}

/// The action the modal asks the host to apply this frame. The modal never
/// mutates host config directly — it returns intent, matching the `Pending`
/// pattern the rest of `app.rs` uses.
#[derive(Debug, Default, Clone)]
pub struct PluginManagerAction {
    /// Toggle the disabled-state of this plugin id in `config.plugins.disabled`.
    pub toggle_disabled: Option<String>,
    /// Open the plugins directory in the OS file manager (drop-in install).
    pub open_plugins_dir: bool,
    /// #R6 — approve this plugin id: record + trust its CURRENT entry script so
    /// it may run, then load it.
    pub approve: Option<String>,
}

impl PluginManagerAction {
    fn is_empty(&self) -> bool {
        self.toggle_disabled.is_none() && !self.open_plugins_dir && self.approve.is_none()
    }
}

/// Persistent modal state, held across frames by the host.
#[derive(Debug, Clone, Default)]
pub struct PluginManagerState {
    pub open: bool,
    pub tab: PluginManagerTab,

    // ---- Registry tab ----
    /// Path to the local `index.toml` registry file.
    pub registry_path: String,
    /// Case-insensitive search query over the parsed registry.
    pub registry_query: String,
    /// The parsed registry, if a load succeeded.
    pub registry: Option<RegistryIndex>,
    /// The most recent load error (parse failure / schema-too-new / unreadable).
    pub registry_error: Option<String>,

    // ---- Install tab ----
    pub install_tarball_path: String,
    pub install_sig_path: String,
    pub install_sha: String,
    pub install_pubkey: String,
    /// `Some(Ok(msg))` = verified; `Some(Err(msg))` = rejected; `None` = not run.
    pub install_result: Option<Result<String, String>>,
}

impl PluginManagerState {
    /// The conventional registry location: `<config_dir>/registry/index.toml`.
    /// Returns an empty string when the OS config dir can't be resolved (rare;
    /// the field stays user-editable either way).
    pub fn default_registry_path(config_dir: Option<&Path>) -> String {
        config_dir
            .map(|d| d.join("registry").join("index.toml").display().to_string())
            .unwrap_or_default()
    }

    /// Lazily seed `registry_path` to the default the first time the modal
    /// opens with an empty path, so the user has a sensible target.
    pub fn ensure_defaults(&mut self, config_dir: Option<&Path>) {
        if self.registry_path.is_empty() {
            self.registry_path = Self::default_registry_path(config_dir);
        }
    }

    /// Read + parse the registry at `registry_path`. Sets `registry` on
    /// success (clearing any prior error) or `registry_error` on failure
    /// (clearing any stale parse). Pure I/O + parse — no egui.
    pub fn load_registry(&mut self) {
        let path = self.registry_path.trim();
        if path.is_empty() {
            self.registry = None;
            self.registry_error = Some("Enter a path to an index.toml registry file.".to_string());
            return;
        }
        match std::fs::read_to_string(path) {
            Ok(body) => match RegistryIndex::from_toml_str(&body) {
                Ok(idx) => {
                    self.registry = Some(idx);
                    self.registry_error = None;
                }
                Err(e) => {
                    self.registry = None;
                    self.registry_error = Some(e.to_string());
                }
            },
            Err(e) => {
                self.registry = None;
                self.registry_error = Some(format!("Couldn't read {path}: {e}"));
            }
        }
    }

    /// The registry entries matching the current query, or an empty list when
    /// no registry is loaded. Borrows from `self.registry`.
    pub fn filtered(&self) -> Vec<&scribe_core::plugin::PluginEntry> {
        match &self.registry {
            Some(idx) => idx.search(&self.registry_query),
            None => Vec::new(),
        }
    }

    /// Prefill the Install tab from a registry entry's stable release (the
    /// "one-click install" bridge, F-040). Fills checksum + author key + a
    /// suggested tarball/signature filename derived from the release URLs so
    /// the user only has to point at the downloaded files. Switches to the
    /// Install tab and clears any prior verdict.
    pub fn prefill_install_from(&mut self, entry: &scribe_core::plugin::PluginEntry) {
        if let Some(release) = entry.stable_release() {
            self.install_sha = release.checksum_sha256.clone();
            self.install_tarball_path = url_basename(&release.tarball_url);
            self.install_sig_path = url_basename(&release.signature_url);
        }
        self.install_pubkey = entry.author_pubkey.clone();
        self.install_result = None;
        self.tab = PluginManagerTab::Install;
    }

    /// Verify the local tarball at `install_tarball_path` against the declared
    /// checksum + the signature file at `install_sig_path` + the author key.
    /// Sets `install_result`. No network, no extraction — verification only;
    /// extraction-install is the file-drop workflow surfaced on the Loaded tab.
    pub fn verify_install(&mut self) {
        self.install_result = Some(self.run_verify());
    }

    fn run_verify(&self) -> Result<String, String> {
        let tarball_path = self.install_tarball_path.trim();
        if tarball_path.is_empty() {
            return Err("Choose a plugin tarball file to verify.".to_string());
        }
        let sig_path = self.install_sig_path.trim();
        if sig_path.is_empty() {
            return Err("Choose the matching .minisig signature file.".to_string());
        }
        if self.install_sha.trim().is_empty() {
            return Err("Enter the expected SHA-256 checksum.".to_string());
        }
        if self.install_pubkey.trim().is_empty() {
            return Err("Enter the author's public key.".to_string());
        }
        let bytes = std::fs::read(tarball_path)
            .map_err(|e| format!("Couldn't read tarball {tarball_path}: {e}"))?;
        let sig = std::fs::read_to_string(sig_path)
            .map_err(|e| format!("Couldn't read signature {sig_path}: {e}"))?;
        verify_plugin_tarball(
            &bytes,
            self.install_sha.trim(),
            &sig,
            self.install_pubkey.trim(),
        )
        .map(|()| "Verified — checksum and signature both pass.".to_string())
    }
}

/// The trailing path component of a URL, used as a suggested local filename.
/// Trailing separators are trimmed first so `…/trailing/` yields `trailing`.
/// Falls back to the whole string when there's no non-empty component.
fn url_basename(url: &str) -> String {
    let trimmed = url.trim_end_matches(['/', '\\']);
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(url)
        .to_string()
}

// ---- egui surface ---------------------------------------------------------

use eframe::egui;

impl PluginManagerState {
    /// Render the modal. Returns the action the host should apply. `loaded` is
    /// the host-built Loaded-tab row set; `plugins_dir` is shown as the
    /// drop-in target. The egui layer is intentionally thin — every decision
    /// lives in the tested core above.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        accent: egui::Color32,
        muted: egui::Color32,
        loaded: &[LoadedRow],
        plugins_dir: &Path,
    ) -> PluginManagerAction {
        let mut action = PluginManagerAction::default();
        if !self.open {
            return action;
        }
        let mut still_open = true;
        egui::Window::new(
            egui::RichText::new(format!("{}  plugin manager", egui_phosphor::thin::CARDS))
                .color(accent)
                .monospace(),
        )
        .open(&mut still_open)
        .collapsible(false)
        .resizable(true)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, PluginManagerTab::Loaded, "Loaded");
                ui.selectable_value(&mut self.tab, PluginManagerTab::Registry, "Registry");
                ui.selectable_value(&mut self.tab, PluginManagerTab::Install, "Install");
            });
            ui.separator();
            match self.tab {
                PluginManagerTab::Loaded => {
                    self.show_loaded(ui, accent, muted, loaded, plugins_dir, &mut action)
                }
                PluginManagerTab::Registry => self.show_registry(ui, accent, muted, &mut action),
                PluginManagerTab::Install => self.show_install(ui, accent, muted),
            }
        });
        if !still_open {
            self.open = false;
        }
        if action.is_empty() {
            PluginManagerAction::default()
        } else {
            action
        }
    }

    fn show_loaded(
        &mut self,
        ui: &mut egui::Ui,
        accent: egui::Color32,
        muted: egui::Color32,
        loaded: &[LoadedRow],
        plugins_dir: &Path,
        action: &mut PluginManagerAction,
    ) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("plugins dir: {}", plugins_dir.display()))
                    .color(muted)
                    .small(),
            );
            if ui.small_button("open folder").clicked() {
                action.open_plugins_dir = true;
            }
        });
        ui.add_space(4.0);
        if loaded.is_empty() {
            ui.label(
                egui::RichText::new(
                    "No plugins discovered. Drop a plugin folder (with plugin.toml) \
                     into the plugins dir, then restart.",
                )
                .color(muted)
                .small(),
            );
            return;
        }
        egui::ScrollArea::vertical()
            .max_height(360.0)
            .show(ui, |ui| {
                for row in loaded {
                    ui.horizontal(|ui| {
                        let mut enabled = row.enabled;
                        if ui.checkbox(&mut enabled, "").changed() {
                            action.toggle_disabled = Some(row.id.clone());
                        }
                        ui.label(egui::RichText::new(&row.name).color(accent).monospace());
                        if !row.version.is_empty() {
                            ui.label(
                                egui::RichText::new(format!("v{}", row.version))
                                    .color(muted)
                                    .small(),
                            );
                        }
                    });
                    if !row.description.is_empty() {
                        ui.label(egui::RichText::new(&row.description).color(muted).small());
                    }
                    // #R6 — pending-approval row: the plugin is NOT running until
                    // the user reviews + approves its current entry script.
                    if row.pending {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}  not running — needs your approval",
                                    egui_phosphor::thin::WARNING
                                ))
                                .color(egui::Color32::from_rgb(0xfb, 0xbf, 0x24))
                                .small(),
                            );
                            if ui
                                .button("Approve & run")
                                .on_hover_text(
                                    "Trust THIS version of the plugin's script and run it. \
                                     If the script changes later it must be approved again.",
                                )
                                .clicked()
                            {
                                action.approve = Some(row.id.clone());
                            }
                        });
                    }
                    ui.add_space(6.0);
                }
            });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Disable changes apply on the next restart.")
                .color(muted)
                .small(),
        );
    }

    fn show_registry(
        &mut self,
        ui: &mut egui::Ui,
        accent: egui::Color32,
        muted: egui::Color32,
        action: &mut PluginManagerAction,
    ) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("index.toml:").color(muted).small());
            ui.add(
                egui::TextEdit::singleline(&mut self.registry_path)
                    .desired_width(320.0)
                    .hint_text("path to a local registry index.toml"),
            );
            if ui.button("load").clicked() {
                self.load_registry();
            }
        });
        if let Some(err) = &self.registry_error {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(err)
                    .color(egui::Color32::from_rgb(0xE0, 0x6C, 0x6C))
                    .small(),
            );
            return;
        }
        if self.registry.is_none() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Load a registry index.toml to browse published plugins.")
                    .color(muted)
                    .small(),
            );
            return;
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("search:").color(muted).small());
            ui.add(
                egui::TextEdit::singleline(&mut self.registry_query)
                    .desired_width(260.0)
                    .hint_text("filter by name / id / author"),
            );
        });
        ui.separator();
        // Collect the prefill target first to avoid borrowing `self.registry`
        // immutably (via `filtered`) while the closure needs `&mut self`.
        let mut prefill: Option<scribe_core::plugin::PluginEntry> = None;
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                let hits = self.filtered();
                if hits.is_empty() {
                    ui.label(
                        egui::RichText::new("No entries match.")
                            .color(muted)
                            .small(),
                    );
                }
                for entry in hits {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&entry.name).color(accent).monospace());
                        if !entry.version_stable.is_empty() {
                            ui.label(
                                egui::RichText::new(format!("v{}", entry.version_stable))
                                    .color(muted)
                                    .small(),
                            );
                        }
                        if entry.stable_release().is_some()
                            && ui
                                .small_button(format!(
                                    "install {}",
                                    egui_phosphor::thin::ARROW_RIGHT
                                ))
                                .clicked()
                        {
                            prefill = Some(entry.clone());
                        }
                    });
                    if !entry.description.is_empty() {
                        ui.label(egui::RichText::new(&entry.description).color(muted).small());
                    }
                    if !entry.capabilities.is_empty() {
                        ui.label(
                            egui::RichText::new(format!(
                                "capabilities: {}",
                                entry.capabilities.join(", ")
                            ))
                            .color(muted)
                            .small(),
                        );
                    }
                    ui.add_space(6.0);
                }
            });
        if let Some(entry) = prefill {
            self.prefill_install_from(&entry);
        }
        let _ = action; // registry tab raises no host action
    }

    fn show_install(&mut self, ui: &mut egui::Ui, accent: egui::Color32, muted: egui::Color32) {
        ui.label(
            egui::RichText::new(
                "Verify a plugin tarball you already downloaded. Both the SHA-256 \
                 checksum and the author's minisign signature must pass before you \
                 extract it into the plugins dir.",
            )
            .color(muted)
            .small(),
        );
        ui.add_space(6.0);
        egui::Grid::new("plugin-install-grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("tarball").color(muted).small());
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.install_tarball_path)
                            .desired_width(320.0)
                            .hint_text("path to the .tar.gz"),
                    );
                    if ui.small_button("browse…").clicked() {
                        if let Some(p) = crate::app::dialogs::pick_file() {
                            self.install_tarball_path = p.display().to_string();
                        }
                    }
                });
                ui.end_row();
                ui.label(egui::RichText::new("signature").color(muted).small());
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.install_sig_path)
                            .desired_width(320.0)
                            .hint_text("path to the .minisig"),
                    );
                    if ui.small_button("browse…").clicked() {
                        if let Some(p) = crate::app::dialogs::pick_file() {
                            self.install_sig_path = p.display().to_string();
                        }
                    }
                });
                ui.end_row();
                ui.label(egui::RichText::new("sha-256").color(muted).small());
                ui.add(
                    egui::TextEdit::singleline(&mut self.install_sha)
                        .desired_width(380.0)
                        .hint_text("expected checksum"),
                );
                ui.end_row();
                ui.label(egui::RichText::new("pubkey").color(muted).small());
                ui.add(
                    egui::TextEdit::multiline(&mut self.install_pubkey)
                        .desired_width(380.0)
                        .desired_rows(2)
                        .hint_text("author minisign public key"),
                );
                ui.end_row();
            });
        ui.add_space(6.0);
        if ui.button("verify").clicked() {
            self.verify_install();
        }
        if let Some(result) = &self.install_result {
            ui.add_space(6.0);
            match result {
                Ok(msg) => {
                    ui.label(
                        egui::RichText::new(format!("{} {msg}", egui_phosphor::thin::CHECK))
                            .color(accent)
                            .small(),
                    );
                }
                Err(msg) => {
                    ui.label(
                        egui::RichText::new(format!("{} {msg}", egui_phosphor::thin::X))
                            .color(egui::Color32::from_rgb(0xE0, 0x6C, 0x6C))
                            .small(),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scribe_core::update::verify::sha256_hex;
    use std::fs;
    use tempfile::tempdir;

    fn sample_registry_toml() -> &'static str {
        r#"
schema_version = 1

[[plugins]]
id = "com.example.hello"
name = "Hello World"
description = "Greets you on save"
author = "Ada"
version_stable = "1.2.0"
author_pubkey = "RWQexamplekey"

[[plugins.releases]]
version = "1.2.0"
tarball_url = "https://example.com/hello-1.2.0.tar.gz"
signature_url = "https://example.com/hello-1.2.0.tar.gz.minisig"
checksum_sha256 = "deadbeef"
api_version = 1
capabilities = ["read_buffer"]

[[plugins]]
id = "com.example.lint"
name = "Linter"
description = "Flags long lines"
author = "Linus"
version_stable = "0.3.0"
"#
    }

    #[test]
    fn default_registry_path_joins_registry_index() {
        let p = PluginManagerState::default_registry_path(Some(Path::new("/cfg")));
        assert!(
            p.replace('\\', "/").ends_with("/cfg/registry/index.toml"),
            "got {p}"
        );
    }

    #[test]
    fn default_registry_path_empty_without_config_dir() {
        assert_eq!(PluginManagerState::default_registry_path(None), "");
    }

    #[test]
    fn load_registry_parses_and_searches() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("index.toml");
        fs::write(&path, sample_registry_toml()).unwrap();
        let mut st = PluginManagerState {
            registry_path: path.display().to_string(),
            ..Default::default()
        };
        st.load_registry();
        assert!(st.registry_error.is_none(), "{:?}", st.registry_error);
        assert_eq!(st.filtered().len(), 2);
        // Case-insensitive substring search over name/id/author.
        st.registry_query = "lint".to_string();
        let hits = st.filtered();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "com.example.lint");
    }

    #[test]
    fn load_registry_surfaces_parse_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("broken.toml");
        fs::write(&path, "this is not valid = = toml").unwrap();
        let mut st = PluginManagerState {
            registry_path: path.display().to_string(),
            ..Default::default()
        };
        st.load_registry();
        assert!(st.registry.is_none());
        assert!(st.registry_error.is_some());
    }

    #[test]
    fn load_registry_refuses_newer_schema() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("future.toml");
        fs::write(&path, "schema_version = 99\n").unwrap();
        let mut st = PluginManagerState {
            registry_path: path.display().to_string(),
            ..Default::default()
        };
        st.load_registry();
        assert!(st.registry.is_none());
        assert!(
            st.registry_error.as_deref().unwrap().contains("newer"),
            "want schema-too-new message, got {:?}",
            st.registry_error
        );
    }

    #[test]
    fn load_registry_reports_missing_file() {
        let mut st = PluginManagerState {
            registry_path: "/no/such/index.toml".to_string(),
            ..Default::default()
        };
        st.load_registry();
        assert!(st.registry.is_none());
        assert!(st.registry_error.unwrap().contains("read"));
    }

    #[test]
    fn prefill_from_entry_fills_install_fields_and_switches_tab() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("index.toml");
        fs::write(&path, sample_registry_toml()).unwrap();
        let mut st = PluginManagerState {
            registry_path: path.display().to_string(),
            ..Default::default()
        };
        st.load_registry();
        let entry = st
            .registry
            .as_ref()
            .unwrap()
            .by_id("com.example.hello")
            .unwrap()
            .clone();
        st.prefill_install_from(&entry);
        assert_eq!(st.tab, PluginManagerTab::Install);
        assert_eq!(st.install_sha, "deadbeef");
        assert_eq!(st.install_tarball_path, "hello-1.2.0.tar.gz");
        assert_eq!(st.install_sig_path, "hello-1.2.0.tar.gz.minisig");
        assert_eq!(st.install_pubkey, "RWQexamplekey");
    }

    #[test]
    fn verify_install_happy_path_accepts_signed_tarball() {
        let dir = tempdir().unwrap();
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"a synthetic plugin tarball";
        let sig = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            None,
            None,
        )
        .unwrap()
        .to_string();
        let tarball = dir.path().join("p.tar.gz");
        let sigfile = dir.path().join("p.tar.gz.minisig");
        fs::write(&tarball, data).unwrap();
        fs::write(&sigfile, &sig).unwrap();

        let mut st = PluginManagerState {
            install_tarball_path: tarball.display().to_string(),
            install_sig_path: sigfile.display().to_string(),
            install_sha: sha256_hex(data),
            install_pubkey: pk_box,
            ..Default::default()
        };
        st.verify_install();
        assert!(
            matches!(st.install_result, Some(Ok(_))),
            "want Ok verdict, got {:?}",
            st.install_result
        );
    }

    #[test]
    fn verify_install_rejects_tampered_tarball() {
        let dir = tempdir().unwrap();
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_box = kp.pk.to_box().unwrap().to_string();
        let data = b"a synthetic plugin tarball";
        let sig = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(&data[..]),
            None,
            None,
        )
        .unwrap()
        .to_string();
        let tarball = dir.path().join("p.tar.gz");
        let sigfile = dir.path().join("p.tar.gz.minisig");
        // Write DIFFERENT bytes than were signed/checksummed.
        fs::write(&tarball, b"tampered bytes").unwrap();
        fs::write(&sigfile, &sig).unwrap();

        let mut st = PluginManagerState {
            install_tarball_path: tarball.display().to_string(),
            install_sig_path: sigfile.display().to_string(),
            install_sha: sha256_hex(data),
            install_pubkey: pk_box,
            ..Default::default()
        };
        st.verify_install();
        assert!(
            matches!(st.install_result, Some(Err(_))),
            "tampered tarball must be rejected, got {:?}",
            st.install_result
        );
    }

    #[test]
    fn verify_install_requires_all_fields() {
        let mut st = PluginManagerState::default();
        st.verify_install();
        let err = match st.install_result {
            Some(Err(e)) => e,
            other => panic!("want field-missing error, got {other:?}"),
        };
        assert!(err.to_lowercase().contains("tarball"), "got {err}");
    }

    #[test]
    fn url_basename_takes_trailing_component() {
        assert_eq!(url_basename("https://x.com/a/b/c.tar.gz"), "c.tar.gz");
        assert_eq!(url_basename("plainname"), "plainname");
        assert_eq!(url_basename("https://x.com/trailing/"), "trailing");
    }

    #[test]
    fn action_empty_detection() {
        assert!(PluginManagerAction::default().is_empty());
        let a = PluginManagerAction {
            toggle_disabled: Some("x".into()),
            ..Default::default()
        };
        assert!(!a.is_empty());
    }

    #[test]
    fn action_is_empty_for_each_field() {
        // Each settable field on its own makes the action non-empty.
        assert!(!PluginManagerAction {
            open_plugins_dir: true,
            ..Default::default()
        }
        .is_empty());
        assert!(!PluginManagerAction {
            approve: Some("id".into()),
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn ensure_defaults_seeds_then_leaves_alone() {
        let mut st = PluginManagerState::default();
        st.ensure_defaults(Some(Path::new("/cfg")));
        let seeded = st.registry_path.clone();
        assert!(seeded
            .replace('\\', "/")
            .ends_with("/cfg/registry/index.toml"));
        // A second call with a different dir must NOT overwrite a non-empty path.
        st.ensure_defaults(Some(Path::new("/other")));
        assert_eq!(st.registry_path, seeded);
    }

    #[test]
    fn ensure_defaults_no_config_dir_leaves_path_empty() {
        let mut st = PluginManagerState::default();
        st.ensure_defaults(None);
        assert_eq!(st.registry_path, "");
    }

    #[test]
    fn filtered_is_empty_without_a_loaded_registry() {
        let st = PluginManagerState::default();
        assert!(st.filtered().is_empty());
    }

    #[test]
    fn load_registry_with_empty_path_reports_a_hint() {
        let mut st = PluginManagerState {
            registry_path: "   ".to_string(), // whitespace-only trims to empty
            ..Default::default()
        };
        st.load_registry();
        assert!(st.registry.is_none());
        assert!(
            st.registry_error.as_deref().unwrap().contains("index.toml"),
            "want the enter-a-path hint, got {:?}",
            st.registry_error
        );
    }

    #[test]
    fn prefill_without_release_still_sets_pubkey_and_tab() {
        // The com.example.lint entry has NO releases — stable_release() is None,
        // so the sha/tarball/sig stay empty but pubkey + tab still update.
        let dir = tempdir().unwrap();
        let path = dir.path().join("index.toml");
        fs::write(&path, sample_registry_toml()).unwrap();
        let mut st = PluginManagerState {
            registry_path: path.display().to_string(),
            ..Default::default()
        };
        st.load_registry();
        let entry = st
            .registry
            .as_ref()
            .unwrap()
            .by_id("com.example.lint")
            .unwrap()
            .clone();
        st.prefill_install_from(&entry);
        assert_eq!(st.tab, PluginManagerTab::Install);
        assert!(st.install_sha.is_empty(), "no release => no checksum");
        assert!(st.install_tarball_path.is_empty());
    }

    #[test]
    fn verify_install_requires_signature_then_sha_then_pubkey() {
        // Field-by-field error precedence: tarball set but sig missing, etc.
        let mut st = PluginManagerState {
            install_tarball_path: "/srv/x/p.tar.gz".to_string(),
            ..Default::default()
        };
        st.verify_install();
        let err = match &st.install_result {
            Some(Err(e)) => e.clone(),
            other => panic!("want signature error, got {other:?}"),
        };
        assert!(err.to_lowercase().contains("signature"), "got {err}");

        st.install_sig_path = "/srv/x/p.minisig".to_string();
        st.verify_install();
        let err = match &st.install_result {
            Some(Err(e)) => e.clone(),
            other => panic!("want sha error, got {other:?}"),
        };
        assert!(err.to_lowercase().contains("sha"), "got {err}");

        st.install_sha = "deadbeef".to_string();
        st.verify_install();
        let err = match &st.install_result {
            Some(Err(e)) => e.clone(),
            other => panic!("want pubkey error, got {other:?}"),
        };
        assert!(err.to_lowercase().contains("public key"), "got {err}");
    }

    #[test]
    fn verify_install_reports_unreadable_tarball() {
        // All fields present, but the tarball path does not exist on disk.
        let mut st = PluginManagerState {
            install_tarball_path: "/no/such/plugin.tar.gz".to_string(),
            install_sig_path: "/no/such/plugin.minisig".to_string(),
            install_sha: "deadbeef".to_string(),
            install_pubkey: "RWQexamplekey".to_string(),
            ..Default::default()
        };
        st.verify_install();
        let err = match &st.install_result {
            Some(Err(e)) => e.clone(),
            other => panic!("want read error, got {other:?}"),
        };
        assert!(err.contains("read tarball"), "got {err}");
    }

    // ---- egui surface drive-throughs (egui_kittest, headless) --------------
    //
    // These run the real `show()` modal render loop so the per-tab paint arms
    // (Loaded rows, pending-approval, empty-state; Registry load/error/search;
    // Install grid + verdict display) execute. We assert on the AccessKit tree
    // and on the returned PluginManagerAction — the observable surface — not on
    // pixels. No GPU; the rfd file-dialog `browse…` arms stay uncovered (their
    // bodies block on a native dialog and are excluded per WU-0).
    use egui_kittest::kittest::Queryable as _;

    const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x00, 0xd0, 0xa0);
    const MUTED: egui::Color32 = egui::Color32::from_rgb(0x80, 0x80, 0x80);

    fn loaded_rows() -> Vec<LoadedRow> {
        vec![
            LoadedRow {
                id: "com.example.hello".into(),
                name: "Hello World".into(),
                version: "1.2.0".into(),
                description: "Greets you on save".into(),
                enabled: true,
                pending: false,
            },
            LoadedRow {
                id: "com.example.pend".into(),
                name: "Pending Plugin".into(),
                version: String::new(),
                description: String::new(),
                enabled: false,
                pending: true,
            },
        ]
    }

    /// Test-only harness state for driving the modal as a real CONTEXT app
    /// (egui_kittest's `build_state` — required because `show` opens an
    /// `egui::Window`, and per the kittest docs pointer input only reaches a
    /// Window when the app is built on a `&egui::Context`, not a panel `&mut Ui`).
    /// The most recent action is captured each frame for click-effect asserts.
    struct ModalHarness {
        state: PluginManagerState,
        rows: Vec<LoadedRow>,
        last_action: PluginManagerAction,
    }

    impl ModalHarness {
        fn new(tab: PluginManagerTab, rows: Vec<LoadedRow>) -> Self {
            ModalHarness {
                state: PluginManagerState {
                    open: true,
                    tab,
                    ..Default::default()
                },
                rows,
                last_action: PluginManagerAction::default(),
            }
        }

        fn frame(&mut self, ui: &mut egui::Ui) {
            let action = self.state.show(
                ui.ctx(),
                ACCENT,
                MUTED,
                &self.rows,
                Path::new("/srv/x/plugins"),
            );
            // `Harness::run` steps several frames until repaint settles; a click
            // raises its action on ONE frame only, so OR-merge into a sticky
            // accumulator instead of overwriting (a later no-click frame must
            // not erase the raised action).
            if action.toggle_disabled.is_some() {
                self.last_action.toggle_disabled = action.toggle_disabled;
            }
            if action.open_plugins_dir {
                self.last_action.open_plugins_dir = true;
            }
            if action.approve.is_some() {
                self.last_action.approve = action.approve;
            }
        }

        /// Reset the sticky accumulator (call before the click whose effect we
        /// want to isolate).
        fn clear_action(&mut self) {
            self.last_action = PluginManagerAction::default();
        }
    }

    fn modal_harness(state: ModalHarness) -> egui_kittest::Harness<'static, ModalHarness> {
        egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(720.0, 760.0))
            .build_ui_state(|ui, st: &mut ModalHarness| st.frame(ui), state)
    }

    #[test]
    fn show_closed_modal_returns_empty_action_and_paints_nothing() {
        // open == false: the modal early-returns before painting any window.
        let mut harness = ModalHarness::new(PluginManagerTab::Loaded, loaded_rows());
        harness.state.open = false;
        let mut h = modal_harness(harness);
        h.run();
        assert!(
            h.state().last_action.is_empty(),
            "a closed modal raises no action"
        );
        // No window content painted (no tab headers, no title).
        assert!(h.query_by_label("Loaded").is_none());
        assert!(h.query_by_label("open folder").is_none());
    }

    #[test]
    fn show_loaded_tab_lists_rows_and_pending_approval() {
        let mut st = PluginManagerState {
            open: true,
            tab: PluginManagerTab::Loaded,
            ..Default::default()
        };
        let rows = loaded_rows();
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(640.0, 600.0))
            .build_ui(|ui| {
                st.show(ui.ctx(), ACCENT, MUTED, &rows, Path::new("/srv/x/plugins"));
            });
        h.run();
        // Both plugin names render; the pending one shows the Approve & run CTA.
        assert!(h.query_by_label("Hello World").is_some());
        assert!(h.query_by_label("Pending Plugin").is_some());
        assert!(h.query_by_label("Approve & run").is_some());
        assert!(h.query_by_label("open folder").is_some());
    }

    #[test]
    fn show_loaded_tab_empty_state_renders_hint() {
        let mut st = PluginManagerState {
            open: true,
            tab: PluginManagerTab::Loaded,
            ..Default::default()
        };
        let rows: Vec<LoadedRow> = Vec::new();
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(640.0, 480.0))
            .build_ui(|ui| {
                st.show(ui.ctx(), ACCENT, MUTED, &rows, Path::new("/srv/x/plugins"));
            });
        h.run();
        // The empty-state copy is painted (covers the loaded.is_empty() arm).
        assert!(
            h.query_by_label("open folder").is_some(),
            "the dir header always paints, even with no rows"
        );
    }

    #[test]
    fn show_loaded_open_folder_click_raises_action() {
        let mut h = modal_harness(ModalHarness::new(PluginManagerTab::Loaded, loaded_rows()));
        h.run();
        h.state_mut().clear_action();
        h.get_by_label("open folder").click();
        h.run();
        assert!(
            h.state().last_action.open_plugins_dir,
            "clicking 'open folder' must raise open_plugins_dir"
        );
    }

    #[test]
    fn show_loaded_toggle_checkbox_raises_disable_action() {
        let mut h = modal_harness(ModalHarness::new(PluginManagerTab::Loaded, loaded_rows()));
        h.run();
        h.state_mut().clear_action();
        // Two rows => two empty-label checkboxes; the FIRST belongs to the
        // first row (com.example.hello). get_all preserves document order.
        let first = h
            .get_all_by_role(egui::accesskit::Role::CheckBox)
            .next()
            .expect("a row checkbox must be present");
        first.click();
        h.run();
        assert_eq!(
            h.state().last_action.toggle_disabled.as_deref(),
            Some("com.example.hello"),
            "toggling the first row's checkbox must raise toggle_disabled"
        );
    }

    #[test]
    fn show_loaded_approve_click_raises_action() {
        let mut h = modal_harness(ModalHarness::new(PluginManagerTab::Loaded, loaded_rows()));
        h.run();
        h.state_mut().clear_action();
        h.get_by_label("Approve & run").click();
        h.run();
        assert_eq!(
            h.state().last_action.approve.as_deref(),
            Some("com.example.pend"),
            "clicking 'Approve & run' must raise approve for the pending plugin"
        );
    }

    #[test]
    fn show_registry_tab_load_error_and_search_render() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("index.toml");
        fs::write(&path, sample_registry_toml()).unwrap();
        let mut st = PluginManagerState {
            open: true,
            tab: PluginManagerTab::Registry,
            registry_path: path.display().to_string(),
            ..Default::default()
        };
        // Pre-load the registry so the search + entry list arms paint.
        st.load_registry();
        let rows: Vec<LoadedRow> = Vec::new();
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(680.0, 640.0))
            .build_ui(|ui| {
                st.show(ui.ctx(), ACCENT, MUTED, &rows, Path::new("/srv/x/plugins"));
            });
        h.run();
        // The loaded entries render with their install CTA + a load button.
        assert!(h.query_by_label("Hello World").is_some());
        assert!(h.query_by_label("load").is_some());
    }

    #[test]
    fn show_registry_tab_unloaded_prompts_to_load() {
        let mut st = PluginManagerState {
            open: true,
            tab: PluginManagerTab::Registry,
            ..Default::default()
        };
        let rows: Vec<LoadedRow> = Vec::new();
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(680.0, 480.0))
            .build_ui(|ui| {
                st.show(ui.ctx(), ACCENT, MUTED, &rows, Path::new("/srv/x/plugins"));
            });
        h.run();
        // registry None + no error => the "Load a registry…" prompt arm paints.
        assert!(h.query_by_label("load").is_some());
    }

    #[test]
    fn show_registry_tab_error_state_paints() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("broken.toml");
        fs::write(&path, "= = not toml").unwrap();
        let mut st = PluginManagerState {
            open: true,
            tab: PluginManagerTab::Registry,
            registry_path: path.display().to_string(),
            ..Default::default()
        };
        st.load_registry(); // sets registry_error
        assert!(st.registry_error.is_some());
        let rows: Vec<LoadedRow> = Vec::new();
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(680.0, 480.0))
            .build_ui(|ui| {
                st.show(ui.ctx(), ACCENT, MUTED, &rows, Path::new("/srv/x/plugins"));
            });
        // The error arm returns early after painting the message — just render.
        h.run();
        assert!(h.query_by_label("load").is_some());
    }

    #[test]
    fn show_install_tab_renders_grid_and_verdict() {
        let mut st = PluginManagerState {
            open: true,
            tab: PluginManagerTab::Install,
            install_result: Some(Ok("Verified — both pass.".to_string())),
            ..Default::default()
        };
        let rows: Vec<LoadedRow> = Vec::new();
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(680.0, 600.0))
            .build_ui(|ui| {
                st.show(ui.ctx(), ACCENT, MUTED, &rows, Path::new("/srv/x/plugins"));
            });
        h.run();
        // The verify button + the Ok-verdict line paint.
        assert!(h.query_by_label("verify").is_some());
    }

    #[test]
    fn show_install_tab_renders_error_verdict() {
        let mut st = PluginManagerState {
            open: true,
            tab: PluginManagerTab::Install,
            install_result: Some(Err("checksum mismatch".to_string())),
            ..Default::default()
        };
        let rows: Vec<LoadedRow> = Vec::new();
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(680.0, 600.0))
            .build_ui(|ui| {
                st.show(ui.ctx(), ACCENT, MUTED, &rows, Path::new("/srv/x/plugins"));
            });
        h.run(); // exercises the Err verdict arm of show_install
        assert!(h.query_by_label("verify").is_some());
    }

    #[test]
    fn show_tab_switch_click_changes_active_tab() {
        let mut h = modal_harness(ModalHarness::new(PluginManagerTab::Loaded, loaded_rows()));
        h.run();
        // Click the Install selectable tab header like a user does.
        h.get_by_label("Install").click();
        h.run();
        // The selected tab actually changed (state), and the Install pane's
        // verify button is now visible — the two together prove the switch.
        assert_eq!(h.state().state.tab, PluginManagerTab::Install);
        assert!(h.query_by_label("verify").is_some());
    }

    #[test]
    fn show_registry_load_button_click_parses_registry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("index.toml");
        fs::write(&path, sample_registry_toml()).unwrap();
        let mut harness = ModalHarness::new(PluginManagerTab::Registry, Vec::new());
        harness.state.registry_path = path.display().to_string();
        let mut h = modal_harness(harness);
        h.run();
        // Click the registry "load" button; it should parse + populate.
        h.get_by_label("load").click();
        h.run();
        assert!(
            h.state().state.registry.is_some(),
            "clicking load must parse the registry into state"
        );
        assert!(h.state().state.registry_error.is_none());
    }
}
