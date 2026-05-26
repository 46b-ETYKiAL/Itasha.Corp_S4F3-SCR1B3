//! LSP base protocol: `Content-Length` message framing + JSON-RPC helpers.
//! Pure functions over `Read`/`Write` so the whole encode→decode→parse loop is
//! testable with in-memory buffers (no child process or async runtime).

use serde_json::{json, Value};
use std::io::{self, BufRead, Read, Write};

/// Encode a JSON-RPC payload with the LSP `Content-Length` header.
pub fn encode(message: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(message).unwrap_or_default();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Write a framed message to a stream.
pub fn write_message<W: Write>(w: &mut W, message: &Value) -> io::Result<()> {
    w.write_all(&encode(message))?;
    w.flush()
}

/// Read one framed message from a buffered stream. Returns `Ok(None)` at EOF.
pub fn read_message<R: BufRead>(r: &mut R) -> io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some(v) = trimmed.strip_prefix("Content-Length:") {
            content_length = v.trim().parse().ok();
        }
    }
    let len = content_length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    let value =
        serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(value))
}

/// Build a JSON-RPC request with an id.
pub fn request(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// Build a JSON-RPC notification (no id).
pub fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

/// A diagnostic surfaced by the server (subset of the LSP `Diagnostic`).
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub uri: String,
    pub line: u32,
    pub character: u32,
    pub severity: u8, // 1=error 2=warning 3=info 4=hint
    pub message: String,
}

/// Extract diagnostics from a `textDocument/publishDiagnostics` notification.
/// Returns empty for any other message.
pub fn parse_publish_diagnostics(msg: &Value) -> Vec<Diagnostic> {
    if msg.get("method").and_then(Value::as_str) != Some("textDocument/publishDiagnostics") {
        return Vec::new();
    }
    let params = &msg["params"];
    let uri = params["uri"].as_str().unwrap_or_default().to_string();
    let mut out = Vec::new();
    if let Some(arr) = params["diagnostics"].as_array() {
        for d in arr {
            let start = &d["range"]["start"];
            out.push(Diagnostic {
                uri: uri.clone(),
                line: start["line"].as_u64().unwrap_or(0) as u32,
                character: start["character"].as_u64().unwrap_or(0) as u32,
                severity: d["severity"].as_u64().unwrap_or(1) as u8,
                message: d["message"].as_str().unwrap_or_default().to_string(),
            });
        }
    }
    out
}

/// Pull the id out of a JSON-RPC response (for correlating requests).
pub fn response_id(msg: &Value) -> Option<i64> {
    msg.get("id").and_then(Value::as_i64)
}

/// Read all available bytes into a String (helper for non-blocking drains).
pub fn read_to_string<R: Read>(r: &mut R) -> io::Result<String> {
    let mut s = String::new();
    r.read_to_string(&mut s)?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn encode_decode_roundtrip() {
        let msg = request(1, "initialize", json!({"capabilities": {}}));
        let bytes = encode(&msg);
        let mut cur = Cursor::new(bytes);
        let back = read_message(&mut cur).unwrap().unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn multiple_messages_in_stream() {
        let mut buf = Vec::new();
        buf.extend(encode(&notification("a", json!({}))));
        buf.extend(encode(&notification("b", json!({}))));
        let mut cur = Cursor::new(buf);
        assert_eq!(read_message(&mut cur).unwrap().unwrap()["method"], "a");
        assert_eq!(read_message(&mut cur).unwrap().unwrap()["method"], "b");
        assert!(read_message(&mut cur).unwrap().is_none()); // EOF
    }

    #[test]
    fn parse_diagnostics() {
        let msg = notification(
            "textDocument/publishDiagnostics",
            json!({
                "uri": "file:///x.rs",
                "diagnostics": [
                    {"range": {"start": {"line": 3, "character": 5}, "end": {"line": 3, "character": 9}},
                     "severity": 1, "message": "mismatched types"}
                ]
            }),
        );
        let diags = parse_publish_diagnostics(&msg);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 3);
        assert_eq!(diags[0].severity, 1);
        assert_eq!(diags[0].message, "mismatched types");
    }

    #[test]
    fn non_diagnostic_message_yields_none() {
        assert!(
            parse_publish_diagnostics(&notification("window/logMessage", json!({}))).is_empty()
        );
    }

    #[test]
    fn response_id_extraction() {
        assert_eq!(response_id(&json!({"id": 7, "result": {}})), Some(7));
        assert_eq!(response_id(&notification("x", json!({}))), None);
    }
}
