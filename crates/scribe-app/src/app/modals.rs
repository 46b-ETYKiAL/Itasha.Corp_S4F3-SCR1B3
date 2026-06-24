//! Modal + notification rendering: the once-per-launch update reminder plus the crash-consent, report-issue, and update-prompt modals. Bodies moved verbatim from the `app` god-module (A-01 decomposition); `use super::*` re-exports the types these methods touch.
#![allow(clippy::wildcard_imports)]
use super::*;

impl ScribeApp {
    /// Once per launch: if the user opted into automatic update checks
    /// (`updates.mode` = `notify` or `auto`) and the configured interval has
    /// elapsed, run a single GitHub-Releases version check on a background
    /// thread. `notify` then surfaces a passive toast if an update is found;
    /// `auto` opens a yes/no modal. `off` and `manual` do NO network at all —
    /// the telemetry-free default. The check itself only reads the public
    /// Releases API and sends no identifiers (see PRIVACY.md). `is_check_due` +
    /// the persisted `last_check_unix` honour the interval across sessions.
    pub(super) fn maybe_remind_update(&mut self, ctx: &egui::Context) {
        if self.did_update_check {
            return;
        }
        self.did_update_check = true;
        // First frame of the launch: reap update artifacts that are no longer
        // needed now that this (possibly just-updated) build is running — the
        // staging download dir (incl. a completed installer's setup.exe, which
        // couldn't be deleted while it ran) and the prior binary's `.bak`.
        crate::updater::cleanup_after_update();
        let Some(kind) = update_launch_action(
            self.config.updates.mode,
            self.config.updates.last_check_unix,
            self.config.updates.check_interval_hours as u64,
            now_unix(),
        ) else {
            return;
        };
        // Only AUTO consumes the interval throttle, so only Auto needs the
        // timestamp persisted. Notify checks every launch and ignores
        // `last_check_unix`, so stamping it here would just be a needless config
        // write on every launch (and re-coupling it to the manual-check field
        // this fix deliberately decoupled).
        if matches!(kind, crate::updater::LaunchKind::Auto) {
            self.config.updates.last_check_unix = Some(now_unix());
            self.save_config();
        }
        self.updater.start_check(ctx, kind);
    }

    /// W1TN3SS opt-in crash-consent modal (ask-each-time). Renders only when a
    /// prior session's panic hook spooled a crash report AND the user opted the
    /// crash stream into AskEachTime. It shows the LITERAL, EDITABLE Tier-1 text
    /// payload (the user can read + redact exactly what would be sent), a "what
    /// is never included" note, a remember-my-choice selector (Always / Never /
    /// Just this time), and EQUAL-WEIGHT Send / Don't-send buttons (identical
    /// affordance, no dark-pattern asymmetry, no pre-selected default — GDPR
    /// "freely given"). The panic hook never auto-sends; transmission happens
    /// ONLY here, on the user's affirmative Send.
    pub(super) fn render_crash_consent(&mut self, ctx: &egui::Context) {
        if !self.crash_consent.has_pending() {
            return;
        }
        // Defer the mutating send/decline past the borrow inside the closure.
        enum Decision {
            Send,
            Dont,
        }
        let mut decision: Option<Decision> = None;
        egui::Modal::new(egui::Id::new("scr1b3_crash_consent")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading("Send a crash report?");
            ui.add_space(6.0);
            ui.label(
                "SCR1B3 closed unexpectedly last time. You can send the report below to help \
                 fix it — or not. Nothing is sent unless you choose to send it, and you can \
                 edit the text first.",
            );
            ui.add_space(8.0);

            ui.label(egui::RichText::new("This is exactly what would be sent:").strong());
            ui.add_space(2.0);
            // The literal, EDITABLE Tier-1 payload. A multiline TextEdit binds the
            // preview text so the user can read AND redact it before sending.
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(self.crash_consent.edited_text_mut())
                            .desired_width(f32::INFINITY)
                            .desired_rows(6)
                            .code_editor(),
                    )
                    .on_hover_text(
                        "Edit or delete anything here before sending. Only this text is sent.",
                    );
                });
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "Never included: your documents, file paths, username, computer name, or any \
                     tracking ID.",
                )
                .weak()
                .small(),
            );
            ui.add_space(10.0);

            // Remember-my-choice: Always / Never / Just this time (equal-weight
            // radios; default is Just-this-time, so neither Always nor Never is
            // privileged).
            ui.label(egui::RichText::new("For future crashes:").small());
            let remember = self.crash_consent.remember_mut();
            ui.horizontal(|ui| {
                ui.radio_value(
                    remember,
                    Some(crate::reporting::RememberChoice::JustThisTime),
                    "Ask me each time",
                );
                ui.radio_value(
                    remember,
                    Some(crate::reporting::RememberChoice::Always),
                    "Always send",
                );
                ui.radio_value(
                    remember,
                    Some(crate::reporting::RememberChoice::Never),
                    "Never send",
                );
            });
            ui.add_space(12.0);

            // EQUAL-WEIGHT Send / Don't-send: both are plain Buttons sized to the
            // SAME explicit width, laid out side by side, neither pre-focused or
            // colour-emphasised. This is both a GDPR "freely given" requirement
            // and a WCAG no-dark-pattern affordance.
            let btn_size = egui::vec2(150.0, 28.0);
            ui.horizontal(|ui| {
                if ui
                    .add_sized(btn_size, egui::Button::new("Send report"))
                    .clicked()
                {
                    decision = Some(Decision::Send);
                }
                if ui
                    .add_sized(btn_size, egui::Button::new("Don't send"))
                    .clicked()
                {
                    decision = Some(Decision::Dont);
                }
            });
        });

        match decision {
            Some(Decision::Send) => {
                // Persist a remembered Always/Never choice to the v3 config BEFORE
                // sending, so the next launch honours it. Just-this-time leaves the
                // mode at AskEachTime.
                if let Some(choice) = *self.crash_consent.remember_mut() {
                    if let Some(mode) = choice.persisted_mode() {
                        self.config.reporting.crash_reports = mode;
                        self.save_config();
                    }
                }
                self.crash_consent.consent_and_send();
            }
            Some(Decision::Dont) => {
                if let Some(choice) = *self.crash_consent.remember_mut() {
                    if let Some(mode) = choice.persisted_mode() {
                        self.config.reporting.crash_reports = mode;
                        self.save_config();
                    }
                }
                self.crash_consent.decline_and_discard();
            }
            None => {}
        }
    }

    /// W1TN3SS user-initiated "Report an issue" modal. Opened from the command
    /// palette (`BuiltinCommand::ReportIssue` → `issue_intake.open_fresh()`);
    /// renders only when `issue_intake.open`. Mirrors `render_crash_consent`'s
    /// deferred-decision pattern so the mutating launch/log runs past the
    /// `&mut self` borrow held by the modal closure.
    ///
    /// Privacy invariants this UI upholds (the logic is unit-tested in
    /// `crate::issue_intake`): nothing leaves until the user clicks a button;
    /// the previewed body is the exact text that is sent; diagnostics are OFF by
    /// default and only ever appear in the preview when explicitly ticked.
    pub(super) fn render_report_issue(&mut self, ctx: &egui::Context) {
        if !self.issue_intake.open {
            return;
        }

        // The repo + mailto alias are config-injected (operator-editable), so
        // no prod values are baked unalterably into the binary.
        let repo = self.config.reporting.issue_intake.repo.clone();
        let alias = self.config.reporting.issue_intake.mailto_alias.clone();
        let renderer = crate::issue_intake::RENDERER;

        // Decisions deferred past the closure borrow (like render_crash_consent).
        enum Decision {
            Submit,
            Email,
            Cancel,
        }
        let mut decision: Option<Decision> = None;

        egui::Modal::new(egui::Id::new("scr1b3_report_issue")).show(ctx, |ui| {
            ui.set_max_width(560.0);
            ui.heading("Report an issue");
            ui.add_space(6.0);
            ui.label(
                "Tell us what happened or what you'd like. Nothing is sent until you click \
                 a button below — and you can read and edit the exact text first.",
            );
            ui.add_space(10.0);

            // Kind selector (Bug / Feature / Other).
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Kind:").strong());
                for kind in crate::issue_intake::IssueKind::ALL {
                    ui.radio_value(&mut self.issue_intake.kind, kind, kind.display());
                }
            });
            ui.add_space(8.0);

            // Free-form description.
            ui.label(egui::RichText::new("Description:").strong());
            ui.add_space(2.0);
            egui::ScrollArea::vertical()
                .id_salt("scr1b3_report_issue_desc")
                .max_height(140.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.issue_intake.description)
                            .desired_width(f32::INFINITY)
                            .desired_rows(5)
                            .hint_text("Describe the bug, request, or question…"),
                    );
                });
            ui.add_space(8.0);

            // Diagnostics opt-in — OFF by default; the toggled-in text shows up
            // in the preview below so the user always sees what it adds.
            ui.checkbox(
                &mut self.issue_intake.include_diagnostics,
                "Include non-identifying diagnostics (app version, OS, renderer)",
            );
            ui.add_space(10.0);

            // Preview of the EXACT body that will be sent — driven by the live
            // description + diagnostics toggle.
            ui.label(egui::RichText::new("This is exactly what will be sent:").strong());
            ui.add_space(2.0);
            let preview = self.issue_intake.preview_body(renderer);
            egui::ScrollArea::vertical()
                .id_salt("scr1b3_report_issue_preview")
                .max_height(140.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Read-only view of the rendered body (the editable source is
                    // the description field above; the preview reflects it +
                    // diagnostics). `&mut &str` makes TextEdit non-editable.
                    let mut shown = preview.as_str();
                    ui.add(
                        egui::TextEdit::multiline(&mut shown)
                            .desired_width(f32::INFINITY)
                            .desired_rows(5)
                            .interactive(false)
                            .code_editor(),
                    );
                });
            ui.add_space(12.0);

            // Faithful UX hint: tell the user up front whether "Open on GitHub"
            // will open a prefilled browser link or fall back to the clipboard
            // (the report is too long for a GitHub deep link — the HTTP-414
            // ceiling). This is the same length decision `open_or_copy` makes.
            if !self.issue_intake.fits_url_length(&repo, renderer) {
                ui.label(
                    egui::RichText::new(
                        "This report is long, so \"Open on GitHub\" will copy it to your \
                         clipboard to paste into a new issue.",
                    )
                    .weak()
                    .small(),
                );
                ui.add_space(6.0);
            }

            // Buttons: Open on GitHub (deep-link, with clipboard fallback) /
            // Email instead (mailto) / Cancel. The mailto button is disabled when
            // no support alias is configured.
            let btn = egui::vec2(150.0, 28.0);
            ui.horizontal(|ui| {
                if ui
                    .add_sized(btn, egui::Button::new("Open on GitHub"))
                    .clicked()
                {
                    decision = Some(Decision::Submit);
                }
                if ui
                    .add_enabled(!alias.is_empty(), egui::Button::new("Email instead"))
                    .clicked()
                {
                    decision = Some(Decision::Email);
                }
                if ui.add_sized(btn, egui::Button::new("Cancel")).clicked() {
                    decision = Some(Decision::Cancel);
                }
            });

            // A small status line reflecting the last outcome.
            if let Some(outcome) = &self.issue_intake.last_outcome {
                ui.add_space(8.0);
                let msg = match outcome {
                    crate::issue_intake::IntakeOutcome::OpenedDeepLink => {
                        "Opened a prefilled issue in your browser.".to_string()
                    }
                    crate::issue_intake::IntakeOutcome::CopiedToClipboard => {
                        "The report was copied to your clipboard — paste it into a new \
                         GitHub issue."
                            .to_string()
                    }
                    crate::issue_intake::IntakeOutcome::OpenedMailto => {
                        "Opened your mail client.".to_string()
                    }
                    crate::issue_intake::IntakeOutcome::Failed(_) => {
                        "Could not complete the action — please try Email instead.".to_string()
                    }
                };
                ui.label(egui::RichText::new(msg).weak().small());
            }
        });

        match decision {
            Some(Decision::Submit) => {
                let req = self.issue_intake.request(&repo, renderer);
                let outcome = crate::issue_intake::open_or_copy(&req);
                crate::issue_intake::log_outcome(&outcome);
                self.issue_intake.last_outcome = Some(outcome);
                self.issue_intake.open = false;
            }
            Some(Decision::Email) => {
                let req = self.issue_intake.request(&repo, renderer);
                let outcome = crate::issue_intake::open_mailto(&alias, &req.title, &req.body);
                crate::issue_intake::log_outcome(&outcome);
                self.issue_intake.last_outcome = Some(outcome);
                self.issue_intake.open = false;
            }
            Some(Decision::Cancel) => {
                self.issue_intake.open = false;
            }
            None => {
                // Esc closes the modal (mirrors the other dialogs' close-on-Esc).
                if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.issue_intake.open = false;
                }
            }
        }
    }

    /// `auto`-mode on-launch update modal. When the launch check finds a newer
    /// release it asks yes/no; the SAME modal then follows the flow through
    /// download → verify → "Restart to finish", so the whole Auto update is
    /// self-contained. "Later" dismisses it for this session (won't re-prompt
    /// for the same version). The manual flow in Settings covers every other case.
    pub(super) fn render_update_prompt(&mut self, ctx: &egui::Context) {
        if !self.updater.show_prompt {
            return;
        }
        use crate::updater::UpdateState;
        // Defer mutating calls past the immutable state borrow in the closure.
        enum Act {
            Download(scribe_core::update::ReleaseInfo),
            Apply,
            RunInstaller,
            Skip(String),
            Close,
        }
        let mut act: Option<Act> = None;
        egui::Modal::new(egui::Id::new("scr1b3_update_prompt")).show(ctx, |ui| {
            ui.set_max_width(400.0);
            ui.heading("Update available");
            ui.add_space(8.0);
            match &self.updater.state {
                UpdateState::Available(info) => {
                    let v = info.version.to_string();
                    ui.label(format!(
                        "SCR1B3 v{v} is available (you have v{}). Update now?",
                        crate::updater::current_version()
                    ));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Update now").clicked() {
                            act = Some(Act::Download(info.clone()));
                        }
                        if ui.button("Later").clicked() {
                            act = Some(Act::Skip(v.clone()));
                        }
                    });
                }
                UpdateState::Downloading { received, total } => {
                    let frac = if *total > 0 {
                        *received as f32 / *total as f32
                    } else {
                        0.0
                    };
                    ui.label("Downloading and verifying…");
                    ui.add_space(6.0);
                    ui.add(egui::ProgressBar::new(frac).show_percentage());
                }
                UpdateState::ReadyToApply { version, .. } => {
                    ui.label(format!("v{version} downloaded and verified."));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Restart to finish").clicked() {
                            act = Some(Act::Apply);
                        }
                        if ui.button("Later").clicked() {
                            act = Some(Act::Close);
                        }
                    });
                }
                UpdateState::ReadyToRunInstaller { version, .. } => {
                    ui.label(format!("v{version} downloaded and verified."));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Install (asks for admin)").clicked() {
                            act = Some(Act::RunInstaller);
                        }
                        if ui.button("Later").clicked() {
                            act = Some(Act::Close);
                        }
                    });
                }
                UpdateState::Applied { version } => {
                    ui.label(format!("Updated to v{version} — restarting…"));
                }
                UpdateState::Failed(e) => {
                    let err = ui.visuals().error_fg_color;
                    ui.colored_label(err, format!("Update failed: {e}"));
                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        act = Some(Act::Close);
                    }
                }
                // Nothing to prompt about in the on-launch modal — close it.
                // (NoAssetForPlatform is surfaced only in the Settings pane,
                // not the auto modal.)
                UpdateState::Idle
                | UpdateState::Checking
                | UpdateState::UpToDate { .. }
                | UpdateState::NoAssetForPlatform { .. } => {
                    act = Some(Act::Close);
                }
            }
        });
        match act {
            Some(Act::Download(info)) => self.updater.start_download(ctx, info),
            Some(Act::Apply) => self.updater.apply_and_restart(ctx),
            Some(Act::RunInstaller) => self.updater.run_installer(ctx),
            Some(Act::Skip(v)) => {
                self.updater.skipped_version = Some(v);
                self.updater.show_prompt = false;
            }
            Some(Act::Close) => self.updater.show_prompt = false,
            None => {}
        }
    }
}
