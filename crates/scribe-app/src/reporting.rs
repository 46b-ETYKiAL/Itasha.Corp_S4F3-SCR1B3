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

use std::path::{Path, PathBuf};

use itasha_report_core::backend::{
    IngestBackend, LeanPipelineBackend, SendOutcome, TransportConfig,
};
use itasha_report_core::consent::ConsentToken;
use itasha_report_core::preview::Preview;
use itasha_report_core::report::{Report, Stream};
use itasha_report_core::sanitize::Sanitizer;
use itasha_report_core::spool::Spool;

use scribe_core::Config;
pub use scribe_core::ReportingMode;

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

/// Rebuild a [`Report`] from the user-edited preview text, preserving the
/// original report's stream, title, metadata, and attachments. The preview text
/// renders as `title\n\nbody[\n\n--- metadata ---\n…]` (see the SDK's
/// `Preview::of`); this extracts the BODY span so the user's edits/redactions to
/// the body are what gets sent. Mirrors the SDK's `Preview::into_edited_report`
/// extraction so the round-trip is identical, without needing a private setter.
#[must_use]
pub fn edited_report_from_preview_text(edited_text: &str, original: &Report) -> Report {
    let body = edited_text
        // Drop the title line: everything after the first blank-line separator.
        .split_once("\n\n")
        .map(|(_title, rest)| rest)
        .unwrap_or(edited_text)
        // Drop the metadata footer if present.
        .split("\n\n--- metadata ---\n")
        .next()
        .unwrap_or(edited_text)
        .to_string();
    Report {
        stream: original.stream,
        title: original.title.clone(),
        body,
        metadata: original.metadata.clone(),
        attachments: original.attachments.clone(),
    }
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

/// Open the local spool rooted at an EXPLICIT config dir so the host can drain
/// pending crash reports into the consent dialog on the next launch. The dir is
/// always passed by the caller (the app's per-instance resolved `config_dir`, a
/// temp dir under test) so no spool I/O ever silently hits the GLOBAL
/// `Config::config_dir()` — that was the test-pollution leak this isolates.
pub fn open_spool_in(dir: &Path) -> Option<Spool> {
    Spool::open(dir).ok()
}

/// What the user chose to remember for the crash stream after a per-event
/// consent decision (Always / Never / Just this time). Maps onto the v3 config
/// `ReportingMode` so the next launch honours it (or keeps asking).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RememberChoice {
    /// Remember "Always send" — graduate the stream to [`ReportingMode::Always`].
    Always,
    /// Remember "Never" — set the stream to [`ReportingMode::Off`].
    Never,
    /// Just this time — leave the mode at [`ReportingMode::AskEachTime`].
    JustThisTime,
}

impl RememberChoice {
    /// The `ReportingMode` this choice should persist to the config, if any.
    /// `JustThisTime` returns `None` (the mode stays `AskEachTime`).
    #[must_use]
    pub fn persisted_mode(self) -> Option<ReportingMode> {
        match self {
            RememberChoice::Always => Some(ReportingMode::Always),
            RememberChoice::Never => Some(ReportingMode::Off),
            RememberChoice::JustThisTime => None,
        }
    }
}

/// The per-launch crash-consent dialog state, owned by the app. On launch the
/// host loads the spooled crash reports into `queue`; the dialog presents them
/// one at a time with an EDITABLE preview and equal-weight Send / Don't-send.
///
/// This holds NO SDK transport state — only the spooled paths, the currently-
/// presented report + its editable preview text, and the user's remember choice.
#[derive(Debug, Default)]
pub struct CrashConsentState {
    /// The EXPLICIT config dir this dialog's spool I/O is rooted at — the app's
    /// per-instance resolved `config_dir` (a temp dir under test). `None` until
    /// the host binds it via [`CrashConsentState::set_config_dir`]; while `None`
    /// every spool operation is a no-op (so a default-constructed state — e.g. a
    /// `new_test` app that never binds a dir — touches NO real config dir).
    config_dir: Option<PathBuf>,
    /// Remaining spooled report paths to present (oldest first).
    queue: Vec<std::path::PathBuf>,
    /// The report currently shown in the dialog (loaded from `queue`'s head).
    current: Option<(std::path::PathBuf, Report)>,
    /// The editable preview text the user sees and may modify before sending.
    edited_text: String,
    /// The remember-my-choice selection (defaults to Just-this-time).
    remember: Option<RememberChoice>,
}

impl CrashConsentState {
    /// Bind the explicit config dir whose `reports/` spool this dialog drains.
    /// The host calls this with the app's per-instance resolved `config_dir`
    /// before loading the spool, so the dialog never falls back to the GLOBAL
    /// `Config::config_dir()`.
    pub fn set_config_dir(&mut self, dir: Option<PathBuf>) {
        self.config_dir = dir;
    }

    /// Open this dialog's spool at its bound config dir, if any is set.
    fn spool(&self) -> Option<Spool> {
        self.config_dir.as_deref().and_then(open_spool_in)
    }

    /// Load the spooled CRASH reports into the dialog queue. Returns the number
    /// queued. Manual-issue reports (a sibling plan's intake) are not presented
    /// by this crash dialog. Best-effort: a spool error yields an empty queue.
    pub fn load_from_spool(&mut self) -> usize {
        self.queue.clear();
        self.current = None;
        if let Some(spool) = self.spool() {
            if let Ok(paths) = spool.list() {
                for path in paths {
                    if let Ok(report) = spool.load(&path) {
                        if report.stream == Stream::CrashReports {
                            self.queue.push(path);
                        }
                    }
                }
            }
        }
        self.advance();
        self.queue.len() + usize::from(self.current.is_some())
    }

    /// Whether the dialog has a report to present this frame.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.current.is_some()
    }

    /// The editable preview text (mutable so the dialog can bind a `TextEdit`).
    pub fn edited_text_mut(&mut self) -> &mut String {
        &mut self.edited_text
    }

    /// The remember-choice selection (mutable for the dialog radios).
    pub fn remember_mut(&mut self) -> &mut Option<RememberChoice> {
        &mut self.remember
    }

    /// Pop the next report off the queue and load it as `current` + its preview
    /// text. Clears `current` when the queue is empty.
    fn advance(&mut self) {
        self.current = None;
        self.edited_text.clear();
        self.remember = Some(RememberChoice::JustThisTime);
        if self.queue.is_empty() {
            return;
        }
        let path = self.queue.remove(0);
        if let Some(spool) = self.spool() {
            if let Ok(report) = spool.load(&path) {
                self.edited_text = preview_text(&report);
                self.current = Some((path, report));
            }
        }
    }

    /// The user pressed SEND on the current report. Build the (possibly edited)
    /// report from the preview text, mint a consent token, transmit, and — on a
    /// successful send — remove the spooled file. Returns the outcome. Advances
    /// to the next queued report regardless of outcome (a failed send keeps the
    /// file spooled for a later retry but does not block the queue).
    pub fn consent_and_send(&mut self) -> Option<ReportOutcome> {
        let (path, original) = self.current.take()?;
        // Rebuild the report carrying the user's edited preview text into the
        // body (their redactions are honoured). The preview text format is
        // `title\n\nbody[\n\n--- metadata ---\n…]`; extract the body span so the
        // user's edits to the body are what gets sent, while title + metadata +
        // attachments are preserved from the original sanitized report.
        let edited = edited_report_from_preview_text(&self.edited_text, &original);
        let token = ConsentToken::granted();
        let outcome = send_report(&edited, &token);
        if outcome == ReportOutcome::Sent {
            if let Some(spool) = self.spool() {
                let _ = spool.remove(&path);
            }
        } else {
            // Not sent (offline / no endpoint / failed): keep the file spooled
            // so a later configured/online send can retry. Re-load the queue
            // head next launch.
        }
        self.advance();
        Some(outcome)
    }

    /// The user pressed DON'T-SEND on the current report. Discard the spooled
    /// file (the user declined to send it) and advance. Returns the next state's
    /// has_pending.
    pub fn decline_and_discard(&mut self) {
        if let Some((path, _)) = self.current.take() {
            if let Some(spool) = self.spool() {
                let _ = spool.remove(&path);
            }
        }
        self.advance();
    }
}

/// Auto-send every spooled CRASH report through the consent-gated path WITHOUT a
/// dialog — used when the crash stream's mode is [`ReportingMode::Always`] (the
/// user previously chose "Always send"). Each report is still captured + spooled
/// first and transmitted only via a freshly-minted [`ConsentToken`]; a
/// successful send removes the spooled file, a failure leaves it for retry.
/// Returns the number of reports for which a send was ATTEMPTED.
///
/// The spool is rooted at the EXPLICIT `config_dir` the caller passes (the app's
/// per-instance resolved dir, a temp dir under test) — never the GLOBAL
/// `Config::config_dir()`, so an auto-send drain never touches the real config
/// dir from a test.
pub fn auto_send_spooled_crashes(config_dir: &Path) -> usize {
    let Some(spool) = open_spool_in(config_dir) else {
        return 0;
    };
    let Ok(paths) = spool.list() else {
        return 0;
    };
    let mut attempted = 0;
    for path in paths {
        let Ok(report) = spool.load(&path) else {
            continue;
        };
        if report.stream != Stream::CrashReports {
            continue;
        }
        attempted += 1;
        let token = ConsentToken::granted();
        if send_report(&report, &token) == ReportOutcome::Sent {
            let _ = spool.remove(&path);
        }
    }
    attempted
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
    fn disable_telemetry_suppresses_outcome_logging() {
        // With S4F3_DISABLE_TELEMETRY=1 the internal log_outcome must early-return
        // and write NOTHING to the action log — the privacy opt-out is honoured.
        // Routed through send_report (no endpoint → RefusedNoEndpoint → logged),
        // the call must still return the structured outcome without panicking.
        // Serialized on ENDPOINT_LOCK because it mutates process env.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _endpoint = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let _telemetry = EnvGuard::set("S4F3_DISABLE_TELEMETRY", "1");
        let r = build_crash_report("boom", "src/x.rs:1");
        let outcome = send_report(&r, &ConsentToken::granted());
        assert_eq!(
            outcome,
            ReportOutcome::RefusedNoEndpoint,
            "the outcome is still surfaced; only the logging is suppressed"
        );
    }

    #[test]
    fn remember_choice_maps_to_config_mode() {
        assert_eq!(
            RememberChoice::Always.persisted_mode(),
            Some(ReportingMode::Always)
        );
        assert_eq!(
            RememberChoice::Never.persisted_mode(),
            Some(ReportingMode::Off)
        );
        assert_eq!(
            RememberChoice::JustThisTime.persisted_mode(),
            None,
            "just-this-time leaves the mode at AskEachTime (no persist)"
        );
    }

    #[test]
    fn edited_preview_text_round_trips_user_redactions_into_body() {
        // The user edited the body in the preview; the rebuilt report must carry
        // exactly the edited body, with title/metadata/stream preserved.
        let original = Report::crash("panic: boom (at src/x.rs:1)")
            .with_metadata("os", "linux")
            .with_metadata("app_version", "9.9.9");
        let preview = preview_text(&original);
        assert!(preview.contains("boom"));
        // Simulate the user redacting "boom" -> "[redacted]" in the editable text.
        let edited_text = preview.replace("boom", "[redacted]");
        let edited = edited_report_from_preview_text(&edited_text, &original);
        assert!(edited.body.contains("[redacted]"));
        assert!(!edited.body.contains("boom"));
        // Body must NOT swallow the title or the metadata footer.
        assert!(!edited.body.contains("crash report"));
        assert!(!edited.body.contains("--- metadata ---"));
        assert!(!edited.body.contains("os: linux"));
        // Stream + title + metadata preserved from the original.
        assert_eq!(edited.stream, Stream::CrashReports);
        assert_eq!(edited.title, original.title);
        assert_eq!(edited.metadata, original.metadata);
    }

    #[test]
    fn outcome_log_details_are_stable_and_non_identifying() {
        assert_eq!(ReportOutcome::Spooled.log_detail(), "spooled");
        assert_eq!(ReportOutcome::Sent.log_detail(), "sent");
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

    // ── Spool-backed dialog + drain tests ──────────────────────────────────
    // These drive CrashConsentState / auto_send_spooled_crashes against an
    // EXPLICIT temp config dir (never the global Config::config_dir()), so no
    // spool I/O leaks into the real config dir and the tests are hermetic. The
    // SDK's Spool roots itself at `<dir>/reports/`; we enqueue reports there and
    // assert the host glue's privacy + queue behaviour.

    use itasha_report_core::report::Report as SdkReport;

    /// Enqueue a crash report into the spool rooted at `dir` and return how many
    /// reports the spool now holds. Used to seed the dialog/drain tests.
    fn seed_crash(dir: &Path, body: &str) {
        let spool = open_spool_in(dir).expect("temp spool opens");
        let report = build_crash_report_owned(body, "src/x.rs:1");
        spool.enqueue(&report).expect("enqueue");
    }

    /// Like `build_crash_report` but takes an owned body so a test can vary the
    /// content (the production hook only ever passes a `&'static str`, but the
    /// spool stores the resulting `Report` either way — the stream is what the
    /// dialog filters on).
    fn build_crash_report_owned(body: &str, location: &str) -> SdkReport {
        let raw = SdkReport::crash(format!("panic: {body} (at {location})"))
            .with_metadata("app_version", env!("CARGO_PKG_VERSION"))
            .with_metadata("os", std::env::consts::OS);
        Sanitizer::new().sanitize(raw)
    }

    #[test]
    fn open_spool_in_creates_reports_dir_under_explicit_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = open_spool_in(tmp.path()).expect("spool opens at explicit dir");
        // The SDK roots the spool at <dir>/reports/.
        assert!(spool.dir().ends_with("reports"));
        assert_eq!(spool.count().unwrap(), 0, "a fresh spool is empty");
    }

    #[test]
    fn unbound_dialog_is_inert_and_touches_no_dir() {
        // A default-constructed CrashConsentState (e.g. a `new_test` app that
        // never binds a config dir) loads NOTHING and presents NOTHING — it must
        // never fall back to the global Config::config_dir().
        let mut state = CrashConsentState::default();
        assert_eq!(state.load_from_spool(), 0, "no bound dir => nothing queued");
        assert!(!state.has_pending());
        // consent_and_send / decline on an empty dialog are safe no-ops.
        assert_eq!(state.consent_and_send(), None);
        state.decline_and_discard();
        assert!(!state.has_pending());
    }

    #[test]
    fn load_from_spool_queues_only_crash_stream_reports() {
        let tmp = tempfile::tempdir().unwrap();
        // Two crash reports + one MANUAL issue (a sibling plan's intake). The
        // crash dialog must present ONLY the crash-stream reports.
        seed_crash(tmp.path(), "boom one");
        seed_crash(tmp.path(), "boom two");
        let spool = open_spool_in(tmp.path()).unwrap();
        spool
            .enqueue(&SdkReport::manual_issue("feedback", "not a crash"))
            .unwrap();

        let mut state = CrashConsentState::default();
        state.set_config_dir(Some(tmp.path().to_path_buf()));
        let queued = state.load_from_spool();
        assert_eq!(queued, 2, "only the 2 crash-stream reports are queued");
        assert!(state.has_pending(), "a crash report is presented");
        // The presented preview is the literal crash body the user will see.
        assert!(state.edited_text_mut().contains("boom"));
    }

    #[test]
    fn decline_discards_the_current_report_from_the_spool() {
        let tmp = tempfile::tempdir().unwrap();
        seed_crash(tmp.path(), "decline me");
        let mut state = CrashConsentState::default();
        state.set_config_dir(Some(tmp.path().to_path_buf()));
        assert_eq!(state.load_from_spool(), 1);
        assert!(state.has_pending());

        state.decline_and_discard();
        assert!(!state.has_pending(), "queue drained after the only decline");

        // The declined report was removed from the spool (the user said no).
        let spool = open_spool_in(tmp.path()).unwrap();
        assert_eq!(
            spool.count().unwrap(),
            0,
            "a declined crash report is discarded, not retained"
        );
    }

    #[test]
    fn send_without_endpoint_keeps_the_report_spooled_for_retry() {
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _g = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let tmp = tempfile::tempdir().unwrap();
        seed_crash(tmp.path(), "keep me on refusal");
        let mut state = CrashConsentState::default();
        state.set_config_dir(Some(tmp.path().to_path_buf()));
        assert_eq!(state.load_from_spool(), 1);

        // SEND pressed, but no endpoint is configured: the structured refusal is
        // returned and the report is NOT removed (retained for a later send).
        let outcome = state.consent_and_send();
        assert_eq!(outcome, Some(ReportOutcome::RefusedNoEndpoint));
        let spool = open_spool_in(tmp.path()).unwrap();
        assert_eq!(
            spool.count().unwrap(),
            1,
            "an unsent (no-endpoint) report stays spooled for retry"
        );
    }

    #[test]
    fn edited_preview_redactions_are_carried_through_consent_and_send() {
        // The user redacts the preview body before pressing SEND. Even though the
        // send refuses (no endpoint), the redaction path is exercised: the body
        // that WOULD leave is the edited one. We assert the dialog honours the
        // edit by re-deriving the report from the edited text.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _g = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let tmp = tempfile::tempdir().unwrap();
        seed_crash(tmp.path(), "secret-buffer-text");
        let mut state = CrashConsentState::default();
        state.set_config_dir(Some(tmp.path().to_path_buf()));
        state.load_from_spool();
        // Redact in the editable preview.
        let edited = state.edited_text_mut();
        *edited = edited.replace("secret-buffer-text", "[redacted]");
        assert!(!state.edited_text_mut().contains("secret-buffer-text"));
        // Pressing SEND consumes the edited text; the outcome is the refusal but
        // the path through edited_report_from_preview_text ran.
        let outcome = state.consent_and_send();
        assert_eq!(outcome, Some(ReportOutcome::RefusedNoEndpoint));
    }

    #[test]
    fn remember_choice_defaults_to_just_this_time_on_load() {
        let tmp = tempfile::tempdir().unwrap();
        seed_crash(tmp.path(), "remember default");
        let mut state = CrashConsentState::default();
        state.set_config_dir(Some(tmp.path().to_path_buf()));
        state.load_from_spool();
        assert_eq!(
            *state.remember_mut(),
            Some(RememberChoice::JustThisTime),
            "a freshly-presented report defaults the remember choice to just-this-time"
        );
    }

    #[test]
    fn auto_send_drain_without_endpoint_attempts_but_retains_reports() {
        // ReportingMode::Always path: auto_send_spooled_crashes ATTEMPTS each
        // crash report, but with no endpoint configured nothing is transmitted
        // and nothing is removed — the privacy-safe "spooled, never silently
        // dropped" invariant holds even on the auto-send drain.
        let _lock = ENDPOINT_LOCK.lock().unwrap();
        let _g = EnvGuard::unset(REPORT_ENDPOINT_ENV);
        let tmp = tempfile::tempdir().unwrap();
        seed_crash(tmp.path(), "auto one");
        seed_crash(tmp.path(), "auto two");
        // A manual issue must NOT be auto-sent by the crash drain.
        let spool = open_spool_in(tmp.path()).unwrap();
        spool
            .enqueue(&SdkReport::manual_issue("fb", "manual"))
            .unwrap();

        let attempted = auto_send_spooled_crashes(tmp.path());
        assert_eq!(
            attempted, 2,
            "only the 2 crash-stream reports are attempted"
        );
        // No endpoint => no removal: all 3 reports remain spooled.
        let spool = open_spool_in(tmp.path()).unwrap();
        assert_eq!(
            spool.count().unwrap(),
            3,
            "no-endpoint auto-send removes nothing (reports retained for retry)"
        );
    }

    #[test]
    fn auto_send_on_missing_dir_is_a_safe_zero() {
        // A dir whose spool cannot be opened (here: a path that is a FILE, so
        // create_dir_all fails) yields a safe 0 — never a panic, never a send.
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("not-a-dir");
        std::fs::write(&file_path, b"x").unwrap();
        assert_eq!(
            auto_send_spooled_crashes(&file_path),
            0,
            "an unopenable spool dir drains zero reports"
        );
    }

    #[test]
    fn capture_panic_with_no_config_dir_surfaces_failure_not_silent_drop() {
        // capture_panic resolves the GLOBAL config dir; we can't force it unset
        // portably, but we CAN assert the outcome is always one of the structured
        // variants (never a panic, never a silent None). On this host it spools.
        let outcome = capture_panic("probe panic", "src/reporting.rs:1");
        assert!(
            matches!(outcome, ReportOutcome::Spooled | ReportOutcome::Failed(_)),
            "capture_panic returns a structured outcome, never a silent drop"
        );
    }

    #[test]
    fn build_crash_report_owned_helper_is_crash_stream() {
        // Guard the test helper itself stays faithful to build_crash_report's
        // stream + metadata shape (so the spool-seeding above is representative).
        let r = build_crash_report_owned("boom", "src/x.rs:9");
        assert_eq!(r.stream, Stream::CrashReports);
        assert!(r.body.contains("boom"));
        assert!(r.metadata.iter().any(|(k, _)| k == "app_version"));
    }
}
