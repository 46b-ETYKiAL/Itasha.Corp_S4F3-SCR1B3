//! Load / soak suite: drive the editing model and document I/O under sustained
//! stress and assert it stays correct, panic-free, and bounded.
//!
//! A text editor is judged on the pathological session: a multi-MB buffer, tens
//! of thousands of sequential edits, a deep undo/redo stack, and a long churn
//! loop that inserts and deletes without leaking memory or drifting the buffer
//! state. The crate's inline unit tests cover single operations; this suite
//! covers VOLUME — the regime where off-by-one caret math, unbounded history
//! growth, or a rope corruption surfaces.
//!
//! Counts are scaled so the whole file runs in a few seconds (not minutes):
//! tens of thousands of ops over low-MB buffers, which is ample to surface a
//! quadratic blow-up or a state-drift bug while staying a fast CI citizen.
//! Public-API only (`scribe_core::editing::*`, `scribe_core::Document`).

use ropey::Rope;
use scribe_core::editing::{backspace, insert, EditKind, EditState, History, Snapshot};
use scribe_core::Document;
use tempfile::tempdir;

#[test]
fn tens_of_thousands_of_sequential_inserts_stay_correct() {
    let mut rope = Rope::new();
    let mut st = EditState::at(0);

    // 30_000 single-char inserts at the caret; the caret must track the end and
    // the final buffer must be exactly the concatenation.
    const N: usize = 30_000;
    for i in 0..N {
        let ch = match i % 4 {
            0 => "a",
            1 => "b",
            2 => "c",
            _ => "\n",
        };
        insert(&mut rope, &mut st, ch);
    }
    assert_eq!(rope.len_chars(), N, "every insert landed");
    assert_eq!(st.cursor, N, "caret advanced to the end");
    // Spot-check the periodic structure survived intact.
    assert_eq!(rope.char(0), 'a');
    assert_eq!(rope.char(1), 'b');
    assert_eq!(rope.char(2), 'c');
    assert_eq!(rope.char(3), '\n');
}

#[test]
fn insert_then_backspace_churn_returns_to_empty() {
    // A long churn loop: insert a run, then backspace it all away, many times.
    // Final state must be empty with the caret at 0 — no residue, no underflow.
    let mut rope = Rope::new();
    let mut st = EditState::at(0);

    for _round in 0..2_000 {
        insert(&mut rope, &mut st, "hello");
        assert_eq!(st.cursor, 5);
        for _ in 0..5 {
            backspace(&mut rope, &mut st);
        }
        assert_eq!(rope.len_chars(), 0);
        assert_eq!(st.cursor, 0);
    }
    assert_eq!(rope.to_string(), "");
}

#[test]
fn multi_megabyte_buffer_edits_and_roundtrips_through_document() {
    let dir = tempdir().unwrap();
    // ~4 MiB of line-structured text — comfortably above any per-line cost but
    // far below the 256 MiB mmap threshold, so this exercises the rope path.
    let line = "the quick brown fox jumps over the lazy dog\n"; // 44 bytes
    let lines = (4 * 1024 * 1024) / line.len();
    let mut body = String::with_capacity(lines * line.len());
    for _ in 0..lines {
        body.push_str(line);
    }
    let original_len = body.len();
    let path = dir.path().join("big.txt");
    std::fs::write(&path, &body).unwrap();

    // Open the multi-MB file, edit it, save, and reopen — the full round-trip
    // must preserve content exactly at scale.
    let mut doc = Document::open(&path).unwrap();
    assert!(
        !doc.is_read_only_large(),
        "4 MiB uses the editable rope path"
    );
    assert_eq!(doc.len_bytes(), original_len);

    let edited = format!("PREFIX\n{body}SUFFIX\n");
    doc.set_text(&edited);
    assert!(!doc.save().unwrap(), "ASCII into UTF-8 is never lossy");

    let reopened = Document::open(&path).unwrap();
    assert_eq!(reopened.text(), edited, "multi-MB round-trip is exact");
    assert_eq!(reopened.len_lines(), lines + 3); // PREFIX + body lines + SUFFIX + trailing
}

#[test]
fn deep_undo_then_full_redo_restores_every_state() {
    // Build a deep history of distinct snapshots, undo ALL the way to the start,
    // then redo ALL the way forward — the round-trip must reproduce the exact
    // terminal state. The count cap is generous so nothing is evicted here.
    const STEPS: usize = 1_000;
    let mut history = History::new(STEPS + 8);

    // Each step records the pre-edit snapshot then advances to "state-{i}".
    let mut current = Snapshot::new("", 0);
    for i in 1..=STEPS {
        let before = current.clone();
        history.record(before, EditKind::Other); // Other never coalesces
        let text = format!("state-{i}");
        current = Snapshot::new(text.clone(), text.chars().count());
    }
    assert!(history.can_undo());
    assert!(!history.can_redo());

    // Undo to the very start.
    let mut live = current.clone();
    let mut undo_count = 0;
    while let Some(prev) = history.undo(live.clone()) {
        live = prev;
        undo_count += 1;
    }
    assert_eq!(undo_count, STEPS, "every checkpoint was undoable");
    assert_eq!(live.text, "", "undone all the way to the empty start");
    assert!(history.can_redo());

    // Redo all the way forward to the terminal state.
    let mut redo_count = 0;
    while let Some(next) = history.redo(live.clone()) {
        live = next;
        redo_count += 1;
    }
    assert_eq!(redo_count, STEPS);
    assert_eq!(
        live.text,
        format!("state-{STEPS}"),
        "redo restored the terminal state"
    );
}

#[test]
fn history_count_cap_evicts_oldest_and_stays_bounded() {
    // With a small count cap, a long edit stream must NOT grow the undo stack
    // without bound — the oldest checkpoints are evicted, the most recent are
    // retained. Bounded behaviour under sustained load is the contract.
    let cap = 64;
    let mut history = History::new(cap);
    let mut current = Snapshot::new("", 0);
    for i in 1..=5_000 {
        history.record(current.clone(), EditKind::Other);
        let t = format!("v{i}");
        current = Snapshot::new(t.clone(), t.chars().count());
    }

    // Undo as far as possible; the depth is capped, never 5_000.
    let mut depth = 0;
    let mut live = current.clone();
    while let Some(prev) = history.undo(live.clone()) {
        live = prev;
        depth += 1;
    }
    assert!(
        depth <= cap,
        "undo depth {depth} must be bounded by the cap {cap}"
    );
    assert!(depth > 0, "some recent history is still undoable");
}

#[test]
fn history_byte_budget_bounds_resident_snapshot_memory() {
    // A tiny byte budget over LARGE snapshots must evict so the summed retained
    // bytes never blow past the budget (modulo the single most-recent checkpoint
    // that is never evicted). Models editing a big buffer with a deep stack.
    let big = "X".repeat(64 * 1024); // 64 KiB per snapshot
    let budget = 256 * 1024; // room for ~4 snapshots
    let mut history = History::with_byte_budget(10_000, budget);

    let mut current = Snapshot::new(big.clone(), big.len());
    for i in 0..200 {
        history.record(current.clone(), EditKind::Other);
        let t = format!("{big}{i}");
        current = Snapshot::new(t.clone(), t.chars().count());
    }
    // Retained bytes are bounded: at most the budget plus one over-budget
    // most-recent checkpoint (the eviction loop preserves the last one).
    assert!(
        history.retained_bytes() <= budget + big.len() + 16,
        "retained {} must stay near the {budget}-byte budget",
        history.retained_bytes()
    );
}

#[test]
fn interleaved_insert_delete_keeps_buffer_and_caret_consistent() {
    // A churn loop that mixes inserts and backspaces with a model string we
    // maintain in parallel; after every op the rope must equal the model and the
    // caret must equal the model length. Any drift (caret/byte/char confusion)
    // surfaces immediately at volume.
    let mut rope = Rope::new();
    let mut st = EditState::at(0);
    let mut model = String::new();

    for i in 0..20_000 {
        if i % 3 == 2 && !model.is_empty() {
            backspace(&mut rope, &mut st);
            model.pop();
        } else {
            // Use a multibyte char every so often to stress char-vs-byte indexing.
            let s = if i % 7 == 0 { "é" } else { "z" };
            insert(&mut rope, &mut st, s);
            model.push_str(s);
        }
        // Cheap invariant on the hot path: char count + caret position.
        debug_assert_eq!(rope.len_chars(), model.chars().count());
        debug_assert_eq!(st.cursor, model.chars().count());
    }
    assert_eq!(rope.to_string(), model, "rope tracked the model exactly");
    assert_eq!(st.cursor, model.chars().count(), "caret at the end");
}
