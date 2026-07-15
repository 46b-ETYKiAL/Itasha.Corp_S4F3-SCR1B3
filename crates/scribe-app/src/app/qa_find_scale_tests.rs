//! QA: FIND / FIND-IN-FILES / REPLACE at PRODUCTION SCALE (#38).
//!
//! Drives the find surfaces of the real `ScribeApp` against the synthetic
//! large-project fixtures from [`super::qa_fixtures`], exactly as a user would:
//! open a file, open the find bar, type a query, count + navigate matches,
//! replace, and run a project-wide search over ~1500 files. Each scenario is an
//! acceptance-criterion with a finite, risk-based edge focus — NOT a fuzz.
//!
//! ## Phase discipline
//!
//! Tests only — no product-code edits. The find engine, off-thread project walk,
//! and the edit_gen-keyed find cache already exist; this module proves they hold
//! their contracts at scale through the same seams the smaller `*_tests.rs`
//! suites use ([`find_in_files_tests`], [`qa_correctness_workflow_tests`]).
//!
//! ## Drive idioms used
//!
//! - **In-buffer find**: state-driven through the public-to-module seams
//!   (`open_path`, `find_query`, `find_matches_active`, `find_navigate`,
//!   `replace_in_active`, `find_recompute_count`). These are deterministic and
//!   need no GPU frame, so the scale tests stay cheap + reproducible.
//! - **Find-in-files**: the egui_kittest headless harness + the bounded
//!   poll-until-`Done` loop (project-find is off-thread + repaint-driven), mirrored
//!   from `find_in_files_tests::run_until_search_done`.
//!
//! ## Known generator tokens (read from `qa_fixtures::synth_line`)
//!
//! The synthetic bodies are index-derived. The token `compute` appears in every
//! `rs`/`py`/`js` file (3 of the 7 cycled languages), and the LOREM vocabulary
//! word `alpha` appears across many files in many languages — both are reliable
//! many-match needles at scale. The token `zzqqxx_no_such_token_zzqqxx` appears
//! in NONE of them (the clean zero-match needle).
#![allow(clippy::wildcard_imports)]
use super::qa_fixtures::{build_large_project, production_config, qa_app};
use super::*;
use egui_kittest::kittest::Queryable as _;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A find-in-files harness app: first-run done + non-frameless so welcome /
/// titlebar chrome never competes for the queried labels. Mirrors the
/// `find_in_files_tests::fif_app` shape.
fn scale_fif_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    ScribeApp::new_test(cfg)
}

fn scale_harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

/// Drive frames until the off-thread project-search worker finishes (its `Done`
/// clears `find_in_files_running`), bounded so a hung worker can't wedge the
/// test. At ~1500 files this is the load-bearing settle loop. `run_ok` (not
/// `run`) because the focused query field's blinking caret keeps requesting
/// repaints, which would trip `run`'s max-steps panic. Returns the number of
/// poll iterations consumed, for the callers' assert messages.
///
/// Bounded by WALL-CLOCK, not iteration count. An iteration cap (this was 400 ×
/// 5ms) reads as generous and starves silently: what the loop is waiting for is
/// a WORKER THREAD, and under CPU contention that worker gets less CPU while
/// each `run_ok` also gets slower — so the loop burns minutes of real time and
/// still gives up with the search unfinished. It failed twice that way on a
/// contended host (146s, then 26s) while passing in 20s on a quiet one, and one
/// of those took a whole 30-mutant sweep down with it via BaselineFailed. The
/// failure surfaces as "the search settles to a clean idle state" being false,
/// which reads exactly like a real bug — a false-RED generator, and CI runners
/// are small and share hosts.
///
/// A deadline is the right shape because it waits LONGER on a slower machine,
/// which is what a starved worker needs. It is deliberately far above any real
/// search (a quiet run settles in a few seconds), so it only ever bounds a
/// genuinely wedged worker.
fn run_until_search_done(h: &mut egui_kittest::Harness<'static, ScribeApp>) -> usize {
    const DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);
    let start = std::time::Instant::now();
    let mut iters = 0;
    while start.elapsed() < DEADLINE {
        iters += 1;
        h.run_ok();
        if !h.state().find_in_files_running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // One more pass so the final drained batch renders into result rows.
    h.run_ok();
    iters
}

/// Walk the synthetic project tree and return the first file path whose body is
/// known to contain the token `compute` (any `rs`/`py`/`js` file). Used to open
/// a real, match-rich buffer into the find bar at scale.
fn first_file_with_compute(root: &std::path::Path) -> std::path::PathBuf {
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(body) = std::fs::read_to_string(&p) {
                if body.contains("compute") {
                    return p;
                }
            }
        }
    }
    panic!("no synthetic file containing the `compute` token was found");
}

// ===========================================================================
// Scenario 1 — Find-in-current-buffer at scale: MANY matches, correct count,
//              next/prev navigation cycles correctly (with wrap-around).
// ===========================================================================

#[test]
fn scenario1_find_in_large_buffer_counts_and_navigation_cycle() {
    // A large project; open ONE real file from it that is dense with the
    // `compute` token so the find bar has many matches to count + cycle.
    let project = build_large_project(1500, 4);
    let mut app = qa_app(production_config(), &project);
    let target = first_file_with_compute(project.path());
    app.open_path(target.clone());
    let active = app.active;

    // Sanity: the opened buffer really holds the synthetic content (file-backed,
    // not one of the in-memory session tabs).
    assert!(
        app.tabs[active].text.contains("compute"),
        "the opened file must carry the synthetic `compute` token"
    );

    // User opens the find bar and types a many-match query.
    app.execute_builtin(BuiltinCommand::OpenFind);
    assert!(app.find_open, "Ctrl+F opens the find bar");
    app.find_query = "compute".to_string();

    let matches = app.find_matches_active();
    let n = matches.len();
    assert!(
        n >= 2,
        "the dense buffer must yield many `compute` matches (got {n})"
    );
    // Ground-truth the count against an independent literal scan (the find bar is
    // literal + case-insensitive: Query default).
    let q = scribe_core::search::Query {
        pattern: "compute".to_string(),
        ..Default::default()
    };
    let truth = scribe_core::search::find_all(&app.tabs[active].text, &q).unwrap();
    assert_eq!(
        n,
        truth.len(),
        "find-bar match count must equal the independent literal scan"
    );

    // Navigate forward through every match and one extra step → must WRAP back to
    // the first match (no out-of-bounds, no stall).
    app.find_match_idx = 0;
    for step in 1..n {
        app.find_navigate(true);
        assert_eq!(
            app.find_match_idx, step,
            "forward navigation advances one match per step"
        );
    }
    app.find_navigate(true); // step n → wraps to 0
    assert_eq!(
        app.find_match_idx, 0,
        "forward navigation past the last match wraps to the first"
    );

    // Previous from the first match wraps to the LAST.
    app.find_navigate(false);
    assert_eq!(
        app.find_match_idx,
        n - 1,
        "previous navigation from the first match wraps to the last"
    );
}

// ===========================================================================
// Scenario 2 — Find-in-FILES across ~1500 files: a known-emitted token yields a
//              non-empty result set pointing at REAL files; the off-thread
//              search COMPLETES within the bounded frame cap.
// ===========================================================================

#[test]
fn scenario2_find_in_files_across_large_project_completes_and_points_at_real_files() {
    let project = build_large_project(1500, 4);
    let mut app = scale_fif_app();
    app.open_folder_root(project.path().to_path_buf());
    app.find_in_files_open = true;
    // `alpha` is a LOREM vocabulary word emitted across many files + languages.
    app.find_in_files_query = "alpha".to_string();
    let mut h = scale_harness(app);
    h.run_ok();
    h.get_by_label("search").click();
    let iters = run_until_search_done(&mut h);

    assert!(
        !h.state().find_in_files_running,
        "the off-thread project search must COMPLETE within the bounded frame cap \
         (consumed {iters} poll iterations)"
    );
    let results = &h.state().find_in_files_results;
    assert!(
        !results.is_empty(),
        "a project-wide search for an emitted token must yield matches at scale"
    );
    // Every result points at a REAL file on disk under the project root.
    let root = project.path().to_path_buf();
    for m in results.iter().take(50) {
        assert!(
            m.path.exists(),
            "every result must point at a real file (missing: {:?})",
            m.path
        );
        assert!(
            m.path.starts_with(&root),
            "every result must live under the searched project root"
        );
    }
    // The off-thread walk spanned multiple files (not just the first hit).
    let distinct: std::collections::BTreeSet<_> = results.iter().map(|m| &m.path).collect();
    assert!(
        distinct.len() >= 2,
        "a many-file token must hit across multiple files (distinct files: {})",
        distinct.len()
    );
}

// ===========================================================================
// Scenario 3 — Regex vs plain-literal parity on the SAME corpus: a regex that
//              denotes the same set as a literal must produce the same hit set.
// ===========================================================================

#[test]
fn scenario3_regex_and_plain_query_parity_on_same_corpus() {
    let project = build_large_project(1500, 4);

    // Plain (regex OFF) literal search for `alpha`.
    let plain_count = {
        let mut app = scale_fif_app();
        app.open_folder_root(project.path().to_path_buf());
        app.find_in_files_open = true;
        app.find_in_files_query = "alpha".to_string();
        app.find_in_files_regex = false;
        let mut h = scale_harness(app);
        h.run_ok();
        h.get_by_label("search").click();
        run_until_search_done(&mut h);
        h.state().find_in_files_results.len()
    };

    // Regex ON for `alpha` — a bare literal IS a valid regex denoting the same
    // set, so the hit count must match the plain search exactly (parity).
    let regex_count = {
        let mut app = scale_fif_app();
        app.open_folder_root(project.path().to_path_buf());
        app.find_in_files_open = true;
        app.find_in_files_query = "alpha".to_string();
        app.find_in_files_regex = true;
        let mut h = scale_harness(app);
        h.run_ok();
        h.get_by_label("search").click();
        run_until_search_done(&mut h);
        h.state().find_in_files_results.len()
    };

    assert!(plain_count > 0, "plain literal search must find matches");
    assert_eq!(
        plain_count, regex_count,
        "a bare-literal regex must denote the SAME hit set as the plain literal \
         (plain={plain_count}, regex={regex_count})"
    );
}

// ===========================================================================
// Scenario 4 — EDGE: zero-width / pathological regex + empty query. No panic,
//              no hang; and (for in-buffer Replace) NO spurious between-every-char
//              insertion (R5 regression guard).
// ===========================================================================

#[test]
fn scenario4_zero_width_and_empty_queries_are_safe_no_spurious_replace() {
    // -- In-buffer side: the find bar query is LITERAL (no regex toggle), so a
    //    `x*` / `\b` / `^` query is searched as a literal substring. The empty
    //    query yields zero matches by contract. None of these may panic. --
    let project = build_large_project(200, 3);
    let mut app = qa_app(production_config(), &project);
    let target = first_file_with_compute(project.path());
    app.open_path(target);

    for pathological in ["", "x*", r"\b", "^", "$"] {
        app.find_query = pathological.to_string();
        // Must not panic; navigation on a (likely) zero-match query is a no-op.
        let before = app.find_match_idx;
        let matches = app.find_matches_active();
        app.find_navigate(true);
        if matches.is_empty() {
            assert_eq!(
                app.find_match_idx, before,
                "navigation on a zero-match query must be a no-op (query {pathological:?})"
            );
        }
    }

    // -- R5 regression guard: Replace must NOT insert the replacement between
    //    every character. The find bar engine skips zero-width matches
    //    (`find_all` filters `start == end`), so a query that *would* be
    //    zero-width as a regex is here a LITERAL — if it is absent, Replace All
    //    is a clean no-op that leaves the text byte-identical (no `^`-anchored
    //    splice spray). --
    let active = app.active;
    let original = app.tabs[active].text.clone();
    let original_len = original.len();
    let gen_before = app.tabs[active].edit_gen;
    for zero_width in ["^", "$", r"\b", "x*"] {
        app.find_query = zero_width.to_string();
        app.replace_query = "INJECTED".to_string();
        app.replace_in_active(true);
        let after = &app.tabs[active].text;
        assert!(
            !after.contains("INJECTED"),
            "a literal {zero_width:?} absent from the buffer must NOT splice the \
             replacement anywhere (R5: no between-every-char insertion)"
        );
        assert_eq!(
            after.len(),
            original_len,
            "a no-match Replace All must leave the buffer byte-length unchanged"
        );
    }
    assert_eq!(
        app.tabs[active].text, original,
        "the buffer is byte-identical after the zero-width Replace-All sweep"
    );
    // No real edit happened → edit_gen must not have advanced for these no-ops.
    assert_eq!(
        app.tabs[active].edit_gen, gen_before,
        "a no-match Replace All must not bump edit_gen (no real edit occurred)"
    );

    // -- Find-in-files side: empty query is a clean no-op; a zero-width regex
    //    must not hang (it completes within the bounded cap). --
    let mut app2 = scale_fif_app();
    app2.open_folder_root(project.path().to_path_buf());
    app2.find_in_files_open = true;
    app2.find_in_files_query = String::new();
    let mut h = scale_harness(app2);
    h.run_ok();
    h.get_by_label("search").click();
    h.run_ok();
    assert!(
        h.state().find_in_files_results.is_empty(),
        "an empty project-find query must yield no results (clean no-op)"
    );
    assert!(
        !h.state().find_in_files_running,
        "an empty query must not even start a worker"
    );

    // A zero-width regex (`x*`) ON: must not hang — `find_all` skips empty
    // matches, so the walk terminates within the bounded cap.
    h.state_mut().find_in_files_query = "x*".to_string();
    h.state_mut().find_in_files_regex = true;
    h.run_ok();
    h.get_by_label("search").click();
    let iters = run_until_search_done(&mut h);
    assert!(
        !h.state().find_in_files_running,
        "a zero-width regex project search must COMPLETE, not hang (iters={iters})"
    );
}

// ===========================================================================
// Scenario 5 — EDGE: a query with NO matches → empty result, clean state, no
//              spurious error.
// ===========================================================================

#[test]
fn scenario5_no_match_query_is_empty_and_clean() {
    let project = build_large_project(1500, 4);

    // In-buffer: a token absent from every synthetic body yields zero matches.
    let mut app = qa_app(production_config(), &project);
    let target = first_file_with_compute(project.path());
    app.open_path(target);
    app.execute_builtin(BuiltinCommand::OpenFind);
    app.find_query = "zzqqxx_no_such_token_zzqqxx".to_string();
    assert!(
        app.find_matches_active().is_empty(),
        "an absent token yields zero in-buffer matches"
    );
    app.find_navigate(true);
    assert_eq!(
        app.find_match_idx, 0,
        "navigation on a zero-match query leaves the index pinned (no underflow)"
    );

    // Find-in-files: the same absent token yields an empty, error-free result set.
    let mut app2 = scale_fif_app();
    app2.open_folder_root(project.path().to_path_buf());
    app2.find_in_files_open = true;
    app2.find_in_files_query = "zzqqxx_no_such_token_zzqqxx".to_string();
    let mut h = scale_harness(app2);
    h.run_ok();
    h.get_by_label("search").click();
    run_until_search_done(&mut h);
    assert!(
        h.state().find_in_files_results.is_empty(),
        "an absent token yields no project-wide matches"
    );
    assert!(
        h.state().find_in_files_error.is_none(),
        "a legitimate zero-match search is NOT an error"
    );
    assert!(
        !h.state().find_in_files_running,
        "the search settles to a clean idle state"
    );
}

// ===========================================================================
// Scenario 6 — Replace-all at scale within a buffer → correct replacement count
//              and resulting text; the edit_gen-keyed find cache invalidates
//              after the edit.
// ===========================================================================

#[test]
fn scenario6_replace_all_at_scale_count_text_and_cache_invalidation() {
    let project = build_large_project(1500, 4);
    let mut app = qa_app(production_config(), &project);
    let target = first_file_with_compute(project.path());
    app.open_path(target);
    let active = app.active;

    // Establish the find cache for `compute` (warms `find_cache`; bumps the
    // recompute counter exactly once).
    app.find_query = "compute".to_string();
    let pre = app.find_matches_active();
    let pre_count = pre.len();
    assert!(
        pre_count >= 2,
        "must have many `compute` matches to replace"
    );
    let recompute_after_warm = app.find_recompute_count.get();
    // A second call with the SAME query + unchanged buffer must be a cache HIT
    // (recompute counter does NOT advance).
    let _ = app.find_matches_active();
    assert_eq!(
        app.find_recompute_count.get(),
        recompute_after_warm,
        "an unchanged query+buffer must be a find-cache HIT (no recompute)"
    );

    // Count the literal occurrences independently as ground truth for replace.
    let q = scribe_core::search::Query {
        pattern: "compute".to_string(),
        ..Default::default()
    };
    let truth = scribe_core::search::find_all(&app.tabs[active].text, &q)
        .unwrap()
        .len();
    assert_eq!(pre_count, truth, "warm cache count == ground truth");

    // Replace All `compute` → `XQXQX` (a token guaranteed absent beforehand so the
    // post-count is unambiguous).
    assert!(
        !app.tabs[active].text.contains("XQXQX"),
        "the replacement token must be absent before replace"
    );
    let gen_before = app.tabs[active].edit_gen;
    app.replace_query = "XQXQX".to_string();
    app.replace_in_active(true);

    // The status reflects the exact replacement count.
    assert!(
        app.status.contains(&format!("replaced {pre_count}")),
        "the replace status must report the exact count (got {:?})",
        app.status
    );
    // Every `compute` is gone; exactly `pre_count` `XQXQX` tokens now exist.
    assert!(
        !app.tabs[active].text.contains("compute"),
        "no `compute` token may survive a Replace All"
    );
    let post = scribe_core::search::find_all(
        &app.tabs[active].text,
        &scribe_core::search::Query {
            pattern: "XQXQX".to_string(),
            ..Default::default()
        },
    )
    .unwrap()
    .len();
    assert_eq!(
        post, pre_count,
        "the replacement-token count must equal the original match count"
    );

    // edit_gen advanced → the cache is invalidated. Searching the OLD query now
    // returns zero AND forces a recompute (cache MISS on the new edit_gen).
    assert_ne!(
        app.tabs[active].edit_gen, gen_before,
        "Replace All must bump edit_gen so gen-keyed caches invalidate"
    );
    let before_miss = app.find_recompute_count.get();
    let stale = app.find_matches_active(); // query still "compute"
    assert!(
        stale.is_empty(),
        "after replace, the old query finds nothing (the buffer changed)"
    );
    assert_eq!(
        app.find_recompute_count.get(),
        before_miss + 1,
        "the edit_gen bump forces a find-cache MISS (exactly one recompute)"
    );
}

// ===========================================================================
// Scenario 7 — EDGE: case-sensitivity + whole-word toggles.
//
// DISCOVERED FROM SOURCE: the in-buffer find BAR (`find_replace.rs` /
// `find_nav.rs`) and the find-in-files panel expose NO case-sensitivity and NO
// whole-word UI control — `find_matches_active`/`replace_in_active` build a
// `Query` with `..Default::default()` (case_sensitive=false, whole_word=false),
// and `run_find_in_files` hard-codes `case_sensitive: false, whole_word: false`
// (only a `regex` checkbox is surfaced). There is therefore no user-facing
// toggle to drive. Rather than skip-with-comment only, we PIN the actual
// behaviour the absent toggle implies: search is case-INSENSITIVE and
// NOT whole-word-bounded, so the scale corpus's case variants and substrings all
// match. This is a regression guard on the documented default, not a fabricated
// control.
// ===========================================================================

#[test]
fn scenario7_no_case_or_wholeword_toggle_default_is_case_insensitive_substring() {
    // Build a buffer with mixed-case + substring-embedded occurrences of a token,
    // open it, and confirm the find bar's default (toggle-less) semantics match
    // all of them. The synthetic corpus uses lowercase tokens; we open a real
    // file and inject controlled case/substring variants via the editable mirror
    // (`tab.text`) to exercise the default matcher precisely.
    let project = build_large_project(50, 2);
    let mut app = qa_app(production_config(), &project);
    let target = first_file_with_compute(project.path());
    app.open_path(target);
    let active = app.active;

    // Controlled content: `alpha`, `ALPHA`, `Alpha`, and the substring `alphabet`.
    app.tabs[active].text = "alpha ALPHA Alpha alphabet betaalpha\n".to_string();
    app.tabs[active].edit_gen = app.tabs[active].edit_gen.wrapping_add(1);

    app.find_query = "alpha".to_string();
    let matches = app.find_matches_active();
    // Case-INSENSITIVE: alpha, ALPHA, Alpha all match. NOT whole-word: the
    // `alpha` inside `alphabet` and `betaalpha` also match. => 5 total.
    assert_eq!(
        matches.len(),
        5,
        "the toggle-less find bar is case-insensitive AND substring (not \
         whole-word): alpha/ALPHA/Alpha + alphabet + betaalpha == 5 (got {})",
        matches.len()
    );

    // Confirm the SAME default holds on the find-in-files Query construction by
    // reading the documented hard-coded flags through an independent scan (the
    // panel builds `case_sensitive: false, whole_word: false`).
    let panel_default = scribe_core::search::Query {
        pattern: "alpha".to_string(),
        regex: false,
        case_sensitive: false,
        whole_word: false,
    };
    let scan = scribe_core::search::find_all(&app.tabs[active].text, &panel_default).unwrap();
    assert_eq!(
        scan.len(),
        5,
        "the find-in-files default Query (case-insensitive, not whole-word) \
         denotes the same 5-match set"
    );
}
