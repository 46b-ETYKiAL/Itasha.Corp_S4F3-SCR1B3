//! Micro-benchmarks for the file load + decode + parse hot path
//! (`scribe_core::document` / `scribe_core::buffer` / `scribe_core::encoding`).
//!
//! Opening a file is: read bytes -> detect+decode encoding -> detect+normalize
//! EOL -> build a `Rope`. We bench the public `Document::open` (the full
//! pipeline) and `Buffer::open` (the rope/mmap-aware loader) over a realistic
//! file written to a temp dir in `setup`, plus the pure in-memory
//! `encoding::decode` + EOL pieces so a regression can be localized. The temp
//! file is created ONCE per benchmark and reused (read is the timed part).

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use scribe_core::buffer::Buffer;
use scribe_core::{document::Document, encoding, eol};
use std::io::Write;
use tempfile::NamedTempFile;

/// Build a UTF-8 source-ish payload of roughly `target_bytes` with CRLF line
/// endings, so the EOL detect+normalize path does real work.
fn make_payload(target_bytes: usize) -> Vec<u8> {
    const LINE: &str = "fn handler(req: Request) -> Response { dispatch(req).into() }\r\n";
    let mut s = String::with_capacity(target_bytes + LINE.len());
    while s.len() < target_bytes {
        s.push_str(LINE);
    }
    s.into_bytes()
}

/// Write `payload` to a fresh temp file and return the handle (kept alive so
/// the path stays valid for the duration of the bench).
fn temp_with(payload: &[u8]) -> NamedTempFile {
    let mut f = NamedTempFile::new().expect("temp file");
    f.write_all(payload).expect("write payload");
    f.flush().expect("flush");
    f
}

fn bench_document_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("document_open");
    for (label, bytes) in [("small_64kib", 64 * 1024), ("large_4mib", 4 * 1024 * 1024)] {
        let payload = make_payload(bytes);
        let tmp = temp_with(&payload);
        let path = tmp.path().to_path_buf();
        group.bench_function(label, |b| {
            b.iter(|| {
                let doc = Document::open(black_box(&path)).expect("open");
                black_box(doc.len_lines())
            })
        });
    }
    group.finish();
}

fn bench_buffer_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_open");
    // Stay under the 16 MiB MMAP_THRESHOLD so this exercises the rope-load path
    // (the edit-ready representation), not the zero-copy mmap browse path.
    for (label, bytes) in [("small_64kib", 64 * 1024), ("large_4mib", 4 * 1024 * 1024)] {
        let payload = make_payload(bytes);
        let tmp = temp_with(&payload);
        let path = tmp.path().to_path_buf();
        group.bench_function(label, |b| {
            b.iter(|| {
                let buf = Buffer::open(black_box(&path)).expect("open");
                black_box(buf.len_bytes())
            })
        });
    }
    group.finish();
}

fn bench_decode_and_eol(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode_and_eol_4mib");
    let payload = make_payload(4 * 1024 * 1024);

    group.bench_function("encoding_decode", |b| {
        b.iter(|| {
            let (text, enc) = encoding::decode(black_box(&payload));
            black_box((text.len(), enc.name.clone()))
        })
    });

    // EOL detect + normalize over the already-decoded text.
    let (decoded, _) = encoding::decode(&payload);
    group.bench_function("eol_detect_normalize", |b| {
        b.iter(|| {
            let detected = eol::detect(black_box(&decoded));
            let normalized = eol::normalize_to_lf(black_box(&decoded));
            black_box((detected, normalized.len()))
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_document_open,
    bench_buffer_open,
    bench_decode_and_eol
);
criterion_main!(benches);
