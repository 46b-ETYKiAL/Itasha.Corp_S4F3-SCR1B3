//! The one seam between the app and the OS file dialogs.
//!
//! Every `rfd` call goes through here so a TEST build never opens a real modal
//! dialog. That is not a tidiness preference — a native picker inside the test
//! process blocks on a human and wedges the runner forever.
//!
//! Why it matters beyond the hang: it was corrupting mutation testing. Any
//! mutant that made a shortcut fire spuriously set `act.open` every frame, which
//! opened a picker, which wedged the run — so cargo-mutants reported TIMEOUT.
//! TIMEOUT is the SAME symptom whether a mutant is caught or survives, so for a
//! whole class of mutants the verdict carried no information. Three real
//! survivors in `Keymap::pressed` hid behind exactly that (`:314` among them),
//! and so did the earlier Alt+Up-moves-lines-DOWN bug. With the dialogs headless
//! under `cfg(test)` those mutants report an honest MISSED or CAUGHT.
//!
//! ADR-0007 excludes the dialog itself from tests. This keeps that exclusion to
//! the dialog and nothing else: everything decidable before it, and everything
//! done after it, stays testable.
//!
//! A test build returning `None` reads as "the user pressed Cancel", which every
//! call site already handles — that is the whole point of the `if let Some(path)`
//! shape they all use.

use std::path::PathBuf;

/// Ask the user to pick one existing file. `None` = cancelled.
pub(crate) fn pick_file() -> Option<PathBuf> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        rfd::FileDialog::new().pick_file()
    }
}

/// Ask the user to pick a folder. `None` = cancelled.
pub(crate) fn pick_folder() -> Option<PathBuf> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        rfd::FileDialog::new().pick_folder()
    }
}

/// Ask the user where to save. `suggested` pre-fills the name; `filters` are
/// `(label, extension)` pairs IN ORDER, the first being the dialog's default.
/// `None` = cancelled.
///
/// In a test build this returns whatever [`test_hooks::set_next_save_path`]
/// injected, or `None` (cancelled) if nothing was injected.
pub(crate) fn save_file(suggested: &str, filters: &[(&str, &str)]) -> Option<PathBuf> {
    #[cfg(test)]
    {
        let _ = (suggested, filters);
        test_hooks::take_next_save_path()
    }
    #[cfg(not(test))]
    {
        let mut dialog = rfd::FileDialog::new().set_file_name(suggested);
        for (label, ext) in filters {
            dialog = dialog.add_filter(*label, &[*ext]);
        }
        dialog.save_file()
    }
}

/// Inject what the OS dialog "returns", so the code AROUND it is testable.
///
/// This is what finally retires the ADR-0007 exclusion on `save_as_active`. That
/// exclusion was correct about the dialog — it blocks on a human — but it had
/// swallowed the whole function, and a fn nothing can call is a fn whose body
/// can be deleted with every test still green (cargo-mutants: `replace
/// save_as_active with ()`, MISSED).
///
/// Stubbing the OS boundary is not the same as supplying the wire: a test still
/// runs the REAL `save_as_prompt` and the REAL `commit_save_as` and only the
/// dialog's answer is injected, so the prompt→dialog→commit wiring is exactly
/// what is under test.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        static NEXT_SAVE_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    /// The next [`super::save_file`] returns `path`, as if the user picked it.
    /// Consumed once; a second call reads as "cancelled".
    pub(crate) fn set_next_save_path(path: PathBuf) {
        NEXT_SAVE_PATH.with(|c| *c.borrow_mut() = Some(path));
    }

    pub(super) fn take_next_save_path() -> Option<PathBuf> {
        NEXT_SAVE_PATH.with(|c| c.borrow_mut().take())
    }
}
