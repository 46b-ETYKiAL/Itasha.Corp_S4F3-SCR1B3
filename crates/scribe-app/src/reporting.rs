//! W1TN3SS opt-in crash/error reporting — the SCR1B3 host glue.
//!
//! This module is thin host glue over the in-house `itasha-report-core` SDK
//! (pinned git tag). SCR1B3 implements NO SDK behavior — the config model,
//! sanitizer, spool, transport, preview API and consent gate all live in the
//! SDK and are CALLED here. The two seams this module owns are:
//!
//! 1. **Capture** ([`capture_panic`]) — the panic hook builds a Tier-1 report
//!    from the panic's `&'static str` message + our own `file:line` SITE,
//!    sanitizes it, and SPOOLS it locally. It transmits NOTHING — local-first,
//!    offline-safe, consent comes later.
//! 2. **Consent-gated send** ([`send_report`]) — given a host-minted
//!    [`ConsentToken`] (which only exists after the user agreed in the consent
//!    dialog, or because the stream's mode is `Always`), transmit one spooled
//!    report through the SDK's hardened transport, then log the outcome.
//!
//! Privacy invariants (inherited from the SDK, asserted at this surface):
//! - default-OFF (both streams default [`ReportingMode::Off`]),
//! - consent-gated (no [`ConsentToken`] => no send — enforced at the type level
//!   by the SDK's `IngestBackend::send` signature),
//! - previewable+editable before send (the dialog calls [`preview_text`]),
//! - no persistent identifier (only the consent token's ephemeral nonce),
//! - the panic `&'static str` discipline (a `String` payload — which could embed
//!   buffer text or a path — is deliberately suppressed at capture).

use itasha_report_core::backend::{IngestBackend, LeanPipelineBackend, SendOutcome, TransportConfig};
use itasha_report_core::consent::ConsentToken;
use itasha_report_core::preview::Preview;
use itasha_report_core::report::{Report, Stream};
use itasha_report_core::sanitize::Sanitizer;
use itasha_report_core::spool::Spool;

use scribe_core::{Config, ReportingMode};

/// The env var that injects the self-hosted ingest endpoint. There is NO
/// hardcoded URL in SCR1B3 and NO default — a build with this unset can spool
/// locally but can NEVER transmit (a mis-build cannot phone home). The
/// server-side endpoint is a separate plan; until one is configured, reports
/// stay in the local spool and a consented send returns a structured
/// `no-endpoint` outcome (never a silent drop, never a fake success).
pub const REPORT_ENDPOINT_ENV: &str = "SCR1B3_REPORT_ENDPOINT";

/// The structured result of attempting a report, logged to the action-log
/// (counts/enums only, never PII). Mirrors the privacy-first taxonomy: a report
/// is either captured-and-spooled, sent, refused for want of consent, refused
/// for want of an endpoint, or failed in transport — never silently dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportOutcome {
    /// The panic was captured and written to the local spool. Nothing sent.
    Spooled,
    /// A consented report was transmitted and accepted by the endpoint.
    Sent,
    /// A send was attempted without consent — refused. (Should be unreachable
    /// given the type-level gate, but surfaced for defense-in-depth + logging.)
    RefusedNoConsent,
    /// Consent was present but no endpoint is configured — the report stays
    /// spooled for a later, configured send.
    RefusedNoEndpoint,
    /// The transport failed (offline, TLS, status). The report is retained.
    Failed(String),
}

impl ReportOutcome {
    /// The stable, non-identifying action-log detail string for this outcome.
    fn log_detail(&self) -> String {
        match self {
            ReportOutcome::Spooled => "spooled".to_string(),
            ReportOutcome::Sent => "sent".to_string(),
            ReportOutcome::RefusedNoConsent => "refused-no-consent".to_string(),
            ReportOutcome::RefusedNoEndpoint => "refused-no-endpoint".to_string(),
            ReportOutcome::Failed(_) => "failed".to_string(),
        }
    }
}

/// Log a report outcome to the existing SCR1B3 action-log under the `report`
/// category (counts/enums only, no PII). Honours `SCR1B3_NO_ACTION_LOG` +
/// `S4F3_DISABLE_TELEMETRY=1` (the latter is an explicit opt-out of any local
/// diagnostic logging for this feature). Best-effort; never blocks.
fn log_outcome(outcome: &ReportOutcome) {
    if std::env::var_os("S4F3_DISABLE_TELEMETRY").is_some() {
        return;
    }
    crate::action_log::record("report", &outcome.log_detail());
}

/// Build a sanitized Tier-1 crash report from the panic's STATIC message + our
/// own panic SITE. Reuses the `&'static str` discipline from the panic hook:
/// only a source-literal message (e.g. an `expect("…")` string) + the
/// `file:line` of our own code enter the report. A runtime `String` payload —
/// which could embed buffer text or a user's path — is the caller's
/// responsibility to keep out (the hook passes `&'static str` only); the SDK's
/// [`Sanitizer`] is the second line of defense (home/username/host scrub).
pub fn build_crash_report(static_msg: &'static str, location: &str) -> Report {
    let raw = Report::crash(format!("panic: {static_msg} (at {location})"))
        .with_metadata("app_version", env!("CARGO_PKG_VERSION"))
        .with_metadata("os", std::env::consts::OS);
    Sanitizer::new().sanitize(raw)
}

/// The literal, editable Tier-1 preview text the consent dialog shows the user
/// BEFORE any send. This is the transparency primitive — the user sees exactly
/// what would leave the machine.
#[must_use]
pub fn preview_text(report: &Report) -> String {
    Preview::of(report).text().to_string()
}

/// Capture a panic into the local spool. Builds the sanitized Tier-1 report,
/// then enqueues it to `<config_dir>/reports/` via the SDK's atomic spool. This
/// is the panic-hook seam: it CAPTURES + SPOOLS but transmits NOTHING — consent
/// is sought on the NEXT launch (ask-each-time) or honoured automatically
/// (`Always`), never inside the panic hook. Returns the outcome (for logging).
///
/// Best-effort and panic-safe: a spool failure inside an already-panicking
/// thread must not re-panic. The outcome is logged either way.
pub fn capture_panic(static_msg: &'static str, location: &str) -> ReportOutcome {
    let outcome = match Config::config_dir() {
        Some(dir) => match Spool::open(&dir) {
            Ok(spool) => {
                let report = build_crash_report(static_msg, location);
                match spool.enqueue(&report) {
                    Ok(_path) => ReportOutcome::Spooled,
                    Err(e) => ReportOutcome::Failed(format!("spool: {e}")),
                }
            }
            Err(e) => ReportOutcome::Failed(format!("spool-open: {e}")),
        },
        // No config dir => nowhere to spool. Surface it rather than swallow.
        None => ReportOutcome::Failed("no-config-dir".to_string()),
    };
    log_outcome(&outcome);
    outcome
}

/// Whether a stream's configured mode permits sending WITHOUT a fresh per-event
/// prompt. Only [`ReportingMode::Always`] does; `Off` and `AskEachTime` both
/// require an explicit per-event consent decision in the dialog.
#[must_use]
pub fn mode_sends_without_prompt(mode: ReportingMode) -> bool {
    mode.is_always()
}

/// Transmit ONE report through the SDK's hardened transport, consent-gated.
///
/// The `consent` argument is mandatory — there is no send path without it (the
/// SDK enforces this at the type level). The host mints the [`ConsentToken`]
/// ONLY after the user agreed in the dialog, or because the stream's mode is
/// `Always`. The transport is the SDK's [`LeanPipelineBackend`]: a static
/// User-Agent, zero redirects, bounded timeout, size-capped, NO persistent
/// identifier (only the token's ephemeral nonce). The outcome is logged.
///
/// If no endpoint is configured (the `SCR1B3_REPORT_ENDPOINT` env is unset),
/// this returns [`ReportOutcome::RefusedNoEndpoint`] and transmits nothing — the
/// report stays in the spool for a later, configured send.
pub fn send_report(report: &Report, consent: &ConsentToken) -> ReportOutcome {
    let outcome = match endpoint_from_env() {
        Some(endpoint) => {
            let backend = LeanPipelineBackend::new(TransportConfig::new(endpoint));
            match backend.send(report, consent) {
                Ok(SendOutcome::Sent) => ReportOutcome::Sent,
                Ok(SendOutcome::Failed(reason)) => ReportOutcome::Failed(reason),
                Err(e) => ReportOutcome::Failed(e.to_string()),
            }
        }
        None => ReportOutcome::RefusedNoEndpoint,
    };
    log_outcome(&outcome);
    outcome
}

/// Read the ingest endpoint from the env var, treating an empty value as unset.
fn endpoint_from_env() -> Option<String> {
    std::env::var(REPORT_ENDPOINT_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Open the local spool (if a config dir exists) so the host can drain pending
/// crash reports into the consent dialog on the next launch.
pub fn open_spool() -> Option<Spool> {
    Config::config_dir().and_then(|dir| Spool::open(&dir).ok())
}

/// The stream a spooled report belongs to (drives which consent toggle gates
/// the send). Re-exported convenience over the SDK's [`Stream`].
#[must_use]
pub fn report_stream(report: &Report) -> Stream {
    report.stream
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scoped guard that sets an env var and restores it on drop, so endpoint
    /// tests don't leak state across the process. Tests touching the endpoint
    /// env run serially via the shared lock below.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }
    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var(key).ok();
            // set_var is safe on edition 2021; ENDPOINT_LOCK serializes the
            // endpoint tests so there is no concurrent-mutation race.
            std::env::set_var(key, val);
            Self { key, prev }
        }
        fn unset(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    use std::sync::Mutex;
    static ENDPOINT_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn crash_report_is_crash_stream_and_carries_static_message() {
        let r = build_crash_report("called `Option::unwrap()` on a `None`", "src/foo.rs:42");
        assert_eq!(r.stream, Stream::CrashReports);
        assert!(r.body.contains("called `Option::unwrap()`"));
        assert!(r.body.contains("src/foo.rs:42"));
        // app_version + os metadata are attached (already sanitized).
        assert!(r.metadata.iter().any(|(k, _)| k == "app_version"));
        assert!(r.metadata.iter().any(|(k, _)| k == "os"));
    }

    #[test]
    fn preview_text_shows_the_literal_payload() {
        let r = build_crash_report("boom", "src/x.rs:1");
        let text = preview_text(&r);
        assert!(text.contains("boom"));
        assert!(text.contains("src/x.rs:1"));
    }

    #[test]
    fn only_always_mode_sends_without_a_prompt() {
        assert!(mode_sends_without_prompt(ReportingMode::Always));
        assert!(!mode_sends_without_prompt(ReportingMode::AskEachTime));
        assert!(!mode_sends_without_prompt(ReportingMode::Off));
    }

    #[test]
    fn send_without_endpoint_refuses_and_transmits_nothing() {
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _g = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        // Even WITH a consent token, an unset endpoint cannot transmit — the
        // report stays spooled and the outcome is the structured refusal (never
        // a fake Sent, never a silent drop).
        let r = build_crash_report("boom", "src/x.rs:1");
        let token = ConsentToken::granted();
        let outcome = send_report(&r, &token);
        assert_eq!(outcome, ReportOutcome::RefusedNoEndpoint);
    }

    #[test]
    fn empty_endpoint_env_is_treated_as_unset() {
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _g = EnvGuard::set(REPORT_ENDPOINT_ENV, "   ");
        assert!(
            endpoint_from_env().is_none(),
            "a whitespace-only endpoint must be treated as unset (cannot phone home)"
        );
    }

    #[test]
    fn outcome_log_details_are_stable_and_non_identifying() {
        assert_eq!(ReportOutcome::Spooled.log_detail(), "spooled");
        assert_eq!(ReportOutcome::Sent.log_detail(), "sent");
        assert_eq!(
            ReportOutcome::RefusedNoConsent.log_detail(),
            "refused-no-consent"
        );
        assert_eq!(
            ReportOutcome::RefusedNoEndpoint.log_detail(),
            "refused-no-endpoint"
        );
        // The Failed reason is NOT inlined into the log detail (no PII leak).
        assert_eq!(
            ReportOutcome::Failed("transport error: https://secret".to_string()).log_detail(),
            "failed"
        );
    }
}
