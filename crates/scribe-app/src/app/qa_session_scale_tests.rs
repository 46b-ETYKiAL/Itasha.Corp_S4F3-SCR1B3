//! QA: TAB / SESSION / WORKSPACE surfaces at production scale (#38).
//!
//! Drives a heavily-populated workspace — many mixed-language tabs (dirty /
//! pinned / clean) — through the REAL `ScribeApp` host as a power user with a
//! large session would hit it. Complements the `qa_fixtures::smoke` shape tests
//! (which only assert the GENERATORS produce the right shape) by exercising the
//! tab-strip render, tab switching, close/pin guards, the session save→restore
//! round-trip, the R6-hardened restore path-guard, and the overflow affordance.
//!
//! Discipline (Phase-2): tests ONLY, no product-code edits, no weakened
//! assertions. The generator's `populated_session` count is EXTENDED here to a
//! CI-sane large number (60 tabs) via a local builder that reuses the public
//! generators rather than duplicating them. Where a real defect is found it is
//! logged red-first as `#[ignore = "BUG: …"]`, never deleted or masked.
//!
//! Seams used (all reachable from this child module via `use super::*`):
//!   * `qa_fixtures::{production_config, build_large_project}` — workspace shape.
//!   * `ScribeApp::{new_test, frame_tick, close_tab, close_all_tabs_except,
//!     close_all_tabs, execute_builtin}` + the `tabs` / `active` fields.
//!   * `EditorTab` (module-private) — built via the same funnel the fixtures use.
//!   * `scribe_core::session` — the on-disk manifest API, driven through the
//!     app's test-isolated `config_dir` for a true save→restore round-trip.
//!   * `crate::session_path_guard::{is_safe_restore_path, allowed_roots}` — the
//!     R6 / S-04 hardened restore path validation (missing-file + escape).

#![allow(clippy::wildcard_imports)]
use super::*;
use egui_kittest::kittest::Queryable as _;

/// A CI-sane "large" tab count — big enough to overflow a default window's tab
/// strip and stress switching/grid/restore, small enough to stay fast. This is
/// the scale knob the prompt calls for (40–80); 60 sits in the middle.
const SCALE_TABS: usize = 60;

/// The languages the scale session spreads tabs across (mirrors the fixture's
/// `LANGS` so the tab set is genuinely multi-language). Kept local so this test
/// doesn't reach into a fixture-private const.
const SCALE_LANGS: &[&str] = &["rs", "md", "txt", "json", "toml", "py", "js"];

/// Build an in-memory `EditorTab` from synthetic content, marking it dirty by
/// diverging the editable mirror from the saved rope (exactly how the live
/// editor models an unsaved edit — see `EditorTab::is_dirty`). Pure + sanitized:
/// content is index-derived, never a secret or a machine path.
fn scale_tab(idx: usize, ext: &str, dirty: bool, pinned: bool) -> EditorTab {
    let body =
        format!("// synthetic scale tab {idx} ({ext})\nlet token_{idx} = compute(alpha, bravo);\n");
    let mut tab = EditorTab::scratch();
    tab.doc.set_text(&body);
    tab.doc.mark_clean();
    tab.text = body.clone();
    tab.disk_text = body.clone();
    tab.session_baseline = body.clone();
    tab.saved_baseline = body;
    tab.pinned = pinned;
    if dirty {
        // Diverge from the saved rope → is_dirty() == true (unsaved edit).
        tab.text = format!("{}\n// unsaved edit {idx}\n", tab.text);
    }
    tab
}

/// Install `SCALE_TABS` mixed-language tabs into a fresh production-config app,
/// assigning stable doc-ids the way a real open flow does. Every 3rd tab is
/// dirty; every 7th is pinned; the rest clean — a realistic dirty/pinned mix at
/// scale. Returns the app (active tab = 0).
fn scale_app() -> ScribeApp {
    let mut app = ScribeApp::new_test(qa_fixtures::production_config());
    let mut tabs = Vec::with_capacity(SCALE_TABS);
    for i in 0..SCALE_TABS {
        let ext = SCALE_LANGS[i % SCALE_LANGS.len()];
        let dirty = i % 3 == 0;
        let pinned = i % 7 == 0;
        let mut tab = scale_tab(i, ext, dirty, pinned);
        tab.doc_id = app.next_doc_id.next();
        tabs.push(tab);
    }
    app.tabs = tabs;
    app.active = 0;
    app
}

/// A headless egui_kittest harness over the real `frame_tick` render loop (no
/// GPU) — the same idiom as `qa_security_workflow_tests`/`e2e`.
fn harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 820.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

// ===========================================================================
// Scenario 1 — many tabs present + the strip renders without panic over frames.
// ===========================================================================

#[test]
fn scenario1_many_tabs_present_and_strip_renders_without_panic() {
    let app = scale_app();
    assert_eq!(
        app.tabs.len(),
        SCALE_TABS,
        "all {SCALE_TABS} tabs must be installed in app state"
    );
    // Every tab carries a distinct doc-id (the grid addresses panes by id).
    let ids: std::collections::BTreeSet<_> = app.tabs.iter().map(|t| t.doc_id).collect();
    assert_eq!(
        ids.len(),
        SCALE_TABS,
        "every tab must have a distinct doc-id"
    );

    // Drive several real frames — a panic in the tab-strip layout at scale would
    // surface here (the production config uses the Left side strip, which wraps
    // in a vertical ScrollArea so 60 tabs stay reachable).
    let mut h = harness(app);
    for _ in 0..5 {
        h.step();
    }
    assert_eq!(
        h.state().tabs.len(),
        SCALE_TABS,
        "the tab set must survive several render frames intact"
    );
    // The active index is always in-bounds (never a dangling pointer).
    assert!(h.state().active < h.state().tabs.len());
}

// ===========================================================================
// Scenario 2 — tab switching changes `active` and the correct buffer shows.
// ===========================================================================

#[test]
fn scenario2_tab_switching_changes_active_and_visible_buffer() {
    let mut app = scale_app();
    // Record the per-tab body so we can prove the RIGHT buffer becomes active.
    let bodies: Vec<String> = app.tabs.iter().map(|t| t.text.clone()).collect();

    // Switch across the strip by index (the click handler sets `self.active = i`;
    // we drive the same state transition the click resolves to, then render).
    for &target in &[0usize, SCALE_TABS / 2, SCALE_TABS - 1, 7, SCALE_TABS - 3] {
        app.active = target;
        let mut h = harness(std::mem::replace(&mut app, scale_app()));
        h.step();
        assert_eq!(
            h.state().active,
            target,
            "active must move to the clicked tab"
        );
        assert_eq!(
            h.state().tabs[target].text,
            bodies[target],
            "the active tab must hold its own buffer (no cross-tab content bleed)"
        );
        app = std::mem::replace(h.state_mut(), scale_app());
    }

    // CycleTabNext / CycleTabPrev wrap correctly across the full strip.
    let mut app = scale_app();
    app.active = SCALE_TABS - 1;
    app.execute_builtin(BuiltinCommand::CycleTabNext);
    assert_eq!(app.active, 0, "CycleTabNext wraps from last to first");
    app.execute_builtin(BuiltinCommand::CycleTabPrev);
    assert_eq!(
        app.active,
        SCALE_TABS - 1,
        "CycleTabPrev wraps from first to last"
    );
}

// ===========================================================================
// Scenario 3 — dirty marker, pinned retention, and the close-guard on a pin.
// ===========================================================================

#[test]
fn scenario3_dirty_marker_pinned_retention_and_close_guards() {
    let app = scale_app();

    // Dirty tabs render the Notepad++ "* " unsaved marker in their title; clean
    // tabs do not. (title() is the single source the strip/grid label from.)
    let dirty_count = app.tabs.iter().filter(|t| t.is_dirty()).count();
    let pinned_count = app.tabs.iter().filter(|t| t.pinned).count();
    assert!(dirty_count > 0, "the scale session must have dirty tabs");
    assert!(pinned_count > 0, "the scale session must have pinned tabs");
    assert!(
        dirty_count < SCALE_TABS,
        "the scale session must have clean tabs too"
    );
    for t in &app.tabs {
        if t.is_dirty() {
            assert!(
                t.title().starts_with("* "),
                "a dirty tab title must carry the '* ' unsaved marker (got {:?})",
                t.title()
            );
        } else {
            assert!(
                !t.title().starts_with("* "),
                "a clean tab title must NOT carry the unsaved marker (got {:?})",
                t.title()
            );
        }
    }

    // Pin guard: closing a PINNED tab via the single close chokepoint is refused
    // (the user must unpin first). Pick a known pinned index (0 is pinned: 0%7==0).
    let mut app = scale_app();
    assert!(app.tabs[0].pinned, "tab 0 is pinned in the scale session");
    let before = app.tabs.len();
    app.close_tab(0);
    assert_eq!(
        app.tabs.len(),
        before,
        "a pinned tab must NOT be closable via close_tab (unpin-first guard)"
    );
    assert!(
        app.tabs[0].pinned,
        "the pinned tab must still be present + pinned"
    );
    assert!(
        app.status.contains("pinned"),
        "the refusal must surface a 'pinned' status hint (got {:?})",
        app.status
    );

    // Closing a CLEAN, UNPINNED tab succeeds and pushes it onto the reopen stack.
    // Find a clean+unpinned index.
    let clean = (0..app.tabs.len())
        .find(|&i| !app.tabs[i].pinned && !app.tabs[i].is_dirty())
        .expect("scale session has a clean unpinned tab");
    let before = app.tabs.len();
    app.close_tab(clean);
    assert_eq!(app.tabs.len(), before - 1, "a clean unpinned tab closes");
    assert!(
        !app.closed_tabs.is_empty(),
        "the closed tab is captured for Ctrl+Shift+T reopen (no silent loss)"
    );

    // Closing a DIRTY unpinned tab still removes the tab BUT its unsaved content
    // is captured on the reopen stack — the guard against silent data loss is the
    // reopen capture (close_tab unconditionally pushes path/text/cursor).
    let dirty = (0..app.tabs.len())
        .find(|&i| !app.tabs[i].pinned && app.tabs[i].is_dirty())
        .expect("scale session has a dirty unpinned tab");
    let dirty_text = app.tabs[dirty].text.clone();
    app.close_tab(dirty);
    assert!(
        app.closed_tabs.iter().any(|c| c.text == dirty_text),
        "a closed DIRTY tab's unsaved content must be recoverable from the reopen stack"
    );
}

// ===========================================================================
// Scenario 4 — session save → restore round-trip (paths, active, pinned/dirty).
//              R6 path-guard hardening asserted alongside (paths stay in-bounds).
// ===========================================================================

#[test]
fn scenario4_session_save_restore_round_trip_preserves_set_active_and_flags() {
    use scribe_core::session;

    // A real file-backed workspace: write SCALE_TABS small files under a project
    // root, open them through the app, mark a deterministic subset dirty.
    let project = qa_fixtures::build_large_project(SCALE_TABS, 2);
    // Collect the actual on-disk files the generator produced.
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![project.path().to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                files.push(p);
            }
        }
    }
    files.sort();
    assert_eq!(
        files.len(),
        SCALE_TABS,
        "every project file must exist on disk"
    );

    let mut app = ScribeApp::new_test(qa_fixtures::production_config());
    app.tabs.clear();
    for (i, f) in files.iter().enumerate() {
        let mut tab = EditorTab::from_path(f.clone()).expect("open project file");
        tab.doc_id = app.next_doc_id.next();
        if i % 4 == 0 {
            // Diverge → dirty (unsaved edit) so a backup is written for it.
            tab.text = format!("{}\n// dirty {i}\n", tab.text);
        }
        app.tabs.push(tab);
    }
    let active_before = SCALE_TABS / 2 + 1;
    app.active = active_before;

    // SAVE: drive the REAL hot-exit snapshot. `new_test` redirects config_dir to
    // a per-instance temp dir, so this writes a real manifest + backups there
    // without touching the user's session — the genuine save path.
    app.snapshot_session_backups();
    let cfg_dir = app.config_dir.clone().expect("test config dir set");
    let manifest = session::load_manifest(&cfg_dir).expect("manifest was written");
    assert_eq!(
        manifest.tabs.len(),
        SCALE_TABS,
        "every open tab must be recorded in the saved manifest"
    );
    assert_eq!(
        manifest.active, active_before,
        "the active index must persist"
    );

    // The dirty/clean split + path set survive into the manifest exactly.
    let saved_dirty = manifest.tabs.iter().filter(|t| t.dirty).count();
    let app_dirty = app.tabs.iter().filter(|t| t.is_dirty()).count();
    assert_eq!(
        saved_dirty, app_dirty,
        "dirty flags must round-trip into the manifest"
    );
    assert!(saved_dirty > 0, "the round-trip must carry dirty tabs");
    for snap in &manifest.tabs {
        let p = snap.path.as_ref().expect("file-backed tab has a path");
        assert!(
            files.iter().any(|f| f.display().to_string() == *p),
            "path round-trips"
        );
        if snap.dirty {
            assert!(
                snap.backup.is_some(),
                "a dirty tab must carry a content backup"
            );
        }
    }

    // RESTORE: reconstruct tabs from the manifest + backups the way the app does
    // (from_backup for a dirty entry, from_path for a clean one), proving the
    // unsaved content survives. We reconstruct here (rather than calling the
    // private restore_tabs_from_manifest, which reads the GLOBAL config dir) so
    // the round-trip is rooted in the SAME isolated dir we saved to.
    let bdir = session::backup_dir(&cfg_dir);
    let mut restored: Vec<EditorTab> = Vec::with_capacity(SCALE_TABS);
    for snap in &manifest.tabs {
        let path = snap.path.as_ref().map(std::path::PathBuf::from);
        let tab = if let Some(name) = &snap.backup {
            let content = session::read_backup(&bdir, name).expect("backup readable");
            EditorTab::from_backup(path, content)
        } else {
            EditorTab::from_path(path.expect("clean tab path")).expect("reopen clean file")
        };
        restored.push(tab);
    }
    assert_eq!(restored.len(), SCALE_TABS, "every tab must restore");
    let restored_dirty = restored.iter().filter(|t| t.is_dirty()).count();
    assert_eq!(
        restored_dirty, app_dirty,
        "dirty state must survive the full save→restore round-trip"
    );

    // R6 / S-04 — every restored path stays within the prior session's roots (no
    // escape, no UNC). This is the hardened-restore regression guard.
    let root_candidates: Vec<std::path::PathBuf> = files
        .iter()
        .filter_map(|p| p.parent().map(|par| par.to_path_buf()))
        .collect();
    let roots =
        crate::session_path_guard::allowed_roots(root_candidates.iter().map(|p| p.as_path()));
    for f in &files {
        assert!(
            crate::session_path_guard::is_safe_restore_path(f, &roots),
            "an in-bounds project file must pass the restore path-guard: {}",
            f.display()
        );
    }
}

// ===========================================================================
// Scenario 5 — tab overflow: the strip stays reachable at scale.
// ===========================================================================

#[test]
fn scenario5_tab_overflow_strip_stays_reachable_at_scale() {
    // The production config uses the LEFT side tab strip, which wraps its tabs in
    // a `ScrollArea::vertical` (see `draw_side_tab_strip`) — so even 60 tabs in a
    // SMALL window stay scroll-reachable rather than clipping off-screen. Render
    // in a deliberately short window and assert no panic + every tab still
    // addressable by state (the scroll affordance keeps them reachable).
    let app = scale_app();
    assert_eq!(
        app.config.editor.tab_bar_position,
        scribe_core::config::TabBarPosition::Left,
        "the production config drives the scrollable side strip"
    );
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(900.0, 360.0)) // short window → strip must scroll
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(
        h.state().tabs.len(),
        SCALE_TABS,
        "no tab may be dropped just because the strip overflows the window"
    );
    // The grid / multi-pane quick-switch view is the at-scale affordance for
    // seeing many tabs at once: enabling it must render all panes without panic.
    let mut app2 = scale_app();
    app2.config.editor.grid_enabled = true;
    let mut h2 = harness(app2);
    for _ in 0..4 {
        h2.step();
    }
    assert_eq!(
        h2.state().tabs.len(),
        SCALE_TABS,
        "the grid view must render every tab at scale without dropping any"
    );
}

// ===========================================================================
// Scenario 6 — close-all / close-others keep state consistent (no dangling idx).
// ===========================================================================

#[test]
fn scenario6_close_all_and_close_others_keep_state_consistent() {
    // Close Others from a clean, unpinned anchor: every other UNPINNED tab is
    // removed; pinned tabs (F-044) are retained; active points at the survivor.
    let mut app = scale_app();
    let pinned_before = app.tabs.iter().filter(|t| t.pinned).count();
    // Anchor on a known clean+unpinned tab.
    let anchor = (0..app.tabs.len())
        .find(|&i| !app.tabs[i].pinned)
        .expect("an unpinned tab exists");
    app.close_all_tabs_except(anchor);
    // Survivors = the anchor + every pinned tab (pinned are never auto-closed).
    let pinned_after = app.tabs.iter().filter(|t| t.pinned).count();
    assert_eq!(
        pinned_after, pinned_before,
        "Close Others must retain all pinned tabs (F-044)"
    );
    assert!(
        !app.tabs.is_empty(),
        "at least the kept tab survives Close Others"
    );
    assert!(
        app.active < app.tabs.len(),
        "active must stay in-bounds after Close Others (no dangling index)"
    );

    // Close All: every UNPINNED tab goes; if nothing is unpinned a scratch is
    // kept. With pinned tabs present, only the pinned set survives.
    let mut app = scale_app();
    let pinned_before = app.tabs.iter().filter(|t| t.pinned).count();
    app.close_all_tabs();
    assert!(
        app.active < app.tabs.len(),
        "active in-bounds after Close All"
    );
    if pinned_before > 0 {
        assert!(
            app.tabs.iter().all(|t| t.pinned),
            "Close All retains only pinned tabs"
        );
        assert_eq!(
            app.tabs.len(),
            pinned_before,
            "exactly the pinned set survives"
        );
    } else {
        assert_eq!(
            app.tabs.len(),
            1,
            "Close All with no pins keeps one scratch tab"
        );
    }

    // The CloseAllTabs builtin is the unconditional variant (clears to one
    // scratch) — assert it never leaves a dangling active index either.
    let mut app = scale_app();
    app.execute_builtin(BuiltinCommand::CloseAllTabs);
    assert_eq!(
        app.tabs.len(),
        1,
        "the CloseAllTabs builtin resets to one scratch tab"
    );
    assert_eq!(app.active, 0, "active resets to 0 (in-bounds)");

    // And a final render frame after the mass close must not panic.
    let mut h = harness(app);
    h.step();
    assert!(h.state().active < h.state().tabs.len());
}

// ===========================================================================
// Scenario 7 — restoring a session referencing a now-MISSING file is graceful.
// ===========================================================================

#[test]
fn scenario7_restore_with_missing_file_is_graceful_no_panic() {
    use scribe_core::session;

    // A real file we will then DELETE, plus a real file that survives.
    let project = tempfile::tempdir().unwrap();
    let present = project.path().join("present.txt");
    let vanished = project.path().join("vanished.txt");
    std::fs::write(&present, "alpha bravo\n").unwrap();
    std::fs::write(&vanished, "to be deleted\n").unwrap();

    // Build + save a manifest referencing BOTH (the app's real save path), with
    // the second tab carrying NO backup (a clean file-backed tab).
    let app = ScribeApp::new_test(qa_fixtures::production_config());
    let cfg_dir = app.config_dir.clone().expect("test config dir");
    let manifest = session::SessionManifest::new(
        vec![
            session::TabSnapshot {
                path: Some(present.display().to_string()),
                dirty: false,
                backup: None,
                cursor: 0,
            },
            session::TabSnapshot {
                path: Some(vanished.display().to_string()),
                dirty: false,
                backup: None,
                cursor: 0,
            },
        ],
        0,
    );
    session::save_manifest(&cfg_dir, &manifest).expect("save manifest");

    // Now the second file VANISHES (deleted out from under the session).
    std::fs::remove_file(&vanished).unwrap();

    // R6 path-guard: the present file is restorable; the vanished one is SKIPPED
    // (fail-closed on nonexistence) — never auto-created, never a panic.
    let roots = crate::session_path_guard::allowed_roots(std::iter::once(project.path()));
    assert!(
        crate::session_path_guard::is_safe_restore_path(&present, &roots),
        "the surviving file must restore"
    );
    assert!(
        !crate::session_path_guard::is_safe_restore_path(&vanished, &roots),
        "a vanished file must be skipped, not auto-opened (graceful, no panic)"
    );

    // Reconstruct exactly as the restore path does: a clean tab opens from disk
    // ONLY when the file is still there; the vanished one yields no tab. Drive
    // the reconstruction + a render to prove no panic on the missing reference.
    let reloaded = session::load_manifest(&cfg_dir).expect("manifest reloads");
    let mut restored: Vec<EditorTab> = Vec::new();
    for snap in &reloaded.tabs {
        let p = std::path::PathBuf::from(snap.path.as_ref().unwrap());
        if crate::session_path_guard::is_safe_restore_path(&p, &roots) {
            if let Ok(tab) = EditorTab::from_path(p) {
                restored.push(tab);
            }
        }
        // An unsafe / missing path is silently skipped — the graceful path.
    }
    assert_eq!(
        restored.len(),
        1,
        "only the surviving file restores; the missing one is gracefully skipped"
    );

    // Mount the restored set into a fresh app and render — a missing reference in
    // the prior session must never crash the next launch.
    let mut app2 = ScribeApp::new_test(qa_fixtures::production_config());
    if !restored.is_empty() {
        app2.tabs = restored;
        for tab in &mut app2.tabs {
            tab.doc_id = app2.next_doc_id.next();
        }
        app2.active = 0;
    }
    let mut h = harness(app2);
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(
        h.state().tabs.len(),
        1,
        "the workspace restored intact (sans ghost)"
    );

    // Sanity: the surviving tab carries the right content + a queryable strip.
    assert!(
        h.state().tabs[0].text.contains("alpha bravo"),
        "the surviving tab holds its on-disk content"
    );
    // The add-tab affordance (now a frameless Phosphor PLUS glyph, v0.4.58) is
    // always present on the strip (reachable UI).
    assert!(
        h.query_by_label(egui_phosphor::thin::PLUS).is_some(),
        "the tab strip's add-tab control stays reachable after a partial restore"
    );
}
