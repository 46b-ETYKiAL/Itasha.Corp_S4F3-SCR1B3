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
    /// The latest release is the running version (or older).
    UpToDate,
    /// A newer release is available and ready to download.
    Available(ReleaseInfo),
    /// The asset is downloading (`received`/`total` bytes).
    Downloading { received: u64, total: u64 },
    /// A verified new binary has been staged; restart to finish.
    ReadyToApply { staged: PathBuf, version: String },
    /// The verified binary was swapped in; restart to run it.
    Applied { version: String },
    /// The last operation failed; `String` is a human-readable reason.
    Failed(String),
}

/// Cross-thread messages from a worker back to the UI thread.
enum UpdateMsg {
    CheckResult(Result<Option<ReleaseInfo>, String>),
    Progress { received: u64, total: u64 },
    Downloaded(Result<(PathBuf, String), String>),
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
        std::thread::spawn(move || {
            let staging = std::env::temp_dir().join("scr1b3-update");
            let _ = std::fs::remove_dir_all(&staging);
            let version = info.version.to_string();
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

    /// Swap the running executable for the staged, verified binary and best-
    /// effort relaunch. On success the caller should close the window.
    pub fn apply_and_restart(&mut self, ctx: &egui::Context) {
        let UpdateState::ReadyToApply { staged, version } = &self.state else {
            return;
        };
        let (staged, version) = (staged.clone(), version.clone());
        match update::apply::replace_running_executable(&staged) {
            Ok(()) => {
                // current_exe() now resolves to the freshly-swapped binary.
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).spawn();
                }
                self.state = UpdateState::Applied { version };
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
                Ok(UpdateMsg::CheckResult(Ok(Some(info)))) => {
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
                Ok(UpdateMsg::CheckResult(Ok(None))) => {
                    self.launch_kind = LaunchKind::Manual;
                    self.state = UpdateState::UpToDate;
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
