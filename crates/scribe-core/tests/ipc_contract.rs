//! IPC message-contract suite for the LSP transport (`scribe_core::lsp`).
//!
//! The LSP client talks to an out-of-process language server over a
//! `Content-Length`-framed JSON-RPC byte stream — the editor's only real IPC
//! boundary. The inline `protocol.rs` tests cover the basic encode/decode
//! round-trip plus several malformed-frame rejections; this suite is the
//! COMPLEMENTARY message-CONTRACT surface, driving the public framing codec
//! (`encode` / `write_message` / `read_message`) and the JSON-RPC builders
//! (`request` / `notification` / `response_id` / `parse_publish_diagnostics`)
//! the way a real peer exchange would:
//!
//!   * request / notification / response wire shapes round-trip serde-faithfully;
//!   * a STREAM of mixed message kinds decodes in order to a clean EOF;
//!   * `Content-Length` framing is byte-exact and survives a body that is
//!     itself split across multiple reads (the BufRead boundary);
//!   * the `publishDiagnostics` parse contract handles 0 / 1 / N diagnostics,
//!     the severity default, and a non-diagnostic message (empty);
//!   * malformed / hostile frames are rejected with a clean `Err`, never a
//!     panic or hang (oversized length, truncation, junk body, bad header);
//!   * `LspRegistry` routes languages and degrades gracefully on an absent one.
//!
//! Public-API only, disjoint from the crate's inline `#[cfg(test)]` modules.

use scribe_core::lsp::protocol::{
    self, encode, notification, parse_publish_diagnostics, read_message, request, response_id,
    write_message, Diagnostic,
};
use scribe_core::lsp::{
    did_open_params, initialize_params, LspClient, LspRegistry, LspServerConfig,
};
use serde_json::{json, Value};
use std::io::BufReader;

// ---------------------------------------------------------------------------
// JSON-RPC wire-shape contracts
// ---------------------------------------------------------------------------

#[test]
fn request_wire_shape_roundtrips() {
    let msg = request(
        42,
        "textDocument/hover",
        json!({ "position": { "line": 1 } }),
    );
    assert_eq!(msg["jsonrpc"], "2.0");
    assert_eq!(msg["id"], 42);
    assert_eq!(msg["method"], "textDocument/hover");
    assert_eq!(msg["params"]["position"]["line"], 1);

    // Frame → unframe is identity.
    let bytes = encode(&msg);
    let back = read_message(&mut BufReader::new(&bytes[..]))
        .unwrap()
        .unwrap();
    assert_eq!(back, msg);
}

#[test]
fn notification_has_no_id_and_roundtrips() {
    let msg = notification("textDocument/didChange", json!({ "version": 7 }));
    assert_eq!(msg["jsonrpc"], "2.0");
    assert!(msg.get("id").is_none(), "a notification carries no id");
    assert_eq!(msg["method"], "textDocument/didChange");

    let bytes = encode(&msg);
    let back = read_message(&mut BufReader::new(&bytes[..]))
        .unwrap()
        .unwrap();
    assert_eq!(back, msg);
}

#[test]
fn response_shape_correlates_by_id() {
    // A server response carries the request's id; the client correlates on it.
    let resp: Value = json!({ "jsonrpc": "2.0", "id": 99, "result": { "ok": true } });
    assert_eq!(response_id(&resp), Some(99));

    // A notification has no id to correlate.
    assert_eq!(response_id(&notification("x", json!({}))), None);

    // The response round-trips through the frame codec unchanged.
    let bytes = encode(&resp);
    let back = read_message(&mut BufReader::new(&bytes[..]))
        .unwrap()
        .unwrap();
    assert_eq!(back, resp);
    assert_eq!(response_id(&back), Some(99));
}

#[test]
fn initialize_and_did_open_params_have_the_lsp_contract_shape() {
    let init = initialize_params("file:///workspace/root");
    assert_eq!(init["rootUri"], "file:///workspace/root");
    assert_eq!(init["clientInfo"]["name"], "SCR1B3");
    assert!(init["capabilities"]["textDocument"]["hover"].is_object());

    let did_open = did_open_params("file:///a.rs", "rust", "fn main() {}");
    assert_eq!(did_open["textDocument"]["uri"], "file:///a.rs");
    assert_eq!(did_open["textDocument"]["languageId"], "rust");
    assert_eq!(did_open["textDocument"]["version"], 1);
    assert_eq!(did_open["textDocument"]["text"], "fn main() {}");

    // Both are real JSON-RPC payloads: they frame + round-trip.
    for p in [init, did_open] {
        let bytes = encode(&p);
        let back = read_message(&mut BufReader::new(&bytes[..]))
            .unwrap()
            .unwrap();
        assert_eq!(back, p);
    }
}

// ---------------------------------------------------------------------------
// Content-Length framing correctness
// ---------------------------------------------------------------------------

#[test]
fn frame_leads_with_exact_content_length_header() {
    let msg = notification("ping", json!({ "n": 1 }));
    let body = serde_json::to_vec(&msg).unwrap();
    let bytes = encode(&msg);
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    assert!(
        bytes.starts_with(header.as_bytes()),
        "frame must lead with the exact byte length of the JSON body"
    );
    assert_eq!(bytes.len(), header.len() + body.len());
}

#[test]
fn stream_of_mixed_messages_decodes_in_order_then_eof() {
    let a = request(1, "initialize", json!({}));
    let b = notification("initialized", json!({}));
    let c = request(2, "shutdown", Value::Null);

    let mut wire: Vec<u8> = Vec::new();
    for m in [&a, &b, &c] {
        write_message(&mut wire, m).unwrap();
    }
    let mut r = BufReader::new(&wire[..]);
    assert_eq!(read_message(&mut r).unwrap().unwrap(), a);
    assert_eq!(read_message(&mut r).unwrap().unwrap(), b);
    assert_eq!(read_message(&mut r).unwrap().unwrap(), c);
    assert!(
        read_message(&mut r).unwrap().is_none(),
        "clean EOF is Ok(None), never an error"
    );
}

#[test]
fn body_split_across_read_boundaries_still_reassembles() {
    // A real socket delivers the body in arbitrary chunks. A BufReader with a
    // 1-byte capacity forces read_exact to reassemble the body across many
    // reads — the framing must not assume the whole body arrives at once.
    let msg = request(
        5,
        "textDocument/completion",
        json!({ "items": [1, 2, 3, 4, 5] }),
    );
    let bytes = encode(&msg);
    let tiny = BufReader::with_capacity(1, &bytes[..]);
    let mut r = tiny;
    let back = read_message(&mut r).unwrap().unwrap();
    assert_eq!(back, msg, "message reassembled despite 1-byte read chunks");
}

#[test]
fn utf8_multibyte_body_length_is_counted_in_bytes_not_chars() {
    // The header counts BYTES. A body full of multibyte glyphs must frame on its
    // byte length, or the reader truncates mid-codepoint.
    let msg = notification("note", json!({ "text": "速記 写本 héllo 🦀" }));
    let bytes = encode(&msg);
    let back = read_message(&mut BufReader::new(&bytes[..]))
        .unwrap()
        .unwrap();
    assert_eq!(back, msg);
    assert_eq!(back["params"]["text"], "速記 写本 héllo 🦀");
}

// ---------------------------------------------------------------------------
// publishDiagnostics parse contract
// ---------------------------------------------------------------------------

#[test]
fn parse_diagnostics_handles_zero_one_and_many() {
    // Zero diagnostics → empty (a "cleared" publish).
    let none = notification(
        "textDocument/publishDiagnostics",
        json!({ "uri": "file:///x.rs", "diagnostics": [] }),
    );
    assert!(parse_publish_diagnostics(&none).is_empty());

    // Many diagnostics, preserving order + fields.
    let many = notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///y.rs",
            "diagnostics": [
                {"range": {"start": {"line": 0, "character": 0}}, "severity": 1, "message": "err"},
                {"range": {"start": {"line": 10, "character": 4}}, "severity": 2, "message": "warn"},
                {"range": {"start": {"line": 99, "character": 1}}, "severity": 3, "message": "info"}
            ]
        }),
    );
    let diags = parse_publish_diagnostics(&many);
    assert_eq!(diags.len(), 3);
    assert_eq!(
        diags[0],
        Diagnostic {
            uri: "file:///y.rs".into(),
            line: 0,
            character: 0,
            severity: 1,
            message: "err".into()
        }
    );
    assert_eq!(diags[1].line, 10);
    assert_eq!(diags[1].character, 4);
    assert_eq!(diags[2].severity, 3);
    assert_eq!(diags[2].message, "info");
}

#[test]
fn parse_diagnostics_defaults_missing_severity_to_error() {
    // A diagnostic with no severity defaults to 1 (error) per the parser.
    let msg = notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///z.rs",
            "diagnostics": [
                {"range": {"start": {"line": 2, "character": 0}}, "message": "no severity"}
            ]
        }),
    );
    let diags = parse_publish_diagnostics(&msg);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].severity, 1, "missing severity defaults to error");
}

#[test]
fn non_diagnostic_method_yields_no_diagnostics() {
    for method in ["window/logMessage", "window/showMessage", "$/progress"] {
        let msg = notification(method, json!({ "anything": true }));
        assert!(
            parse_publish_diagnostics(&msg).is_empty(),
            "{method} is not a diagnostics publish"
        );
    }
}

#[test]
fn full_diagnostics_publish_roundtrips_through_the_wire_then_parses() {
    // End-to-end: a server frames a publishDiagnostics, the client unframes and
    // parses it — the contract the diagnostics reader thread relies on.
    let msg = notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///main.rs",
            "diagnostics": [
                {"range": {"start": {"line": 3, "character": 5}, "end": {"line": 3, "character": 9}},
                 "severity": 1, "message": "mismatched types"}
            ]
        }),
    );
    let bytes = encode(&msg);
    let back = read_message(&mut BufReader::new(&bytes[..]))
        .unwrap()
        .unwrap();
    let diags = parse_publish_diagnostics(&back);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].uri, "file:///main.rs");
    assert_eq!(diags[0].message, "mismatched types");
}

// ---------------------------------------------------------------------------
// Malformed-frame rejection (an untrusted server stdout)
// ---------------------------------------------------------------------------

#[test]
fn missing_content_length_header_is_rejected() {
    let mut r = BufReader::new(&b"X-Custom: 1\r\n\r\n{}"[..]);
    let err = read_message(&mut r).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn truncated_body_is_rejected_not_hung() {
    // Declared length far exceeds the actual body → read_exact hits EOF → Err.
    let mut r = BufReader::new(&b"Content-Length: 500\r\n\r\n{\"a\":1}"[..]);
    assert!(read_message(&mut r).is_err());
}

#[test]
fn junk_non_json_body_is_rejected() {
    let body = b"<<<not json>>>";
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(body);
    let err = read_message(&mut BufReader::new(&frame[..])).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn hostile_oversized_content_length_is_rejected_before_allocation() {
    // A huge declared length must be rejected on the header alone — the giant
    // body need not even be present (no OOM/abort under panic=abort).
    let huge: u64 = 100 * 1024 * 1024 * 1024; // 100 GiB
    let header = format!("Content-Length: {huge}\r\n\r\n");
    let mut r = BufReader::new(header.as_bytes());
    let err = read_message(&mut r).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn non_numeric_content_length_is_rejected() {
    let mut r = BufReader::new(&b"Content-Length: twelve\r\n\r\n{}"[..]);
    assert!(read_message(&mut r).is_err());
}

// ---------------------------------------------------------------------------
// Server registry routing contract
// ---------------------------------------------------------------------------

#[test]
fn registry_defaults_route_languages_and_degrade_on_absence() {
    let r = LspRegistry::with_defaults();
    assert_eq!(r.for_language("rs").unwrap().command, "rust-analyzer");
    assert_eq!(r.for_language("py").unwrap().command, "pylsp");
    let ts = r.for_language("ts").unwrap();
    assert!(ts.args.contains(&"--stdio".to_string()));
    // An unconfigured language degrades gracefully (None, not a panic/Err).
    assert!(r.for_language("brainfuck").is_none());
}

#[test]
fn user_added_server_routes_its_language() {
    let mut r = LspRegistry::default();
    assert!(r.for_language("zig").is_none());
    r.add(LspServerConfig {
        command: "zls".into(),
        args: vec![],
        languages: vec!["zig".into()],
    });
    assert_eq!(r.for_language("zig").unwrap().command, "zls");
}

#[test]
fn spawning_a_missing_server_binary_errors_gracefully() {
    // Launching a non-existent server is an Err the caller ignores — never a
    // crash. This is the "no LSP installed" degradation path.
    let cfg = LspServerConfig {
        command: "scr1b3-definitely-no-such-lsp-binary-zzz".into(),
        args: vec![],
        languages: vec!["rs".into()],
    };
    assert!(LspClient::spawn(&cfg, "file:///proj").is_err());
}

// ---------------------------------------------------------------------------
// read_to_string drain helper contract
// ---------------------------------------------------------------------------

#[test]
fn read_to_string_drains_a_byte_stream() {
    let src = b"diagnostic dump tail";
    let mut cur = std::io::Cursor::new(&src[..]);
    let s = protocol::read_to_string(&mut cur).unwrap();
    assert_eq!(s, "diagnostic dump tail");
}
