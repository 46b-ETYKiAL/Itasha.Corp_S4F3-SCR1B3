//! In-app self-updater UI orchestration.
//!
//! The network discovery, signature/checksum verification, archive extraction,
//! and binary swap all live in `scribe_core::update` (and are unit-tested
//! there). This module owns only the egui-thread-friendly orchestration: each
//! operation runs on a `std::thread`, communicates back over an `mpsc` channel,
//! and calls `ctx.request_repaint()` so the UI wakes to drain it. The Settings
//! "Updates" pane renders [`UpdateState`]; the on-launch (Auto) path drives a
//! yes/no modal.
//!
//! Privacy: the ONLY network the updater performs is a single HTTPS GET to the
//! public GitHub Releases API (plus the asset/sig/sha downloads when the user
//! chooses to install). No identifiers, no analytics — see PRIVACY.md.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use scribe_core::update::{self, ReleaseInfo};

/// GitHub repo coordinates for the Releases API. Public values.
pub const UPDATE_OWNER: &str = "46b-ETYKiAL";
pub const UPDATE_REPO: &str = "Itasha.Corp_S4F3-SCR1B3";

/// This build's Rust target triple, baked by `build.rs` (`SCR1B3_TARGET`), used
/// to pick the matching `scr1b3-<target>.tar.gz` release asset. Falls back to an
/// empty string if the build script did not run (no asset will match → the
/// updater reports "no update for this platform" rather than misbehaving).
pub const BUILD_TARGET: &str = match option_env!("SCR1B3_TARGET") {
    Some(t) => t,
    None => "",
};

/// The running app version (compile-time, authoritative).
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Can we write into `dir`? Probes by creating + deleting a temp file. Used to
/// decide whether an in-place self-replace is even possible: an install under
/// `C:\Program Files` (the default installer location) is owned by admin, so
/// the swap would fail with a cryptic "Access is denied" — we detect that up
/// front and give an actionable message (run the self-elevating installer)
/// instead.
fn dir_writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(".scr1b3-write-probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Is the directory holding the running executable writable (so an in-place
/// self-replace can succeed)? Unknown (`current_exe()` fails) → assume yes and
/// let the swap attempt surface any real error.
fn running_exe_dir_writable() -> bool {
    match std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()))
    {
        Some(dir) => dir_writable(&dir),
        None => true,
    }
}

/// Why a check was started — decides what a found update does on completion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LaunchKind {
    /// User pressed "Check for updates" — show inline state only.
    #[default]
    Manual,
    /// Auto on-launch (`UpdateMode::Notify`) — surface a passive toast.
    Notify,
    /// Auto on-launch (`UpdateMode::Auto`) — open the yes/no modal.
    Auto,
}

/// What the updater is doing right now. Rendered by the Settings Updates pane.
#[derive(Clone, Debug, Default)]
pub enum UpdateState {
    /// Nothing in flight; no result yet.
    #[default]
    Idle,
    /// A version check is running.
    Checking,
    /// The newest published release is the running version (or older). `latest`
    /// is the highest semver seen, shown next to the current version so "up to
    /// date" is never ambiguous.
    UpToDate { latest: String },
    /// A newer release is available and ready to download.
    Available(ReleaseInfo),
    /// A newer release exists but ships no asset for this build's platform —
    /// the user is pointed at the release page rather than told "up to date".
    NoAssetForPlatform {
        latest: String,
        target: String,
        html_url: String,
    },
    /// The asset is downloading (`received`/`total` bytes).
    Downloading { received: u64, total: u64 },
    /// A verified new binary has been staged; restart to finish (in-place swap;
    /// the writable-install path).
    ReadyToApply { staged: PathBuf, version: String },
    /// The install dir is admin-owned (Program Files) so an in-place swap can't
    /// write it — a verified self-elevating installer has been staged instead;
    /// running it updates in place (prompts for admin).
    ReadyToRunInstaller { installer: PathBuf, version: String },
    /// The verified binary was swapped in; restart to run it.
    Applied { version: String },
    /// The last operation failed; `String` is a human-readable reason.
    Failed(String),
}

/// Cross-thread messages from a worker back to the UI thread.
enum UpdateMsg {
    CheckResult(Result<update::UpdateOutcome, String>),
    Progress { received: u64, total: u64 },
    Downloaded(Result<(PathBuf, String), String>),
    InstallerReady(Result<(PathBuf, String), String>),
}

/// UI-thread updater model: a polled [`UpdateState`] plus the channel to the
/// current worker.
#[derive(Default)]
pub struct Updater {
    pub state: UpdateState,
    rx: Option<Receiver<UpdateMsg>>,
    /// Why the in-flight check was started (decides toast vs. modal on success).
    launch_kind: LaunchKind,
    /// Drives the on-launch (`Auto`) yes/no modal; the host renders it while set.
    pub show_prompt: bool,
    /// Set when a `Notify` launch check finds an update — the host shows a one-
    /// shot toast and clears it.
    pub toast_pending: Option<String>,
    /// A version the user declined this session — don't re-prompt for it.
    pub skipped_version: Option<String>,
}

impl Updater {
    /// True while a network/apply operation is in flight (used to disable the
    /// "Check for updates" button so a second click can't spawn a second job).
    pub fn is_busy(&self) -> bool {
        matches!(
            self.state,
            UpdateState::Checking | UpdateState::Downloading { .. }
        )
    }

    /// Spawn a background version check. `kind` decides what a found update does
    /// on completion: [`LaunchKind::Auto`] opens the modal, [`LaunchKind::Notify`]
    /// queues a toast, [`LaunchKind::Manual`] (the button) shows inline state only.
    pub fn start_check(&mut self, ctx: &egui::Context, kind: LaunchKind) {
        if self.is_busy() {
            return;
        }
        self.state = UpdateState::Checking;
        self.launch_kind = kind;
        let (tx, rx) = std::sync::mpsc::channel();
        self.rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = match semver::Version::parse(current_version()) {
                Ok(current) => {
                    update::check_for_update(UPDATE_OWNER, UPDATE_REPO, &current, BUILD_TARGET)
                }
                Err(e) => Err(format!("internal: bad current version: {e}")),
            };
            let _ = tx.send(UpdateMsg::CheckResult(result));
            ctx.request_repaint();
        });
    }

    /// Spawn the download + verify + extract worker for a chosen release.
    pub fn start_download(&mut self, ctx: &egui::Context, info: ReleaseInfo) {
        if self.is_busy() {
            return;
        }
        // NOTE: do not clear `show_prompt` here — the on-launch (Auto) modal
        // stays open and follows the download → ready → restart states.
        self.state = UpdateState::Downloading {
            received: 0,
            total: 0,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.rx = Some(rx);
        let ctx = ctx.clone();

        // Pick the apply strategy by whether the install dir is writable:
        //  • writable (portable / per-user) → download the tar.gz, swap in place.
        //  • admin-owned (Program Files) WITH a self-elevating installer →
        //    download the verified setup.exe and run it (prompts for admin).
        //  • admin-owned WITHOUT an installer → an actionable failure.
        let use_installer = !running_exe_dir_writable();
        let installer = info.installer.clone();

        std::thread::spawn(move || {
            let staging = std::env::temp_dir().join("scr1b3-update");
            let _ = std::fs::remove_dir_all(&staging);
            let version = info.version.to_string();

            if use_installer {
                match installer {
                    Some(inst) => {
                        let ptx = tx.clone();
                        let pctx = ctx.clone();
                        let result = std::fs::create_dir_all(&staging)
                            .map_err(|e| format!("cannot create staging dir: {e}"))
                            .and_then(|()| {
                                update::download_verify_installer(
                                    &inst,
                                    &staging,
                                    move |received, total| {
                                        let _ = ptx.send(UpdateMsg::Progress { received, total });
                                        pctx.request_repaint();
                                    },
                                )
                            })
                            .map(|path| (path, version));
                        let _ = tx.send(UpdateMsg::InstallerReady(result));
                    }
                    None => {
                        let _ = tx.send(UpdateMsg::InstallerReady(Err(format!(
                            "v{version} can't be installed in place — SCR1B3 is in a \
                             protected location (e.g. Program Files) and this release \
                             has no installer for your platform. Download it from the \
                             releases page and run it as administrator."
                        ))));
                    }
                }
                ctx.request_repaint();
                return;
            }

            let result = match std::fs::create_dir_all(&staging) {
                Ok(()) => {
                    let ptx = tx.clone();
                    let pctx = ctx.clone();
                    update::download_verify_extract(&info, &staging, move |received, total| {
                        let _ = ptx.send(UpdateMsg::Progress { received, total });
                        pctx.request_repaint();
                    })
                    .map(|path| (path, version))
                }
                Err(e) => Err(format!("cannot create staging dir: {e}")),
            };
            let _ = tx.send(UpdateMsg::Downloaded(result));
            ctx.request_repaint();
        });
    }

    /// Launch the staged, verified self-elevating installer and close the app so
    /// it can replace the files in place (the installer requests UAC).
    pub fn run_installer(&mut self, ctx: &egui::Context) {
        let UpdateState::ReadyToRunInstaller { installer, version } = &self.state else {
            return;
        };
        let (installer, version) = (installer.clone(), version.clone());
        match std::process::Command::new(&installer).spawn() {
            Ok(_) => {
                self.state = UpdateState::Applied { version };
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                self.state = UpdateState::Failed(format!("couldn't launch the installer: {e}"));
            }
        }
    }

    /// Swap the running executable for the staged, verified binary and best-
    /// effort relaunch. On success the caller should close the window.
    pub fn apply_and_restart(&mut self, ctx: &egui::Context) {
        let UpdateState::ReadyToApply { staged, version } = &self.state else {
            return;
        };
        let (staged, version) = (staged.clone(), version.clone());
        // Anti-downgrade (TUF rollback-attack defense): re-check at APPLY time,
        // not only at selection, that the staged release is strictly newer than
        // the running build — so a tampered or replayed older-but-validly-signed
        // artifact can never be installed over us.
        if let Err(e) = update::ensure_upgrade(&version, current_version()) {
            self.state = UpdateState::Failed(e);
            return;
        }
        // ReadyToApply is only ever reached on a WRITABLE install (start_download
        // routes an admin-owned install to the installer path instead), so an
        // in-place swap is the right action here. The swap first snapshots the
        // prior binary to a `.bak` so a failed relaunch can roll back to it.
        match update::apply::replace_running_executable(&staged) {
            Ok(backup) => {
                // current_exe() now resolves to the freshly-swapped binary;
                // relaunch it. Do NOT discard the spawn result — if the relaunch
                // fails we must not close the app into nothing (the prior bug).
                match std::env::current_exe()
                    .and_then(|exe| std::process::Command::new(exe).spawn())
                {
                    Ok(_) => {
                        self.state = UpdateState::Applied { version };
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    Err(e) => {
                        // The verified update was installed but wouldn't start.
                        // Revert to the known-good prior binary and keep THIS
                        // process running, so the user is never left windowless.
                        let reverted = update::apply::rollback_running_executable(&backup).is_ok();
                        self.state = UpdateState::Failed(if reverted {
                            format!(
                                "v{version} was installed but couldn't be started ({e}); \
                                 reverted to the current version — please restart SCR1B3 to retry."
                            )
                        } else {
                            format!(
                                "v{version} was installed but couldn't be started ({e}), and the \
                                 automatic revert also failed — reinstall from the releases page."
                            )
                        });
                    }
                }
            }
            Err(e) => self.state = UpdateState::Failed(format!("install failed: {e}")),
        }
    }

    /// Drain worker messages and advance the state. Call once per frame.
    pub fn poll(&mut self) {
        let Some(rx) = &self.rx else {
            return;
        };
        let mut disconnect = false;
        loop {
            match rx.try_recv() {
                Ok(UpdateMsg::CheckResult(Ok(update::UpdateOutcome::Available(info)))) => {
                    let v = info.version.to_string();
                    let already_skipped = self.skipped_version.as_deref() == Some(v.as_str());
                    if !already_skipped {
                        match self.launch_kind {
                            LaunchKind::Auto => self.show_prompt = true,
                            LaunchKind::Notify => self.toast_pending = Some(v),
                            LaunchKind::Manual => {}
                        }
                    }
                    self.launch_kind = LaunchKind::Manual;
                    self.state = UpdateState::Available(info);
                }
                Ok(UpdateMsg::CheckResult(Ok(update::UpdateOutcome::UpToDate { latest }))) => {
                    self.launch_kind = LaunchKind::Manual;
                    self.state = UpdateState::UpToDate {
                        latest: latest.to_string(),
                    };
                }
                Ok(UpdateMsg::CheckResult(Ok(update::UpdateOutcome::NewerButNoAsset {
                    latest,
                    target,
                    html_url,
                }))) => {
                    self.launch_kind = LaunchKind::Manual;
                    self.state = UpdateState::NoAssetForPlatform {
                        latest: latest.to_string(),
                        target,
                        html_url,
                    };
                }
                Ok(UpdateMsg::CheckResult(Err(e))) => {
                    self.launch_kind = LaunchKind::Manual;
                    self.state = UpdateState::Failed(e);
                }
                Ok(UpdateMsg::Progress { received, total }) => {
                    self.state = UpdateState::Downloading { received, total };
                }
                Ok(UpdateMsg::Downloaded(Ok((staged, version)))) => {
                    self.state = UpdateState::ReadyToApply { staged, version };
                }
                Ok(UpdateMsg::Downloaded(Err(e))) => {
                    self.state = UpdateState::Failed(e);
                }
                Ok(UpdateMsg::InstallerReady(Ok((installer, version)))) => {
                    self.state = UpdateState::ReadyToRunInstaller { installer, version };
                }
                Ok(UpdateMsg::InstallerReady(Err(e))) => {
                    self.state = UpdateState::Failed(e);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    disconnect = true;
                    break;
                }
            }
        }
        if disconnect {
            self.rx = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_target_is_baked_or_empty() {
        // build.rs bakes SCR1B3_TARGET; under `cargo test` it is present. Either
        // way the constant resolves (never panics) — that is the contract.
        let _ = BUILD_TARGET;
    }

    #[test]
    fn dir_writable_true_for_tempdir_false_for_missing() {
        let tmp = std::env::temp_dir();
        assert!(dir_writable(&tmp), "the OS temp dir must be writable");
        let missing = tmp
            .join("scr1b3-definitely-not-a-real-dir-xyzzy")
            .join("nested");
        assert!(
            !dir_writable(&missing),
            "a non-existent nested dir must probe as not-writable"
        );
    }

    #[test]
    fn current_version_parses_as_semver() {
        assert!(semver::Version::parse(current_version()).is_ok());
    }

    #[test]
    fn idle_updater_is_not_busy() {
        let u = Updater::default();
        assert!(!u.is_busy());
        assert!(matches!(u.state, UpdateState::Idle));
        assert!(!u.show_prompt);
    }
}
