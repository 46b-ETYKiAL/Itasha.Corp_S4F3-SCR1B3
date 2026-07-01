//! The per-frame render loop for `ScribeApp` — `frame_tick` (the eframe
//! `update` body), plus its two private helpers `apply_scroll_settings`
//! (Wave-2 scroll knobs + middle-click autoscroll) and
//! `invalidate_galley_caches`. Extracted from `mod.rs` (A-01 wave 3 —
//! behavior-preserving move of the whole `impl ScribeApp` block; only
//! `invalidate_galley_caches` is widened to `pub(super)` for the `e2e`
//! sibling test that calls it. `frame_tick` keeps its `pub(crate)`
//! visibility; `apply_scroll_settings` stays private (called only here).
#![allow(clippy::wildcard_imports)]

use super::*;

impl ScribeApp {
    /// Apply the Wave-2 scroll knobs and drive middle-click autoscroll. Called
    /// at the very top of [`Self::frame_tick`], before any `ScrollArea` shows.
    ///
    /// - **Wheel speed** is `line_scroll_speed` (pre-smoothing; egui's built-in
    ///   "reach 90% in 0.1s" wheel smoothing still applies, so no double-smooth).
    /// - **Jump animation** eases programmatic scrolls (goto-line / find-next).
    /// - **Autoscroll** injects into `smooth_scroll_delta` so the ScrollArea the
    ///   pointer is over consumes it — the same additive contract `ScrollArea`
    ///   uses for the wheel, without threading a handle through every pane.
    fn apply_scroll_settings(&self, ctx: &egui::Context) {
        let scroll = self.config.scroll;
        ctx.options_mut(|o| o.input_options.line_scroll_speed = scroll.clamped_speed());
        // Wave-6 smooth-scroll: when the editor's smooth_scroll is OFF, kill the
        // jump easing so the wheel moves in discrete notches (snappier).
        let smooth = scroll.animate_jumps && self.config.editor.smooth_scroll;
        ctx.all_styles_mut(|s| {
            s.scroll_animation = if smooth {
                egui::style::ScrollAnimation::new(1500.0, egui::Rangef::new(0.05, 0.20))
            } else {
                egui::style::ScrollAnimation::none()
            };
        });
        if !scroll.autoscroll {
            return;
        }
        let id = egui::Id::new("scr1b3_autoscroll");
        let mut st: AutoScrollState = ctx.data(|d| d.get_temp(id).unwrap_or_default());
        // The central editor region (everything left after the titlebar / toolbar
        // / tab / status panels). At the top of a frame `available_rect` still
        // holds last frame's central area, so this excludes a middle-click on a
        // tab / toolbar button (which must keep its own middle-click meaning, e.g.
        // close-tab) from starting an autoscroll drift + repaint loop.
        let editor_area = ctx.available_rect();
        let (mb_pressed, exit_pressed, pos, dt) = ctx.input(|i| {
            (
                i.pointer.button_pressed(egui::PointerButton::Middle),
                i.pointer.button_pressed(egui::PointerButton::Primary)
                    || i.pointer.button_pressed(egui::PointerButton::Secondary),
                i.pointer.latest_pos(),
                i.stable_dt,
            )
        });
        // Enter on a middle press (toggles off if already active); otherwise a
        // left/right press exits. `entered` gates the entering frame so the same
        // press can't both enter and immediately drift.
        let mut entered = false;
        if mb_pressed {
            if st.active {
                st.active = false;
            } else if let Some(p) = pos {
                // Only arm autoscroll for a middle-click inside the editor surface
                // — never on the tabs / toolbar / status chrome.
                if editor_area.contains(p) {
                    st.active = true;
                    st.anchor = p;
                    entered = true;
                }
            }
        } else if st.active && exit_pressed {
            st.active = false;
        }
        if st.active && !entered {
            if let Some(p) = pos {
                let from_anchor = p - st.anchor;
                let dead = scroll.clamped_dead_zone();
                let drifting = from_anchor.length() >= dead;
                if drifting {
                    // smooth_scroll_delta +y moves content down (view toward the
                    // top), so to scroll toward the END when the pointer is BELOW
                    // the anchor (from_anchor.y > 0) the injected delta is negated.
                    let delta = -from_anchor * scroll.clamped_sensitivity() * dt;
                    // ScrollArea consumes `smooth_scroll_delta` (zeroing it when it
                    // takes it), so injecting here scrolls the hovered area.
                    ctx.input_mut(|i| i.smooth_scroll_delta += delta);
                    // Keep integrating the drift even when the pointer is held
                    // stationary-but-offset (no input event would otherwise wake
                    // the reactive loop). Crucially, when the pointer is AT rest in
                    // the dead-zone we do NOT request a repaint — otherwise a plain
                    // middle-click (e.g. that also closed a tab) would spin forever.
                    ctx.request_repaint();
                }
                // Origin glyph on a foreground layer + a directional cursor so the
                // affordance reads like the Windows wheel-click autoscroll. Drawn
                // whenever active (cheap; persists between input events at rest).
                let col = ctx.style().visuals.text_color();
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("scr1b3_autoscroll_glyph"),
                ));
                painter.circle_stroke(st.anchor, 11.0, egui::Stroke::new(1.5, col));
                painter.circle_filled(st.anchor, 1.5, col);
                let icon = if !drifting {
                    egui::CursorIcon::Move
                } else if from_anchor.y.abs() >= from_anchor.x.abs() {
                    if from_anchor.y < 0.0 {
                        egui::CursorIcon::ResizeNorth
                    } else {
                        egui::CursorIcon::ResizeSouth
                    }
                } else if from_anchor.x < 0.0 {
                    egui::CursorIcon::ResizeWest
                } else {
                    egui::CursorIcon::ResizeEast
                };
                ctx.set_cursor_icon(icon);
            }
        }
        ctx.data_mut(|d| d.insert_temp(id, st));
    }

    /// Drop every cached, atlas-baked galley (the note-text highlight galley and
    /// the minimap galley) plus the highlight-job memo. MUST be called right
    /// after `ctx.set_fonts()` rebuilds the font atlas: an `Arc<Galley>` baked
    /// against the OLD atlas keeps stale glyph→texture UVs, so reusing it after
    /// the rebuild paints garbled "broken" text. The layouter cache key
    /// (`make_layouter`) keys on font SIZE but not the family face — and
    /// `FontId::monospace` is identical before/after a face swap — so the cache
    /// cannot self-invalidate on a family change; this explicit drop is the only
    /// signal. (Bug: changing the app UI font silently rebuilt the atlas and the
    /// note text rendered from the stale galley.)
    pub(super) fn invalidate_galley_caches(&self) {
        *self.hl_cache.borrow_mut() = None;
        *self.hl_galley_cache.borrow_mut() = None;
        *self.minimap_cache.borrow_mut() = None;
        *self.minimap_draw_cache.borrow_mut() = None;
    }

    /// One per-frame tick of the editor UI. Separated from `eframe::App::ui` so
    /// `egui_kittest` E2E tests can drive it through `Context::run` without an
    /// `eframe::Frame`. Drives every top-level panel via the deprecated-but-
    /// functional `Panel::show(ctx, …)` path.
    pub(crate) fn frame_tick(&mut self, ctx: &egui::Context) {
        // Font-switch step 2 (see step 1 at the `ctx.set_fonts` call below):
        // `set_fonts` took effect at the START of this frame, so the NEW atlas is
        // now live. Drop the galley caches that were (re)baked against the OLD
        // atlas on the switch frame, BEFORE any panel renders this frame — the
        // editor then re-bakes against the new atlas and the note paints correctly
        // immediately (no blank/garbled frame, no need to type to refresh).
        if self.font_rebuild_pending {
            self.invalidate_galley_caches();
            self.font_rebuild_pending = false;
        }
        // Wave 2 scroll: apply the wheel-speed + jump-animation knobs and run the
        // middle-click autoscroll state machine BEFORE any ScrollArea shows this
        // frame (egui reads line_scroll_speed while building the wheel delta, and
        // the autoscroll injects into smooth_scroll_delta which the hovered
        // ScrollArea consumes when it renders later this tick).
        self.apply_scroll_settings(ctx);
        // Drain a palette-requested clipboard/history action BEFORE any panel
        // renders, so the injected event reaches the central editor (shown
        // later this frame) and egui's TextEdit performs it natively.
        self.drain_pending_editor_action(ctx);
        // F-022 — poll the disk mtimes of every open file-backed tab. Cheap
        // when nothing changed (one stat per tab); silent reload when the
        // buffer is clean; status toast when local edits would be clobbered.
        // P-06: throttled to once every N frames (see `should_poll_disk`).
        self.poll_external_disk_changes(ctx.cumulative_pass_nr());
        // Phase 18 T18.2 — keep the grid in step with the editor.grid_enabled
        // config preference (toggled in Settings or via TOML edit + watcher).
        // This is cheap on the common path (config unchanged + ids already
        // assigned) and lets the grid show up the same frame the user flips
        // the checkbox.
        self.sync_grid_state();
        // Follow-OS-theme watcher: when `appearance.follow_os_theme` is on,
        // re-resolve + apply the theme whenever the OS flips light/dark. Cheap
        // — one input read; only re-applies on an actual change.
        {
            let os_theme = ctx.theme();
            if self.config.appearance.follow_os_theme && Some(os_theme) != self.last_os_theme {
                self.reapply_theme(ctx);
            }
        }
        // Once per launch: kick off an automatic update check if opted in.
        self.maybe_remind_update(ctx);
        // Drain the updater worker each frame. A `notify`-mode launch check that
        // found a release raises a prominent top banner (Update / Dismiss) instead
        // of the easily-missed passive toast — see the "update-notice" panel below.
        self.updater.poll(ctx);
        if let Some(v) = self.updater.toast_pending.take() {
            self.update_notice = Some(v);
        }
        // `auto`-mode found-an-update yes/no modal.
        self.render_update_prompt(ctx);
        // W1TN3SS opt-in crash-consent modal (ask-each-time). Renders only when a
        // prior session spooled a crash report AND the user opted into
        // AskEachTime; presents an editable preview + equal-weight Send/Don't-send.
        self.render_crash_consent(ctx);
        // W1TN3SS user-initiated "Report an issue" modal. Renders only when the
        // user has opened it from the command palette; previews the exact body,
        // diagnostics OFF by default, and launches the GitHub deep-link / mailto
        // only on an explicit button click.
        self.render_report_issue(ctx);
        // Keep egui's animation time + caret style in sync with the motion
        // preferences every frame (cheap; also covers startup before any
        // theme reapply).
        self.apply_motion_style(ctx);
        // ---- Two-phase close (T19.1 ghost-window fix) ----
        // A transparent / layered window (frameless or translucent) must be
        // HIDDEN one frame before it is destroyed, or the Windows DWM keeps its
        // last composited frame on screen as a ghost after the process exits.
        // Phase 1: on any close request (custom ✕ or OS close) cancel the
        // immediate close, hide the window, repaint. Phase 2 (next frame): the
        // window is hidden, so issue the real Close.
        if self.closing {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let os_close = ctx.input(|i| i.viewport().close_requested());
        if os_close || self.want_close {
            self.want_close = false;
            self.closing = true;
            if os_close {
                // Stop eframe acting on the OS close THIS frame; we drive it.
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            ctx.request_repaint();
            return;
        }

        if !self.visuals_applied {
            ctx.set_visuals(self.current_visuals());
            self.visuals_applied = true;
        }

        // #24/#40 — the "doubled caption buttons" fix. ROOT CAUSE: winit keeps
        // `WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX` on undecorated TOP-LEVEL
        // windows (only the WS_CHILD branch strips caption bits — winit #2754), and
        // Windows 11 DWM paints the three native caption buttons from those residual
        // style bits over our custom titlebar. It is NOT the DWM backdrop (removing
        // window-vibrancy changed nothing) and NOT transparency; the old per-frame
        // `Decorations(false)` re-assert only toggled winit's decorations marker,
        // never the style bits, so it was a no-op. The real fix strips those bits
        // off the HWND — quarantined in the `scribe-win32-chrome` crate (the only
        // `unsafe` besides scribe-core's mmap). Called every frame because it is
        // idempotent + cheap (a single GetWindowLongPtrW read once stripped) and a
        // maximize re-applies winit's styles, which would otherwise re-add them.
        // `!cfg!(test)`: the headless kittest harness has no real OS window.
        if !cfg!(test) && self.config.appearance.frameless {
            scribe_win32_chrome::ensure_caption_stripped();
        }

        // #87/#103 — restart-free font switch: rebuild + re-apply the font set
        // whenever the chosen note OR UI family changes (cheap string compare).
        let font_key = font_state_key(&self.config.fonts);
        if font_key != self.applied_font_family {
            // Font-switch step 1: queue the new font set. `set_fonts` only takes
            // effect at the START of the NEXT frame — THIS frame still renders with
            // the old atlas. So: drop the stale caches now (cheap), then mark a
            // rebuild pending + request a repaint so step 2 (top of `frame_tick`)
            // drops the caches AGAIN next frame once the new atlas is live. Without
            // the next-frame drop, this frame re-bakes a galley against the still-
            // old atlas and the note renders blank/garbled until the next edit.
            ctx.set_fonts(build_fonts(
                &self.config.fonts.editor_family,
                &self.config.fonts.ui_family,
            ));
            self.applied_font_family = font_key;
            self.invalidate_galley_caches();
            self.font_rebuild_pending = true;
            ctx.request_repaint();
        }

        // #104 / #E P1 — apply the editor highlight theme when it changes (also
        // runs once on the first frame to honour the saved config). When
        // `syntax_from_theme` is ON the active CHROME theme's documented
        // `[syntax]` map (incl. the `markup.*` keys) drives editor colours;
        // otherwise the `note_theme` syntect preset does. The applied marker
        // reuses `applied_note_theme`: a NUL-prefixed `\0core:<name>` sentinel
        // (which can never equal a real note-theme name) keys off the chrome
        // theme so a theme switch — or toggling the flag — re-applies. Clearing
        // the highlight cache forces a re-colour next render.
        let desired_hl_theme = if self.config.editor.syntax_from_theme {
            format!("\u{0}core:{}", self.theme.name)
        } else {
            self.config.editor.note_theme.clone()
        };
        if desired_hl_theme != self.applied_note_theme {
            if self.config.editor.syntax_from_theme {
                self.hl.set_core_theme(&self.theme);
            } else {
                self.hl.set_theme(&self.config.editor.note_theme);
            }
            *self.hl_cache.borrow_mut() = None;
            *self.hl_galley_cache.borrow_mut() = None;
            self.applied_note_theme = desired_hl_theme;
        }

        // Live-reload config when the file changes on disk (external edit).
        let mut reload_cfg = false;
        if let Some(rx) = &self.cfg_rx {
            while rx.try_recv().is_ok() {
                reload_cfg = true;
            }
        }
        if reload_cfg {
            self.reload_config_from_disk(ctx);
        }

        // 4-02 — drain any batches the off-thread project-find worker streamed
        // back this frame so the results pane fills in progressively. Cheap
        // (one `try_recv` loop) and a no-op when no search is in flight.
        self.drain_find_in_files();

        // Drain LSP diagnostics published by the server thread.
        let mut new_diags: Option<Vec<Diagnostic>> = None;
        if let Some(client) = &self.lsp {
            while let Ok(d) = client.diagnostics.try_recv() {
                new_diags = Some(d);
            }
        }
        if let Some(d) = new_diags {
            self.diagnostics = d;
        }

        // Collect deferred actions from shortcuts.
        let mut act = Pending::default();
        // #R6 — find-bar F3 navigation direction, recorded here and applied
        // after the input closure so `find_navigate` can re-borrow `self`.
        let mut find_nav: Option<bool> = None;
        self.handle_keyboard_shortcuts(ctx, &mut act, &mut find_nav);
        // #R6 — apply the find-bar F3 navigation collected above (outside the
        // input borrow so `find_navigate` can re-borrow `self`).
        if let Some(forward) = find_nav {
            self.find_navigate(forward);
        }
        // #72 — identifier completion is an EDITOR-surface popup. While any
        // text-input / navigation modal owns the keyboard (find bar, command
        // palette, fuzzy finder, go-to-symbol / go-to-line, recent files,
        // settings, cheatsheet, welcome), completion must NOT open and must NOT
        // intercept ↑↓/Enter — otherwise a Ctrl+Space typed into (say) the find
        // field would spawn a popup that then steals the find bar's navigation
        // keys. Force any open popup closed and leave Ctrl+Space for the modal.
        let modal_owns_keys = self.modal_owns_keyboard();
        if modal_owns_keys {
            self.completion = None;
        }
        // Ctrl/Cmd+Space requests identifier completion at the cursor (only when
        // the editor — not a modal — owns the keyboard; short-circuits so the
        // key is left unconsumed for a focused modal field).
        let want_completion = !modal_owns_keys
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Space));
        // While the completion popup is open, intercept navigation keys BEFORE
        // the TextEdit sees them so arrows/enter drive the list, not the caret.
        let mut accept_completion = false;
        if self.completion.is_some() {
            ctx.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    if let Some(c) = &mut self.completion {
                        c.selected = (c.selected + 1).min(c.items.len().saturating_sub(1));
                    }
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    if let Some(c) = &mut self.completion {
                        c.selected = c.selected.saturating_sub(1);
                    }
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                {
                    accept_completion = true;
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    self.completion = None;
                }
            });
        }
        // Deferred plugin-command invocation (set by palette/menu, applied after UI).
        let mut run_cmd: Option<String> = None;
        // Deferred config persistence (set by View-menu toggles).
        let mut save_cfg = false;
        // Deferred file-tree actions.
        let mut open_from_tree: Option<PathBuf> = None;
        let mut close_tree = false;
        // Deferred LSP start (set by the Language menu).
        let mut start_lsp = false;

        let accent = ui_color(&self.theme, "accent", Rgba::new(0, 255, 254, 255));
        // Secondary brand colour for the split-tone wordmark (`1 B 3` half). Falls
        // back to a complementary violet when a theme does not define `accent_alt`,
        // so existing single-accent themes keep working; the 12 brand themes each
        // set their own. Chrome stays one-accent everywhere ELSE (the split wordmark
        // is the single deliberate two-tone mark, per the brand discipline).
        let accent_alt = ui_color(&self.theme, "accent_alt", Rgba::new(0x9d, 0x7c, 0xff, 255));
        let muted = ui_color(&self.theme, "line_number", Rgba::new(0x5a, 0x58, 0x69, 255));
        // Chrome panels (titlebar/toolbar/status/filetree/split/gutter/minimap) all
        // fill with this color. In a translucent window mode the fill MUST carry the
        // reduced alpha — otherwise opaque chrome covers the transparent/blurred
        // surface and "transparency doesn't work" (the T19.2 root cause). The master
        // `transparency_enabled` toggle gates this via `effective_translucent()`.
        let panel = panel_fill(
            &self.theme,
            &self.config.window,
            self.config.appearance.background_override.as_deref(),
        );
        let warn = ui_color(&self.theme, "warning", Rgba::new(0xfb, 0xbf, 0x24, 255));

        // F11 fullscreen (editor-only): derive the OS fullscreen state each frame
        // (no separate field — avoids a re-sync race when the user exits via the
        // OS). `chrome_hidden` hides the toolbar/tabs/status/minimap/gutter; the
        // custom titlebar additionally hides in fullscreen (the OS gives no frame),
        // whereas zen keeps it for window dragging.
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        let chrome_hidden = self.zen_mode || fullscreen;
        // Toolbar-in-titlebar mode (only meaningful with the custom titlebar).
        let toolbar_in_titlebar =
            self.config.appearance.toolbar_in_titlebar && self.config.appearance.frameless;

        // ---- Custom frameless titlebar ----
        // Height is CONSTANT regardless of `toolbar_in_titlebar` — it is sized to
        // fit the quick-access toolbar buttons in BOTH states (so toggling the
        // option never resizes the titlebar). It grows only if the user raises the
        // toolbar button-size setting (a separate, expected knob), and never drops
        // below the bare-chrome baseline (34). Previously it was 40 when the
        // toolbar lived here and 34 otherwise, so flipping the toggle jumped it.
        let titlebar_h = (self.config.toolbar.clamped_button_size() + 10.0).max(34.0);
        if self.config.appearance.frameless && !fullscreen {
            egui::TopBottomPanel::top("titlebar")
                .exact_height(titlebar_h)
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    let resp = ui.interact(
                        ui.max_rect(),
                        egui::Id::new("titlebar-drag"),
                        egui::Sense::click_and_drag(),
                    );
                    if resp.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if resp.double_clicked() {
                        let is_max = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_max));
                    }
                    ui.horizontal_centered(|ui| {
                        // RESERVE the window caption buttons on the RIGHT first. The
                        // wordmark + in-titlebar toolbar then fill the space to their
                        // LEFT, clipped to that boundary — so on a narrow window the
                        // toolbar compresses/clips instead of the min/max/close buttons
                        // being painted over by it (the "caption buttons go over the
                        // toolbar when narrow" report). Previously the left content was
                        // laid out first and the caption buttons took only the leftover
                        // width, so a wide toolbar pushed them under itself / off-edge.
                        // Caption-button height tracks the toolbar button size so
                        // they stay consistent when the user picks a large size,
                        // while preserving the default 28px (`.max(28.0)`).
                        let cap_h = self.config.toolbar.clamped_button_size().max(28.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let is_max = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
                            let close_hover = Color32::from_rgb(0xE8, 0x11, 0x23);
                            let soft_hover = Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 26);
                            if caption_btn(ui, CaptionIcon::Close, muted, close_hover, cap_h)
                                .clicked()
                            {
                                // Funnel into the two-phase close (hide-before-destroy)
                                // so a transparent window leaves no DWM ghost (T19.1).
                                self.want_close = true;
                            }
                            let max_icon = if is_max {
                                CaptionIcon::Restore
                            } else {
                                CaptionIcon::Maximize
                            };
                            if caption_btn(ui, max_icon, muted, soft_hover, cap_h).clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_max));
                            }
                            if caption_btn(ui, CaptionIcon::Minimize, muted, soft_hover, cap_h)
                                .clicked()
                            {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                            }
                            // Settings "gear", relocated here from the quick-access
                            // toolbar. In this right_to_left layout, painting it AFTER
                            // Minimize places it visually to the LEFT of Minimize — the
                            // rightmost non-window-control button. Opens Settings, the
                            // same effect as the old toolbar gear (the command-palette
                            // "Open Settings" command + its shortcut are unchanged).
                            if caption_btn(ui, CaptionIcon::Settings, muted, soft_hover, cap_h)
                                .on_hover_text("Settings")
                                .clicked()
                            {
                                self.settings_open = true;
                            }
                            // LEFT content: wordmark + (optional) in-titlebar toolbar,
                            // laid out left-to-right in the width remaining to the left
                            // of the caption buttons. The clip rect is pinned to that
                            // region so an overflowing toolbar can never paint over the
                            // reserved caption buttons.
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                                    ui.add_space(10.0);
                                    // Chrome text follows the APP UI font (Proportional
                                    // family), NOT the note/editor font. Split-tone
                                    // wordmark: "S C R " accent, "1 B 3" secondary;
                                    // painted with zero item-spacing so they read as ONE
                                    // wordmark.
                                    let saved_spacing = ui.spacing().item_spacing.x;
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    ui.label(RichText::new("S C R ").color(accent).strong());
                                    ui.label(RichText::new("1 B 3").color(accent_alt).strong());
                                    ui.spacing_mut().item_spacing.x = saved_spacing;
                                    // Decorative separator + JP subtitle (写本 —
                                    // shahon) drop out FIRST when the titlebar is
                                    // tight, so the core "SCR1B3" wordmark never has
                                    // to clip mid-glyph on a narrow window.
                                    if ui.available_width() > 120.0 {
                                        ui.add_space(6.0);
                                        ui.label(RichText::new("//").color(muted));
                                        ui.label(
                                            RichText::new(scribe_core::PRODUCT_SUBTITLE_JP)
                                                .color(muted)
                                                .small(),
                                        );
                                    }
                                    if toolbar_in_titlebar {
                                        ui.add_space(12.0);
                                        // Button PARITY with the standalone toolbar row:
                                        // same configured height + spacing so the buttons
                                        // are identical whether the toolbar lives here or
                                        // in its own row.
                                        let btn = self.config.toolbar.clamped_button_size();
                                        let gap = self.config.toolbar.clamped_button_spacing();
                                        ui.spacing_mut().interact_size.y = btn;
                                        ui.spacing_mut().item_spacing.x = gap;
                                        self.toolbar_contents(
                                            ui,
                                            &mut act,
                                            &mut save_cfg,
                                            &mut start_lsp,
                                        );
                                    }
                                },
                            );
                        });
                    });
                });
        }

        // ---- Quick-access toolbar (replaces the classic menu bar) ----
        // Hidden in zen / fullscreen; suppressed when moved into the titlebar.
        if !chrome_hidden && !toolbar_in_titlebar {
            egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
                // Phase 18 T18.5: apply the user-configurable button size + spacing
                // BEFORE the horizontal row so every quick-access item inherits the
                // sizing. All values are clamped at the config layer to defend
                // against a malformed user toml producing a 4000-px-tall toolbar.
                let btn = self.config.toolbar.clamped_button_size();
                let gap = self.config.toolbar.clamped_button_spacing();
                ui.spacing_mut().interact_size.y = btn;
                ui.spacing_mut().item_spacing.x = gap;
                ui.horizontal(|ui| {
                    self.toolbar_contents(ui, &mut act, &mut save_cfg, &mut start_lsp);
                });
            });
        }

        // ---- Tab strip in its OWN bar (T18.4) — separate from the toolbar ----
        //
        // #R5: in split/grid view the top tab strip is redundant — every pane
        // now carries its own chip header (note name + pin + close), so the
        // global strip is suppressed. New notes remain reachable via Ctrl+N,
        // the command palette, and the toolbar's customizable items.
        // The whole tab strip is hidden in zen mode and F11 fullscreen.
        // Set when the tab bar is at Bottom: its panel is rendered later, AFTER
        // the status bar, so the status bar keeps the very bottom screen edge and
        // the tab strip stacks directly above it (egui gives the first-shown bottom
        // panel the outermost slot).
        let mut bottom_tabs_deferred = false;
        if !chrome_hidden && !self.config.editor.grid_enabled {
            match self.config.editor.tab_bar_position {
                scribe_core::config::TabBarPosition::Top => {
                    // A dedicated tab bar directly below the quick-access toolbar
                    // (added after the "toolbar" top panel, so it stacks beneath it).
                    egui::TopBottomPanel::top("tabs-top")
                        .frame(egui::Frame::default().fill(panel))
                        .show(ctx, |ui| {
                            // PA-06: wrap the top strip in a HORIZONTAL ScrollArea
                            // (mirroring the side strips' vertical ScrollArea in
                            // `draw_side_tab_strip`) so that with many open tabs the
                            // overflowing tabs stay scroll-reachable instead of
                            // clipping off the right edge with no affordance.
                            egui::ScrollArea::horizontal()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| self.draw_tab_strip(ui, accent, muted));
                                });
                        });
                }
                scribe_core::config::TabBarPosition::Bottom => {
                    // DEFERRED: a bottom tab bar must sit ABOVE the status bar, but
                    // egui gives the FIRST-shown bottom panel the screen edge. The
                    // status panel is shown later (below), so rendering the tab strip
                    // here would pin it under the status bar. Defer it and render it
                    // immediately AFTER the status panel so status keeps the very
                    // bottom edge and the tab strip stacks directly above it.
                    bottom_tabs_deferred = true;
                }
                scribe_core::config::TabBarPosition::Left => {
                    let rotated = self.config.editor.side_tabs_rotated;
                    // Fit-to-content width (#16): the bar hugs the widest tab
                    // rather than a fixed 180px slab, so a short note name doesn't
                    // leave a big empty bar. `exact_width` auto-tracks the content
                    // every frame (no manual resize needed — it just fits).
                    let w = self.side_tab_bar_width(ctx, rotated);
                    egui::SidePanel::left("tabs-left")
                        .exact_width(w)
                        .frame(egui::Frame::default().fill(panel).inner_margin(4.0))
                        .show(ctx, |ui| {
                            self.draw_side_tab_strip(ui, accent, muted, rotated);
                        });
                }
                scribe_core::config::TabBarPosition::Right => {
                    let rotated = self.config.editor.side_tabs_rotated;
                    let w = self.side_tab_bar_width(ctx, rotated);
                    egui::SidePanel::right("tabs-right")
                        .exact_width(w)
                        .frame(egui::Frame::default().fill(panel).inner_margin(4.0))
                        .show(ctx, |ui| {
                            self.draw_side_tab_strip(ui, accent, muted, rotated);
                        });
                }
            }
        }

        // ---- Config-error banner (F-038) ----
        //
        // Persistent top banner when the config TOML failed to parse on
        // launch. Surfaces the error message + actionable choices:
        // "Open config" (opens the TOML file as a new tab so the user can
        // hand-edit it), "Restore default" (overwrites the file with the
        // default Config and reloads), and "Dismiss" (clears the banner
        // for the session — the user took ownership of the warning).
        let mut want_open_cfg = false;
        let mut want_restore_cfg = false;
        let mut want_dismiss_cfg = false;
        if let Some(msg) = self.config_error_banner.clone() {
            egui::TopBottomPanel::top("config-error-banner")
                .frame(
                    egui::Frame::default()
                        .fill(warn.linear_multiply(0.20))
                        .inner_margin(egui::Margin::same(6)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(egui_phosphor::thin::WARNING)
                                .color(warn)
                                .strong(),
                        );
                        ui.label(
                            RichText::new(format!("Config has errors: {msg}"))
                                .color(warn)
                                .monospace(),
                        );
                        if ui.button("Open config").clicked() {
                            want_open_cfg = true;
                        }
                        if ui.button("Restore default").clicked() {
                            want_restore_cfg = true;
                        }
                        if ui.button("Dismiss").clicked() {
                            want_dismiss_cfg = true;
                        }
                    });
                });
        }

        // ---- Update-available notice (notify mode) ----
        //
        // A PROMINENT top banner (accent-filled, bold) — not the passive toast —
        // so a found update is actually noticeable. Carries an "Update" button
        // that jumps straight to Settings → Updates to begin the update, plus a
        // "Dismiss" button. Shown only in `notify` mode (auto mode uses the modal).
        if let Some(v) = self.update_notice.clone() {
            let mut want_update = false;
            let mut want_dismiss = false;
            egui::TopBottomPanel::top("update-notice")
                .frame(
                    egui::Frame::default()
                        .fill(accent.linear_multiply(0.22))
                        .inner_margin(egui::Margin::symmetric(10, 7)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "SCR1B3 v{v} is available — you have v{}.",
                                crate::updater::current_version()
                            ))
                            .color(accent)
                            .strong(),
                        );
                        ui.add_space(8.0);
                        if ui.button(RichText::new("Update").strong()).clicked() {
                            want_update = true;
                        }
                        if ui.button("Dismiss").clicked() {
                            want_dismiss = true;
                        }
                    });
                });
            if want_update {
                // Jump to Settings → Updates so the user can start the update
                // (download → verify → restart) from the manual update controls.
                crate::settings::request_category(ctx, "Updates");
                self.settings_open = true;
                self.update_notice = None;
            }
            if want_dismiss {
                self.update_notice = None;
            }
        }

        // ---- External-change banner (F-022b) ----
        // A file open here was modified on disk WHILE it holds unsaved local
        // edits. Prompt the user to update to the saved version (or keep theirs)
        // instead of silently overwriting the newer file on save. A CLEAN tab is
        // reloaded silently by `poll_external_disk_changes` and never reaches here.
        if self.active < self.tabs.len() && self.tabs[self.active].external_change {
            let name = self.tabs[self.active].doc.file_name();
            let warn = egui::Color32::from_rgb(0xE0, 0x9A, 0x20);
            let mut want_reload = false;
            let mut want_keep = false;
            egui::TopBottomPanel::top("external-change-notice")
                .frame(
                    egui::Frame::default()
                        .fill(warn.linear_multiply(0.20))
                        .inner_margin(egui::Margin::symmetric(10, 7)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "{}  \"{name}\" was changed on disk, and you have unsaved edits here.",
                                egui_phosphor::thin::WARNING
                            ))
                            .color(warn)
                            .strong(),
                        );
                        ui.add_space(8.0);
                        if ui
                            .button(RichText::new("Reload from disk").strong())
                            .on_hover_text(
                                "Discard your unsaved edits and load the current saved version.",
                            )
                            .clicked()
                        {
                            want_reload = true;
                        }
                        if ui
                            .button("Keep my version")
                            .on_hover_text(
                                "Keep your edits — the next save will overwrite the disk version.",
                            )
                            .clicked()
                        {
                            want_keep = true;
                        }
                    });
                });
            let i = self.active;
            if want_reload {
                if let Some(path) = self.tabs[i].doc.path().map(|p| p.to_path_buf()) {
                    // ENC-1: encoding-preserving reload (see session_io.rs) — the
                    // user's explicit "reload from disk" must honour the file's
                    // detected encoding, not assume UTF-8.
                    if self.tabs[i].doc.reload_from_disk().is_ok() {
                        let fresh = self.tabs[i].doc.text();
                        self.tabs[i].set_text(fresh.clone());
                        self.tabs[i].disk_text = fresh;
                        if let Some(m) = file_mtime(&path) {
                            self.tabs[i].disk_mtime = Some(m);
                        }
                        // Change-bar: reloaded content is the new clean baseline.
                        self.tabs[i].reset_change_baselines();
                        self.status = format!("reloaded {} from disk", path.display());
                    }
                }
                self.tabs[i].external_change = false;
            }
            if want_keep {
                // Accept the current disk mtime as known so we stop re-prompting,
                // but keep the buffer + its unsaved edits (a later save overwrites
                // the disk file).
                if let Some(path) = self.tabs[i].doc.path().map(|p| p.to_path_buf()) {
                    if let Some(m) = file_mtime(&path) {
                        self.tabs[i].disk_mtime = Some(m);
                    }
                }
                self.tabs[i].external_change = false;
            }
        }

        // ---- Find / Replace bar ----
        //
        // F-008 from docs/audits/overlooked-surfaces-2026-05-29.md: the
        // pre-audit find bar had no replace field. Ctrl+F still opens
        // find-only; Ctrl+H opens the same bar with focus pre-set to the
        // replace field. "Replace next" replaces only the first match,
        // "Replace all" walks every match in the active buffer.
        if self.find_open {
            egui::TopBottomPanel::top("find").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("find").color(accent).monospace());
                    let r = ui.text_edit_singleline(&mut self.find_query);
                    if self.focus_find {
                        r.request_focus();
                        self.focus_find = false;
                    }
                    // Editing the query restarts navigation at the first match.
                    if self.find_query != self.find_last_query {
                        self.find_match_idx = 0;
                        self.find_last_query = self.find_query.clone();
                    }
                    let count = self.find_matches_active().len();
                    self.find_match_idx = self.find_match_idx.min(count.saturating_sub(1));
                    // Enter in the find field jumps to the next match.
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.find_navigate(true);
                    }
                    if ui
                        .add_enabled(
                            count > 0,
                            egui::Button::new(egui_phosphor::thin::ARROW_UP).small(),
                        )
                        .on_hover_text("Previous match (Shift+F3)")
                        .clicked()
                    {
                        self.find_navigate(false);
                    }
                    if ui
                        .add_enabled(
                            count > 0,
                            egui::Button::new(egui_phosphor::thin::ARROW_DOWN).small(),
                        )
                        .on_hover_text("Next match (F3 / Enter)")
                        .clicked()
                    {
                        self.find_navigate(true);
                    }
                    let counter = if count == 0 {
                        if self.find_query.is_empty() {
                            String::new()
                        } else {
                            "no matches".to_string()
                        }
                    } else {
                        format!("{}/{}", self.find_match_idx + 1, count)
                    };
                    ui.label(RichText::new(counter).color(muted).small());
                    if ui.button("close").clicked() {
                        self.find_open = false;
                    }
                });
                // Second row: replace field + actions.
                ui.horizontal(|ui| {
                    ui.label(RichText::new("with").color(accent).monospace());
                    let rr = ui.text_edit_singleline(&mut self.replace_query);
                    if self.focus_replace {
                        rr.request_focus();
                        self.focus_replace = false;
                    }
                    if ui.button("Replace next").clicked() {
                        self.replace_in_active(false);
                    }
                    if ui.button("Replace all").clicked() {
                        self.replace_in_active(true);
                    }
                });
            });
        }

        // ---- Wave-5: find in files (project-wide search results pane) ----
        if self.find_in_files_open {
            // PA-02: read Up/Down/Enter for RESULT navigation here (outside the
            // panel body), mirroring the command-palette / fuzzy-finder list-nav.
            // Enter opens the selected result, but ONLY when the query field is
            // not focused — an Enter in the query field triggers SEARCH (handled
            // below via `lost_focus()`), so the two Enter meanings never collide.
            let result_count = self.find_in_files_results.len();
            let (up, down, enter_pressed) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if result_count == 0 {
                self.find_in_files_selected = 0;
            } else {
                self.find_in_files_selected =
                    fuzzy_move_selection(self.find_in_files_selected, result_count, up, down);
            }
            let selected = self.find_in_files_selected;
            let mut open_selected_via_enter = false;
            egui::SidePanel::right("find_in_files")
                .resizable(true)
                .default_width(360.0)
                .frame(egui::Frame::default().fill(panel).inner_margin(6.0))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("find in files").color(accent).monospace());
                        if ui.button("close").clicked() {
                            self.find_in_files_open = false;
                        }
                    });
                    let r = ui.text_edit_singleline(&mut self.find_in_files_query);
                    if self.focus_find_in_files {
                        r.request_focus();
                        self.focus_find_in_files = false;
                    }
                    let query_focused = r.has_focus();
                    // Enter while the query is NOT focused (e.g. after arrow-key
                    // navigation moved focus into the results) opens the selected
                    // result — the keyboard-activate leg the audit (PA-02) flagged.
                    if enter_pressed && !query_focused && result_count > 0 {
                        open_selected_via_enter = true;
                    }
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.find_in_files_regex, "regex");
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if enter || ui.button("search").clicked() {
                            self.run_find_in_files(ctx);
                        }
                    });
                    if let Some(err) = &self.find_in_files_error {
                        ui.colored_label(Color32::from_rgb(0xe5, 0x3e, 0x3e), err);
                    }
                    // 4-02: streaming hint while the off-thread worker is walking.
                    if self.find_in_files_running {
                        ui.label(
                            RichText::new(format!(
                                "searching… {} so far",
                                self.find_in_files_results.len()
                            ))
                            .color(muted)
                            .small(),
                        );
                    }
                    ui.separator();
                    let mut open_target: Option<(PathBuf, usize)> = None;
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for (idx, m) in self.find_in_files_results.iter().enumerate() {
                            let name = m.path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                            let label = format!("{}:{}  {}", name, m.line, m.line_text.trim());
                            // PA-02: a highlighted selectable row (mirroring the
                            // palette / fuzzy finder) replaces the bare click-Label,
                            // so the keyboard-selected result is visibly distinct
                            // and Up/Down/Enter drive it — not mouse-click only.
                            let row = ui.selectable_label(
                                idx == selected,
                                RichText::new(label).monospace().small(),
                            );
                            if row.clicked() {
                                open_target = Some((m.path.clone(), m.line));
                            }
                            if idx == selected && (up || down) {
                                row.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                    });
                    // Enter (query unfocused) opens the keyboard-selected result.
                    if open_selected_via_enter {
                        if let Some(m) = self.find_in_files_results.get(selected) {
                            open_target = Some((m.path.clone(), m.line));
                        }
                    }
                    if let Some((path, line)) = open_target {
                        self.open_find_in_files_result(path, line);
                    }
                });
        }

        // ---- Command palette (built-in + plugin commands) ----
        //
        // F-004 fix from docs/audits/overlooked-surfaces-2026-05-29.md:
        // the palette previously surfaced only plugin commands. On a fresh
        // install (zero plugins loaded), opening Ctrl+Shift+P showed
        // "no plugin commands yet" — the editor's primary self-discovery
        // surface was empty. Now every built-in editor action is listed
        // alphabetically alongside plugin commands and the fuzzy filter
        // searches both.
        let mut run_builtin: Option<BuiltinCommand> = None;
        if self.palette_open {
            // BUG-APP-01 fix: build the filtered command list ONCE up front so
            // keyboard nav (Up/Down/Enter) and the rendered rows agree on the
            // same set — mirroring the fuzzy-file-finder's "rank once up front"
            // pattern (frame_modals.rs `if self.fuzzy_open`). Each entry carries what to
            // run; the index into this Vec is the selectable highlight.
            enum PaletteAction {
                Builtin(BuiltinCommand),
                Plugin(String),
            }
            struct PaletteItem {
                display: String,
                action: PaletteAction,
                /// True for the first plugin command — render a separator above
                /// it, preserving the prior built-in/plugin visual split.
                separator_before: bool,
            }
            let q = self.palette_query.to_lowercase();
            let mut items: Vec<PaletteItem> = Vec::new();
            // Built-in commands first — universally available even with zero
            // plugins.
            for cmd in BUILTIN_COMMANDS {
                let label = cmd.label;
                let shortcut = cmd.shortcut;
                if q.is_empty()
                    || label.to_lowercase().contains(&q)
                    || shortcut.to_lowercase().contains(&q)
                {
                    let display = if shortcut.is_empty() {
                        label.to_string()
                    } else {
                        format!("{label}  ·  {shortcut}")
                    };
                    items.push(PaletteItem {
                        display,
                        action: PaletteAction::Builtin(cmd.action),
                        separator_before: false,
                    });
                }
            }
            let mut first_plugin = true;
            for c in &self.plugin_cmds {
                if q.is_empty() || c.label.to_lowercase().contains(&q) || c.id.contains(&q) {
                    items.push(PaletteItem {
                        display: format!("{}  ·  {}", c.label, c.plugin_id),
                        action: PaletteAction::Plugin(c.id.clone()),
                        separator_before: first_plugin,
                    });
                    first_plugin = false;
                }
            }

            // Read Up/Down/Enter here (outside the window body). A singleline
            // TextEdit ignores these keys, so this does not fight the query
            // field's caret — same rationale as the fuzzy finder.
            let (up, down, enter) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if items.is_empty() {
                self.palette_selected = 0;
            } else {
                self.palette_selected =
                    fuzzy_move_selection(self.palette_selected, items.len(), up, down);
                if enter {
                    match &items[self.palette_selected].action {
                        PaletteAction::Builtin(a) => run_builtin = Some(*a),
                        PaletteAction::Plugin(id) => run_cmd = Some(id.clone()),
                    }
                }
            }
            let selected = self.palette_selected;

            let mut query_changed = false;
            egui::Window::new(
                RichText::new(format!("{}  command palette", egui_phosphor::thin::COMMAND))
                    .color(accent)
                    .monospace(),
            )
            .collapsible(false)
            .resizable(false)
            // A fixed width so the primary command-discovery surface opens at a
            // consistent size (matching the other modal pickers) instead of
            // sizing to its content. Aligns with go-to-symbol/recent/fuzzy.
            .default_width(600.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 64.0])
            .show(ctx, |ui| {
                let r = ui.text_edit_singleline(&mut self.palette_query);
                if self.focus_palette {
                    r.request_focus();
                    self.focus_palette = false;
                }
                query_changed = r.changed();
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for (idx, item) in items.iter().enumerate() {
                            if item.separator_before {
                                ui.separator();
                            }
                            let row = ui.selectable_label(idx == selected, item.display.clone());
                            if row.clicked() {
                                match &item.action {
                                    PaletteAction::Builtin(a) => run_builtin = Some(*a),
                                    PaletteAction::Plugin(id) => run_cmd = Some(id.clone()),
                                }
                            }
                            // Keep the keyboard-highlighted row in view.
                            if idx == selected && (up || down) {
                                row.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                        if items.is_empty() {
                            ui.label(RichText::new("no match").color(muted).small());
                        }
                    });
            });
            // A new query invalidates the old highlight position — reset to the
            // top so Enter runs the new top match (acceptance criterion 2).
            if query_changed {
                self.palette_selected = 0;
            }
        }

        // ---- Settings window (deep customization, live preview) ----
        if self.settings_open {
            let changed = crate::settings::show(
                ctx,
                &mut self.config,
                &mut self.settings_open,
                &mut self.updater,
            );
            // F-039 — the Plugins section's "Manage plugins…" button stashes a
            // request flag; pick it up and open the plugin-manager modal.
            if crate::settings::take_open_plugin_manager_request(ctx) {
                self.plugin_manager
                    .ensure_defaults(Config::config_dir().as_deref());
                self.plugin_manager.open = true;
            }
            if changed {
                self.reapply_theme(ctx);
                // Spellcheck language / custom-dict edits take effect live.
                self.reload_spell_engine();
                // F-035 — push the always-on-top flag to the viewport
                // immediately so the toggle is live (no restart required).
                let level = if self.config.window.always_on_top {
                    egui::WindowLevel::AlwaysOnTop
                } else {
                    egui::WindowLevel::Normal
                };
                ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
                self.save_config();
            }
        }

        // ---- Keyboard cheatsheet (F1) ----
        //
        // F-014 from docs/audits/overlooked-surfaces-2026-05-29.md. Lists
        // every wired shortcut so the user doesn't have to guess. The table
        // is rendered as a markdown-like 2-column grid; the data lives in
        // KEYBOARD_SHORTCUTS so any future shortcut addition lands in one
        // place + the modal stays current.
        if self.cheatsheet_open {
            let mut still_open = true;
            egui::Window::new(
                RichText::new(format!(
                    "{}  keyboard shortcuts",
                    egui_phosphor::thin::KEYBOARD
                ))
                .color(accent)
                .monospace(),
            )
            .open(&mut still_open)
            .collapsible(false)
            .resizable(true)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| {
                        egui::Grid::new("cheatsheet-grid")
                            .num_columns(2)
                            .spacing([24.0, 6.0])
                            .striped(true)
                            .show(ui, |ui| {
                                for entry in KEYBOARD_SHORTCUTS {
                                    ui.label(RichText::new(entry.chord).color(accent).monospace());
                                    ui.label(RichText::new(entry.action).color(muted).small());
                                    ui.end_row();
                                }
                            });
                    });
                ui.add_space(8.0);
                ui.label(
                    RichText::new("press F1 or Esc to close")
                        .color(muted)
                        .small()
                        .monospace(),
                );
            });
            if !still_open {
                self.cheatsheet_open = false;
            }
        }

        // ---- Plugin manager modal (F-039 + F-040) ----
        //
        // Surfaces the Phase-20 plugin foundation. The host builds the Loaded
        // rows from `discover()` + `config.plugins.disabled`, passes the
        // plugins dir, and applies whatever action the modal returns.
        if self.plugin_manager.open {
            let plugins_dir = Config::config_dir()
                .map(|d| d.join("plugins"))
                .unwrap_or_else(|| PathBuf::from("plugins"));
            let loaded = self.discovered_plugin_rows(&plugins_dir);
            let action = self
                .plugin_manager
                .show(ctx, accent, muted, &loaded, &plugins_dir);
            if let Some(id) = action.toggle_disabled {
                if let Some(pos) = self.config.plugins.disabled.iter().position(|d| *d == id) {
                    self.config.plugins.disabled.remove(pos);
                } else {
                    self.config.plugins.disabled.push(id);
                }
                self.save_config();
            }
            if action.open_plugins_dir {
                // Best-effort: create the dir so the reveal lands somewhere,
                // then open it in the OS file manager.
                let _ = std::fs::create_dir_all(&plugins_dir);
                open_in_file_manager(&plugins_dir);
            }
            if let Some(id) = action.approve {
                self.approve_plugin(&id);
            }
        }

        self.render_picker_modals(ctx, accent, muted);

        // Spellcheck status (computed before the status-bar closure borrows self).
        let spell_on = self.config.spellcheck.enabled;
        let spell_misspellings = self.spell_count();
        let diag_errors = self.diagnostics.iter().filter(|d| d.severity == 1).count();
        let diag_total = self.diagnostics.len();

        // ---- Status bar ----
        let mut cycle_eol_for_active = false;
        let mut open_settings_for = None;
        // Hidden in zen / distraction-free mode and in F11 fullscreen.
        if !chrome_hidden {
            egui::TopBottomPanel::bottom("status")
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // Edge padding so the leftmost status segment isn't flush against
                        // the window edge (mirrors the titlebar's 10px lead-in).
                        ui.add_space(8.0);
                        let active = self.active.min(self.tabs.len().saturating_sub(1));
                        // PA-04: line/word/char counts via the (edit_gen, doc_id)
                        // memo — recomputed only on edit, not every idle frame.
                        let (lines, words, chars) = self.doc_counts_active(active);
                        if let Some(t) = self.tabs.get(active) {
                            // F-025 — clickable EOL segment cycles LF → CRLF → CR.
                            if ui
                                .selectable_label(
                                    false,
                                    RichText::new(t.doc.eol().label().to_string())
                                        .color(muted)
                                        .small()
                                        .monospace(),
                                )
                                .on_hover_text("Click to cycle line-ending: LF → CRLF → CR")
                                .clicked()
                            {
                                cycle_eol_for_active = true;
                            }
                            // F-025 — encoding + language: click opens Settings
                            // so the user lands on the relevant editor section.
                            if ui
                                .selectable_label(
                                    false,
                                    RichText::new(t.doc.encoding().name.clone())
                                        .color(muted)
                                        .small()
                                        .monospace(),
                                )
                                .on_hover_text("Click to open Settings → Editor")
                                .clicked()
                            {
                                open_settings_for = Some("Editor");
                            }
                            let lang = t.doc.language_hint().unwrap_or_else(|| "text".into());
                            if ui
                                .selectable_label(
                                    false,
                                    RichText::new(lang).color(accent).small().monospace(),
                                )
                                .on_hover_text("Click to open Settings → Editor (language hint)")
                                .clicked()
                            {
                                open_settings_for = Some("Editor");
                            }
                            // F-024 — word + line counters in the status bar.
                            // Computed via `doc_counts_active` (PA-04 memo): the
                            // three O(n) passes run once per edit, not per frame.
                            // Word/char are 0 for is_read_only_large() (multi-GB
                            // rope-browser) buffers, as before.
                            ui.label(
                                RichText::new(format!("{lines} ln · {words} w · {chars} ch"))
                                    .color(muted)
                                    .small()
                                    .monospace(),
                            );
                            // F-005 / F-024 from docs/audits/overlooked-surfaces-2026-05-29.md:
                            // Render the caret position ("Ln 4, Col 17") + the selection
                            // length when non-empty. Every editor on Earth ships this
                            // indicator; SCR1B3 used to omit it.
                            if let Some((ln, col)) = self.last_cursor_line_col {
                                ui.label(
                                    RichText::new(format!("Ln {ln}, Col {col}"))
                                        .color(muted)
                                        .small()
                                        .monospace(),
                                );
                            }
                            if self.last_selection_chars > 0 {
                                let sel = self.last_selection_chars;
                                let noun = if sel == 1 { "char" } else { "chars" };
                                ui.label(
                                    RichText::new(format!("({sel} {noun} sel)"))
                                        .color(accent)
                                        .small()
                                        .monospace(),
                                );
                            }
                            if t.doc.is_read_only_large() {
                                ui.label(
                                    RichText::new("[ large file: read-only ]")
                                        .color(muted)
                                        .small()
                                        .monospace(),
                                );
                            }
                            if spell_on {
                                let (txt, col) = if spell_misspellings == 0 {
                                    (format!("spell {}", egui_phosphor::thin::CHECK), accent)
                                } else {
                                    (format!("spell: {spell_misspellings}"), warn)
                                };
                                ui.label(RichText::new(txt).color(col).small().monospace());
                            }
                            if diag_total > 0 {
                                let col = if diag_errors > 0 { warn } else { muted };
                                ui.label(
                                    RichText::new(format!(
                                        "{} {diag_errors}e / {diag_total}",
                                        egui_phosphor::thin::PROHIBIT
                                    ))
                                    .color(col)
                                    .small()
                                    .monospace(),
                                );
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Inset from the right window edge so the last glyph
                            // isn't flush against it (right_to_left places this
                            // space at the right edge, before the text).
                            ui.add_space(8.0);
                            // The status text (e.g. "opened C:\…\file.rs") gets the
                            // width REMAINING to the right of the left-side indicators
                            // and TRUNCATES with an ellipsis instead of overflowing
                            // leftward and overlapping them on a narrow window. The
                            // full text stays available on hover. `.truncate()` clips
                            // to the laid-out width (the remaining space in this
                            // right_to_left child), so it never collides with the left
                            // segments; on a wide window it has room for the whole
                            // string and looks unchanged.
                            if !self.status.is_empty() {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&self.status)
                                            .color(muted)
                                            .small()
                                            .monospace(),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(&self.status);
                            }
                        });
                    });
                });
        }
        // Bottom tab bar (deferred from the tab-position match): rendered HERE,
        // after the status panel, so the status bar keeps the very bottom edge
        // and the tab strip sits directly above it.
        if bottom_tabs_deferred {
            egui::TopBottomPanel::bottom("tabs-bottom")
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| self.draw_tab_strip(ui, accent, muted));
                });
        }
        // F-025 — apply the click-to-edit status-bar actions captured above.
        if cycle_eol_for_active {
            let active = self.active.min(self.tabs.len().saturating_sub(1));
            if let Some(t) = self.tabs.get_mut(active) {
                let next = match t.doc.eol() {
                    scribe_core::eol::Eol::Lf => scribe_core::eol::Eol::Crlf,
                    scribe_core::eol::Eol::Crlf => scribe_core::eol::Eol::Cr,
                    scribe_core::eol::Eol::Cr => scribe_core::eol::Eol::Lf,
                };
                t.doc.set_eol(next);
                self.status = format!("line-ending: {}", next.label());
            }
        }
        if let Some(section) = open_settings_for {
            // Honour the deep-link: open Settings ON the advertised category
            // (the tooltips promise "Settings → Editor"), not the last-used one.
            crate::settings::request_category(ctx, section);
            self.settings_open = true;
        }

        // ---- Toast (errors / notices) ----
        if let Some(msg) = self.toast.clone() {
            egui::TopBottomPanel::bottom("toast").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("!")
                            .color(ui_color(
                                &self.theme,
                                "warning",
                                Rgba::new(0xfb, 0xbf, 0x24, 255),
                            ))
                            .strong(),
                    );
                    ui.label(RichText::new(&msg).small());
                    if ui.small_button("dismiss").clicked() {
                        self.toast = None;
                    }
                });
            });
        }

        // ---- File-tree sidebar ----
        if let Some(root) = self.file_tree_root.clone() {
            egui::SidePanel::left("filetree")
                .default_width(220.0)
                .frame(egui::Frame::default().fill(panel).inner_margin(6.0))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // #74 — the tree supports ↑↓ Home End ⏎ navigation, but
                        // that was undiscoverable. Surface it: a hover tip on the
                        // header plus a always-visible muted key hint.
                        ui.label(RichText::new("EXPLORER").color(accent).small().monospace())
                            .on_hover_text(
                                "File explorer. Keyboard: ↑/↓ move · Home/End jump to first/last \
                                 · Enter open · (works when no dialog is open and the editor isn't \
                                 focused).",
                            );
                        ui.label(
                            RichText::new(format!(
                                "{}{} Home End {}",
                                egui_phosphor::thin::ARROW_UP,
                                egui_phosphor::thin::ARROW_DOWN,
                                egui_phosphor::thin::ARROW_ELBOW_DOWN_LEFT
                            ))
                            .color(muted)
                            .small()
                            .monospace(),
                        )
                        .on_hover_text("Navigate the file tree from the keyboard.");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").clicked() {
                                close_tree = true;
                            }
                        });
                    });
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if let Some(p) = self.file_tree_state.show(ui, &root) {
                            open_from_tree = Some(p);
                        }
                    });
                });
            // F-041: arrow-key / Enter / Home / End nav for the sidebar.
            // Only fires when no modal is open AND the editor isn't focused
            // (egui owns key events when a TextEdit holds focus, so we don't
            // need to gate explicitly on that — `consume_key` is a no-op
            // when the key was already routed to a widget).
            let modal_open = self.palette_open
                || self.find_open
                || self.fuzzy_open
                || self.goto_open
                || self.goto_symbol_open
                || self.recent_open
                || self.recent_folders_open
                || self.cheatsheet_open
                || self.settings_open
                || self.welcome_open;
            if !modal_open {
                if let Some(p) = self.file_tree_state.handle_input(ctx) {
                    open_from_tree = Some(p);
                }
            }
        }

        let active = self.active.min(self.tabs.len().saturating_sub(1));
        self.active = active;
        let font = FontId::monospace(self.config.fonts.clamped_editor_size());
        let line_height = self.config.fonts.clamped_line_height();
        let word_wrap = self.config.editor.word_wrap;
        let show_line_numbers = self.config.editor.show_line_numbers;
        let gutter_row_h = font.size * line_height;
        let ext = self.tabs[active].doc.language_hint();
        let read_only = self.tabs[active].doc.is_read_only_large();
        // The editor should be ready to type whenever no field/menu is open.
        let overlay_open = self.find_open || self.palette_open || self.settings_open;

        // ---- Wave-5 P1: markdown live preview (right side panel) ----
        // Only for markdown buffers; renders the buffer via pulldown-cmark.
        if self.md_preview_open && !chrome_hidden {
            let is_md = self
                .tabs
                .get(active)
                .and_then(|t| t.doc.language_hint())
                .map(|l| l == "md" || l == "markdown")
                .unwrap_or(false);
            if is_md {
                let md = self.tabs[active].text.clone();
                egui::SidePanel::right("md-preview")
                    .default_width(360.0)
                    .frame(egui::Frame::default().fill(panel).inner_margin(8.0))
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Markdown preview").color(muted).small());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("close").clicked() {
                                        self.md_preview_open = false;
                                    }
                                },
                            );
                        });
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                crate::md_preview::show(ui, &md, accent, muted);
                            });
                    });
            }
        }

        // ---- Wave-5 P1: diff vs disk (right side panel) ----
        if self.diff_view_open && !chrome_hidden {
            let cur = self.tabs.get(active).map(|t| t.text.clone());
            let disk = self
                .tabs
                .get(active)
                .and_then(|t| t.doc.path())
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default();
            let colors = crate::diff_view::DiffColors {
                insert: ui_color(&self.theme, "ok", Rgba::new(0x6e, 0xc7, 0x7a, 255)),
                delete: ui_color(&self.theme, "error", Rgba::new(0xd0, 0x6e, 0x6e, 255)),
                context: muted,
            };
            if let Some(cur) = cur {
                egui::SidePanel::right("diff-view")
                    .default_width(420.0)
                    .frame(egui::Frame::default().fill(panel).inner_margin(8.0))
                    .show(ctx, |ui| {
                        let rows = crate::diff_view::diff_lines(&disk, &cur);
                        let (ins, del) = crate::diff_view::summary(&rows);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Diff vs disk").color(muted).small());
                            ui.label(
                                RichText::new(format!("+{ins}"))
                                    .color(colors.insert)
                                    .small(),
                            );
                            ui.label(
                                RichText::new(format!("-{del}"))
                                    .color(colors.delete)
                                    .small(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("close").clicked() {
                                        self.diff_view_open = false;
                                    }
                                },
                            );
                        });
                        ui.separator();
                        crate::diff_view::show_rows(ui, &rows, colors);
                    });
            }
        }

        // ---- Minimap (rightmost strip) ----
        // Skipped for read-only huge files: the minimap hashes + lays out the
        // whole buffer, which defeats the viewport-culled browse path below.
        if self.config.editor.show_minimap && !read_only && !chrome_hidden {
            self.show_minimap(ctx, panel, accent);
        }

        // Split is no longer a separate same-buffer side panel — it is unified
        // with the multi-note grid (`editor.grid_enabled`): the open tabs render
        // as panes (two = side-by-side split, more = grid) via
        // `render_grid_central_panel`. See the "split" toolbar button + the
        // grid central-panel branch above.

        // ---- Line-number gutter (sticky left strip; numbers are synced to the
        // editor galley rows captured last frame — one-frame lag, like minimap).
        // The external gutter is driven by the TextEdit's per-line galley Ys
        // (`line_gutter`). The read-only RopeEditor draws its OWN gutter, so
        // skip this one there (and avoid the O(n) `lines().count()` on a
        // 256 MiB+ buffer).
        if show_line_numbers && !self.fold_view && !read_only && !chrome_hidden {
            // Change-bar: refresh the per-line state cache before borrowing it.
            self.ensure_change_states(active);
            // PA-05: reuse the PA-04 (edit_gen, doc_id) memo for the gutter
            // digit-width line count — no extra per-frame O(n) `lines().count()`.
            let total = self.doc_counts_active(active).0;
            let digits = total.to_string().len().max(2);
            let gutter_w = digits as f32 * (font.size * 0.62) + 16.0;
            let rows = &self.line_gutter;
            let bookmarks = &self.tabs[active].bookmarks;
            let show_change_bar = self.config.editor.show_change_bar;
            let change_states = &self.tabs[active].change_states;
            let cb_unsaved = ui_color(
                &self.theme,
                "change_bar_unsaved",
                Rgba::new(0xf2, 0xb3, 0x3d, 255),
            );
            let cb_saved = ui_color(
                &self.theme,
                "change_bar_saved",
                Rgba::new(0x6f, 0xb8, 0x9a, 255),
            );
            egui::SidePanel::left("line-gutter")
                .exact_width(gutter_w)
                .resizable(false)
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    let painter = ui.painter();
                    let clip = ui.clip_rect();
                    let rx = ui.max_rect().right() - 8.0;
                    let lx = ui.max_rect().left() + 4.0;
                    // Change bar sits flush against the gutter's right edge
                    // (between the numbers and the text), Notepad++-style.
                    let bar_r = ui.max_rect().right();
                    let nfont = FontId::monospace((font.size * 0.92).max(8.0));
                    for (i, &y) in rows.iter().enumerate() {
                        if y < clip.top() - gutter_row_h || y > clip.bottom() {
                            continue;
                        }
                        // Change-bar stripe: amber for edited-unsaved lines,
                        // green for edited-then-saved; untouched lines have none.
                        if show_change_bar {
                            let col = match change_states.get(i) {
                                Some(crate::change_bar::LineChange::Unsaved) => Some(cb_unsaved),
                                Some(crate::change_bar::LineChange::Saved) => Some(cb_saved),
                                _ => None,
                            };
                            if let Some(col) = col {
                                // 3.5px stripe flush to the gutter's right edge
                                // (Notepad++/VS Code use ~3px; a touch wider here
                                // so it reads clearly at the gutter boundary).
                                painter.rect_filled(
                                    egui::Rect::from_min_max(
                                        egui::pos2(bar_r - 3.5, y),
                                        egui::pos2(bar_r, y + gutter_row_h),
                                    ),
                                    0.0,
                                    col,
                                );
                            }
                        }
                        // Bookmark marker: a small filled dot at the gutter's
                        // left edge for each bookmarked (0-based) line.
                        if bookmarks.contains(&i) {
                            painter.circle_filled(
                                egui::pos2(lx, y + gutter_row_h * 0.5),
                                3.0,
                                accent,
                            );
                        }
                        painter.text(
                            egui::pos2(rx, y),
                            egui::Align2::RIGHT_TOP,
                            (i + 1).to_string(),
                            nfont.clone(),
                            muted,
                        );
                    }
                });
        }

        // ---- Central editor surface ----
        // Phase 18 T18.2 — when the multi-note grid is enabled, render
        // every open tab as a movable / resizable pane via egui_tiles.
        // The single-pane code path below stays the default for users
        // who don't opt in.
        if self.grid_tree.is_some() {
            self.render_grid_central_panel(ctx, font.clone());
        } else {
            egui::CentralPanel::default().show(ctx, |ui| {
                // Folded read-only preview is a distinct surface (no live editing).
                if self.fold_view {
                    self.show_fold_view(ui, font.clone(), ext.as_deref());
                    return;
                }

                // Read-only huge-file browse (KEYSTONE): a file past the
                // 256 MiB threshold opens read-only. Rendering it through the
                // viewport-culled RopeEditor — instead of laying out the whole
                // multi-hundred-MiB string in a TextEdit every frame — is the
                // O(viewport) browse path. Read-only ⇒ no editing regression;
                // the widget draws its own line numbers + viewport-scoped
                // syntax highlighting (F-030).
                if read_only {
                    let rope = self.tabs[active].doc.rope().clone();
                    let mut buf = scribe_core::buffer::Buffer::Rope(rope);
                    let fg = ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
                    scribe_render::RopeEditor::new(&mut buf, font.clone(), gutter_row_h)
                        .with_text_color(fg)
                        .with_gutter_color(muted)
                        .with_line_numbers(show_line_numbers)
                        .with_syntax(&self.hl, ext.clone())
                        .show(ui);
                    return;
                }

                // KEYSTONE — experimental owned rope editor (opt-in). Renders
                // normal files through the in-house editor (own caret /
                // selection / undo) instead of egui's TextEdit. The rope is
                // bridged from `text` each frame and written back after, so the
                // rest of the app (save, status bar, find) keeps seeing a
                // String. Default OFF — the egui path below stays canonical.
                // Wave-3: ALSO auto-engaged for buffers past the configured byte
                // threshold (default 16 MiB) so a multi-MiB file gets O(viewport)
                // rendering instead of the per-frame O(n) egui TextEdit.
                if use_rope_editor(
                    self.config.editor.experimental_rope_editor,
                    self.tabs[active].text.len(),
                    self.config.editor.rope_editor_auto_threshold_bytes,
                ) {
                    let fg = ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
                    // KEYSTONE perf: the rope persists across frames in the tab.
                    // Build it once (O(n)) from `text`; thereafter the widget
                    // mutates it in place and we sync back to `text` ONLY when
                    // an edit actually changed content. `ropey` clones are O(1)
                    // (Arc-shared), so persistence costs no extra memory churn.
                    // Capture the disjoint fields the editor needs BEFORE the
                    // `&mut self.tabs` borrow (Wave-5 P1 snippets — gated on the
                    // config toggle; `&self.snippets` coexists with the tab's
                    // mutable rope borrow as a disjoint-field borrow).
                    let render_whitespace = self.config.editor.render_whitespace;
                    let snippets_enabled = self.config.editor.snippets_enabled;
                    let snippets = &self.snippets;
                    let hl = &self.hl;
                    let tab = &mut self.tabs[active];
                    // Lazily (re)build the persistent rope from `text`. Done as a
                    // separate `is_none` check rather than `get_or_insert_with`
                    // so the closure does not capture `tab` while `rope_buf` is
                    // mutably borrowed (disjoint-field borrow).
                    if tab.rope_buf.is_none() {
                        tab.rope_buf = Some(scribe_core::buffer::Buffer::from_text(&tab.text));
                    }
                    let buf = tab.rope_buf.as_mut().expect("rope_buf set above");
                    let state = tab
                        .rope_state
                        .get_or_insert_with(scribe_render::RopeEditorState::new);
                    let mut editor =
                        scribe_render::RopeEditor::new(buf, font.clone(), gutter_row_h)
                            .with_text_color(fg)
                            .with_gutter_color(muted)
                            .with_line_numbers(show_line_numbers)
                            .with_render_whitespace(render_whitespace)
                            .with_syntax(hl, ext.clone());
                    if snippets_enabled {
                        editor = editor.with_snippets(snippets);
                    }
                    let (resp, clipboard) = editor.show_editable(ui, state);
                    // Sync `text` from the rope ONLY on a real content edit — the
                    // O(n) `to_string()` now runs on keystrokes, not every frame.
                    if resp.content_changed {
                        if let Some(rope) = tab.rope_buf.as_ref().and_then(|b| b.as_rope()) {
                            tab.text = rope.to_string();
                            tab.doc.mark_dirty();
                        }
                        // Wave-3: rope write-back bypasses set_text + the egui
                        // Response, so bump the gen counter here for parity.
                        tab.edit_gen = tab.edit_gen.wrapping_add(1);
                    }
                    if let Some(text) = clipboard {
                        // On Cut the selection is already removed from the buffer,
                        // so a clipboard failure here means the text is only
                        // undo-recoverable — log it rather than swallow silently.
                        match arboard::Clipboard::new() {
                            Ok(mut cb) => {
                                if let Err(e) = cb.set_text(text) {
                                    tracing::warn!(
                                        "clipboard write after cut/copy failed; text is \
                                         still undo-recoverable: {e}"
                                    );
                                }
                            }
                            Err(e) => tracing::warn!(
                                "could not open the clipboard for cut/copy; text is still \
                                 undo-recoverable: {e}"
                            ),
                        }
                    }
                    return;
                }

                // F-033 / F-034 from docs/audits/overlooked-surfaces-2026-05-29.md:
                // brace-delimited definition scopes for the breadcrumb bar (above
                // the editor) and the sticky-scroll headers (pinned at the
                // viewport top). P-05: memoized by `(edit_gen, doc_id)` so the
                // O(n) scan runs only on an edit or a tab switch, not every
                // frame. Still skipped for very large buffers inside the memo.
                let scopes = self.symbol_scopes_for_active();
                // Breadcrumb bar (F-033): the enclosing-symbol path of the
                // cursor line, outermost first (`mod foo › impl Bar › fn baz`).
                if !scopes.is_empty() {
                    let cursor_line0 = self
                        .last_cursor_line_col
                        .map(|(l, _)| l.saturating_sub(1))
                        .unwrap_or(0);
                    let crumbs = crate::editor_features::breadcrumb_at(&scopes, cursor_line0);
                    if !crumbs.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for (i, s) in crumbs.iter().enumerate() {
                                if i > 0 {
                                    ui.label(RichText::new("›").color(muted).small());
                                }
                                ui.label(RichText::new(&s.label).color(accent).small().monospace());
                            }
                        });
                        ui.separator();
                    }
                }

                // Tab inserts the configured number of spaces (when insert_spaces is
                // on) rather than a literal tab — honours editor.tab_width /
                // insert_spaces. Consume the key before the TextEdit can see it.
                let editor_id = egui::Id::new("scr1b3-central-editor");
                if !read_only
                    && self.config.editor.insert_spaces
                    && ctx.memory(|m| m.has_focus(editor_id))
                    && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
                {
                    self.indent_with_spaces(ctx, editor_id, active);
                }

                // #107 — auto-indent on Enter: the new line keeps the current
                // line's leading whitespace. Only consume Enter when there IS
                // indentation to carry (otherwise let egui insert the plain
                // newline). Skipped while the completion popup owns Enter.
                if !read_only
                    && self.completion.is_none()
                    && ctx.memory(|m| m.has_focus(editor_id))
                    && ctx.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.is_none())
                    && self.auto_indent_newline(ctx, editor_id, active)
                {
                    ctx.input_mut(|i| {
                        i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                    });
                }

                // Caret commands from the keyboard (`act.*`) or the command
                // palette (`self.pending_*`). They need the egui TextEditState,
                // so they run here — after the editor stored its state this
                // frame — and `store` takes effect next frame.
                if !read_only {
                    if act.jump_bracket || std::mem::take(&mut self.pending_jump_bracket) {
                        self.jump_matching_bracket(ctx, editor_id, active);
                    }
                    if std::mem::take(&mut self.pending_insert_datetime) {
                        self.insert_datetime_at_caret(ctx, editor_id, active);
                    }
                    if std::mem::take(&mut self.pending_dup_selection) {
                        self.duplicate_selection(ctx, editor_id, active);
                    }
                }

                // #78 — misspellings for the active buffer, computed (memoized)
                // BEFORE the partial borrows below so the owned Vec can move into
                // the editor closure and drive the red underline painter.
                let misspellings = self.misspellings_for_active();
                // Wave-5: compute all find matches once (needs &self) so the
                // highlight-all overlay can paint every match, not just the
                // navigated one. Empty when the find bar is closed.
                let find_hits: Vec<scribe_core::search::Match> = if self.find_open {
                    self.find_matches_active()
                } else {
                    Vec::new()
                };
                let find_cur = self.find_match_idx;
                // Scope the layouter (which borrows `self.hl`) so it drops before
                // the `&mut self` completion calls below.
                let mut new_gutter: Vec<f32> = Vec::new();
                // F-034: a clicked sticky header records its target line here;
                // it is applied to `pending_scroll` after the hl borrow drops.
                let mut sticky_jump: Option<usize> = None;
                let anchor: Option<(egui::Pos2, usize)> = {
                    let hl = &self.hl;
                    let ext_ref = ext.as_deref();
                    let layout_fg =
                        ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
                    // #D — the themeable URL colour: the chrome theme's `[syntax]`
                    // `url` token, falling back to the `accent` UI colour so every
                    // theme colours links coherently without extra config.
                    let detect_links = self.config.editor.detect_links;
                    let url_color = scribe_render::color32(self.theme.syntax_color(
                        "url",
                        self.theme.ui("accent", Rgba::new(0x4c, 0xc2, 0xff, 255)),
                    ));
                    let mut layouter = make_layouter(
                        hl,
                        &self.hl_cache,
                        &self.hl_galley_cache,
                        &self.hl_inc_cache,
                        ext_ref,
                        font.clone(),
                        line_height,
                        word_wrap,
                        layout_fg,
                        url_color,
                        detect_links,
                    );
                    let mut sa = if word_wrap {
                        egui::ScrollArea::vertical()
                    } else {
                        egui::ScrollArea::both()
                    };
                    if let Some(off) = self.pending_scroll.take() {
                        sa = sa.vertical_scroll_offset(off);
                    }
                    // Wave-6 scrollbar style.
                    sa = match self.config.editor.scrollbar_style {
                        scribe_core::config::ScrollbarStyle::Hidden => sa.scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                        ),
                        scribe_core::config::ScrollbarStyle::Thin
                        | scribe_core::config::ScrollbarStyle::Auto => sa.scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                        ),
                    };
                    let thin_scrollbar = self.config.editor.scrollbar_style
                        == scribe_core::config::ScrollbarStyle::Thin;
                    let mut a: Option<(egui::Pos2, usize)> = None;
                    let sa_out = sa.show(ui, |ui| {
                        if thin_scrollbar {
                            ui.style_mut().spacing.scroll.bar_width = 6.0;
                        }
                        let dw = if word_wrap {
                            ui.available_width()
                        } else {
                            f32::INFINITY
                        };
                        let editor = egui::TextEdit::multiline(&mut self.tabs[active].text)
                            .id(editor_id)
                            .code_editor()
                            .desired_width(dw)
                            .desired_rows(30)
                            .lock_focus(true)
                            .interactive(!read_only)
                            .layouter(&mut layouter);
                        let out = editor.show(ui);
                        // Wave-3: the egui in-place edit happened inside show();
                        // `.changed()` is true exactly on the edited frame, so this
                        // is the ONLY hook for the default editor's text mutation.
                        // Bump the gen counter so the minimap + spell caches refresh.
                        if out.response.changed() {
                            self.tabs[active].edit_gen = self.tabs[active].edit_gen.wrapping_add(1);
                        }
                        // #D — clickable-URL overlay pass. The persistent colour +
                        // underline is painted by the syntax layer (highlight_job);
                        // here we add the hover affordance (P1) and the click-open
                        // (P0). Detect http(s):// URLs as GLOBAL byte ranges, map the
                        // pointer to a char via the galley, and on Ctrl/Cmd-click
                        // over a URL open it in the OS browser — scheme-allow-listed
                        // to http/https (a URL in a file is untrusted data; open only
                        // on an explicit modifier-click, never on render). Bounded by
                        // a buffer-size cap like the other per-frame overlays.
                        if self.config.editor.detect_links
                            && self.tabs[active].text.len() <= 1_000_000
                        {
                            let text_ref = &self.tabs[active].text;
                            let mut url_spans: Vec<(usize, usize, &str)> = Vec::new();
                            let mut base = 0usize;
                            for line in text_ref.split_inclusive('\n') {
                                for r in scribe_core::url_scan::detect_urls(line) {
                                    url_spans.push((base + r.start, base + r.end, &line[r]));
                                }
                                base += line.len();
                            }
                            if !url_spans.is_empty() {
                                if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                                    if out.response.rect.contains(p) {
                                        let rel = p - out.galley_pos;
                                        let ci = out.galley.cursor_from_pos(rel).index;
                                        let byte = char_to_byte(text_ref, ci);
                                        if let Some(&(_, _, url)) = url_spans
                                            .iter()
                                            .find(|(s, e, _)| byte >= *s && byte < *e)
                                        {
                                            let cmd = ui.input(|i| i.modifiers.command);
                                            // P1 — pointer affordance when the follow
                                            // modifier is held.
                                            if cmd {
                                                ui.ctx().set_cursor_icon(
                                                    egui::CursorIcon::PointingHand,
                                                );
                                            }
                                            // P1 — anti-phishing hover preview of the
                                            // destination (so the user sees where a
                                            // link goes before opening it).
                                            egui::show_tooltip_at_pointer(
                                                ui.ctx(),
                                                out.response.layer_id,
                                                egui::Id::new("scr1b3-url-tooltip"),
                                                |ui| {
                                                    ui.label(if cmd {
                                                        url.to_string()
                                                    } else {
                                                        format!("{url}  —  Ctrl+click to open")
                                                    });
                                                },
                                            );
                                            // P0 — open only on explicit modifier-click,
                                            // and only for an http/https scheme.
                                            if cmd
                                                && ui.input(|i| i.pointer.primary_clicked())
                                                && scribe_core::url_scan::is_clickable_url(url)
                                            {
                                                ui.ctx().open_url(egui::OpenUrl::new_tab(
                                                    url.to_string(),
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // #78 — paint a red squiggle under each misspelling. Map
                        // the byte span to galley cursor rects and draw a wavy
                        // underline along the word's baseline. Painted on the
                        // editor's own layer so it scrolls with the text.
                        if !misspellings.is_empty() {
                            let text_ref = &self.tabs[active].text;
                            let painter = ui.painter();
                            let red = Color32::from_rgb(0xe5, 0x3e, 0x3e);
                            for m in &misspellings {
                                let c0 = byte_to_char_index(text_ref, m.start);
                                let c1 = byte_to_char_index(text_ref, m.end);
                                let r0 = out.galley.pos_from_cursor(egui::text::CCursor::new(c0));
                                let r1 = out.galley.pos_from_cursor(egui::text::CCursor::new(c1));
                                // Same row only (words don't wrap); skip if the
                                // span spans rows (rare) to avoid a stray line.
                                if (r0.min.y - r1.min.y).abs() > 0.5 {
                                    continue;
                                }
                                let y = out.galley_pos.y + r0.max.y;
                                let x0 = out.galley_pos.x + r0.min.x;
                                let x1 = out.galley_pos.x + r1.min.x;
                                paint_squiggle(painter, x0, x1, y, red);
                            }
                        }
                        // Wave-5: incremental highlight-all — paint a translucent
                        // accent wash behind EVERY live find match (the current
                        // match stronger). Same galley-rect mapping as the
                        // squiggle painter; low alpha keeps the glyph legible.
                        if !find_hits.is_empty() {
                            let text_ref = &self.tabs[active].text;
                            let painter = ui.painter();
                            let hl_fill = accent.gamma_multiply(0.28);
                            let cur_fill = accent.gamma_multiply(0.5);
                            for (idx, m) in find_hits.iter().enumerate() {
                                let c0 = byte_to_char_index(text_ref, m.start);
                                let c1 = byte_to_char_index(text_ref, m.end);
                                let r0 = out.galley.pos_from_cursor(egui::text::CCursor::new(c0));
                                let r1 = out.galley.pos_from_cursor(egui::text::CCursor::new(c1));
                                if (r0.min.y - r1.min.y).abs() > 0.5 {
                                    continue;
                                }
                                let top = out.galley_pos.y + r0.min.y;
                                let bot = out.galley_pos.y + r0.max.y;
                                let x0 = out.galley_pos.x + r0.min.x;
                                let x1 = out.galley_pos.x + r1.min.x;
                                let fill = if idx == find_cur { cur_fill } else { hl_fill };
                                painter.rect_filled(
                                    egui::Rect::from_min_max(
                                        egui::pos2(x0, top),
                                        egui::pos2(x1, bot),
                                    ),
                                    2.0,
                                    fill,
                                );
                            }
                        }
                        // #28 — render-whitespace overlay for the DEFAULT egui
                        // TextEdit path. Previously the `·`/`→` markers only drew
                        // in the experimental rope editor, so the toggle did
                        // nothing in the default editor. Walk the laid-out galley
                        // glyphs (so the markers follow wrapping AND the chosen
                        // monospace face) and paint a faint `·` centred in each
                        // space cell, `→` in each tab cell. Pure overlay — the
                        // buffer text and the syntax spans are untouched.
                        if self.config.editor.render_whitespace {
                            let painter = ui.painter();
                            let ws_font =
                                FontId::monospace(self.config.fonts.clamped_editor_size());
                            let ws_color = muted.gamma_multiply(0.7);
                            let origin = out.galley_pos.to_vec2();
                            for row in &out.galley.rows {
                                let row_off = origin + row.pos.to_vec2();
                                let cy = row_off.y + row.size.y * 0.5;
                                for g in &row.glyphs {
                                    let marker = match g.chr {
                                        ' ' => "·",
                                        '\t' => "→",
                                        _ => continue,
                                    };
                                    let cx = row_off.x + g.pos.x + g.advance_width * 0.5;
                                    painter.text(
                                        egui::pos2(cx, cy),
                                        egui::Align2::CENTER_CENTER,
                                        marker,
                                        ws_font.clone(),
                                        ws_color,
                                    );
                                }
                            }
                        }
                        // Wave-6 indent guides: faint vertical lines at each
                        // tab_width column, drawn by walking the laid-out galley so
                        // they follow the chosen monospace face + wrapping.
                        if self.config.editor.indent_guides {
                            let painter = ui.painter();
                            let origin = out.galley_pos.to_vec2();
                            let cell_w = out
                                .galley
                                .rows
                                .iter()
                                .flat_map(|r| r.glyphs.iter())
                                .map(|g| g.advance_width)
                                .find(|w| *w > 0.0)
                                .unwrap_or(self.config.fonts.clamped_editor_size() * 0.6);
                            let step = cell_w * self.config.editor.tab_width as f32;
                            if step > 1.0 {
                                let guide = Color32::from_rgba_unmultiplied(
                                    muted.r(),
                                    muted.g(),
                                    muted.b(),
                                    40,
                                );
                                for row in &out.galley.rows {
                                    let row_off = origin + row.pos.to_vec2();
                                    let lead: f32 = row
                                        .glyphs
                                        .iter()
                                        .take_while(|g| g.chr == ' ' || g.chr == '\t')
                                        .map(|g| g.advance_width)
                                        .sum();
                                    let top = row_off.y;
                                    let bot = row_off.y + row.size.y;
                                    let mut x = row_off.x + step;
                                    while x <= row_off.x + lead + 0.5 {
                                        painter.line_segment(
                                            [egui::pos2(x, top), egui::pos2(x, bot)],
                                            egui::Stroke::new(1.0, guide),
                                        );
                                        x += step;
                                    }
                                }
                            }
                        }
                        // Trailing-whitespace tint: faintly mark the trailing
                        // space/tab run on each line (distinct from
                        // render_whitespace, which marks ALL whitespace).
                        if self.config.editor.highlight_trailing_whitespace {
                            let painter = ui.painter();
                            let tint = ui_color(
                                &self.theme,
                                "trailing_whitespace",
                                Rgba::new(0xd0, 0x6e, 0x6e, 28),
                            );
                            let origin = out.galley_pos.to_vec2();
                            for row in &out.galley.rows {
                                let row_off = origin + row.pos.to_vec2();
                                let mut run_start: Option<f32> = None;
                                let mut run_end = 0.0;
                                for g in &row.glyphs {
                                    if g.chr == ' ' || g.chr == '\t' {
                                        if run_start.is_none() {
                                            run_start = Some(row_off.x + g.pos.x);
                                        }
                                        run_end = row_off.x + g.pos.x + g.advance_width;
                                    } else {
                                        run_start = None;
                                    }
                                }
                                if let Some(sx) = run_start {
                                    painter.rect_filled(
                                        egui::Rect::from_min_max(
                                            egui::pos2(sx, row_off.y),
                                            egui::pos2(run_end, row_off.y + row.size.y),
                                        ),
                                        0.0,
                                        tint,
                                    );
                                }
                            }
                        }
                        // Column rulers: thin vertical guides at the configured
                        // 1-based columns (monospace; most meaningful without wrap).
                        if !self.config.editor.rulers.is_empty() {
                            let painter = ui.painter();
                            let cell_w = out
                                .galley
                                .rows
                                .iter()
                                .flat_map(|r| r.glyphs.iter())
                                .map(|g| g.advance_width)
                                .find(|w| *w > 0.0)
                                .unwrap_or(self.config.fonts.clamped_editor_size() * 0.6);
                            let ruler = ui_color(
                                &self.theme,
                                "ruler",
                                Rgba::new(muted.r(), muted.g(), muted.b(), 40),
                            );
                            let top = out.galley_pos.y;
                            let bot =
                                out.galley_pos.y + out.galley.size().y.max(ui.available_height());
                            for &col in &self.config.editor.rulers {
                                let x = out.galley_pos.x + cell_w * col as f32;
                                painter.line_segment(
                                    [egui::pos2(x, top), egui::pos2(x, bot)],
                                    egui::Stroke::new(1.0, ruler),
                                );
                            }
                        }
                        if let Some(range) = out.cursor_range {
                            // egui 0.34: CursorRange.primary is a CCursor directly
                            // (no nested .ccursor); Galley::pos_from_ccursor was
                            // renamed to pos_from_cursor (takes CCursor by value).
                            let cc = range.primary;
                            let rect = out.galley.pos_from_cursor(cc);
                            let pos = out.galley_pos + egui::vec2(rect.min.x, rect.max.y);
                            a = Some((pos, cc.index));
                            // F-005 / F-024 from docs/audits/overlooked-surfaces-2026-05-29.md:
                            // compute the human-visible (1-based) line + column and the
                            // selection-length-in-chars from the rope buffer + the
                            // egui CursorRange. This drives the status-bar "Ln N, Col N"
                            // and "(N chars selected)" indicators.
                            let text_ref = &self.tabs[active].text;
                            self.last_cursor_line_col =
                                Some(line_col_from_char_index(text_ref, cc.index));
                            self.last_selection_chars =
                                range.primary.index.abs_diff(range.secondary.index);
                            // Wave-6 motion: feed the caret-trail when the caret moves.
                            if self.config.motion.enabled && self.config.motion.caret_trail {
                                let t = ui.input(|i| i.time);
                                let caret_rect = egui::Rect::from_min_max(
                                    out.galley_pos + rect.min.to_vec2(),
                                    out.galley_pos + rect.max.to_vec2(),
                                )
                                .expand2(egui::vec2(1.0, 0.0));
                                let moved = self
                                    .caret_trail
                                    .back()
                                    .is_none_or(|(r, _)| r.min.distance(caret_rect.min) > 1.0);
                                if moved {
                                    self.caret_trail.push_back((caret_rect, t));
                                    while self.caret_trail.len() > 24 {
                                        self.caret_trail.pop_front();
                                    }
                                }
                            }
                            let collapsed = range.primary.index == range.secondary.index;
                            // Highlight every OTHER occurrence of the current
                            // selection (VS Code style). Single-line,
                            // non-whitespace selections only; bounded like
                            // bracket_match to stay cheap on huge files.
                            if self.config.editor.highlight_selection_occurrences
                                && !collapsed
                                && self.tabs[active].text.len() <= 500_000
                            {
                                let text_ref = &self.tabs[active].text;
                                let lo_ci = range.primary.index.min(range.secondary.index);
                                let hi_ci = range.primary.index.max(range.secondary.index);
                                let lo_b = char_to_byte(text_ref, lo_ci);
                                let hi_b = char_to_byte(text_ref, hi_ci);
                                let selected = &text_ref[lo_b..hi_b];
                                if !selected.trim().is_empty() && !selected.contains('\n') {
                                    let q = scribe_core::search::Query {
                                        pattern: selected.to_string(),
                                        case_sensitive: true,
                                        ..Default::default()
                                    };
                                    if let Ok(hits) = scribe_core::search::find_all(text_ref, &q) {
                                        let painter = ui.painter();
                                        let occ = ui_color(
                                            &self.theme,
                                            "selection_occurrence",
                                            Rgba::new(accent.r(), accent.g(), accent.b(), 130),
                                        );
                                        for m in &hits {
                                            if m.start == lo_b {
                                                continue; // skip the active selection itself
                                            }
                                            let c0 = byte_to_char_index(text_ref, m.start);
                                            let c1 = byte_to_char_index(text_ref, m.end);
                                            let r0 = out
                                                .galley
                                                .pos_from_cursor(egui::text::CCursor::new(c0));
                                            let r1 = out
                                                .galley
                                                .pos_from_cursor(egui::text::CCursor::new(c1));
                                            if (r0.min.y - r1.min.y).abs() > 0.5 {
                                                continue; // wrapped span; skip
                                            }
                                            let bx = egui::Rect::from_min_max(
                                                out.galley_pos + egui::vec2(r0.min.x, r0.min.y),
                                                out.galley_pos + egui::vec2(r1.min.x, r0.max.y),
                                            );
                                            painter.rect_stroke(
                                                bx,
                                                2.0,
                                                egui::Stroke::new(1.0, occ),
                                                egui::StrokeKind::Inside,
                                            );
                                        }
                                    }
                                }
                            }
                            // Wave-6 current-line highlight: a faint full-width band
                            // across the caret's galley row. Low alpha so it reads as
                            // a tint behind the (opaque) glyphs. Skipped on selection.
                            if self.config.editor.current_line_highlight && collapsed {
                                let painter = ui.painter();
                                let y0 = out.galley_pos.y + rect.min.y;
                                let y1 = out.galley_pos.y + rect.max.y;
                                let band = egui::Rect::from_min_max(
                                    egui::pos2(out.galley_pos.x, y0),
                                    egui::pos2(
                                        out.galley_pos.x
                                            + out.galley.size().x.max(ui.available_width()),
                                        y1,
                                    ),
                                );
                                let hl = Color32::from_rgba_unmultiplied(
                                    accent.r(),
                                    accent.g(),
                                    accent.b(),
                                    22,
                                );
                                painter.rect_filled(band, 0.0, hl);
                            }
                            // Wave-6 bracket-match: box the bracket next to the caret
                            // and its partner. The O(n) scan is bounded to a sane
                            // buffer size to stay cheap on huge files.
                            if self.config.editor.bracket_match
                                && collapsed
                                && self.tabs[active].text.len() <= 500_000
                            {
                                let text_ref = &self.tabs[active].text;
                                if let Some((open_ci, close_ci)) =
                                    matching_bracket_char_indices(text_ref, cc.index)
                                {
                                    let painter = ui.painter();
                                    let box_col = Color32::from_rgba_unmultiplied(
                                        accent.r(),
                                        accent.g(),
                                        accent.b(),
                                        60,
                                    );
                                    for ci in [open_ci, close_ci] {
                                        let r0 = out
                                            .galley
                                            .pos_from_cursor(egui::text::CCursor::new(ci));
                                        let r1 = out
                                            .galley
                                            .pos_from_cursor(egui::text::CCursor::new(ci + 1));
                                        if (r0.min.y - r1.min.y).abs() > 0.5 {
                                            continue; // span wrapped; skip
                                        }
                                        let bx = egui::Rect::from_min_max(
                                            out.galley_pos + egui::vec2(r0.min.x, r0.min.y),
                                            out.galley_pos + egui::vec2(r1.min.x, r0.max.y),
                                        );
                                        painter.rect_stroke(
                                            bx,
                                            1.0,
                                            egui::Stroke::new(1.0, box_col),
                                            egui::StrokeKind::Inside,
                                        );
                                    }
                                }
                            }
                            // Wave-6 caret style: draw a Block/Underline shape over
                            // egui's native caret (focus + no selection only). Honour
                            // blink when motion.cursor_blink is on.
                            if self.config.editor.caret_style
                                != scribe_core::config::CaretStyle::Bar
                                && collapsed
                                && out.response.has_focus()
                            {
                                let now = ui.ctx().input(|i| i.time);
                                let blink =
                                    self.config.motion.enabled && self.config.motion.cursor_blink;
                                let visible = if blink {
                                    (now / 1.06).rem_euclid(1.0) < 0.6
                                } else {
                                    true
                                };
                                if blink {
                                    ui.ctx().request_repaint_after(
                                        std::time::Duration::from_millis(120),
                                    );
                                }
                                if visible {
                                    let painter = ui.painter();
                                    let caret_col = ui_color(
                                        &self.theme,
                                        "caret",
                                        Rgba::new(accent.r(), accent.g(), accent.b(), 255),
                                    );
                                    let x = out.galley_pos.x + rect.min.x;
                                    let y0 = out.galley_pos.y + rect.min.y;
                                    let y1 = out.galley_pos.y + rect.max.y;
                                    let w = self.config.editor.clamped_caret_width();
                                    let cell_w = out
                                        .galley
                                        .rows
                                        .iter()
                                        .flat_map(|r| r.glyphs.iter())
                                        .map(|g| g.advance_width)
                                        .find(|w| *w > 0.0)
                                        .unwrap_or(self.config.fonts.clamped_editor_size() * 0.6);
                                    match self.config.editor.caret_style {
                                        scribe_core::config::CaretStyle::Block => {
                                            let blk = Color32::from_rgba_unmultiplied(
                                                caret_col.r(),
                                                caret_col.g(),
                                                caret_col.b(),
                                                110,
                                            );
                                            painter.rect_filled(
                                                egui::Rect::from_min_max(
                                                    egui::pos2(x, y0),
                                                    egui::pos2(x + cell_w, y1),
                                                ),
                                                0.0,
                                                blk,
                                            );
                                        }
                                        scribe_core::config::CaretStyle::Underline => {
                                            painter.rect_filled(
                                                egui::Rect::from_min_max(
                                                    egui::pos2(x, y1 - w.max(2.0)),
                                                    egui::pos2(x + cell_w, y1),
                                                ),
                                                0.0,
                                                caret_col,
                                            );
                                        }
                                        scribe_core::config::CaretStyle::Bar => {}
                                    }
                                }
                            }
                            // Wider Bar caret (width only): egui's caret is ~1px;
                            // overpaint a wider bar at the same x when width > 1.5.
                            if self.config.editor.caret_style
                                == scribe_core::config::CaretStyle::Bar
                                && self.config.editor.clamped_caret_width() > 1.5
                                && collapsed
                                && out.response.has_focus()
                            {
                                let painter = ui.painter();
                                let caret_col = ui_color(
                                    &self.theme,
                                    "caret",
                                    Rgba::new(accent.r(), accent.g(), accent.b(), 255),
                                );
                                let x = out.galley_pos.x + rect.min.x;
                                let y0 = out.galley_pos.y + rect.min.y;
                                let y1 = out.galley_pos.y + rect.max.y;
                                painter.rect_filled(
                                    egui::Rect::from_min_max(
                                        egui::pos2(x, y0),
                                        egui::pos2(
                                            x + self.config.editor.clamped_caret_width(),
                                            y1,
                                        ),
                                    ),
                                    0.0,
                                    caret_col,
                                );
                            }
                        }
                        // Capture each logical line's screen Y for the gutter (a row
                        // starts a logical line iff the previous row ended with \n).
                        if show_line_numbers {
                            let top = out.galley_pos.y;
                            let mut prev_newline = true;
                            for row in &out.galley.rows {
                                if prev_newline {
                                    // egui 0.34: PlacedRow.rect is now a method, not a field.
                                    new_gutter.push(top + row.rect().min.y);
                                }
                                prev_newline = row.ends_with_newline;
                            }
                        }
                        // Auto-focus the editor so typing works immediately on launch,
                        // new tab, or tab switch — no click required — unless a field,
                        // menu, or popup currently owns keyboard focus.
                        if !read_only
                            && !overlay_open
                            && ui.ctx().memory(|m| m.focused().is_none())
                            && !egui::Popup::is_any_open(ui.ctx())
                        {
                            out.response.request_focus();
                        }
                    });
                    // Record scroll metrics for the minimap's viewport indicator.
                    self.scroll_metrics = (
                        sa_out.state.offset.y,
                        sa_out.content_size.y.max(1.0),
                        sa_out.inner_rect.height().max(1.0),
                    );
                    // F-034 sticky scroll: pin the enclosing definition headers
                    // at the top of the viewport once their own header line has
                    // scrolled above it. Drawn with an opaque chrome fill so the
                    // pinned line occludes the scrolled body behind it. Clicking
                    // a pinned header jumps to that definition.
                    if !scopes.is_empty() {
                        let lh_px = (font.size * line_height).max(1.0);
                        let first_visible_line = (sa_out.state.offset.y / lh_px).floor() as usize;
                        let pinned =
                            crate::editor_features::sticky_chain_at(&scopes, first_visible_line, 5);
                        let vp = sa_out.inner_rect;
                        let bg = Color32::from_rgb(panel.r(), panel.g(), panel.b());
                        let painter = ui.painter_at(vp);
                        for (i, s) in pinned.iter().enumerate() {
                            let y = vp.top() + (i as f32) * lh_px;
                            let row = egui::Rect::from_min_max(
                                egui::pos2(vp.left(), y),
                                egui::pos2(vp.right(), y + lh_px),
                            );
                            painter.rect_filled(row, 0.0, bg);
                            let indent = 6.0 + (s.depth as f32) * 12.0;
                            painter.text(
                                egui::pos2(vp.left() + indent, y + lh_px * 0.5),
                                egui::Align2::LEFT_CENTER,
                                &s.label,
                                font.clone(),
                                accent,
                            );
                            if i + 1 == pinned.len() {
                                // Underline the bottom of the pinned stack so it
                                // reads as a header band, not part of the buffer.
                                painter.line_segment(
                                    [
                                        egui::pos2(vp.left(), row.bottom()),
                                        egui::pos2(vp.right(), row.bottom()),
                                    ],
                                    egui::Stroke::new(1.0, muted),
                                );
                            }
                            let resp = ui.interact(
                                row,
                                ui.id().with(("scr1b3-sticky", i)),
                                egui::Sense::click(),
                            );
                            if resp.clicked() {
                                sticky_jump = Some(s.start_line);
                            }
                        }
                    }
                    a
                };
                self.line_gutter = new_gutter;
                // F-034: apply a sticky-header click now that the hl borrow is
                // released. Scrolls so the clicked definition sits at the top.
                if let Some(line0) = sticky_jump {
                    let lh_px = (font.size * line_height).max(1.0);
                    self.pending_scroll = Some((line0 as f32) * lh_px);
                }

                // Completion: open on Ctrl+Space, accept on Enter/Tab, render popup.
                let cursor_idx = anchor.map(|(_, i)| i);
                if want_completion {
                    self.open_completion(active, cursor_idx);
                }
                if accept_completion {
                    self.accept_completion(active, cursor_idx);
                }
                if let Some((pos, _)) = anchor {
                    let choice = self
                        .completion
                        .as_ref()
                        .and_then(|c| completion_popup(ui, pos, c));
                    if let Some(idx) = choice {
                        if let Some(c) = self.completion.as_mut() {
                            c.selected = idx;
                        }
                        self.accept_completion(active, cursor_idx);
                    }
                }
            });
        }

        // Window color-tint overlay (subtle wash; portable across modes/OSes).
        if self.config.window.tint_strength > 0.0 {
            paint_tint_overlay(
                ctx,
                &self.config.window.tint,
                self.config.window.tint_strength,
            );
        }
        // CRT scanlines post-effect (#14, ported from C0PL4ND). A calm animated
        // retro overlay; only when motion AND scanlines are both enabled. Drives
        // a modest ~30 fps repaint while on so the bands drift (no busy-spin), and
        // never paints in the headless test harness (no real window to overlay).
        if !cfg!(test) && self.config.motion.enabled && self.config.motion.crt_scanlines {
            let t = ctx.input(|i| i.time);
            paint_crt_scanlines(ctx, self.config.motion.scanline_darkness, t);
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
        // Wave-6 motion overlays (master-gated; never in the headless harness).
        // Each is a calm post-effect; while any is active we drive a ~30 fps
        // repaint so it animates. The resting (motion-off) frame is unchanged.
        if !cfg!(test) && self.config.motion.enabled {
            let t = ctx.input(|i| i.time);
            let accent = ui_color(&self.theme, "accent", Rgba::new(0x4c, 0xc2, 0xff, 255));
            let mut animating = false;
            if self.config.motion.wired_ambient {
                paint_wired_mesh(ctx, self.config.motion.clamped_mesh_density(), accent, t);
                animating = true;
            }
            if self.config.motion.vhs_tracking {
                paint_vhs_tracking(ctx, t);
                animating = true;
            }
            if self.config.motion.flicker {
                paint_flicker(ctx, self.config.motion.clamped_flicker_strength(), t);
                animating = true;
            }
            if self.config.motion.caret_trail {
                while let Some(&(_, born)) = self.caret_trail.front() {
                    if t - born > 0.45 {
                        self.caret_trail.pop_front();
                    } else {
                        break;
                    }
                }
                paint_caret_trail(ctx, &self.caret_trail, accent, t);
                if !self.caret_trail.is_empty() {
                    animating = true;
                }
            }
            if self.config.motion.boot_glitch {
                let started = *self.boot_glitch_started.get_or_insert(t);
                let elapsed = t - started;
                if elapsed <= 0.55 {
                    paint_boot_glitch(ctx, elapsed);
                    animating = true;
                }
            }
            if animating {
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
            }
        }
        // Phase 18 T18.1: 8-zone resize overlay for the frameless window. egui
        // doesn't restore OS resize when window decorations are off (winit
        // #4186) so we paint invisible interact rectangles at the edges + four
        // corners that send `ViewportCommand::BeginResize(dir)` on drag and
        // hint the right cursor on hover.
        //
        // No persistent Foreground Areas (those swallowed tab/settings clicks
        // window-wide and could leave resize stuck after the first drag). This
        // is a pure per-frame check: hint the resize cursor at an edge and start
        // an OS resize on a press there — only when egui isn't already using the
        // pointer for a widget. Works repeatedly by construction.
        let _ = overlay_open;
        if self.config.appearance.frameless {
            let maximized = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
            if !maximized && !fullscreen {
                handle_frameless_resize(ctx);
            }
        }

        // Apply deferred actions after all UI borrows are released.
        self.apply_deferred_actions(
            ctx,
            &mut act,
            deferred_actions::DeferredFlags {
                run_cmd,
                run_builtin,
                save_cfg,
                open_from_tree,
                close_tree,
                start_lsp,
                want_open_cfg,
                want_restore_cfg,
                want_dismiss_cfg,
            },
        );

        self.persist_session_and_autosave();
    }
}
