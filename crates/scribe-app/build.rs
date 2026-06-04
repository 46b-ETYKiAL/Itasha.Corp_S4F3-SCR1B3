//! SCR1B3 build script.
//!
//! Two jobs:
//!   1. Bake the build's target triple into `SCR1B3_TARGET` so the self-updater
//!      can match release assets to the running binary's platform.
//!   2. On Windows, embed the multi-size `.ico` as a Win32 resource so Explorer,
//!      the taskbar, and pre-launch Alt-Tab show the app icon before eframe's
//!      runtime `with_icon` ever runs.
//!
//! Build scripts run on the HOST, not the target — so platform branching MUST
//! key off `CARGO_CFG_TARGET_OS` (set by Cargo to the *target* OS), never
//! `cfg!(target_os = ...)` (which reflects the host). The `winresource`
//! build-dependency is scoped to `cfg(windows)` in Cargo.toml, so it is not
//! pulled in on Linux/macOS CI builds.

fn main() {
    // (1) Bake the build's target triple for the self-updater's asset matching.
    if let Ok(t) = std::env::var("TARGET") {
        println!("cargo:rustc-env=SCR1B3_TARGET={t}");
    }

    // (2) Embed the Windows .exe icon (Explorer / taskbar / Alt-Tab pre-launch).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/scr1b3.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/scr1b3.ico");
        res.compile().expect("embed Windows .ico resource");
    }
}
