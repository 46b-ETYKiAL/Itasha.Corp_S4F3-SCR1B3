//! Windows per-user file-association registration via `reg.exe` (HKCU, no admin,
//! no FFI, no `unsafe`). Modern Windows BLOCKS setting the default handler
//! programmatically (UserChoice hash protection), so we register the ProgID +
//! `OpenWithProgids` + `Capabilities` + `RegisteredApplications` (making SCR1B3 a
//! first-class choice) and deep-link the user to the Default Apps UI to confirm.

use super::windows_entries::{registry_entries, RegEntry};
use super::RegisterReport;
use scribe_core::config::ClaimType;
use std::process::Command;

fn current_exe_string() -> Option<String> {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Apply one [`RegEntry`] via `reg.exe add` under `HKCU`. Returns the trimmed
/// stderr on failure.
pub(crate) fn apply_entry(e: &RegEntry) -> Result<(), String> {
    let full_key = format!("HKCU\\{}", e.key);
    let mut cmd = Command::new("reg");
    cmd.arg("add").arg(&full_key);
    if e.name.is_empty() {
        cmd.arg("/ve"); // the key's default value
    } else {
        cmd.args(["/v", e.name.as_str()]);
    }
    cmd.args(["/t", "REG_SZ", "/d", e.data.as_str(), "/f"]);
    let out = cmd
        .output()
        .map_err(|err| format!("reg.exe unavailable: {err}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Apply every entry registering `types` under the given roots. Factored out so
/// the `#[ignore]` integration test can target a throwaway subtree instead of
/// real `Software\Classes`.
pub(crate) fn register_under(
    types: &[ClaimType],
    exe: &str,
    class_root: &str,
    app_root: &str,
) -> RegisterReport {
    let entries = registry_entries(types, exe, class_root, app_root);
    let mut failed = Vec::new();
    for e in &entries {
        if let Err(err) = apply_entry(e) {
            failed.push((e.key.clone(), err));
        }
    }
    let registered = if failed.is_empty() {
        types.iter().map(|t| t.key().to_string()).collect()
    } else {
        Vec::new()
    };
    RegisterReport {
        registered,
        failed,
        needs_user_action: true,
        message: "SCR1B3 is registered. Windows requires you to pick it in the \
                  Default Apps window — choose SCR1B3 for each file type. (Windows \
                  doesn't let an app change the default for you.)"
            .into(),
    }
}

pub fn register(types: &[ClaimType]) -> RegisterReport {
    let Some(exe) = current_exe_string() else {
        return RegisterReport {
            message: "Couldn't find the SCR1B3 program path to register.".into(),
            ..Default::default()
        };
    };
    let report = register_under(types, &exe, "Software\\Classes", "Software\\SCR1B3");
    if report.failed.is_empty() {
        open_default_apps_ui();
    }
    report
}

/// Open the Windows Default Apps page (focused on SCR1B3 on Win11) so the user
/// can confirm the choice. Launched through the shell — no FFI.
pub fn open_default_apps_ui() {
    let _ = Command::new("cmd")
        .args([
            "/c",
            "start",
            "",
            "ms-settings:defaultapps?registeredAppUser=SCR1B3",
        ])
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mutates the real `HKCU` hive (under a throwaway subtree it cleans up), so
    /// it is `#[ignore]` by default and skipped in CI. Run locally with:
    ///   cargo test -p scribe-app --bin scr1b3 integration::windows -- --ignored
    #[test]
    #[ignore = "writes to HKCU; run with --ignored on Windows"]
    fn register_under_throwaway_root_writes_and_reads_back() {
        let class_root = "Software\\SCR1B3-Test\\Classes";
        let app_root = "Software\\SCR1B3-Test\\App";
        let exe = r"C:\tmp\scr1b3.exe";

        let report = register_under(&[ClaimType::PlainText], exe, class_root, app_root);
        assert!(
            report.failed.is_empty(),
            "writes failed: {:?}",
            report.failed
        );

        let out = Command::new("reg")
            .args([
                "query",
                &format!("HKCU\\{class_root}\\SCR1B3.txt\\shell\\open\\command"),
                "/ve",
            ])
            .output()
            .expect("reg query");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("scr1b3.exe"),
            "open command missing exe: {text}"
        );

        // Cleanup the throwaway subtree + the RegisteredApplications value.
        let _ = Command::new("reg")
            .args(["delete", "HKCU\\Software\\SCR1B3-Test", "/f"])
            .output();
        let _ = Command::new("reg")
            .args([
                "delete",
                "HKCU\\Software\\RegisteredApplications",
                "/v",
                "SCR1B3",
                "/f",
            ])
            .output();
    }
}
