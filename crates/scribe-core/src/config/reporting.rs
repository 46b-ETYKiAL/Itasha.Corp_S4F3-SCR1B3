//! W1TN3SS opt-in crash/error/feedback reporting configuration (schema v3):
//! per-stream consent posture ([`ReportingMode`] / [`ReportingConfig`]) and the
//! manual-issue intake destinations ([`IssueIntakeConfig`]).

use serde::{Deserialize, Serialize};

/// Per-stream consent posture for W1TN3SS opt-in reporting.
///
/// Mirrors `itasha_report_core::config::ReportingMode` but is owned by
/// `scribe-core` so the config crate carries NO SDK dependency (the SDK lives
/// only in the `scribe-app` binary, which maps this enum onto the SDK's at the
/// capture/consent boundary). The default is [`ReportingMode::Off`] — there is
/// no constructor that yields an on-by-default mode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReportingMode {
    /// Never report for this stream (the default, opt-in posture).
    #[default]
    Off,
    /// Ask the user each time a report is available (per-event consent).
    AskEachTime,
    /// Always report for this stream — the user previously chose "Always" in
    /// the consent dialog. Even then, the report is captured + spooled locally
    /// and the SDK still requires a host-minted consent token to transmit.
    Always,
}

impl ReportingMode {
    /// Whether this mode permits transmission **without** a fresh per-event
    /// prompt. Only [`ReportingMode::Always`] does; `Off` and `AskEachTime`
    /// both require an explicit per-event consent decision.
    pub fn is_always(self) -> bool {
        matches!(self, ReportingMode::Always)
    }

    /// Whether this mode permits *any* transmission at all (i.e. not `Off`).
    pub fn permits_reporting(self) -> bool {
        !matches!(self, ReportingMode::Off)
    }
}

/// W1TN3SS opt-in reporting configuration: one [`ReportingMode`] per
/// independent data stream.
///
/// The two streams are NEVER bundled under one toggle (the cardinal privacy
/// rule from the consent research): the high-sensitivity **crash-report**
/// stream and the user-initiated **manual-issue** stream each carry their own
/// posture, both defaulting `Off`. Usage telemetry is explicitly out of scope —
/// these two streams are the ONLY consented channels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReportingConfig {
    /// Consent posture for the crash-report stream (panic backtrace; the
    /// previewable Tier-1 text payload). Default: `Off`.
    pub crash_reports: ReportingMode,
    /// Consent posture for the manual issue/feedback stream (user-typed, always
    /// user-initiated). Default: `Off`. (The manual-issue intake UI itself is a
    /// sibling plan; this field is the consent posture it will read.)
    pub manual_issues: ReportingMode,
    /// Where the user-initiated "Report an issue" dialog sends its deep links and
    /// mailto fallback. Config-injected (no hardcoded prod values baked
    /// unalterably): an operator can repoint the repo or the support alias by
    /// editing the config. `#[serde(default)]` means a config written before this
    /// field reads it as [`IssueIntakeConfig::default`].
    #[serde(default)]
    pub issue_intake: IssueIntakeConfig,
}

impl Default for ReportingConfig {
    /// Both streams `Off` — the privacy-default, opt-in posture.
    fn default() -> Self {
        Self {
            crash_reports: ReportingMode::Off,
            manual_issues: ReportingMode::Off,
            issue_intake: IssueIntakeConfig::default(),
        }
    }
}

/// Destinations for the user-initiated "Report an issue" dialog. This path opens
/// a browser / mail client out-of-band — it has NO consent posture of its own
/// (it transmits nothing autonomously; the user clicks Open), so it lives here
/// purely as config-injected destinations rather than as a [`ReportingMode`].
///
/// These are deliberately editable so the GitHub repo and the support alias are
/// not baked unalterably into the binary (`dev_prod_isolation`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct IssueIntakeConfig {
    /// The `owner/repo` slug the prefilled GitHub Issue-Form deep link targets,
    /// e.g. `46b-ETYKiAL/Itasha.Corp_S4F3-W1TN3SS`.
    pub repo: String,
    /// The `mailto:` support alias used by the "Email feedback instead"
    /// fallback (a self-controlled Cloudflare Email-Routing alias). Empty
    /// disables the mailto fallback.
    pub mailto_alias: String,
}

impl Default for IssueIntakeConfig {
    /// Point at the public W1TN3SS repo's shared Issue-Form templates and a
    /// self-controlled support alias. Both are operator-editable in the config.
    fn default() -> Self {
        Self {
            repo: "46b-ETYKiAL/Itasha.Corp_S4F3-W1TN3SS".to_string(),
            mailto_alias: "support@witness.itasha.example".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn reporting_defaults_to_off_for_both_streams() {
        // The privacy default: a fresh config has BOTH reporting streams Off.
        // There is no constructor that yields an on-by-default reporting mode.
        let c = Config::default();
        assert_eq!(c.reporting.crash_reports, ReportingMode::Off);
        assert_eq!(c.reporting.manual_issues, ReportingMode::Off);
        assert!(!c.reporting.crash_reports.permits_reporting());
        assert!(!c.reporting.manual_issues.permits_reporting());
    }

    #[test]
    fn reporting_modes_round_trip_through_toml() {
        // A user who has opted a stream into Always/AskEachTime must have that
        // choice persist across save/load — and the v2->v3 migrate must NOT
        // clobber it. Build a v3 config with explicit reporting modes, serialize,
        // reload, and assert the modes survive AND migrate() is a no-op.
        let toml = "\
schema_version = 3

[reporting]
crash_reports = \"always\"
manual_issues = \"ask_each_time\"
";
        let mut c = Config::from_toml_str(toml).unwrap();
        assert_eq!(c.reporting.crash_reports, ReportingMode::Always);
        assert_eq!(c.reporting.manual_issues, ReportingMode::AskEachTime);
        assert!(
            !c.migrate(),
            "a v3 config must not migrate (no clobber of an opted-in choice)"
        );
        assert_eq!(c.reporting.crash_reports, ReportingMode::Always);
        assert_eq!(c.reporting.manual_issues, ReportingMode::AskEachTime);

        // Full save/load round-trip preserves the modes.
        let back = Config::from_toml_str(&c.to_toml_string()).unwrap();
        assert_eq!(back.reporting.crash_reports, ReportingMode::Always);
        assert_eq!(back.reporting.manual_issues, ReportingMode::AskEachTime);
    }

    #[test]
    fn reporting_mode_predicates() {
        assert!(ReportingMode::Always.is_always());
        assert!(!ReportingMode::AskEachTime.is_always());
        assert!(!ReportingMode::Off.is_always());
        assert!(ReportingMode::Always.permits_reporting());
        assert!(ReportingMode::AskEachTime.permits_reporting());
        assert!(!ReportingMode::Off.permits_reporting());
    }
}
