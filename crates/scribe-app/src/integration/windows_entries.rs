//! Pure generation of the per-user (HKCU) registry entries that register SCR1B3
//! as a file handler. The exe path + roots are injected, so the full key/value
//! set is unit-testable on ANY OS (the executor in `windows.rs` is the only
//! Windows-only part). Compiled under `#[cfg(any(test, windows))]`.

use scribe_core::config::ClaimType;

/// One registry write: a key path under HKCU, a value name (`""` = the key's
/// default value), and the string data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegEntry {
    pub key: String,
    pub name: String,
    pub data: String,
}

/// Build the HKCU registry entries registering `types` with `exe` as the open
/// handler. `class_root` is normally `Software\Classes` and `app_root`
/// `Software\SCR1B3`; a test passes throwaway roots so it never touches real
/// associations. The writes are ADDITIVE — each extension's `OpenWithProgids`
/// gets our ProgID added (it never overwrites the user's existing default), and
/// `Capabilities` + `RegisteredApplications` make SCR1B3 a first-class entry in
/// the Default Apps UI.
pub(crate) fn registry_entries(
    types: &[ClaimType],
    exe: &str,
    class_root: &str,
    app_root: &str,
) -> Vec<RegEntry> {
    let mut out = Vec::new();

    // Capabilities + RegisteredApplications (one set, always — makes SCR1B3
    // appear in Settings ▸ Default Apps).
    out.push(RegEntry {
        key: format!("{app_root}\\Capabilities"),
        name: "ApplicationName".into(),
        data: "SCR1B3".into(),
    });
    out.push(RegEntry {
        key: format!("{app_root}\\Capabilities"),
        name: "ApplicationDescription".into(),
        data: "Fast, telemetry-free code & text editor.".into(),
    });
    out.push(RegEntry {
        key: "Software\\RegisteredApplications".into(),
        name: "SCR1B3".into(),
        data: format!("{app_root}\\Capabilities"),
    });

    for t in types {
        let progid = t.windows_progid();
        // ProgID: the open command (`"<exe>" "%1"`), an icon, and a friendly name.
        out.push(RegEntry {
            key: format!("{class_root}\\{progid}\\shell\\open\\command"),
            name: String::new(),
            data: format!("\"{exe}\" \"%1\""),
        });
        out.push(RegEntry {
            key: format!("{class_root}\\{progid}\\DefaultIcon"),
            name: String::new(),
            data: format!("{exe},0"),
        });
        out.push(RegEntry {
            key: format!("{class_root}\\{progid}"),
            name: String::new(),
            data: "SCR1B3 document".into(),
        });
        for ext in t.windows_extensions() {
            // Additive: register our ProgID as an "open with" option for the ext.
            out.push(RegEntry {
                key: format!("{class_root}\\.{ext}\\OpenWithProgids"),
                name: progid.to_string(),
                data: String::new(),
            });
            // And declare the association under Capabilities for the Default
            // Apps UI.
            out.push(RegEntry {
                key: format!("{app_root}\\Capabilities\\FileAssociations"),
                name: format!(".{ext}"),
                data: progid.to_string(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // A path WITH a space, so the open-command quoting is genuinely exercised.
    const EXE: &str = r"C:\Apps\SCR 1B3\scr1b3.exe";

    fn entries(types: &[ClaimType]) -> Vec<RegEntry> {
        registry_entries(types, EXE, "Software\\Classes", "Software\\SCR1B3")
    }

    #[test]
    fn progid_open_command_quotes_exe_and_file_arg() {
        let es = entries(&[ClaimType::PlainText]);
        let cmd = es
            .iter()
            .find(|e| e.key.ends_with("SCR1B3.txt\\shell\\open\\command"))
            .expect("open command entry");
        assert_eq!(cmd.name, "", "open command is the key's default value");
        assert_eq!(
            cmd.data,
            format!("\"{EXE}\" \"%1\""),
            "exe and %1 both quoted"
        );
    }

    #[test]
    fn each_extension_gets_an_additive_openwithprogids_entry() {
        let es = entries(&[ClaimType::Markdown]);
        for ext in ClaimType::Markdown.windows_extensions() {
            let e = es
                .iter()
                .find(|e| e.key == format!("Software\\Classes\\.{ext}\\OpenWithProgids"))
                .unwrap_or_else(|| panic!("OpenWithProgids entry for .{ext}"));
            assert_eq!(e.name, "SCR1B3.md", "value NAME is the ProgID");
            assert_eq!(
                e.data, "",
                "OpenWithProgids data is empty (additive, never steals)"
            );
        }
    }

    #[test]
    fn registered_applications_points_at_capabilities() {
        let es = entries(&[ClaimType::Json]);
        let ra = es
            .iter()
            .find(|e| e.key == "Software\\RegisteredApplications" && e.name == "SCR1B3")
            .expect("RegisteredApplications entry");
        assert_eq!(ra.data, "Software\\SCR1B3\\Capabilities");
    }

    #[test]
    fn capabilities_file_associations_cover_every_extension() {
        let es = entries(&[ClaimType::PlainText]);
        for ext in ClaimType::PlainText.windows_extensions() {
            assert!(
                es.iter().any(
                    |e| e.key == "Software\\SCR1B3\\Capabilities\\FileAssociations"
                        && e.name == format!(".{ext}")
                        && e.data == "SCR1B3.txt"
                ),
                "FileAssociations entry for .{ext}"
            );
        }
    }

    #[test]
    fn a_test_class_root_keeps_all_writes_inside_the_throwaway_subtree() {
        // The Windows #[ignore] integration test relies on this: with a test
        // root, NO entry touches a real `Software\Classes\.<ext>` key.
        let es = registry_entries(
            &ClaimType::ALL,
            EXE,
            "Software\\SCR1B3-Test\\Classes",
            "Software\\SCR1B3-Test\\App",
        );
        for e in &es {
            assert!(
                e.key.starts_with("Software\\SCR1B3-Test\\")
                    || e.key == "Software\\RegisteredApplications",
                "entry escaped the test root: {}",
                e.key
            );
        }
    }

    #[test]
    fn empty_types_still_registers_the_app_capabilities() {
        let es = entries(&[]);
        assert!(es
            .iter()
            .any(|e| e.key.ends_with("Capabilities") && e.name == "ApplicationName"));
        // ...but declares no per-extension ProgID.
        assert!(!es.iter().any(|e| e.key.contains("\\shell\\open\\command")));
    }
}
