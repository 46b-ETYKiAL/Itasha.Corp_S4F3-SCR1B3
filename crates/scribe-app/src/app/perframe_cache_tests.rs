//! P-05 / P-06 / P-08 per-frame-scan caching + throttle coverage.
//!
//! These pin the THROTTLE / RECOMPUTE decisions for the per-frame scans that
//! were uncached/unthrottled before this batch:
//!   * P-05 -- the breadcrumb/sticky `symbol_scopes` scan is now memoized by
//!     `(edit_gen, doc_id)`; an idle frame must NOT re-run the scan.
//!   * P-06 -- the disk-change poll is throttled by `should_poll_disk`, a pure
//!     decision function tested directly across its matrix.
//!   * P-08 -- `spell_count` reads the misspelling memo without cloning; the
//!     count stays correct and stable across idle frames.
use super::{should_poll_disk, ScribeApp, DISK_POLL_INTERVAL_FRAMES};
use scribe_core::Config;

// ---- P-06: the pure disk-poll throttle decision ----

#[test]
fn should_poll_disk_first_call_always_polls() {
    // u64::MAX sentinel = "never polled yet" -> always poll on the first frame,
    // regardless of the interval or the current frame number.
    assert!(should_poll_disk(0, u64::MAX, 30));
    assert!(should_poll_disk(5, u64::MAX, 30));
    assert!(should_poll_disk(1_000_000, u64::MAX, 1));
}

#[test]
fn should_poll_disk_throttles_within_interval() {
    // Within the interval since the last poll -> do NOT poll.
    assert!(!should_poll_disk(100, 100, 30), "same frame as last poll");
    assert!(!should_poll_disk(101, 100, 30), "1 frame later");
    assert!(
        !should_poll_disk(129, 100, 30),
        "29 frames later (just under)"
    );
}

#[test]
fn should_poll_disk_polls_at_or_past_interval() {
    assert!(
        should_poll_disk(130, 100, 30),
        "exactly interval frames later"
    );
    assert!(should_poll_disk(131, 100, 30), "past the interval");
    assert!(should_poll_disk(10_000, 100, 30), "long past the interval");
}

#[test]
fn should_poll_disk_polls_on_counter_wrap() {
    // A wrapped frame counter (current < last) must poll rather than stall for
    // ~2^64 frames. interval is irrelevant on the wrap path.
    assert!(should_poll_disk(5, u64::MAX - 2, 30));
    assert!(should_poll_disk(0, 100, 30));
}

#[test]
fn disk_poll_interval_is_a_sane_cadence() {
    // Guard the cadence constant: frequent enough to catch external edits
    // promptly (~0.5s at 60fps), not so frequent it stats every frame. A
    // `const` block makes this a compile-time invariant on the constant.
    const {
        assert!(DISK_POLL_INTERVAL_FRAMES >= 2, "must not poll every frame");
        assert!(
            DISK_POLL_INTERVAL_FRAMES <= 120,
            "must still catch external edits within ~2s at 60fps"
        );
    }
}

// ---- P-05: the breadcrumb/sticky symbol-scope memo ----

fn app_with(text: &str) -> ScribeApp {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs.clear();
    let mut tab = super::EditorTab::scratch();
    tab.text = text.to_string();
    tab.doc_id = crate::grid::DocId(1);
    app.tabs.push(tab);
    app.active = 0;
    app
}

#[test]
fn symbol_scopes_idle_frame_does_not_recompute() {
    let app = app_with(
        "fn alpha() {
    let x = 1;
}
",
    );
    // First call: a cache miss, the scan runs once.
    let first = app.symbol_scopes_for_active();
    assert!(
        !first.is_empty(),
        "the brace-delimited fn must produce a scope"
    );
    let after_first = app.symbol_scan_count.get();
    assert_eq!(after_first, 1, "exactly one scan on the first (miss) call");

    // Repeated calls with NO edit (no edit_gen change) must hit the cache and
    // NOT re-run the O(n) scan -- the idle-frame proof.
    for _ in 0..50 {
        let again = app.symbol_scopes_for_active();
        assert_eq!(again, first, "cached result is identical");
    }
    assert_eq!(
        app.symbol_scan_count.get(),
        after_first,
        "the scan counter is FLAT across 50 idle frames (no recompute)"
    );
}

#[test]
fn symbol_scopes_recompute_on_edit() {
    let mut app = app_with(
        "fn alpha() {
}
",
    );
    let _ = app.symbol_scopes_for_active();
    let base = app.symbol_scan_count.get();
    assert_eq!(base, 1);

    // An edit bumps edit_gen -> the next call is a miss and re-scans.
    app.tabs[0].set_text(
        "fn alpha() {
}
fn beta() {
}
"
        .to_string(),
    );
    let after_edit = app.symbol_scopes_for_active();
    assert_eq!(
        app.symbol_scan_count.get(),
        base + 1,
        "an edit forces exactly one recompute"
    );
    assert_eq!(
        after_edit.len(),
        2,
        "the new fn is reflected after the edit re-scan"
    );

    // Idle again after the edit: no further recompute.
    let _ = app.symbol_scopes_for_active();
    assert_eq!(
        app.symbol_scan_count.get(),
        base + 1,
        "no recompute on the idle frame after the edit"
    );
}

#[test]
fn symbol_scopes_skips_oversize_buffer_without_scanning() {
    // Above the scan cap the memo returns empty WITHOUT running the O(n) scan.
    let big = "x".repeat(600_000);
    let app = app_with(&big);
    let scopes = app.symbol_scopes_for_active();
    assert!(scopes.is_empty(), "oversize buffer yields no scopes");
    assert_eq!(
        app.symbol_scan_count.get(),
        0,
        "the O(n) scan is never run for an oversize buffer"
    );
}

// ---- P-08: spell_count reads the memo without cloning, behavior unchanged ----

#[test]
fn spell_count_stable_and_correct_across_idle_frames() {
    let mut cfg = Config::default();
    cfg.spellcheck.enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut tab = super::EditorTab::scratch();
    // A plainly-misspelled token in prose so the whole-text scan flags it.
    tab.text = "this is deffinitely wrong
"
    .to_string();
    tab.doc_id = crate::grid::DocId(7);
    app.tabs.push(tab);
    app.active = 0;

    let n0 = app.spell_count();
    // The owned snapshot and the borrow-counted path must agree exactly.
    assert_eq!(
        n0,
        app.misspellings_for_active().len(),
        "spell_count (borrow) == misspellings_for_active (owned snapshot) length"
    );
    // Idle frames return the same count off the memo.
    for _ in 0..20 {
        assert_eq!(app.spell_count(), n0, "idle-frame count is stable");
    }
}

// ---- text_analysis.rs mutation-survivor kills ----

#[test]
fn symbol_scopes_scans_a_buffer_exactly_at_the_size_cap() {
    // A buffer of EXACTLY MAX_SYMBOL_SCAN_BYTES (500_000): clean `>` false ->
    // scans (scan_count 1); `>=` -> returns empty (scan_count 0). The existing
    // oversize test uses 600_000 (skipped under both). Kills 132:27.
    let mut text = String::from("fn a() {\n");
    text.push_str(&"x".repeat(499_988));
    text.push('\n');
    text.push_str("}\n");
    assert_eq!(text.len(), 500_000);
    let app = app_with(&text);
    let scopes = app.symbol_scopes_for_active();
    assert!(
        !scopes.is_empty(),
        "a buffer exactly AT the cap must still be scanned"
    );
    assert_eq!(
        app.symbol_scan_count.get(),
        1,
        "the O(n) scan ran (cap is exclusive)"
    );
}

#[test]
fn spell_count_is_zero_when_spellcheck_disabled() {
    // With spellcheck OFF, with_active_misspellings returns f(&[]) -> 0, killing
    // the `spell_count -> 1` stub (existing test uses a single misspelling == 1).
    let mut cfg = Config::default();
    cfg.spellcheck.enabled = false;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut tab = super::EditorTab::scratch();
    tab.text = "zzqqxx wwvvbb".to_string();
    tab.doc_id = crate::grid::DocId(3);
    app.tabs.push(tab);
    app.active = 0;
    assert_eq!(
        app.spell_count(),
        0,
        "no misspellings counted while spellcheck is off"
    );
}

#[test]
fn spell_memo_recomputes_per_tab_not_stale_across_doc_ids() {
    // Two tabs with DIFFERENT doc_ids and different misspelling counts. A broken
    // cache-key compare (40:19 == -> !=) or a constant key (56:9 -> 0/1) returns
    // tab0's stale zero for tab1. Kills 40:19 and 56:9.
    let mut cfg = Config::default();
    cfg.spellcheck.enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t0 = super::EditorTab::scratch();
    t0.text = String::new();
    t0.doc_id = crate::grid::DocId(1);
    app.tabs.push(t0);
    let mut t1 = super::EditorTab::scratch();
    t1.text = "deffinitely zzqqxx".to_string();
    t1.doc_id = crate::grid::DocId(2);
    app.tabs.push(t1);

    app.active = 0;
    assert_eq!(
        app.spell_count(),
        0,
        "empty tab primes the memo with a zero count"
    );
    app.active = 1;
    assert!(
        app.spell_count() > 0,
        "the second tab must NOT reuse the first tab's memo"
    );
}

#[test]
fn reload_spell_engine_picks_up_a_custom_dictionary() {
    // reload_spell_engine replaces self.spell. Observe via the UNCACHED
    // compute_misspellings: flag a nonword, add it to a custom dict, reload,
    // re-check -> the flag clears. Under the `-> ()` stub self.spell is unchanged.
    let mut cfg = Config::default();
    cfg.spellcheck.enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut tab = super::EditorTab::scratch();
    tab.text = "zqxwv".to_string();
    tab.doc_id = crate::grid::DocId(9);
    app.tabs.push(tab);
    app.active = 0;

    let before = app.compute_misspellings(0).len();
    assert!(
        before >= 1,
        "the nonword is flagged before the custom dict loads"
    );

    let dir = tempfile::tempdir().unwrap();
    let dict = dir.path().join("user.txt");
    std::fs::write(&dict, "zqxwv\n").unwrap();
    app.config.spellcheck.custom_dict_path = Some(dict);
    app.reload_spell_engine();

    let after = app.compute_misspellings(0).len();
    assert!(
        after < before,
        "reloading with the custom dict clears the flag (stub leaves it flagged)"
    );
}
