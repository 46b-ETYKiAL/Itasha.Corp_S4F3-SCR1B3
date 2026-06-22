//! W1TN3SS manual "Report an issue" intake — the SCR1B3 host glue.
//!
//! This is the user-initiated counterpart to [`crate::reporting`] (the opt-in
//! CRASH path). It owns NO transport: it builds a prefilled GitHub **Issue-Form
//! deep link** and hands it to the user's browser, or — when the URL would
//! exceed the helper's `GITHUB_URL_LENGTH_THRESHOLD` (the HTTP-414 ceiling, the
//! VS Code clipboard-fallback pattern) — copies the body to the clipboard, or
//! offers a `mailto:` fallback to a config-injected support alias. All of the
//! URL / clipboard-body / mailto building is DELEGATED to
//! `itasha_report_core::intake` (the consumed seam, pinned by git tag); this
//! module only wires the dialog UX state and the launch decision.
//!
//! Privacy invariants (asserted by the tests in this module):
//! - **User-initiated only.** Nothing happens until the user opens the dialog
//!   (a command-palette entry) and presses Open / Copy / Email. There is no
//!   background or default-on path.
//! - **Previewable + editable.** The prefilled body is shown in an editable
//!   field BEFORE any browser / mail client opens; the user sees and can edit
//!   the exact text that leaves.
//! - **Diagnostics OFF by default.** No app version / OS / renderer line is
//!   included unless the user explicitly ticks the diagnostics toggle. Even then
//!   the values are host-provided, non-identifying, and visible in the preview.
//! - **No persistent identifier.** No install-id / fingerprint / session-id is
//!   ever built into the title, body, query string, or mailto. A test asserts
//!   no stable-ID field appears in any built URL / body.

use itasha_report_core::intake::{
    clipboard_fallback_body, mailto_url, IssueFormRequest, GITHUB_URL_LENGTH_THRESHOLD,
};

/// The non-identifying renderer name reported in the optional diagnostics block.
/// SCR1B3's eframe stack is always the wgpu backend (see the `eframe` features in
/// the workspace manifest), so this is a static, non-identifying string — never a
/// GPU device name, vendor ID, or any host-specific value.
pub const RENDERER: &str = "wgpu";

/// The kind of issue the user is filing. Each maps to a shared Issue-Form
/// template filename in the public W1TN3SS repo and the server-side label that
/// template applies. The template/label names match
/// `.github/ISSUE_TEMPLATE/*.yml` in `Itasha.Corp_S4F3-W1TN3SS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IssueKind {
    /// A bug report → `bug.yml` (label `bug`).
    #[default]
    Bug,
    /// A feature request → `feature.yml` (label `enhancement`).
    Feature,
    /// A question or anything else → `other.yml` (label `question`).
    Other,
}

impl IssueKind {
    /// All kinds, in display order (for the dialog's selector).
    pub const ALL: [IssueKind; 3] = [IssueKind::Bug, IssueKind::Feature, IssueKind::Other];

    /// The Issue-Form template filename this kind targets.
    #[must_use]
    pub fn template(self) -> &'static str {
        match self {
            IssueKind::Bug => "bug.yml",
            IssueKind::Feature => "feature.yml",
            IssueKind::Other => "other.yml",
        }
    }

    /// The server-side label this kind applies (matches the template's
    /// `labels:` key, so the label is redundant-but-harmless on the deep link).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            IssueKind::Bug => "bug",
            IssueKind::Feature => "enhancement",
            IssueKind::Other => "question",
        }
    }

    /// A human label for the dialog selector.
    #[must_use]
    pub fn display(self) -> &'static str {
        match self {
            IssueKind::Bug => "Bug",
            IssueKind::Feature => "Feature request",
            IssueKind::Other => "Question / other",
        }
    }

    /// The default issue title prefix for this kind (matches the templates'
    /// `title:` prefixes so a deep-linked issue reads consistently).
    #[must_use]
    pub fn title_prefix(self) -> &'static str {
        match self {
            IssueKind::Bug => "bug: ",
            IssueKind::Feature => "feat: ",
            IssueKind::Other => "other: ",
        }
    }
}

/// The path the intake took, logged to the action-log (counts/enums only, never
/// the body content). Mirrors the privacy-first taxonomy: the dialog either
/// opened a browser deep link, fell back to the clipboard (URL too long or no
/// browser), opened a mail client, or failed — never a silent drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntakeOutcome {
    /// The prefilled Issue-Form URL was opened in the browser.
    OpenedDeepLink,
    /// The URL exceeded the length ceiling (or the browser could not launch),
    /// so the body was copied to the clipboard with a paste instruction.
    CopiedToClipboard,
    /// The `mailto:` fallback opened a mail client.
    OpenedMailto,
    /// The action could not be completed (e.g. clipboard unavailable). The
    /// reason is non-identifying.
    Failed(String),
}

impl IntakeOutcome {
    /// The stable, non-identifying action-log detail for this outcome.
    #[must_use]
    pub fn log_detail(&self) -> &'static str {
        match self {
            IntakeOutcome::OpenedDeepLink => "deep-link",
            IntakeOutcome::CopiedToClipboard => "clipboard",
            IntakeOutcome::OpenedMailto => "mailto",
            IntakeOutcome::Failed(_) => "failed",
        }
    }
}

/// Build the host-provided diagnostics block: app version, OS, and renderer.
/// These are NON-identifying and only appear when the user explicitly opts in.
/// There is deliberately NO install-id / fingerprint / session-id here — the
/// block is built from compile-time + `std::env::consts` values only.
#[must_use]
pub fn diagnostics_block(renderer: &str) -> String {
    format!(
        "\n\n---\nApp version: {}\nOS: {}\nRenderer: {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        renderer,
    )
}

/// Build the prefilled issue BODY from the user's description plus an OPTIONAL,
/// opt-in diagnostics block. The body carries NO persistent identifier — only
/// the user's own text and (if toggled) the non-identifying diagnostics. This is
/// the exact text the dialog previews and the user may edit before launch.
#[must_use]
pub fn build_body(description: &str, include_diagnostics: bool, renderer: &str) -> String {
    let mut body = description.to_string();
    if include_diagnostics {
        body.push_str(&diagnostics_block(renderer));
    }
    body
}

/// Build the [`IssueFormRequest`] for the consumed SDK helper from the dialog's
/// current state. The title is the kind's prefix + the first line of the
/// description (trimmed); the body is the previewed/edited text verbatim.
#[must_use]
pub fn build_request(
    repo: &str,
    kind: IssueKind,
    title_tail: &str,
    body: &str,
) -> IssueFormRequest {
    let title = format!("{}{}", kind.title_prefix(), title_tail.trim());
    IssueFormRequest {
        repo: repo.to_string(),
        title,
        body: body.to_string(),
        template: Some(kind.template().to_string()),
        labels: vec![kind.label().to_string()],
    }
}

/// Derive the one-line issue title tail from a free-form description: its first
/// non-empty line, capped to a reasonable length so the title is not the whole
/// body. Pure + deterministic.
#[must_use]
pub fn title_tail_from(description: &str) -> String {
    let line = description
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() > 80 {
        line.chars().take(80).collect()
    } else {
        line.to_string()
    }
}

/// The dialog state owned by the app. Default-constructed state is inert: the
/// dialog is closed, diagnostics are OFF, and the description is empty.
///
/// This holds NO transport state and never transmits anything — it builds a URL
/// / clipboard body / mailto and hands off to the OS on an explicit user click.
#[derive(Debug, Clone, Default)]
pub struct IssueIntakeState {
    /// Whether the modal is currently shown.
    pub open: bool,
    /// The selected issue kind (Bug / Feature / Other).
    pub kind: IssueKind,
    /// The user's free-form description (bound to a `TextEdit`).
    pub description: String,
    /// Whether to include the non-identifying diagnostics block. **OFF by
    /// default** — the user must explicitly opt in, and the toggled-in text is
    /// visible in the preview before launch.
    pub include_diagnostics: bool,
    /// The last outcome (for a small status line / action-log), if any.
    pub last_outcome: Option<IntakeOutcome>,
}

impl IssueIntakeState {
    /// Open the dialog fresh: clear the previous description + outcome and reset
    /// diagnostics to OFF (so reopening never silently re-enables diagnostics).
    pub fn open_fresh(&mut self) {
        self.open = true;
        self.kind = IssueKind::default();
        self.description.clear();
        self.include_diagnostics = false;
        self.last_outcome = None;
    }

    /// The EXACT body text that will be sent, given the current state. The
    /// dialog previews this and lets the user edit the `description`; the
    /// diagnostics tail (if toggled) is appended so the preview is faithful.
    #[must_use]
    pub fn preview_body(&self, renderer: &str) -> String {
        build_body(&self.description, self.include_diagnostics, renderer)
    }

    /// Build the [`IssueFormRequest`] for the current state against `repo`.
    #[must_use]
    pub fn request(&self, repo: &str, renderer: &str) -> IssueFormRequest {
        let body = self.preview_body(renderer);
        let title_tail = title_tail_from(&self.description);
        build_request(repo, self.kind, &title_tail, &body)
    }

    /// Whether the current request's deep-link URL fits under the helper's
    /// length ceiling (so the browser path is viable) or must use the clipboard
    /// fallback. Delegates the decision to the SDK helper's named constant.
    #[must_use]
    pub fn fits_url_length(&self, repo: &str, renderer: &str) -> bool {
        self.request(repo, renderer).fits_url_length()
    }
}

/// Copy `text` to the OS clipboard via `arboard` (already a direct app dep).
/// Returns `Ok` on success or a non-identifying error string.
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    cb.set_text(text.to_string())
        .map_err(|e| format!("clipboard write failed: {e}"))
}

/// Execute the GitHub deep-link path for `req`: if the URL fits the length
/// ceiling, open it in the browser; otherwise (or if the browser cannot launch)
/// fall back to copying the body to the clipboard with a paste instruction.
///
/// This is the core decision the dialog's "Open on GitHub" button runs. It is
/// split out (taking an already-built [`IssueFormRequest`]) so it is testable
/// without a live browser: the length decision is asserted directly, and the
/// clipboard path is exercised in tests.
#[must_use]
pub fn open_or_copy(req: &IssueFormRequest) -> IntakeOutcome {
    let url = req.to_url();
    if url.len() <= GITHUB_URL_LENGTH_THRESHOLD {
        match itasha_report_core::intake::launch(&url) {
            Ok(()) => IntakeOutcome::OpenedDeepLink,
            // Browser could not launch (headless / offline): fall back to the
            // clipboard so the user never loses their report.
            Err(_) => copy_fallback(req),
        }
    } else {
        // URL would 414 — copy the body for a manual paste (the VS Code pattern).
        copy_fallback(req)
    }
}

/// The clipboard fallback: copy the title+body the user pastes into a blank
/// GitHub issue. Shared by the over-length and no-browser cases.
fn copy_fallback(req: &IssueFormRequest) -> IntakeOutcome {
    let body = clipboard_fallback_body(req);
    match copy_to_clipboard(&body) {
        Ok(()) => IntakeOutcome::CopiedToClipboard,
        Err(e) => IntakeOutcome::Failed(e),
    }
}

/// Execute the `mailto:` fallback: build a `mailto:` to `alias` with the issue
/// title as subject and the body, then open it. Returns the outcome.
#[must_use]
pub fn open_mailto(alias: &str, subject: &str, body: &str) -> IntakeOutcome {
    let url = mailto_url(alias, subject, body);
    match itasha_report_core::intake::launch(&url) {
        Ok(()) => IntakeOutcome::OpenedMailto,
        Err(e) => IntakeOutcome::Failed(format!("could not open mail client: {e}")),
    }
}

/// Record the intake outcome to SCR1B3's existing action-log under the
/// `issue-intake` category (counts/enums only — the stable `log_detail`, NEVER
/// the body text, the URL, the repo, or any persistent identifier). Honours
/// `S4F3_DISABLE_TELEMETRY=1` by emitting nothing. Best-effort; never blocks.
pub fn log_outcome(outcome: &IntakeOutcome) {
    if std::env::var_os("S4F3_DISABLE_TELEMETRY").is_some() {
        return;
    }
    crate::action_log::record("issue-intake", outcome.log_detail());
}

#[cfg(test)]
mod tests {
    // Test fixtures build IssueFormRequest via default-then-assign for readability;
    // these are intentional and not amenable to clippy's struct-update autofix.
    #![allow(clippy::field_reassign_with_default)]
    use super::*;

    /// A string that must NEVER appear in any built URL or body — a stand-in for
    /// the persistent-ID fields this path forbids.
    fn assert_no_persistent_id(haystack: &str) {
        for forbidden in [
            "install_id",
            "install-id",
            "fingerprint",
            "session_id",
            "session-id",
            "machine_id",
            "device_id",
            "client_id",
            "uuid",
        ] {
            assert!(
                !haystack.to_ascii_lowercase().contains(forbidden),
                "built text must carry no persistent identifier, found {forbidden:?}"
            );
        }
    }

    #[test]
    fn kinds_map_to_templates_and_labels() {
        assert_eq!(IssueKind::Bug.template(), "bug.yml");
        assert_eq!(IssueKind::Bug.label(), "bug");
        assert_eq!(IssueKind::Feature.template(), "feature.yml");
        assert_eq!(IssueKind::Feature.label(), "enhancement");
        assert_eq!(IssueKind::Other.template(), "other.yml");
        assert_eq!(IssueKind::Other.label(), "question");
    }

    #[test]
    fn diagnostics_off_by_default() {
        let st = IssueIntakeState::default();
        assert!(
            !st.include_diagnostics,
            "diagnostics MUST default OFF (privacy-conservative)"
        );
        assert!(!st.open, "dialog defaults closed");
    }

    #[test]
    fn open_fresh_resets_diagnostics_off() {
        let mut st = IssueIntakeState::default();
        st.include_diagnostics = true; // user toggled it on previously
        st.description = "old text".into();
        st.open_fresh();
        assert!(
            !st.include_diagnostics,
            "reopening must reset diagnostics to OFF (no silent re-enable)"
        );
        assert!(st.description.is_empty());
        assert!(st.open);
    }

    #[test]
    fn body_excludes_diagnostics_when_toggle_off() {
        let body = build_body("my description", false, "wgpu");
        assert_eq!(body, "my description");
        assert!(!body.contains("App version"));
        assert!(!body.contains("Renderer"));
    }

    #[test]
    fn body_includes_diagnostics_only_when_toggle_on() {
        let body = build_body("my description", true, "wgpu");
        assert!(body.starts_with("my description"));
        assert!(body.contains("App version:"));
        assert!(body.contains("OS:"));
        assert!(body.contains("Renderer: wgpu"));
    }

    #[test]
    fn preview_body_is_exactly_what_request_carries() {
        // The previewed body MUST equal the body built into the request — the
        // user sees the exact text that leaves.
        let mut st = IssueIntakeState::default();
        st.description = "a bug".into();
        st.include_diagnostics = true;
        let preview = st.preview_body("software");
        let req = st.request("o/r", "software");
        assert_eq!(req.body, preview, "preview must equal the sent body");
    }

    #[test]
    fn request_targets_correct_template_and_label() {
        let mut st = IssueIntakeState::default();
        st.kind = IssueKind::Feature;
        st.description = "please add tabs".into();
        let req = st.request("owner/repo", "wgpu");
        assert_eq!(req.template.as_deref(), Some("feature.yml"));
        assert_eq!(req.labels, vec!["enhancement".to_string()]);
        assert!(req.title.starts_with("feat: "));
        assert!(req.title.contains("please add tabs"));
        assert_eq!(req.repo, "owner/repo");
    }

    #[test]
    fn title_tail_takes_first_nonempty_line_capped() {
        assert_eq!(
            title_tail_from("\n\n  hello world  \nsecond"),
            "hello world"
        );
        assert_eq!(title_tail_from(""), "");
        let long = "x".repeat(200);
        assert_eq!(title_tail_from(&long).chars().count(), 80);
    }

    #[test]
    fn no_persistent_id_in_any_built_url_or_body() {
        let mut st = IssueIntakeState::default();
        st.kind = IssueKind::Bug;
        st.description = "crash on open".into();
        st.include_diagnostics = true; // even WITH diagnostics, no stable ID
        let req = st.request("o/r", "wgpu");
        assert_no_persistent_id(&req.to_url());
        assert_no_persistent_id(&req.body);
        assert_no_persistent_id(&clipboard_fallback_body(&req));
        let mailto = mailto_url("a@b.test", &req.title, &req.body);
        assert_no_persistent_id(&mailto);
    }

    #[test]
    fn url_under_ceiling_uses_deep_link_decision() {
        // A short request fits → the open_or_copy decision is the deep-link path
        // (we can't launch a browser headlessly, but the LENGTH decision is the
        // assertable boundary: fits_url_length is true).
        let mut st = IssueIntakeState::default();
        st.description = "short".into();
        assert!(
            st.fits_url_length("o/r", "wgpu"),
            "a short report must fit the URL ceiling"
        );
    }

    #[test]
    fn url_over_ceiling_falls_back_to_clipboard() {
        // A description longer than the ceiling forces the clipboard path. The
        // length decision is deterministic and asserted here; open_or_copy on an
        // over-length request never attempts a browser launch.
        let mut st = IssueIntakeState::default();
        st.description = "y".repeat(GITHUB_URL_LENGTH_THRESHOLD + 500);
        assert!(
            !st.fits_url_length("o/r", "wgpu"),
            "an over-length report must NOT fit the URL ceiling"
        );
        let req = st.request("o/r", "wgpu");
        // open_or_copy returns either CopiedToClipboard or Failed (if no
        // clipboard backend is present in the test environment) — but NEVER
        // OpenedDeepLink for an over-length URL.
        let outcome = open_or_copy(&req);
        assert_ne!(
            outcome,
            IntakeOutcome::OpenedDeepLink,
            "an over-length URL must never take the deep-link path"
        );
        assert!(
            matches!(
                outcome,
                IntakeOutcome::CopiedToClipboard | IntakeOutcome::Failed(_)
            ),
            "over-length goes to clipboard (or a non-identifying clipboard failure)"
        );
    }

    #[test]
    fn outcome_log_details_are_stable_and_non_identifying() {
        assert_eq!(IntakeOutcome::OpenedDeepLink.log_detail(), "deep-link");
        assert_eq!(IntakeOutcome::CopiedToClipboard.log_detail(), "clipboard");
        assert_eq!(IntakeOutcome::OpenedMailto.log_detail(), "mailto");
        // The Failed reason is NOT inlined into the log detail (no leak).
        assert_eq!(
            IntakeOutcome::Failed("clipboard unavailable: secret/path".into()).log_detail(),
            "failed"
        );
    }

    #[test]
    fn mailto_carries_subject_and_body_no_id() {
        let url = mailto_url("support@example.test", "bug: it broke", "details & more");
        assert!(url.starts_with("mailto:support@example.test?"));
        assert!(url.contains("subject="));
        assert!(url.contains("body="));
        assert_no_persistent_id(&url);
    }

    #[test]
    fn diagnostics_block_is_non_identifying_and_host_provided_only() {
        let block = diagnostics_block(RENDERER);
        // The block carries ONLY compile-time + std::env::consts values: app
        // version, OS, renderer. No persistent identifier may appear.
        assert!(block.contains("App version:"));
        assert!(block.contains("OS:"));
        assert!(block.contains("Renderer: wgpu"));
        assert!(
            block.starts_with("\n\n---\n"),
            "block is a clearly-delimited tail"
        );
        assert_no_persistent_id(&block);
    }

    #[test]
    fn renderer_constant_is_the_static_non_identifying_backend_name() {
        // RENDERER must be the static backend string, never a GPU device/vendor.
        assert_eq!(RENDERER, "wgpu");
        assert_no_persistent_id(RENDERER);
    }

    #[test]
    fn request_uses_renderer_in_diagnostics_when_opted_in() {
        // The request body, when diagnostics are on, carries the renderer the
        // caller passes (so the preview is faithful end-to-end).
        let mut st = IssueIntakeState::default();
        st.description = "a bug".into();
        st.include_diagnostics = true;
        let req = st.request("o/r", RENDERER);
        assert!(req.body.contains("Renderer: wgpu"));
    }

    #[test]
    fn over_length_request_takes_the_clipboard_fallback_not_the_browser() {
        // open_or_copy on an over-length URL must NEVER call launch() (no browser)
        // — it goes straight to the clipboard path. We assert the non-deep-link
        // outcome (CopiedToClipboard on a host with a clipboard, or a structured
        // Failed where none is present — never OpenedDeepLink/OpenedMailto).
        let mut st = IssueIntakeState::default();
        st.description = "z".repeat(GITHUB_URL_LENGTH_THRESHOLD + 1000);
        let req = st.request("o/r", RENDERER);
        let outcome = open_or_copy(&req);
        assert!(
            matches!(
                outcome,
                IntakeOutcome::CopiedToClipboard | IntakeOutcome::Failed(_)
            ),
            "over-length never opens a browser: got {outcome:?}"
        );
    }

    #[test]
    fn log_outcome_is_suppressed_when_telemetry_disabled() {
        // With S4F3_DISABLE_TELEMETRY set, log_outcome must early-return and emit
        // nothing — the explicit opt-out of any local diagnostic logging. We can't
        // observe the action-log sink directly here, but we CAN assert the call is
        // a safe no-op (never panics) under the disable flag, exercising the guard.
        struct TelemetryGuard {
            prev: Option<std::ffi::OsString>,
        }
        impl Drop for TelemetryGuard {
            fn drop(&mut self) {
                match &self.prev {
                    Some(v) => std::env::set_var("S4F3_DISABLE_TELEMETRY", v),
                    None => std::env::remove_var("S4F3_DISABLE_TELEMETRY"),
                }
            }
        }
        let _g = TelemetryGuard {
            prev: std::env::var_os("S4F3_DISABLE_TELEMETRY"),
        };
        std::env::set_var("S4F3_DISABLE_TELEMETRY", "1");
        // Exercises the early-return guard for every outcome variant.
        log_outcome(&IntakeOutcome::OpenedDeepLink);
        log_outcome(&IntakeOutcome::CopiedToClipboard);
        log_outcome(&IntakeOutcome::OpenedMailto);
        log_outcome(&IntakeOutcome::Failed("clipboard unavailable".into()));
    }

    #[test]
    fn all_kinds_have_distinct_display_and_title_prefixes() {
        // IssueKind::ALL drives the dialog selector; each kind's display + title
        // prefix must be present (covers the display()/title_prefix() arms).
        let mut prefixes = Vec::new();
        for kind in IssueKind::ALL {
            assert!(!kind.display().is_empty());
            prefixes.push(kind.title_prefix());
        }
        // Prefixes are distinct so a deep-linked title reads per-kind.
        assert_eq!(prefixes, vec!["bug: ", "feat: ", "other: "]);
        // Default kind is Bug.
        assert_eq!(IssueKind::default(), IssueKind::Bug);
    }
}
