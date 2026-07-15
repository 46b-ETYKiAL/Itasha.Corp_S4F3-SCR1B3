//! OS default-app / file-association registration.
//!
//! Registers SCR1B3 as a handler for the user's chosen [`ClaimType`]s and, where
//! the OS allows it, sets it as the default. Every backend uses OS built-ins
//! (`reg.exe` on Windows, `xdg-mime` / `mimeapps.list` on Linux) or a documented
//! manual path (macOS) — **no FFI, no `unsafe`**, so `scribe-app` keeps its
//! crate-level `#![forbid(unsafe_code)]`.
//!
//! The hard per-OS reality:
//! - **Windows** — you CANNOT silently flip the default (UserChoice is hash-
//!   protected). We register the ProgID + `OpenWithProgids` + `Capabilities` +
//!   `RegisteredApplications` under `HKCU` (no admin) so SCR1B3 becomes a
//!   first-class choice, then deep-link the user to the Default Apps UI to
//!   confirm. Same constraint VS Code / Notepad++ live under.
//! - **Linux** — `xdg-mime default` sets it silently, per-user, no root.
//! - **macOS** — the bundle's `Info.plist` declares the document types; the user
//!   sets the default via Finder ▸ Get Info ▸ Open With ▸ Change All (no
//!   third-party CLI / no `unsafe` objc2 call from this `forbid(unsafe_code)` crate).

use scribe_core::config::ClaimType;

/// The outcome of a registration attempt, surfaced by the Settings UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegisterReport {
    /// [`ClaimType::key`]s that registered successfully.
    pub registered: Vec<String>,
    /// `(key, error)` for groups that failed to register.
    pub failed: Vec<(String, String)>,
    /// The OS needs the user to finish in a system UI (Windows: confirm in the
    /// Default Apps window; macOS: pick SCR1B3 in Finder ▸ Get Info). The Settings
    /// UI surfaces the follow-up step when this is set.
    pub needs_user_action: bool,
    /// A short, user-facing status / next-step message.
    pub message: String,
}

// `linux` compiles on every OS under `test` so its pure `mimeapps.list` editor +
// tests run on all hosts; the runtime parts inside it are `cfg(target_os =
// "linux")`-gated.
#[cfg(any(test, target_os = "linux"))]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(any(test, windows))]
pub(crate) mod windows_entries;

/// Register SCR1B3 as a handler for `types` and, where the OS permits, set it as
/// the default. Returns a [`RegisterReport`] for the Settings UI. Never panics —
/// a backend failure is reported, not raised.
pub fn register(types: &[ClaimType]) -> RegisterReport {
    #[cfg(windows)]
    {
        windows::register(types)
    }
    #[cfg(target_os = "linux")]
    {
        linux::register(types)
    }
    #[cfg(target_os = "macos")]
    {
        macos_register(types)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = types;
        RegisterReport {
            message: "Setting the default app isn't supported on this platform.".into(),
            ..Default::default()
        }
    }
}

/// macOS: the document types are declared in the app bundle's `Info.plist`, so
/// SCR1B3 already appears in Finder's "Open With". Setting the default is a
/// one-time manual step (Get Info ▸ Open With ▸ Change All) — we return the
/// guidance rather than make an `unsafe` Launch-Services call from this
/// `forbid(unsafe_code)` crate.
#[cfg(target_os = "macos")]
fn macos_register(types: &[ClaimType]) -> RegisterReport {
    RegisterReport {
        registered: types.iter().map(|t| t.key().to_string()).collect(),
        failed: Vec::new(),
        needs_user_action: true,
        message: "SCR1B3 is registered for these file types. To make it the \
                  default, select a file in Finder, press ⌘I, expand \"Open \
                  with\", choose SCR1B3, and click \"Change All…\"."
            .into(),
    }
}

#[cfg(test)]
mod packaging_consistency_tests {
    //! The OS handler-eligibility declarations (the Linux `.desktop` MimeType,
    //! the macOS `Info.plist` UTIs) MUST cover every type SCR1B3 claims — else a
    //! `register()` would set a default the OS doesn't recognise SCR1B3 as a
    //! handler for, and the choice would silently not stick. These tests bind the
    //! packaging files to [`ClaimType`] so they can't drift.

    use scribe_core::config::ClaimType;

    const DESKTOP: &str = include_str!("../../../../packaging/linux/scr1b3.desktop");
    const INFO_PLIST: &str = include_str!("../../../../packaging/macos/Info.plist");

    #[test]
    fn linux_desktop_declares_every_claimed_mime() {
        let mime_line = DESKTOP
            .lines()
            .find(|l| l.starts_with("MimeType="))
            .expect("MimeType= line in scr1b3.desktop");
        for ct in ClaimType::ALL {
            for m in ct.linux_mimes() {
                assert!(
                    mime_line.contains(m),
                    "scr1b3.desktop MimeType= is missing {m:?} (claimed by {ct:?}) — \
                     xdg-mime default for it wouldn't stick"
                );
            }
        }
    }

    #[test]
    fn macos_info_plist_declares_every_claimed_uti() {
        for ct in ClaimType::ALL {
            for uti in ct.macos_utis() {
                assert!(
                    INFO_PLIST.contains(uti),
                    "Info.plist LSItemContentTypes is missing {uti:?} (claimed by {ct:?})"
                );
            }
        }
    }

    /// The CI mutation gate EXCLUDES `integration/windows.rs`, and this is the
    /// premise that makes that exclusion honest rather than a blind spot.
    ///
    /// The gate runs on ubuntu. `windows.rs` is `#[cfg(windows)]`, so it is not
    /// compiled there: mutating it changes nothing that builds, no test can
    /// fail, and every mutant reports MISSED. Excluding it drops 100% false
    /// positives — but ONLY while it stays cfg-gated. If it is ever made
    /// cross-platform (or test-compiled like its `windows_entries` sibling), its
    /// mutants become real signal and the `--exclude` in .github/workflows/ci.yml
    /// would start hiding genuine gaps. Fail here so that cannot happen quietly.
    #[test]
    fn windows_module_is_cfg_gated_so_the_mutation_exclusion_stays_honest() {
        let src = include_str!("mod.rs");
        // ASSEMBLED, never written as a literal. This test reads its OWN file, so
        // a literal needle would sit in the source and `contains` would match the
        // needle itself — passing no matter what the real declaration said. The
        // first draft did exactly that, and only re-running the premise (flip the
        // gate, expect a failure) caught it. Building the string from parts keeps
        // the declaration up top the one and only match.
        let needle = ["#[cfg(", "windows", ")]\n", "mod windows;"].concat();
        assert_eq!(
            src.matches(needle.as_str()).count(),
            1,
            "`mod windows;` is no longer exactly `#[cfg(windows)]`-gated (or this \
             test now self-matches). If that module compiles off Windows its \
             mutants are real signal: DROP `--exclude \
             '**/src/integration/windows.rs'` from the mutation-in-diff job in \
             .github/workflows/ci.yml and delete this test, rather than leaving a \
             gate that silently skips a file it claims to cover."
        );
    }
}
