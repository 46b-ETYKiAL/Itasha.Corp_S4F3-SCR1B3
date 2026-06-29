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
        tracing::info!("update check started (current v{})", current_version());
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
            // TUF anti-rollback refusal. Log version strings ONLY — never the
            // signature or any key material.
            tracing::warn!(
                "anti-rollback: refused installer for non-newer release \
                 (attempted v{version}, current v{})",
                current_version()
            );
            self.state = UpdateState::Failed(e);
            return;
        }
        // Launch with a UAC elevation prompt — the setup.exe is
        // requireAdministrator, so a plain CreateProcess fails with os error 740.
        // The staging dir is NOT cleaned here: the installer is running FROM it;
        // it is reaped by `cleanup_after_update()` on the next launch.
        match launch_installer_elevated(&installer) {
            Ok(()) => {
                tracing::info!(
                    "update applied: installer for v{version} launched; closing to apply"
                );
                self.state = UpdateState::Applied { version };
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                tracing::error!(
                    "failed to launch staged installer for v{version} ({:?})",
                    e.kind()
                );
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
            // TUF anti-rollback refusal on the in-place-swap route. Log version
            // strings ONLY — never the signature or any key material.
            tracing::warn!(
                "anti-rollback: refused in-place apply for non-newer release \
                 (attempted v{version}, current v{})",
                current_version()
            );
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
                        tracing::info!("update applied: v{version} swapped in and relaunching");
                        self.state = UpdateState::Applied { version };
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    Err(e) => {
                        // The verified update was installed but wouldn't start.
                        // Revert to the known-good prior binary and keep THIS
                        // process running, so the user is never left windowless.
                        let reverted = update::apply::rollback_running_executable(&backup).is_ok();
                        if reverted {
                            tracing::warn!(
                                "update v{version} installed but failed to relaunch ({:?}); \
                                 reverted to the prior version",
                                e.kind()
                            );
                        } else {
                            // Worst case: a half-updated install — the swap took
                            // but neither the new binary nor the revert works.
                            tracing::error!(
                                "update v{version} installed but failed to relaunch ({:?}) AND the \
                                 automatic revert ALSO failed — install is half-updated",
                                e.kind()
                            );
                        }
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
            Err(e) => {
                tracing::error!("in-place install (executable swap) failed: {:?}", e.kind());
                self.state = UpdateState::Failed(format!("install failed: {e}"));
            }
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
                tracing::info!("update available: v{v}");
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
                // Operational (network / API / parse) check failure — the app's
                // own error string is a human summary, no token (the check is an
                // unauthenticated GET) and no signature.
                tracing::warn!("update check failed: {e}");
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
                // The download path's failure is dominated by the fail-closed
                // signature+checksum verification. Log the KIND only — never the
                // raw error string (which could echo signature/key text) — so a
                // tampered or replayed artifact leaves an operator-visible trail.
                tracing::error!("update download failed to verify (checksum/signature) or stage");
                self.state = UpdateState::Failed(e);
            }
            UpdateMsg::InstallerReady(Ok((installer, version))) => {
                self.state = UpdateState::ReadyToRunInstaller { installer, version };
                self.run_installer(ctx);
            }
            UpdateMsg::InstallerReady(Err(e)) => {
                // Same fail-closed verification as the archive path; log KIND only.
                tracing::error!(
                    "installer download failed to verify (checksum/signature) or stage"
                );
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

    // ----------------------------------------------------------------------
    // The update-decision reducer: handle_update_msg + state-transition logic.
    //
    // The on-launch routing here is the surface of the prior "Notify-default +
    // update-mode" bug — `LaunchKind` decides whether a found update opens the
    // yes/no modal (Auto), queues a passive toast (Notify), or shows inline
    // state only (Manual). These tests pin every arm so a refactor can never
    // again silently route a Notify launch to a modal (or vice-versa), and so
    // the per-session "I declined this version" suppression (`skipped_version`)
    // can never regress into a re-prompt loop.
    //
    // `egui::Context::default()` is headless (no GPU, no window) — the same
    // construction the e2e/kittest suites use — so `request_repaint` /
    // `send_viewport_cmd` are inert no-ops and the reducer is fully testable.
    // ----------------------------------------------------------------------

    /// A minimal `ReleaseInfo` fixture at `version` with no installer — enough
    /// to drive the reducer's `Available` arm without touching the network.
    fn fake_release(version: &str) -> ReleaseInfo {
        ReleaseInfo {
            version: semver::Version::parse(version).unwrap(),
            tag: format!("v{version}"),
            asset_url: "https://dl/scr1b3.tar.gz".to_string(),
            sig_url: "https://dl/scr1b3.tar.gz.minisig".to_string(),
            sha_url: "https://dl/scr1b3.tar.gz.sha256".to_string(),
            html_url: "https://github.com/o/r/releases/tag/x".to_string(),
            installer: None,
        }
    }

    /// Feed `msg` to the reducer with a fresh `Updater` whose `launch_kind` is
    /// pre-set to `kind` (mimicking the state a real `start_check(kind)` leaves
    /// behind), and return the mutated updater for assertions.
    fn reduce_with_kind(kind: LaunchKind, msg: UpdateMsg) -> Updater {
        let ctx = egui::Context::default();
        let mut u = Updater {
            launch_kind: kind,
            ..Default::default()
        };
        u.handle_update_msg(msg, &ctx);
        u
    }

    fn available_msg(version: &str) -> UpdateMsg {
        UpdateMsg::CheckResult(Ok(update::UpdateOutcome::Available(fake_release(version))))
    }

    #[test]
    fn available_under_auto_opens_modal_not_toast() {
        let u = reduce_with_kind(LaunchKind::Auto, available_msg("9.9.9"));
        assert!(u.show_prompt, "Auto launch must open the yes/no modal");
        assert!(
            u.toast_pending.is_none(),
            "Auto must NOT also queue a toast"
        );
        assert!(matches!(u.state, UpdateState::Available(_)));
        // launch_kind is reset to Manual after consuming the result so a later
        // manual "Check for updates" press shows inline state only.
        assert_eq!(u.launch_kind, LaunchKind::Manual);
    }

    #[test]
    fn available_under_notify_queues_toast_not_modal() {
        let u = reduce_with_kind(LaunchKind::Notify, available_msg("9.9.9"));
        assert_eq!(
            u.toast_pending.as_deref(),
            Some("9.9.9"),
            "Notify launch must queue a passive toast carrying the version"
        );
        assert!(
            !u.show_prompt,
            "Notify must NOT open the modal — that was the prior bug"
        );
        assert!(matches!(u.state, UpdateState::Available(_)));
        assert_eq!(u.launch_kind, LaunchKind::Manual);
    }

    #[test]
    fn available_under_manual_shows_inline_only() {
        let u = reduce_with_kind(LaunchKind::Manual, available_msg("9.9.9"));
        assert!(!u.show_prompt, "Manual must not open the modal");
        assert!(u.toast_pending.is_none(), "Manual must not queue a toast");
        assert!(matches!(u.state, UpdateState::Available(_)));
    }

    #[test]
    fn skipped_version_suppresses_auto_modal() {
        // The user declined v9.9.9 earlier this session; a re-check that finds
        // the SAME version must not re-open the modal (no re-prompt loop), even
        // though the state still advances to Available so the inline pane shows.
        let ctx = egui::Context::default();
        let mut u = Updater {
            launch_kind: LaunchKind::Auto,
            skipped_version: Some("9.9.9".to_string()),
            ..Default::default()
        };
        u.handle_update_msg(available_msg("9.9.9"), &ctx);
        assert!(
            !u.show_prompt,
            "a skipped version must never re-open the modal"
        );
        assert!(matches!(u.state, UpdateState::Available(_)));
    }

    #[test]
    fn skipped_version_suppresses_notify_toast() {
        let ctx = egui::Context::default();
        let mut u = Updater {
            launch_kind: LaunchKind::Notify,
            skipped_version: Some("9.9.9".to_string()),
            ..Default::default()
        };
        u.handle_update_msg(available_msg("9.9.9"), &ctx);
        assert!(
            u.toast_pending.is_none(),
            "a skipped version must never re-queue the toast"
        );
        assert!(matches!(u.state, UpdateState::Available(_)));
    }

    #[test]
    fn skipped_version_does_not_suppress_a_different_version() {
        // Declined v9.9.9, but a NEWER v10.0.0 appears — the prompt MUST fire
        // (the suppression is version-specific, not a blanket mute).
        let ctx = egui::Context::default();
        let mut u = Updater {
            launch_kind: LaunchKind::Auto,
            skipped_version: Some("9.9.9".to_string()),
            ..Default::default()
        };
        u.handle_update_msg(available_msg("10.0.0"), &ctx);
        assert!(
            u.show_prompt,
            "a different (newer) version must still open the modal"
        );
    }

    #[test]
    fn up_to_date_sets_state_and_resets_kind() {
        let ctx = egui::Context::default();
        let mut u = Updater {
            launch_kind: LaunchKind::Auto,
            ..Default::default()
        };
        u.handle_update_msg(
            UpdateMsg::CheckResult(Ok(update::UpdateOutcome::UpToDate {
                latest: semver::Version::parse("1.2.3").unwrap(),
            })),
            &ctx,
        );
        match &u.state {
            UpdateState::UpToDate { latest } => assert_eq!(latest, "1.2.3"),
            other => panic!("expected UpToDate, got {other:?}"),
        }
        assert!(!u.show_prompt);
        assert_eq!(u.launch_kind, LaunchKind::Manual);
    }

    #[test]
    fn newer_but_no_asset_maps_to_platform_state() {
        let ctx = egui::Context::default();
        let mut u = Updater::default();
        u.handle_update_msg(
            UpdateMsg::CheckResult(Ok(update::UpdateOutcome::NewerButNoAsset {
                latest: semver::Version::parse("2.0.0").unwrap(),
                target: "x86_64-pc-windows-msvc".to_string(),
                html_url: "https://github.com/o/r/releases".to_string(),
            })),
            &ctx,
        );
        match &u.state {
            UpdateState::NoAssetForPlatform {
                latest,
                target,
                html_url,
            } => {
                assert_eq!(latest, "2.0.0");
                assert_eq!(target, "x86_64-pc-windows-msvc");
                assert_eq!(html_url, "https://github.com/o/r/releases");
            }
            other => panic!("expected NoAssetForPlatform, got {other:?}"),
        }
        assert!(
            !u.show_prompt,
            "a no-asset result must never open the modal"
        );
    }

    #[test]
    fn check_error_maps_to_failed_state() {
        let ctx = egui::Context::default();
        let mut u = Updater {
            launch_kind: LaunchKind::Auto,
            ..Default::default()
        };
        u.handle_update_msg(
            UpdateMsg::CheckResult(Err("rate limited".to_string())),
            &ctx,
        );
        match &u.state {
            UpdateState::Failed(e) => assert_eq!(e, "rate limited"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(!u.show_prompt);
        assert_eq!(u.launch_kind, LaunchKind::Manual);
    }

    #[test]
    fn progress_message_advances_downloading_bytes() {
        let ctx = egui::Context::default();
        let mut u = Updater::default();
        u.handle_update_msg(
            UpdateMsg::Progress {
                received: 512,
                total: 2048,
            },
            &ctx,
        );
        match u.state {
            UpdateState::Downloading { received, total } => {
                assert_eq!(received, 512);
                assert_eq!(total, 2048);
            }
            other => panic!("expected Downloading, got {other:?}"),
        }
    }

    #[test]
    fn download_error_maps_to_failed() {
        let ctx = egui::Context::default();
        let mut u = Updater::default();
        u.handle_update_msg(
            UpdateMsg::Downloaded(Err("verify failed".to_string())),
            &ctx,
        );
        assert!(matches!(u.state, UpdateState::Failed(e) if e == "verify failed"));
    }

    #[test]
    fn installer_ready_error_maps_to_failed() {
        let ctx = egui::Context::default();
        let mut u = Updater::default();
        u.handle_update_msg(
            UpdateMsg::InstallerReady(Err("no installer for platform".to_string())),
            &ctx,
        );
        assert!(matches!(u.state, UpdateState::Failed(e) if e == "no installer for platform"));
    }

    #[test]
    fn is_busy_only_for_in_flight_states() {
        let mut u = Updater::default();
        // Idle, Available, terminal states are NOT busy.
        assert!(!u.is_busy());
        u.state = UpdateState::Available(fake_release("9.9.9"));
        assert!(!u.is_busy());
        u.state = UpdateState::UpToDate {
            latest: "1.0.0".to_string(),
        };
        assert!(!u.is_busy());
        u.state = UpdateState::Applied {
            version: "9.9.9".to_string(),
        };
        assert!(!u.is_busy());
        u.state = UpdateState::Failed("x".to_string());
        assert!(!u.is_busy());
        // Checking + Downloading ARE busy (so a second click can't double-spawn).
        u.state = UpdateState::Checking;
        assert!(u.is_busy());
        u.state = UpdateState::Downloading {
            received: 1,
            total: 2,
        };
        assert!(u.is_busy());
    }

    #[test]
    fn run_installer_is_noop_when_not_in_installer_ready_state() {
        // Guard arm: calling run_installer from a non-ReadyToRunInstaller state
        // must change nothing (the `let-else` early return).
        let ctx = egui::Context::default();
        let mut u = Updater {
            state: UpdateState::Idle,
            ..Default::default()
        };
        u.run_installer(&ctx);
        assert!(matches!(u.state, UpdateState::Idle));
    }

    #[test]
    fn apply_and_restart_is_noop_when_not_ready_to_apply() {
        let ctx = egui::Context::default();
        let mut u = Updater {
            state: UpdateState::Checking,
            ..Default::default()
        };
        u.apply_and_restart(&ctx);
        assert!(matches!(u.state, UpdateState::Checking));
    }

    #[test]
    fn run_installer_refuses_downgrade_at_apply_time() {
        // Anti-downgrade guard: a staged installer whose version is NOT strictly
        // newer than the running build is refused at apply time (TUF rollback
        // defense) and lands in Failed, never launching anything.
        let ctx = egui::Context::default();
        let mut u = Updater {
            state: UpdateState::ReadyToRunInstaller {
                installer: std::path::PathBuf::from("/nonexistent/scr1b3-setup.exe"),
                version: "0.0.1".to_string(), // older than the running build
            },
            ..Default::default()
        };
        u.run_installer(&ctx);
        match &u.state {
            UpdateState::Failed(e) => assert!(
                e.contains("downgrade") || e.contains("not newer"),
                "expected a downgrade-protection failure, got: {e}"
            ),
            other => panic!("expected Failed(downgrade), got {other:?}"),
        }
    }

    #[test]
    fn apply_and_restart_refuses_downgrade_at_apply_time() {
        let ctx = egui::Context::default();
        let mut u = Updater {
            state: UpdateState::ReadyToApply {
                staged: std::path::PathBuf::from("/nonexistent/scr1b3"),
                version: "0.0.1".to_string(),
            },
            ..Default::default()
        };
        u.apply_and_restart(&ctx);
        match &u.state {
            UpdateState::Failed(e) => assert!(
                e.contains("downgrade") || e.contains("not newer"),
                "expected a downgrade-protection failure, got: {e}"
            ),
            other => panic!("expected Failed(downgrade), got {other:?}"),
        }
    }

    #[test]
    fn poll_with_no_channel_is_inert() {
        // poll() on an updater that never started a worker (rx is None) must not
        // panic and must leave the state untouched.
        let ctx = egui::Context::default();
        let mut u = Updater::default();
        u.poll(&ctx);
        assert!(matches!(u.state, UpdateState::Idle));
    }

    #[test]
    fn poll_drains_a_queued_message_and_advances_state() {
        // Inject a channel directly (as start_check would), send a CheckResult,
        // and confirm poll() drains it into the state — exercising the
        // try_recv loop + the buffer-then-handle path without spawning a thread.
        let ctx = egui::Context::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut u = Updater {
            launch_kind: LaunchKind::Notify,
            rx: Some(rx),
            ..Default::default()
        };
        tx.send(available_msg("9.9.9")).unwrap();
        u.poll(&ctx);
        assert!(matches!(u.state, UpdateState::Available(_)));
        assert_eq!(u.toast_pending.as_deref(), Some("9.9.9"));
    }

    #[test]
    fn poll_clears_rx_on_sender_disconnect() {
        // When every sender is dropped, try_recv yields Disconnected and poll()
        // releases the receiver so a stale channel is not polled forever.
        let ctx = egui::Context::default();
        let (tx, rx) = std::sync::mpsc::channel::<UpdateMsg>();
        let mut u = Updater {
            rx: Some(rx),
            ..Default::default()
        };
        drop(tx); // disconnect with no pending messages
        u.poll(&ctx);
        assert!(
            u.rx.is_none(),
            "a disconnected channel must be released so poll stops draining it"
        );
    }

    #[test]
    fn default_updater_starts_idle_with_no_pending_signals() {
        let u = Updater::default();
        assert!(matches!(u.state, UpdateState::Idle));
        assert!(!u.show_prompt);
        assert!(u.toast_pending.is_none());
        assert!(u.skipped_version.is_none());
        assert_eq!(u.launch_kind, LaunchKind::Manual);
    }

    #[test]
    fn launch_kind_default_is_manual() {
        assert_eq!(LaunchKind::default(), LaunchKind::Manual);
    }

    #[test]
    fn running_exe_dir_writable_resolves_without_panicking() {
        // Probes the directory holding the test runner binary. The result is
        // environment-dependent (true on a writable target dir, possibly false
        // on a locked install), so we assert only that it RESOLVES to a bool
        // without panicking — exercising the current_exe() → parent → probe path
        // and its unknown→assume-yes fallback.
        let _ = running_exe_dir_writable();
    }

    #[test]
    fn cleanup_after_update_is_idempotent_and_never_errors() {
        // Best-effort housekeeping: removing a (possibly absent) staging dir and
        // a (possibly absent) `.bak` must be a silent no-op when there is nothing
        // to remove, and must be safe to call repeatedly. It returns () and must
        // never panic regardless of what is or isn't on disk.
        cleanup_after_update();
        cleanup_after_update();
    }

    #[test]
    fn downloaded_ok_chains_into_apply_and_surfaces_an_install_failure() {
        // A successful download drives the one-click flow: the reducer sets
        // ReadyToApply then immediately calls apply_and_restart within the same
        // poll. With a NEWER version (so the anti-downgrade gate passes) but a
        // nonexistent staged binary, the in-place swap fails BEFORE any process
        // spawn — landing in Failed("install failed: …"), never a fake Applied.
        let ctx = egui::Context::default();
        let mut u = Updater::default();
        u.handle_update_msg(
            UpdateMsg::Downloaded(Ok((
                std::path::PathBuf::from("/nonexistent/scr1b3-staged-binary"),
                "9.9.9".to_string(), // newer than the running build → passes the gate
            ))),
            &ctx,
        );
        match &u.state {
            UpdateState::Failed(e) => assert!(
                e.contains("install failed"),
                "expected an install-failure (the staged binary does not exist), got: {e}"
            ),
            other => panic!("expected Failed(install failed), got {other:?}"),
        }
    }

    // ----------------------------------------------------------------------
    // Structured-logging assertions. The crate's test-only `log_capture`
    // helper installs an in-process subscriber for the duration of a closure
    // and records each event's (level, message). NOTE: the capture layer only
    // records the `message` field (not structured key/value fields), so the
    // logs under test deliberately inline their tokens + (non-secret) version
    // strings into the message. Security paths log version + error KIND only —
    // never a signature, key, or token — and these tests pin that.
    // ----------------------------------------------------------------------

    /// Heuristic secret-leak tripwire: a >=32-char run of base64/hex/minisign
    /// alphabet characters looks like signature/key material and must NEVER
    /// appear in a log message. Version strings + `io::ErrorKind` debug never
    /// produce such a run, so this is a safe over-approximation for the tests.
    fn looks_like_secret(s: &str) -> bool {
        if s.contains("minisig") || s.contains("untrusted comment") {
            return true;
        }
        let mut run = 0usize;
        for ch in s.chars() {
            if ch.is_ascii_alphanumeric() || ch == '+' || ch == '/' || ch == '=' {
                run += 1;
                if run >= 32 {
                    return true;
                }
            } else {
                run = 0;
            }
        }
        false
    }

    /// No captured message may look like signature/key material — asserted on
    /// every security-path logging test.
    fn assert_no_secret_logged(logs: &crate::log_capture::Captured) {
        for (lvl, msg) in logs.events() {
            assert!(
                !looks_like_secret(&msg),
                "a {lvl:?} log message looks like it leaked secret/signature bytes: {msg:?}"
            );
        }
    }

    #[test]
    fn download_verify_error_logs_error_without_secret() {
        // The `Downloaded(Err)` transition is dominated by the fail-closed
        // signature+checksum verification. It must emit an ERROR carrying
        // "verify", and must NOT echo the raw error (which could contain
        // signature/key text).
        let (u, logs) = crate::log_capture::capture(|| {
            let ctx = egui::Context::default();
            let mut u = Updater::default();
            u.handle_update_msg(
                // A realistic raw verify error that embeds signature-shaped text;
                // the log must summarize, never echo it.
                UpdateMsg::Downloaded(Err(
                    "bad signature: RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0STRBdvF_zXBXR"
                        .to_string(),
                )),
                &ctx,
            );
            u
        });
        assert!(matches!(u.state, UpdateState::Failed(_)));
        assert!(
            logs.has(tracing::Level::ERROR, "verify"),
            "expected an ERROR mentioning verify; got {:?}",
            logs.events()
        );
        // The raw signature-shaped error string must NOT have been logged.
        assert!(
            !logs.any("RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0STRBdvF_zXBXR"),
            "the raw (signature-shaped) error must never be logged: {:?}",
            logs.events()
        );
        assert_no_secret_logged(&logs);
    }

    #[test]
    fn installer_verify_error_logs_error_without_secret() {
        let (u, logs) = crate::log_capture::capture(|| {
            let ctx = egui::Context::default();
            let mut u = Updater::default();
            u.handle_update_msg(
                UpdateMsg::InstallerReady(Err("checksum mismatch".to_string())),
                &ctx,
            );
            u
        });
        assert!(matches!(u.state, UpdateState::Failed(_)));
        assert!(
            logs.has(tracing::Level::ERROR, "verify"),
            "expected an ERROR mentioning verify; got {:?}",
            logs.events()
        );
        assert_no_secret_logged(&logs);
    }

    #[test]
    fn check_error_logs_warn() {
        let (u, logs) = crate::log_capture::capture(|| {
            let ctx = egui::Context::default();
            let mut u = Updater {
                launch_kind: LaunchKind::Auto,
                ..Default::default()
            };
            u.handle_update_msg(
                UpdateMsg::CheckResult(Err("rate limited".to_string())),
                &ctx,
            );
            u
        });
        assert!(matches!(u.state, UpdateState::Failed(_)));
        assert!(
            logs.has(tracing::Level::WARN, "update check failed"),
            "expected a WARN for a failed check; got {:?}",
            logs.events()
        );
    }

    #[test]
    fn available_logs_info_with_version() {
        let (_, logs) = crate::log_capture::capture(|| {
            reduce_with_kind(LaunchKind::Manual, available_msg("9.9.9"));
        });
        assert!(
            logs.has(tracing::Level::INFO, "update available"),
            "expected an INFO lifecycle log; got {:?}",
            logs.events()
        );
        assert!(
            logs.has(tracing::Level::INFO, "9.9.9"),
            "the available log should carry the version string; got {:?}",
            logs.events()
        );
    }

    #[test]
    fn run_installer_rollback_refusal_logs_warn_without_secret() {
        // Anti-rollback (TUF) refusal on the installer route must emit a WARN
        // and must NEVER leak signature/key bytes — only version strings.
        let (u, logs) = crate::log_capture::capture(|| {
            let ctx = egui::Context::default();
            let mut u = Updater {
                state: UpdateState::ReadyToRunInstaller {
                    installer: std::path::PathBuf::from("/nonexistent/scr1b3-setup.exe"),
                    version: "0.0.1".to_string(), // older than the running build
                },
                ..Default::default()
            };
            u.run_installer(&ctx);
            u
        });
        assert!(matches!(u.state, UpdateState::Failed(_)));
        assert!(
            logs.has(tracing::Level::WARN, "anti-rollback"),
            "expected a WARN for the anti-rollback refusal; got {:?}",
            logs.events()
        );
        // The attempted version is fine to log; a signature/key is not.
        assert!(logs.any("0.0.1"), "the attempted version should be logged");
        assert_no_secret_logged(&logs);
    }

    #[test]
    fn apply_and_restart_rollback_refusal_logs_warn_without_secret() {
        let (u, logs) = crate::log_capture::capture(|| {
            let ctx = egui::Context::default();
            let mut u = Updater {
                state: UpdateState::ReadyToApply {
                    staged: std::path::PathBuf::from("/nonexistent/scr1b3"),
                    version: "0.0.1".to_string(),
                },
                ..Default::default()
            };
            u.apply_and_restart(&ctx);
            u
        });
        assert!(matches!(u.state, UpdateState::Failed(_)));
        assert!(
            logs.has(tracing::Level::WARN, "anti-rollback"),
            "expected a WARN for the anti-rollback refusal; got {:?}",
            logs.events()
        );
        assert!(logs.any("0.0.1"), "the attempted version should be logged");
        assert_no_secret_logged(&logs);
    }

    #[test]
    fn in_place_swap_failure_logs_error() {
        // A successful download chains into apply; with a nonexistent staged
        // binary (but a newer version, so the anti-rollback gate passes) the
        // in-place swap fails before any spawn — and that operational failure
        // must surface as an ERROR.
        let (u, logs) = crate::log_capture::capture(|| {
            let ctx = egui::Context::default();
            let mut u = Updater::default();
            u.handle_update_msg(
                UpdateMsg::Downloaded(Ok((
                    std::path::PathBuf::from("/nonexistent/scr1b3-staged-binary"),
                    "9.9.9".to_string(),
                ))),
                &ctx,
            );
            u
        });
        assert!(matches!(u.state, UpdateState::Failed(_)));
        assert!(
            logs.has(tracing::Level::ERROR, "install"),
            "expected an ERROR for the failed in-place swap; got {:?}",
            logs.events()
        );
        assert_no_secret_logged(&logs);
    }
}
