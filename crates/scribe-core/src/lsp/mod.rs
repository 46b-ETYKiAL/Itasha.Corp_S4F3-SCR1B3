//! Language Server Protocol client.
//!
//! A minimal, pure-Rust LSP client: spawn a user-configured language server,
//! complete the `initialize` handshake, and stream `publishDiagnostics` back to
//! the UI over a channel. Engine-agnostic (no AI) — it speaks standard LSP to
//! whatever server the user installs. Missing/unconfigured servers degrade
//! gracefully (no crash).

pub mod protocol;

pub use protocol::Diagnostic;

use serde_json::{json, Value};
use std::io::BufReader;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, Sender};

/// One language server: the command to run + the languages it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspServerConfig {
    pub command: String,
    pub args: Vec<String>,
    /// Language ids / file extensions this server handles (e.g. ["rs"]).
    pub languages: Vec<String>,
}

/// Registry of configured servers. Ships sensible defaults; the user can add
/// more via config. Servers are opt-in — absence means "no LSP for this lang".
#[derive(Debug, Clone, Default)]
pub struct LspRegistry {
    servers: Vec<LspServerConfig>,
}

impl LspRegistry {
    /// Common open-source servers (used only if the user has them installed).
    pub fn with_defaults() -> Self {
        let s = |command: &str, args: &[&str], langs: &[&str]| LspServerConfig {
            command: command.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            languages: langs.iter().map(|l| l.to_string()).collect(),
        };
        Self {
            servers: vec![
                s("rust-analyzer", &[], &["rs"]),
                s("pylsp", &[], &["py"]),
                s(
                    "typescript-language-server",
                    &["--stdio"],
                    &["ts", "tsx", "js", "jsx"],
                ),
                s("gopls", &[], &["go"]),
                s("clangd", &[], &["c", "cc", "cpp", "h", "hpp"]),
            ],
        }
    }

    pub fn add(&mut self, cfg: LspServerConfig) {
        self.servers.push(cfg);
    }

    /// The server (if any) configured for a language id / extension.
    pub fn for_language(&self, lang: &str) -> Option<&LspServerConfig> {
        self.servers
            .iter()
            .find(|s| s.languages.iter().any(|l| l == lang))
    }
}

/// `initialize` request params for a workspace root.
pub fn initialize_params(root_uri: &str) -> Value {
    json!({
        "processId": std::process::id(),
        "rootUri": root_uri,
        "capabilities": {
            "textDocument": {
                "publishDiagnostics": { "relatedInformation": true },
                "completion": { "completionItem": { "snippetSupport": false } },
                "hover": {},
                "definition": {}
            }
        },
        "clientInfo": { "name": "SCR1B3", "version": env!("CARGO_PKG_VERSION") }
    })
}

/// `textDocument/didOpen` params.
pub fn did_open_params(uri: &str, language_id: &str, text: &str) -> Value {
    json!({
        "textDocument": { "uri": uri, "languageId": language_id, "version": 1, "text": text }
    })
}

/// A running LSP server connection. Diagnostics arrive on `diagnostics`.
pub struct LspClient {
    child: Child,
    stdin: std::process::ChildStdin,
    next_id: AtomicI64,
    pub diagnostics: Receiver<Vec<Diagnostic>>,
}

impl LspClient {
    /// Spawn the server, send `initialize` + `initialized`, and start a reader
    /// thread that forwards diagnostics. Returns an error if the command can't
    /// be launched (caller degrades gracefully).
    pub fn spawn(cfg: &LspServerConfig, root_uri: &str) -> std::io::Result<Self> {
        let mut child = Command::new(&cfg.command)
            .args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        let (tx, rx): (Sender<Vec<Diagnostic>>, Receiver<Vec<Diagnostic>>) =
            std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(msg)) = protocol::read_message(&mut reader) {
                let diags = protocol::parse_publish_diagnostics(&msg);
                if !diags.is_empty() && tx.send(diags).is_err() {
                    break; // UI dropped the receiver
                }
            }
        });

        let next_id = AtomicI64::new(1);
        let init = protocol::request(
            next_id.fetch_add(1, Ordering::Relaxed),
            "initialize",
            initialize_params(root_uri),
        );
        protocol::write_message(&mut stdin, &init)?;
        protocol::write_message(
            &mut stdin,
            &protocol::notification("initialized", json!({})),
        )?;

        Ok(Self {
            child,
            stdin,
            next_id,
            diagnostics: rx,
        })
    }

    /// Notify the server a document was opened.
    pub fn did_open(&mut self, uri: &str, language_id: &str, text: &str) -> std::io::Result<()> {
        let msg = protocol::notification(
            "textDocument/didOpen",
            did_open_params(uri, language_id, text),
        );
        protocol::write_message(&mut self.stdin, &msg)
    }

    fn id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Explicitly shut the server down now. Consuming `self` runs the same
    /// graceful-shutdown-then-reap path that [`Drop`] guarantees — so callers
    /// may use this for an eager shutdown, but a client that is simply dropped
    /// (language switch, app exit) is reaped just the same.
    pub fn shutdown(self) {
        // Drop does the work.
    }
}

impl Drop for LspClient {
    /// Reap the language server so we never leak an orphaned process. The
    /// default `Child` drop only *detaches* — a large server (rust-analyzer,
    /// clangd) would linger for the OS session. We send the LSP graceful
    /// `shutdown`+`exit`, then `kill`+`wait` to guarantee termination even if
    /// the server ignores the request. All steps are best-effort; a child that
    /// has already exited makes `kill`/`wait` return harmless errors.
    fn drop(&mut self) {
        let id = self.id();
        let _ = protocol::write_message(
            &mut self.stdin,
            &protocol::request(id, "shutdown", Value::Null),
        );
        let _ = protocol::write_message(
            &mut self.stdin,
            &protocol::notification("exit", Value::Null),
        );
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_defaults_route_languages() {
        let r = LspRegistry::with_defaults();
        assert_eq!(r.for_language("rs").unwrap().command, "rust-analyzer");
        assert_eq!(r.for_language("py").unwrap().command, "pylsp");
        assert!(r
            .for_language("ts")
            .unwrap()
            .args
            .contains(&"--stdio".to_string()));
        assert!(r.for_language("nonsense").is_none()); // graceful absence
    }

    #[test]
    fn user_can_add_server() {
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
    fn initialize_params_shape() {
        let p = initialize_params("file:///proj");
        assert_eq!(p["rootUri"], "file:///proj");
        assert_eq!(p["clientInfo"]["name"], "SCR1B3");
        assert!(p["capabilities"]["textDocument"]["publishDiagnostics"].is_object());
    }

    #[test]
    fn did_open_params_shape() {
        let p = did_open_params("file:///x.rs", "rust", "fn main(){}");
        assert_eq!(p["textDocument"]["languageId"], "rust");
        assert_eq!(p["textDocument"]["version"], 1);
    }

    #[test]
    fn spawn_missing_server_errors_gracefully() {
        let cfg = LspServerConfig {
            command: "scr1b3-no-such-lsp-binary-xyz".into(),
            args: vec![],
            languages: vec!["rs".into()],
        };
        // No crash — just an Err the caller can ignore.
        assert!(LspClient::spawn(&cfg, "file:///x").is_err());
    }

    #[test]
    fn message_round_trips_through_content_length_framing() {
        // The Content-Length framing is the fragile part of the LSP transport;
        // exercise write_message -> read_message end-to-end in memory (the
        // real-process path was previously untested).
        let msg = protocol::request(7, "textDocument/hover", json!({ "x": 1 }));
        let mut buf: Vec<u8> = Vec::new();
        protocol::write_message(&mut buf, &msg).unwrap();
        assert!(
            buf.starts_with(b"Content-Length: "),
            "wire format must lead with a Content-Length header"
        );
        let mut reader = BufReader::new(&buf[..]);
        let back = protocol::read_message(&mut reader)
            .unwrap()
            .expect("one framed message");
        assert_eq!(back, msg);
    }

    #[test]
    fn read_message_decodes_a_stream_of_two_then_eof() {
        let a = protocol::notification("initialized", json!({}));
        let b = protocol::request(2, "shutdown", Value::Null);
        let mut buf: Vec<u8> = Vec::new();
        protocol::write_message(&mut buf, &a).unwrap();
        protocol::write_message(&mut buf, &b).unwrap();
        let mut reader = BufReader::new(&buf[..]);
        assert_eq!(protocol::read_message(&mut reader).unwrap().unwrap(), a);
        assert_eq!(protocol::read_message(&mut reader).unwrap().unwrap(), b);
        // Clean EOF -> Ok(None), never an error.
        assert!(protocol::read_message(&mut reader).unwrap().is_none());
    }
}
