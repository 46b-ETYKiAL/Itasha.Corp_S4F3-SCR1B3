//! Linux per-user file-association registration. Sets SCR1B3 as the default
//! handler via `xdg-mime default scr1b3.desktop <mimes…>` (spec-blessed,
//! per-user, no root), falling back to a preserve-existing edit of
//! `~/.config/mimeapps.list` when `xdg-mime` is absent. No `unsafe`, no FFI.

#[cfg(target_os = "linux")]
use super::RegisterReport;
#[cfg(target_os = "linux")]
use scribe_core::config::ClaimType;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;

/// The `.desktop` id SCR1B3 ships as (and the installer / packaging registers).
#[cfg(target_os = "linux")]
const DESKTOP: &str = "scr1b3.desktop";

/// De-duplicated MIME types across all requested groups.
#[cfg(target_os = "linux")]
fn all_mimes(types: &[ClaimType]) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for t in types {
        for m in t.linux_mimes() {
            if !v.iter().any(|x| x == m) {
                v.push((*m).to_string());
            }
        }
    }
    v
}

#[cfg(target_os = "linux")]
pub fn register(types: &[ClaimType]) -> RegisterReport {
    let mimes = all_mimes(types);
    if mimes.is_empty() {
        return RegisterReport {
            message: "No file types selected to register.".into(),
            ..Default::default()
        };
    }
    let ok_report = || RegisterReport {
        registered: types.iter().map(|t| t.key().to_string()).collect(),
        failed: Vec::new(),
        needs_user_action: false,
        message: "SCR1B3 is now the default for the selected file types.".into(),
    };

    // Prefer xdg-mime (writes the right precedence into mimeapps.list for us).
    match Command::new("xdg-mime")
        .arg("default")
        .arg(DESKTOP)
        .args(&mimes)
        .output()
    {
        Ok(o) if o.status.success() => ok_report(),
        // xdg-mime present but failed, or absent → edit mimeapps.list directly.
        Ok(_) | Err(_) => fallback_mimeapps(&mimes, types, ok_report),
    }
}

#[cfg(target_os = "linux")]
fn fallback_mimeapps(
    mimes: &[String],
    types: &[ClaimType],
    ok_report: impl Fn() -> RegisterReport,
) -> RegisterReport {
    let Some(path) = mimeapps_path() else {
        return RegisterReport {
            failed: vec![("mimeapps".into(), "could not locate ~/.config".into())],
            message: "Couldn't find your config directory to set the default app.".into(),
            ..Default::default()
        };
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = set_default_in_mimeapps(&existing, DESKTOP, mimes);
    match std::fs::write(&path, updated) {
        Ok(()) => ok_report(),
        Err(e) => RegisterReport {
            failed: types
                .iter()
                .map(|t| (t.key().to_string(), e.to_string()))
                .collect(),
            message: "Couldn't write the default-app setting. Check the \
                      permissions on ~/.config/mimeapps.list."
                .into(),
            ..Default::default()
        },
    }
}

#[cfg(target_os = "linux")]
fn mimeapps_path() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        let p = PathBuf::from(x);
        if p.is_absolute() {
            return Some(p.join("mimeapps.list"));
        }
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("mimeapps.list"))
}

/// PURE: set `desktop` as the default handler for each of `mimes` in the
/// `[Default Applications]` group of an existing `mimeapps.list` body, while
/// PRESERVING every other line, section, and association. Compiled on all OSes
/// so it is unit-testable everywhere.
///
/// - An existing `[Default Applications]` section is updated in place; a missing
///   one is appended.
/// - A mime already mapped to another handler is overwritten (the user just
///   asked for SCR1B3); a mime not present is added.
/// - All other sections (`[Added Associations]`, comments, blank lines) are kept
///   verbatim.
pub(crate) fn set_default_in_mimeapps(existing: &str, desktop: &str, mimes: &[String]) -> String {
    use std::collections::BTreeSet;
    let want: BTreeSet<&str> = mimes.iter().map(String::as_str).collect();

    let mut out: Vec<String> = Vec::new();
    let mut in_default = false;
    let mut handled: BTreeSet<String> = BTreeSet::new();
    let mut wrote_section = false;

    let flush_remaining = |out: &mut Vec<String>, handled: &BTreeSet<String>| {
        for m in mimes {
            if !handled.contains(m) {
                out.push(format!("{m}={desktop};"));
            }
        }
    };

    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            // Leaving a section: if it was [Default Applications], append any
            // wanted mimes we didn't see.
            if in_default {
                flush_remaining(&mut out, &handled);
            }
            in_default = trimmed == "[Default Applications]";
            if in_default {
                wrote_section = true;
            }
            out.push(line.to_string());
            continue;
        }
        if in_default {
            if let Some((key, _)) = trimmed.split_once('=') {
                let key = key.trim();
                if want.contains(key) {
                    // Replace this mapping with SCR1B3.
                    out.push(format!("{key}={desktop};"));
                    handled.insert(key.to_string());
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    // EOF while still inside [Default Applications]: flush the rest there.
    if in_default {
        flush_remaining(&mut out, &handled);
    } else if !wrote_section {
        // No [Default Applications] section existed — append one.
        if out.last().is_some_and(|l| !l.is_empty()) {
            out.push(String::new());
        }
        out.push("[Default Applications]".to_string());
        for m in mimes {
            out.push(format!("{m}={desktop};"));
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mimes(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn appends_section_when_none_exists() {
        let out = set_default_in_mimeapps("", "scr1b3.desktop", &mimes(&["text/plain"]));
        assert!(out.contains("[Default Applications]"));
        assert!(out.contains("text/plain=scr1b3.desktop;"));
    }

    #[test]
    fn preserves_existing_sections_and_overwrites_only_requested_mimes() {
        let existing = "[Added Associations]\ntext/plain=other.desktop;scr1b3.desktop;\n\n\
                        [Default Applications]\ntext/plain=other.desktop;\napplication/pdf=evince.desktop;\n";
        let out = set_default_in_mimeapps(existing, "scr1b3.desktop", &mimes(&["text/plain"]));
        // The PDF default and the Added Associations group are untouched.
        assert!(
            out.contains("application/pdf=evince.desktop;"),
            "other default preserved"
        );
        assert!(
            out.contains("[Added Associations]"),
            "other section preserved"
        );
        // The [Default Applications] text/plain default now points at SCR1B3
        // (exactly once); the [Added Associations] line is preserved verbatim
        // (it legitimately still lists other.desktop AND scr1b3.desktop).
        assert_eq!(
            out.matches("text/plain=scr1b3.desktop;").count(),
            1,
            "text/plain default set once:\n{out}"
        );
        let defaults = out
            .split_once("[Default Applications]")
            .expect("defaults section")
            .1;
        assert!(
            defaults.contains("text/plain=scr1b3.desktop;")
                && !defaults.contains("text/plain=other.desktop;"),
            "the DEFAULT for text/plain is SCR1B3, not the old handler:\n{defaults}"
        );
        assert!(
            out.contains("text/plain=other.desktop;scr1b3.desktop;"),
            "the [Added Associations] line is preserved verbatim"
        );
    }

    #[test]
    fn adds_new_mime_inside_an_existing_section() {
        let existing = "[Default Applications]\napplication/pdf=evince.desktop;\n";
        let out = set_default_in_mimeapps(existing, "scr1b3.desktop", &mimes(&["text/markdown"]));
        assert!(out.contains("application/pdf=evince.desktop;"));
        assert!(out.contains("text/markdown=scr1b3.desktop;"));
        // Exactly one [Default Applications] section (no duplicate appended).
        assert_eq!(out.matches("[Default Applications]").count(), 1);
    }

    #[test]
    fn is_idempotent() {
        let once = set_default_in_mimeapps(
            "",
            "scr1b3.desktop",
            &mimes(&["text/plain", "application/json"]),
        );
        let twice = set_default_in_mimeapps(
            &once,
            "scr1b3.desktop",
            &mimes(&["text/plain", "application/json"]),
        );
        assert_eq!(once, twice, "re-applying the same defaults is a no-op");
    }
}
