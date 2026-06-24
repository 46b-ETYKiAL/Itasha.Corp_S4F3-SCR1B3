//! Micro-benchmarks for the find/replace engine (`scribe_core::search`).
//!
//! Find-all over a large buffer is the hot path behind incremental search,
//! match-highlighting, and replace-all. We bench the four query shapes the UI
//! exposes — literal case-insensitive, literal case-sensitive, whole-word, and
//! regex — plus a `replace_all` pass, all over a ~4 MiB buffer. The buffer is
//! built once in `setup`; only `find_all` / `replace_all` are timed.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use scribe_core::search::{find_all, replace_all, Query};

/// Build a ~`target_bytes` buffer salted with the literal `needle` at a known
/// cadence so each query shape actually matches (and matches a realistic count).
fn make_haystack(target_bytes: usize, needle: &str) -> String {
    const FILLER: &str =
        "the quick brown fox jumps over the lazy dog while counting tokens and bytes\n";
    let mut s = String::with_capacity(target_bytes + FILLER.len());
    let mut lines = 0usize;
    while s.len() < target_bytes {
        if lines.is_multiple_of(7) {
            s.push_str(needle);
            s.push('\n');
        }
        s.push_str(FILLER);
        lines += 1;
    }
    s
}

fn lit(pattern: &str, case_sensitive: bool, whole_word: bool) -> Query {
    Query {
        pattern: pattern.into(),
        regex: false,
        case_sensitive,
        whole_word,
    }
}

fn rx(pattern: &str) -> Query {
    Query {
        pattern: pattern.into(),
        regex: true,
        case_sensitive: false,
        whole_word: false,
    }
}

fn bench_find_all(c: &mut Criterion) {
    const TARGET: usize = 4 * 1024 * 1024;
    let needle = "FindMeMarker";
    let hay = make_haystack(TARGET, needle);

    let mut group = c.benchmark_group("find_all_4mib");
    group.bench_function("literal_case_insensitive", |b| {
        let q = lit("findmemarker", false, false);
        b.iter(|| black_box(find_all(black_box(&hay), &q).unwrap()))
    });
    group.bench_function("literal_case_sensitive", |b| {
        let q = lit("FindMeMarker", true, false);
        b.iter(|| black_box(find_all(black_box(&hay), &q).unwrap()))
    });
    group.bench_function("whole_word", |b| {
        let q = lit("fox", false, true);
        b.iter(|| black_box(find_all(black_box(&hay), &q).unwrap()))
    });
    group.bench_function("regex_word_class", |b| {
        let q = rx(r"\b\w{5}\b");
        b.iter(|| black_box(find_all(black_box(&hay), &q).unwrap()))
    });
    group.finish();
}

fn bench_replace_all(c: &mut Criterion) {
    const TARGET: usize = 4 * 1024 * 1024;
    let needle = "FindMeMarker";
    let hay = make_haystack(TARGET, needle);

    let mut group = c.benchmark_group("replace_all_4mib");
    group.bench_function("literal", |b| {
        let q = lit("the", false, false);
        b.iter(|| black_box(replace_all(black_box(&hay), &q, "THE").unwrap()))
    });
    group.bench_function("regex_capture", |b| {
        let q = rx(r"(\w+)\s+(\w+)");
        b.iter(|| black_box(replace_all(black_box(&hay), &q, "$2 $1").unwrap()))
    });
    group.finish();
}

criterion_group!(benches, bench_find_all, bench_replace_all);
criterion_main!(benches);
