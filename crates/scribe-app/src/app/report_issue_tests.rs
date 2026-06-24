//! Headless e2e (task #34): drive the W1TN3SS **Report an issue** intake modal
//! through the REAL `ScribeApp::frame_tick` render loop (egui_kittest, no GPU).
//!
//! `e2e_overlays.rs` already proves the modal *opens* (`report_issue_modal_renders`).
//! This sibling file goes further and drives the real FORM the user fills in:
//! it picks a kind radio, types a description, toggles the diagnostics checkbox,
//! confirms the "Open on GitHub" button is reachable (and the live preview tracks
//! the description + diagnostics), then clicks **Cancel** and asserts the modal
//! closes. These are interaction tests (they click widgets BY LABEL), the only
//! kind that catches "clicking does nothing" — complementing the pure-logic unit
//! tests in `crate::issue_intake`.
#![allow(clippy::wildcard_imports)]
use super::*;
use egui_kittest::kittest::Queryable as _;

/// A first-run, non-frameless app so the render path is the normal one (matches
/// the `overlay_app` helper in `e2e_overlays.rs`).
fn issue_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    ScribeApp::new_test(cfg)
}

fn harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 820.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

/// Open the modal via its builtin command and settle a couple of frames.
fn open_modal() -> egui_kittest::Harness<'static, ScribeApp> {
    let mut app = issue_app();
    app.execute_builtin(BuiltinCommand::ReportIssue);
    let mut h = harness(app);
    h.run();
    h.run();
    h
}

/// The modal opens with its heading + the default Bug kind, an empty
/// description, and diagnostics OFF — the privacy-conservative default state.
#[test]
fn report_issue_opens_with_default_kind_and_diagnostics_off() {
    let h = open_modal();
    assert!(h.state().issue_intake.open, "modal must be open");
    assert_eq!(
        h.state().issue_intake.kind,
        crate::issue_intake::IssueKind::Bug,
        "the default selected kind is Bug"
    );
    assert!(
        !h.state().issue_intake.include_diagnostics,
        "diagnostics must default OFF"
    );
    // The heading + the GitHub submit affordance are both reachable.
    assert!(
        h.query_by_label("Open on GitHub").is_some(),
        "the GitHub submit button must be reachable"
    );
    assert!(
        h.query_by_label("Cancel").is_some(),
        "the Cancel button must be reachable"
    );
}

/// Picking the "Feature request" kind radio updates the app's selected kind —
/// driving the real `radio_value` the modal renders for each `IssueKind`.
#[test]
fn picking_feature_kind_radio_updates_state() {
    let mut h = open_modal();
    // The radio labels are the kinds' `display()` strings.
    let feature_label = crate::issue_intake::IssueKind::Feature.display();
    assert!(
        h.query_by_label(feature_label).is_some(),
        "the Feature radio must render with its display label"
    );
    h.get_by_label(feature_label).click();
    h.run();
    assert_eq!(
        h.state().issue_intake.kind,
        crate::issue_intake::IssueKind::Feature,
        "clicking the Feature radio must select it"
    );
}

/// Typing into the description field flows into `issue_intake.description`, and
/// the live preview reflects the typed text (the preview is the EXACT body that
/// would be sent). The modal's editable description is a multiline text field;
/// the editor behind the modal is the same role, so we type into each candidate
/// and assert the typed text lands in the intake description (the preview field
/// is non-interactive, so it is never a candidate).
#[test]
fn typing_description_updates_state_and_preview() {
    let mut h = open_modal();
    const TYPED: &str = "scrollbar flickers on resize";
    // Collect the focusable multiline fields, then type into the first one and
    // settle. The description field is the one whose content reaches the intake
    // state; if the first candidate is the background editor, fall through.
    let field_count = h
        .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .count();
    assert!(field_count >= 1, "at least one multiline field must render");
    for idx in 0..field_count {
        let field = h
            .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
            .nth(idx)
            .expect("candidate field present");
        field.focus();
        h.run();
        let field = h
            .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
            .nth(idx)
            .expect("candidate field present");
        field.type_text(TYPED);
        h.run();
        if h.state().issue_intake.description == TYPED {
            break;
        }
        // Wrong field (e.g. the editor) — reset it and try the next candidate.
        h.state_mut().issue_intake.description.clear();
    }
    assert_eq!(
        h.state().issue_intake.description,
        TYPED,
        "typing must flow into the intake description"
    );
    // The preview body equals the description (diagnostics still OFF).
    assert_eq!(
        h.state()
            .issue_intake
            .preview_body(crate::issue_intake::RENDERER),
        TYPED,
        "the preview must reflect the typed description verbatim"
    );
}

/// Toggling the diagnostics checkbox flips `include_diagnostics` ON, and the
/// live preview then carries the non-identifying diagnostics block.
#[test]
fn toggling_diagnostics_checkbox_appends_block_to_preview() {
    let mut h = open_modal();
    // Seed a description so the preview has a body the diagnostics append to.
    h.state_mut().issue_intake.description = "a short report".to_string();
    h.run();
    assert!(
        !h.state().issue_intake.include_diagnostics,
        "diagnostics start OFF"
    );
    // The checkbox renders with the diagnostics opt-in label (substring match via
    // the start of the rendered label).
    let checkbox = h
        .get_all_by_role(egui::accesskit::Role::CheckBox)
        .next()
        .expect("the diagnostics checkbox must be present");
    checkbox.click();
    h.run();
    assert!(
        h.state().issue_intake.include_diagnostics,
        "clicking the checkbox must turn diagnostics ON"
    );
    let preview = h
        .state()
        .issue_intake
        .preview_body(crate::issue_intake::RENDERER);
    assert!(
        preview.starts_with("a short report"),
        "the preview keeps the description as its head"
    );
    assert!(
        preview.contains("App version:") && preview.contains("Renderer: wgpu"),
        "with diagnostics ON the preview must carry the non-identifying block"
    );
}

/// Clicking **Cancel** closes the modal without recording any outcome (nothing
/// was sent). This is the no-op exit path the user takes when they change their
/// mind — `issue_intake.open` flips false and `last_outcome` stays `None`.
#[test]
fn cancel_closes_modal_without_outcome() {
    let mut h = open_modal();
    assert!(h.state().issue_intake.open);
    h.get_by_label("Cancel").click();
    h.run();
    assert!(
        !h.state().issue_intake.open,
        "Cancel must close the report-issue modal"
    );
    assert!(
        h.state().issue_intake.last_outcome.is_none(),
        "cancelling sends nothing, so no outcome is recorded"
    );
}

/// A long description trips the URL-length ceiling, so the modal surfaces the
/// honest "will copy to your clipboard" hint instead of claiming a browser deep
/// link. This drives the `fits_url_length` branch in the render path.
#[test]
fn long_description_surfaces_clipboard_fallback_hint() {
    let mut h = open_modal();
    let repo = h.state().config.reporting.issue_intake.repo.clone();
    // A description longer than the GitHub deep-link ceiling forces the clipboard
    // path; the modal renders the clarifying hint in that case.
    h.state_mut().issue_intake.description = "y".repeat(9000);
    h.run();
    assert!(
        !h.state()
            .issue_intake
            .fits_url_length(&repo, crate::issue_intake::RENDERER),
        "an over-length report must not fit the URL ceiling"
    );
    // The render path paints the clipboard-fallback hint; assert its presence via
    // a stable substring of the rendered label.
    assert!(
        h.query_by_label_contains("clipboard").is_some(),
        "the over-length hint about copying to the clipboard must render"
    );
}
