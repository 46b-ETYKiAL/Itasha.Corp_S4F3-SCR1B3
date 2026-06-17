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

/// The temp directory the updater downloads + stages release artifacts into
/// (`%TEMP%/scr1b3-update`). Centralized so the download, the post-install
/// cleanup, and the startup sweep all agree on the path.
fn staging_dir() -> PathBuf {
    std::env::temp_dir().join("scr1b3-update")
}

/// Delete the staging directory and everything in it — the downloaded archive /
/// installer plus its `.minisig` / `.sha256` sidecars and any extracted binary.
/// Best-effort: a missing dir or a locked file is not an error.
fn clean_staging_dir() {
    let _ = std::fs::remove_dir_all(staging_dir());
}

/// Startup housekeeping: remove update artifacts that are no longer needed once
/// the new version is running. Removes (1) the staging download directory — this
/// is where a completed installer's `setup.exe` lived, which could not be
/// deleted while it was executing, so it is reaped here on the next launch — and
/// (2) the `<exe>.bak` keep-one-prior backup beside the running executable: the
/// fact that THIS binary is running is proof the update succeeded, so the
/// rollback copy is no longer needed. Best-effort throughout (e.g. a
/// Program-Files backup the unelevated app can't delete is silently skipped).
/// Call once per launch.
pub fn cleanup_after_update() {
    clean_staging_dir();
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(update::apply::backup_path_for(&exe));
    }
}

/// Build the PowerShell `Start-Process -Verb RunAs` script that launches the
/// installer with a UAC elevation prompt. Pure (so it is unit-testable): the
/// path is single-quoted for PowerShell with any embedded single quote escaped
/// by doubling.
#[cfg(windows)]
fn powershell_runas_script(installer: &std::path::Path) -> String {
    let p = installer.to_string_lossy().replace('\'', "''");
    format!("Start-Process -FilePath '{p}' -Verb RunAs")
}

/// Launch the verified self-elevating installer WITH a UAC elevation prompt.
///
/// The `setup.exe` carries a `requireAdministrator` manifest. A plain
/// `Command::spawn` (CreateProcess) CANNOT start such a binary — Windows returns
/// ERROR_ELEVATION_REQUIRED (os error 740), i.e. "The requested operation
/// requires elevation". Elevation needs ShellExecute semantics, reached here
/// WITHOUT any `unsafe` (the app is `#![forbid(unsafe_code)]`) via PowerShell's
/// `Start-Process -Verb RunAs`, which raises the standard UAC prompt and runs
/// the installer elevated. `CREATE_NO_WINDOW` keeps the helper PowerShell from
/// flashing a console.
#[cfg(windows)]
fn launch_installer_elevated(installer: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = powershell_runas_script(installer);
    // Hand our foreground right to the about-to-spawn process tree BEFORE the
    // spawn (while we still own the foreground), so the elevated installer — a
    // grandchild via PowerShell + UAC — can bring its window to the FRONT instead
    // of flashing behind us. ASFW_ANY is required because the real installer's PID
    // is not the PowerShell child's. See `allow_foreground_handoff`.
    scribe_win32_chrome::allow_foreground_handoff();
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

/// Non-Windows fallback: installers are a Windows-only path, but keep the
/// updater buildable everywhere — a plain spawn is correct off Windows.
#[cfg(not(windows))]
fn launch_installer_elevated(installer: &std::path::Path) -> std::io::Result<()> {
    std::process::Command::new(installer).spawn().map(|_| ())
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
            let staging = staging_dir();
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
        // Anti-downgrade (TUF rollback-attack defense): re-check at APPLY time
        // that the staged release is strictly newer than the running build, so a
        // replayed older-but-validly-signed installer can never be run over us.
        // The in-place-swap path (`apply_and_restart`) already does this; the
        // installer path must too, or the two apply routes defend asymmetrically.
        if let Err(e) = update::ensure_upgrade(&version, current_version()) {
            self.state = UpdateState::Failed(e);
            return;
        }
        // Launch with a UAC elevation prompt — the setup.exe is
        // requireAdministrator, so a plain CreateProcess fails with os error 740.
        // The staging dir is NOT cleaned here: the installer is running FROM it;
        // it is reaped by `cleanup_after_update()` on the next launch.
        match launch_installer_elevated(&installer) {
            Ok(()) => {
                self.state = UpdateState::Applied { version };
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                self.state = UpdateState::Failed(format!(
                    "couldn't launch the installer ({e}). You can run it manually: {}",
                    installer.display()
                ));
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
                // Hand our foreground right to the relaunched binary first (while
                // we still own the foreground) so it comes to the FRONT rather
                // than opening behind us as the close is processed.
                #[cfg(windows)]
                scribe_win32_chrome::allow_foreground_handoff();
                match std::env::current_exe()
                    .and_then(|exe| std::process::Command::new(exe).spawn())
                {
                    Ok(_) => {
                        // The new binary is in place and relaunching — the
                        // downloaded archive + sidecars in the staging dir are no
                        // longer needed, so reap them now (the `.bak` is kept for
                        // rollback and is reaped by the relaunched build's
                        // startup cleanup once it confirms it runs).
                        clean_staging_dir();
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
    ///
    /// Messages are drained into a buffer FIRST (which releases the borrow on
    /// `rx`), then handled — so a handler can take `&mut self` to chain straight
    /// from a completed download into applying the update (the one-click flow).
    pub fn poll(&mut self, ctx: &egui::Context) {
        let mut msgs = Vec::new();
        let mut disconnect = false;
        if let Some(rx) = &self.rx {
            loop {
                match rx.try_recv() {
                    Ok(msg) => msgs.push(msg),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnect = true;
                        break;
                    }
                }
            }
        }
        if disconnect {
            self.rx = None;
        }
        for msg in msgs {
            self.handle_update_msg(msg, ctx);
        }
    }

    /// Apply one drained worker message to the state. A completed download chains
    /// straight into applying — ONE click ("Update now") downloads AND installs;
    /// the user never needs a second click. The intermediate `ReadyToApply` /
    /// `ReadyToRunInstaller` states are set then immediately consumed within the
    /// same `poll`, before the frame renders, so the UI advances seamlessly from
    /// the progress bar to the relaunch (or the installer's UAC prompt).
    fn handle_update_msg(&mut self, msg: UpdateMsg, ctx: &egui::Context) {
        match msg {
            UpdateMsg::CheckResult(Ok(update::UpdateOutcome::Available(info))) => {
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
            UpdateMsg::CheckResult(Ok(update::UpdateOutcome::UpToDate { latest })) => {
                self.launch_kind = LaunchKind::Manual;
                self.state = UpdateState::UpToDate {
                    latest: latest.to_string(),
                };
            }
            UpdateMsg::CheckResult(Ok(update::UpdateOutcome::NewerButNoAsset {
                latest,
                target,
                html_url,
            })) => {
                self.launch_kind = LaunchKind::Manual;
                self.state = UpdateState::NoAssetForPlatform {
                    latest: latest.to_string(),
                    target,
                    html_url,
                };
            }
            UpdateMsg::CheckResult(Err(e)) => {
                self.launch_kind = LaunchKind::Manual;
                self.state = UpdateState::Failed(e);
            }
            UpdateMsg::Progress { received, total } => {
                self.state = UpdateState::Downloading { received, total };
            }
            UpdateMsg::Downloaded(Ok((staged, version))) => {
                self.state = UpdateState::ReadyToApply { staged, version };
                self.apply_and_restart(ctx);
            }
            UpdateMsg::Downloaded(Err(e)) => {
                self.state = UpdateState::Failed(e);
            }
            UpdateMsg::InstallerReady(Ok((installer, version))) => {
                self.state = UpdateState::ReadyToRunInstaller { installer, version };
                self.run_installer(ctx);
            }
            UpdateMsg::InstallerReady(Err(e)) => {
                self.state = UpdateState::Failed(e);
            }
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

    #[test]
    fn staging_dir_is_under_temp_and_named() {
        let d = staging_dir();
        assert!(d.ends_with("scr1b3-update"), "got {d:?}");
        assert!(d.starts_with(std::env::temp_dir()), "got {d:?}");
    }

    #[cfg(windows)]
    #[test]
    fn powershell_runas_script_quotes_and_escapes_path() {
        use std::path::Path;
        // A plain path is single-quoted into a Start-Process -Verb RunAs command.
        assert_eq!(
            powershell_runas_script(Path::new(r"C:\tmp\scr1b3-setup.exe")),
            r"Start-Process -FilePath 'C:\tmp\scr1b3-setup.exe' -Verb RunAs"
        );
        // An embedded single quote is escaped by doubling (the PowerShell rule),
        // so a crafted path can never break out of the quoted string.
        assert_eq!(
            powershell_runas_script(Path::new(r"C:\o'brien\scr1b3-setup.exe")),
            r"Start-Process -FilePath 'C:\o''brien\scr1b3-setup.exe' -Verb RunAs"
        );
    }
}
