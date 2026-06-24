//! Micro-benchmarks for the syntax-highlight hot path (`scribe_core::syntax`).
//!
//! Highlighting runs on every buffer change the widget repaints. Two paths
//! matter: the tree-sitter Rust path (`ext = Some("rs")`) and the syntect
//! fallback path (everything else). We bench:
//!   * `highlight_document` full pass (tree-sitter Rust + syntect generic),
//!   * `highlight_document_incremental` warm-cache re-pass (the per-keystroke
//!     cost the incremental cache is meant to shrink),
//!   * `classify_document` (the spell/structure classification pass).
//!
//! The `Highlighter` is built once in `setup` (its `new()` loads syntect's
//! default syntaxes/themes — that cost is NOT the thing under test and must not
//! be inside the timed loop). Inputs are generated up front. The tree-sitter
//! grammar compiles lazily on the first Rust highlight, so a warm-up call is
//! made before timing the Rust path.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use scribe_core::syntax::{Highlighter, IncrementalHighlightState};

/// Build a realistic multi-function Rust source of roughly `repeat` blocks.
/// Stays well under the 4 MiB `MAX_HIGHLIGHT_BYTES` highlight cap.
fn make_rust_source(repeat: usize) -> String {
    const BLOCK: &str = r#"
/// Doc comment describing the function and its invariants.
pub fn process_item(input: &Item, ctx: &mut Context) -> Result<Output, Error> {
    let mut total: u64 = 0;
    for (idx, field) in input.fields.iter().enumerate() {
        // line comment explaining the branch
        match field.kind {
            FieldKind::Number(n) => total += n as u64,
            FieldKind::Text(ref s) => total += s.len() as u64,
            FieldKind::Nested => {
                let sub = process_nested(field, ctx)?;
                total = total.wrapping_add(sub);
            }
        }
    }
    let label = format!("item-{}-total-{}", input.id, total);
    ctx.record(&label);
    Ok(Output { id: input.id, total, label })
}
"#;
    let mut s = String::with_capacity(BLOCK.len() * repeat);
    for _ in 0..repeat {
        s.push_str(BLOCK);
    }
    s
}

/// Build a generic (non-Rust) source so the syntect path is exercised. Uses a
/// `.toml`-ish shape so syntect picks a real syntax definition.
fn make_generic_source(repeat: usize) -> String {
    const BLOCK: &str = "\
[section.name]\n\
key_one = \"a string value\"\n\
key_two = 12345\n\
key_three = [1, 2, 3, 4, 5]\n\
# a comment line\n\
nested = { inner = true, scale = 0.75 }\n\n";
    let mut s = String::with_capacity(BLOCK.len() * repeat);
    for _ in 0..repeat {
        s.push_str(BLOCK);
    }
    s
}

fn bench_highlight_full(c: &mut Criterion) {
    let hl = Highlighter::new();
    let rust = make_rust_source(200); // ~realistic large source file
    let generic = make_generic_source(400);

    // Warm the lazily-compiled tree-sitter grammar so the first timed sample
    // does not pay the one-time grammar+query compile.
    let _ = hl.highlight_document(&rust, Some("rs"));

    let mut group = c.benchmark_group("highlight_document");
    group.bench_function("rust_tree_sitter", |b| {
        b.iter(|| black_box(hl.highlight_document(black_box(&rust), Some("rs"))))
    });
    group.bench_function("generic_syntect", |b| {
        b.iter(|| black_box(hl.highlight_document(black_box(&generic), Some("toml"))))
    });
    group.finish();
}

fn bench_highlight_incremental(c: &mut Criterion) {
    let hl = Highlighter::new();
    let generic = make_generic_source(400);

    let mut group = c.benchmark_group("highlight_document_incremental");
    // Warm-cache re-pass: prime the cache once, then time the steady-state
    // re-highlight that the editor pays per repaint when nothing (or one line)
    // changed — the path the incremental cache is designed to make cheap.
    group.bench_function("generic_warm_cache", |b| {
        let mut cache = IncrementalHighlightState::default();
        let _ = hl.highlight_document_incremental(&generic, Some("toml"), &mut cache);
        b.iter(|| {
            black_box(hl.highlight_document_incremental(
                black_box(&generic),
                Some("toml"),
                &mut cache,
            ))
        })
    });
    group.finish();
}

fn bench_classify(c: &mut Criterion) {
    let hl = Highlighter::new();
    let rust = make_rust_source(200);
    let _ = hl.classify_document(&rust, Some("rs")); // warm grammar

    let mut group = c.benchmark_group("classify_document");
    group.bench_function("rust_tree_sitter", |b| {
        b.iter(|| black_box(hl.classify_document(black_box(&rust), Some("rs"))))
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_highlight_full,
    bench_highlight_incremental,
    bench_classify
);
criterion_main!(benches);
