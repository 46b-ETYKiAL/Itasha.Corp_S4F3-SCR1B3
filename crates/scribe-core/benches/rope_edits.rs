//! Micro-benchmarks for the rope edit hot paths (`scribe_core::editing`).
//!
//! These cover the per-keystroke edit ops the widget drives on every input:
//! `insert`, `backspace`, `delete_forward`, `delete_selection`, and the
//! line-oriented `delete_line` / `indent_lines`. Each is benchmarked against a
//! small buffer (a few KiB — the common case) AND a large buffer (~4 MiB — a
//! big source file) so a regression that only shows up on large ropes is
//! visible. Large inputs are generated in `setup` (the `iter_batched` closure),
//! never inside the timed routine.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use ropey::Rope;
use scribe_core::editing::{
    backspace, delete_forward, delete_line, delete_selection, indent_lines, insert, EditState,
};

/// Build a deterministic multi-line source-like buffer of roughly `target_bytes`.
fn make_buffer(target_bytes: usize) -> String {
    // A representative-ish line with leading indent, identifiers, and punctuation.
    const LINE: &str = "    let value_result = compute(alpha, beta, gamma) + offset * factor;\n";
    let mut s = String::with_capacity(target_bytes + LINE.len());
    while s.len() < target_bytes {
        s.push_str(LINE);
    }
    s
}

/// A rope + a collapsed caret positioned near the middle of the buffer, which
/// is the worst-ish case for rope splits (forces a descent into the tree).
fn rope_with_mid_caret(text: &str) -> (Rope, EditState) {
    let rope = Rope::from_str(text);
    let mid = rope.len_chars() / 2;
    (rope, EditState::at(mid))
}

fn bench_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("rope_insert");
    for (label, bytes) in [("small_4kib", 4 * 1024), ("large_4mib", 4 * 1024 * 1024)] {
        let text = make_buffer(bytes);
        group.bench_function(label, |b| {
            b.iter_batched(
                || rope_with_mid_caret(&text),
                |(mut rope, mut st)| {
                    insert(&mut rope, &mut st, black_box("hello_world"));
                    black_box((rope.len_chars(), st.cursor))
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

fn bench_backspace_and_delete(c: &mut Criterion) {
    let mut group = c.benchmark_group("rope_delete_char");
    for (label, bytes) in [("small_4kib", 4 * 1024), ("large_4mib", 4 * 1024 * 1024)] {
        let text = make_buffer(bytes);
        group.bench_function(format!("backspace_{label}"), |b| {
            b.iter_batched(
                || rope_with_mid_caret(&text),
                |(mut rope, mut st)| {
                    backspace(&mut rope, &mut st);
                    black_box((rope.len_chars(), st.cursor))
                },
                BatchSize::SmallInput,
            )
        });
        group.bench_function(format!("delete_forward_{label}"), |b| {
            b.iter_batched(
                || rope_with_mid_caret(&text),
                |(mut rope, mut st)| {
                    delete_forward(&mut rope, &mut st);
                    black_box((rope.len_chars(), st.cursor))
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

fn bench_delete_selection(c: &mut Criterion) {
    let mut group = c.benchmark_group("rope_delete_selection");
    for (label, bytes) in [("small_4kib", 4 * 1024), ("large_4mib", 4 * 1024 * 1024)] {
        let text = make_buffer(bytes);
        group.bench_function(label, |b| {
            b.iter_batched(
                || {
                    let rope = Rope::from_str(&text);
                    // Select a ~1 KiB span across the middle of the buffer.
                    let mid = rope.len_chars() / 2;
                    let span = 1024.min(rope.len_chars().saturating_sub(mid));
                    let st = EditState {
                        anchor: mid,
                        cursor: mid + span,
                        goal_col: None,
                    };
                    (rope, st)
                },
                |(mut rope, mut st)| {
                    let removed = delete_selection(&mut rope, &mut st);
                    black_box((removed, rope.len_chars()))
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

fn bench_line_ops(c: &mut Criterion) {
    let mut group = c.benchmark_group("rope_line_ops");
    for (label, bytes) in [("small_4kib", 4 * 1024), ("large_4mib", 4 * 1024 * 1024)] {
        let text = make_buffer(bytes);
        group.bench_function(format!("delete_line_{label}"), |b| {
            b.iter_batched(
                || rope_with_mid_caret(&text),
                |(mut rope, mut st)| {
                    delete_line(&mut rope, &mut st);
                    black_box((rope.len_lines(), st.cursor))
                },
                BatchSize::SmallInput,
            )
        });
        group.bench_function(format!("indent_lines_{label}"), |b| {
            b.iter_batched(
                || {
                    let rope = Rope::from_str(&text);
                    // Select a block of lines around the middle.
                    let mid = rope.len_chars() / 2;
                    let start_line = rope.char_to_line(mid);
                    let end_line = (start_line + 32).min(rope.len_lines().saturating_sub(1));
                    let st = EditState {
                        anchor: rope.line_to_char(start_line),
                        cursor: rope.line_to_char(end_line),
                        goal_col: None,
                    };
                    (rope, st)
                },
                |(mut rope, mut st)| {
                    indent_lines(&mut rope, &mut st, black_box("    "), false);
                    black_box((rope.len_chars(), st.cursor))
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_insert,
    bench_backspace_and_delete,
    bench_delete_selection,
    bench_line_ops
);
criterion_main!(benches);
