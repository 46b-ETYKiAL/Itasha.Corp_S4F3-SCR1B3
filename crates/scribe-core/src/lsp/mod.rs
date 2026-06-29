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
use std::io::{BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

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

/// Drive a writer thread that owns `stdin` and serialises every outgoing
/// message in FIFO order. This is the root-cause fix for "a full stdin pipe
/// freezes the UI": the egui frame thread enqueues via a channel
/// ([`std::sync::mpsc::Sender::send`] never blocks on an unbounded channel) and
/// the *writer thread* — never the frame thread — is the one that blocks on a
/// stalled `write_all`/`flush`. A single writer draining a FIFO channel also
/// preserves the on-the-wire ordering callers expect (`initialize` before
/// `initialized` before `didOpen`, …).
///
/// The thread exits when the [`Sender`] is dropped: `recv()` returns `Err`, we
/// flush and return. It is the same off-thread idiom as the diagnostics reader
/// thread spawned alongside it.
fn run_writer_loop<W: Write>(mut stdin: W, rx: Receiver<Value>) {
    // FIFO drain: one message at a time, in the exact order they were enqueued.
    while let Ok(msg) = rx.recv() {
        // A broken pipe (server died / closed stdin) ends the loop — there is
        // nothing left to write to. Best-effort: the Drop path still reaps the
        // child regardless.
        if protocol::write_message(&mut stdin, &msg).is_err() {
            break;
        }
    }
    // Sender dropped (graceful shutdown) or the pipe broke: flush whatever the
    // OS still buffers, then let `stdin` drop (closing the write end, which the
    // server observes as EOF).
    let _ = stdin.flush();
}

/// A running LSP server connection. Diagnostics arrive on `diagnostics`.
///
/// Outgoing messages are never written on the caller's (egui frame) thread:
/// they are enqueued on `outgoing` and drained by a dedicated writer thread
/// that owns the child's `stdin`. A slow or stalled server can therefore never
/// block the UI — at worst the writer thread blocks, and the channel buffers.
pub struct LspClient {
    child: Child,
    /// Enqueue outgoing framed messages. `send` is non-blocking; the writer
    /// thread performs the actual (potentially blocking) `write_all`/`flush`.
    /// `Option` so [`Drop`] can take it and drop it to signal shutdown before
    /// joining the writer thread.
    outgoing: Option<Sender<Value>>,
    /// Handle to the writer thread, joined on [`Drop`] after the sender is
    /// dropped so `stdin` is flushed + closed before we reap the child.
    writer: Option<JoinHandle<()>>,
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
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        let (tx, rx): (Sender<Vec<Diagnostic>>, Receiver<Vec<Diagnostic>>) =
            std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match protocol::read_message(&mut reader) {
                    Ok(Some(msg)) => {
                        let diags = protocol::parse_publish_diagnostics(&msg);
                        if !diags.is_empty() && tx.send(diags).is_err() {
                            // UI dropped the receiver — ordinary teardown, not a
                            // failure of the server. Debug, not warn.
                            tracing::debug!(
                                target: "scribe::lsp",
                                "language-server reader stopped: diagnostics receiver dropped"
                            );
                            break;
                        }
                    }
                    // Clean EOF: the server closed stdout (exited / was reaped).
                    // Diagnostics will no longer update — a recoverable degrade.
                    Ok(None) => {
                        tracing::warn!(
                            target: "scribe::lsp",
                            reason = "eof",
                            "language-server reader stopped: server closed the connection (diagnostics will no longer update)"
                        );
                        break;
                    }
                    // Malformed frame or broken pipe: the diagnostics stream dies
                    // here. Log the error KIND only (never frame/buffer content).
                    Err(e) => {
                        tracing::warn!(
                            target: "scribe::lsp",
                            reason = "read-error",
                            error_kind = ?e.kind(),
                            "language-server reader stopped: unreadable frame or broken pipe (diagnostics will no longer update)"
                        );
                        break;
                    }
                }
            }
        });

        // Writer thread: owns `stdin`, drains `out_rx` FIFO. Every outgoing
        // message — including the handshake below — flows through this thread,
        // so no `write_all`/`flush` ever runs on the caller's frame thread.
        let (out_tx, out_rx): (Sender<Value>, Receiver<Value>) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || run_writer_loop(stdin, out_rx));

        let next_id = AtomicI64::new(1);
        // Enqueue the handshake. `send` is non-blocking; ordering is guaranteed
        // by the single FIFO writer (initialize → initialized → any later
        // did_open). A send error here means the writer thread already died
        // (the spawn failed pathologically) — surface it as a broken pipe so
        // the caller degrades gracefully, exactly as a failed write would have.
        let init = protocol::request(
            next_id.fetch_add(1, Ordering::Relaxed),
            "initialize",
            initialize_params(root_uri),
        );
        let send = |m: Value| -> std::io::Result<()> {
            out_tx
                .send(m)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "lsp writer gone"))
        };
        send(init)?;
        send(protocol::notification("initialized", json!({})))?;

        Ok(Self {
            child,
            outgoing: Some(out_tx),
            writer: Some(writer),
            next_id,
            diagnostics: rx,
        })
    }

    /// Notify the server a document was opened.
    ///
    /// Non-blocking: the message is enqueued for the writer thread. Even if the
    /// server's stdin pipe is full, this returns promptly — the writer thread,
    /// not the caller, owns the blocking `write_all`. An `Err` means the writer
    /// thread has gone (server died); the caller degrades gracefully.
    pub fn did_open(&mut self, uri: &str, language_id: &str, text: &str) -> std::io::Result<()> {
        let msg = protocol::notification(
            "textDocument/didOpen",
            did_open_params(uri, language_id, text),
        );
        self.enqueue(msg)
    }

    /// Enqueue a framed message for the writer thread (FIFO, non-blocking).
    fn enqueue(&self, msg: Value) -> std::io::Result<()> {
        match &self.outgoing {
            Some(tx) => tx.send(msg).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "lsp writer gone")
            }),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "lsp client shutting down",
            )),
        }
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

/// A process handle the teardown can terminate. Abstracted so the [`Drop`]
/// ordering (C-1) is unit-testable against a fake without spawning a real,
/// genuinely-wedged server (which cannot be deterministically constructed).
trait Killable {
    /// Request termination NOW. On a real [`Child`] this breaks the stdin pipe,
    /// which is what UNBLOCKS an in-flight `write_all` in the writer thread so
    /// the subsequent join cannot hang.
    fn start_kill(&mut self);
    /// Reap the (now-terminated) process.
    fn reap(&mut self);
}

impl Killable for Child {
    fn start_kill(&mut self) {
        // `kill()` sends SIGKILL / TerminateProcess and closes our handles to
        // the child's pipes, breaking a full stdin pipe so a stalled
        // `write_all` returns with a broken-pipe error and the writer exits.
        let _ = self.kill();
    }
    fn reap(&mut self) {
        let _ = self.wait();
    }
}

/// C-1 root-cause fix: kill the child BEFORE joining the writer thread.
///
/// The prior order joined the writer FIRST. If that thread was inside a
/// blocking `write_all` to a live-but-not-reading server with a FULL stdin
/// pipe, dropping the sender could NOT interrupt the in-flight write, so the
/// join (and thus the egui frame thread, which runs `Drop`) blocked until the
/// pipe drained or broke. Killing the child first breaks the pipe, which
/// unblocks the write, which lets the join complete promptly. Reap order
/// (kill → join → wait) preserves the existing no-orphan-process semantics.
///
/// Generic over [`Killable`] + the join thunk so the ordering is asserted in a
/// unit test without a real wedged server.
fn teardown<K: Killable>(child: &mut K, join_writer: impl FnOnce()) {
    // 1. Kill FIRST — breaks the stdin pipe, unblocking any stalled writer.
    child.start_kill();
    // 2. Now the writer's `write_all` is guaranteed to return (broken pipe), so
    //    the join cannot hang.
    join_writer();
    // 3. Reap the terminated child (no orphaned process).
    child.reap();
}

impl Drop for LspClient {
    /// Reap the language server so we never leak an orphaned process. The
    /// default `Child` drop only *detaches* — a large server (rust-analyzer,
    /// clangd) would linger for the OS session. We enqueue the LSP graceful
    /// `shutdown`+`exit`, drop the sender so the writer thread flushes the
    /// queue + closes `stdin`, then **kill the child BEFORE joining the writer**
    /// (C-1) so a wedged server's full stdin pipe can never hang the join (and
    /// thus the egui frame thread), then `wait` to guarantee termination. All
    /// steps are best-effort; a child that has already exited makes `kill`/
    /// `wait` return harmless errors.
    fn drop(&mut self) {
        let id = self.id();
        // Enqueue the graceful shutdown handshake (FIFO — drained after any
        // already-queued message). Errors are ignored: if the writer is already
        // gone the child is reaped below regardless.
        let _ = self.enqueue(protocol::request(id, "shutdown", Value::Null));
        let _ = self.enqueue(protocol::notification("exit", Value::Null));
        // Drop the sender: the writer thread's `recv()` now returns `Err`, so it
        // flushes and exits, closing `stdin`. (A graceful server drains the
        // queue and exits on `exit`; a wedged one is force-broken by the kill
        // below.)
        drop(self.outgoing.take());
        let writer = self.writer.take();
        teardown(&mut self.child, move || {
            if let Some(handle) = writer {
                let _ = handle.join();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// C-1 regression: a real "wedged server with a full stdin pipe" cannot be
    /// deterministically constructed in a unit test, so we assert the load-
    /// bearing PROPERTY directly — `teardown` kills the child BEFORE joining the
    /// writer thread. A fake [`Killable`] records the kill event into a shared
    /// trace; the join thunk records its own event. The kill index MUST precede
    /// the join index, otherwise a stalled write could hang the join (and the
    /// egui frame thread) — the exact failure C-1 fixes.
    #[derive(Default)]
    struct FakeChild {
        trace: Rc<RefCell<Vec<&'static str>>>,
    }
    impl Killable for FakeChild {
        fn start_kill(&mut self) {
            self.trace.borrow_mut().push("kill");
        }
        fn reap(&mut self) {
            self.trace.borrow_mut().push("reap");
        }
    }

    #[test]
    fn teardown_kills_child_before_joining_writer() {
        let trace: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let mut child = FakeChild {
            trace: trace.clone(),
        };
        let t2 = trace.clone();
        teardown(&mut child, move || {
            // Stand-in for `writer.join()`. If this ran BEFORE the kill, a
            // full-pipe write could block it forever.
            t2.borrow_mut().push("join");
        });
        let order = trace.borrow();
        assert_eq!(
            order.as_slice(),
            &["kill", "join", "reap"],
            "child must be killed BEFORE the writer join so a stalled write can't hang it"
        );
        let kill_at = order.iter().position(|e| *e == "kill").unwrap();
        let join_at = order.iter().position(|e| *e == "join").unwrap();
        assert!(kill_at < join_at, "kill must strictly precede join");
    }

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

    // ---- writer-thread off-thread send: a full/stalled stdin pipe must never
    // block the caller (the egui frame thread). Proven against a mock `Write`
    // sink, with no child process. ----

    use std::sync::mpsc::{channel, Sender as MpscSender};
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::{Duration, Instant};

    /// A `Write` sink that blocks on the FIRST `write_all` until released, then
    /// records every subsequent write verbatim. Models a server whose stdin
    /// pipe is full: the writer thread stalls inside `write_all`, exactly where
    /// the old synchronous code stalled the frame thread.
    struct StallingSink {
        gate: Arc<Barrier>,
        gated: Mutex<bool>,
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for StallingSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            // Block exactly once, on the first write, until the test releases us.
            let mut already = self.gated.lock().unwrap();
            if !*already {
                *already = true;
                drop(already);
                self.gate.wait(); // stall here until the test signals
            }
            self.written.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn spawn_writer(sink: StallingSink) -> (MpscSender<Value>, std::thread::JoinHandle<()>) {
        let (tx, rx) = channel::<Value>();
        let handle = std::thread::spawn(move || run_writer_loop(sink, rx));
        (tx, handle)
    }

    #[test]
    fn send_returns_promptly_even_when_the_sink_is_stalled() {
        // The sink blocks on its first write. The caller enqueues several
        // messages; each `send` must return immediately (well under a generous
        // bound) — the blocking lives on the writer thread, never the caller.
        let gate = Arc::new(Barrier::new(2));
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = StallingSink {
            gate: gate.clone(),
            gated: Mutex::new(false),
            written: written.clone(),
        };
        let (tx, handle) = spawn_writer(sink);

        let start = Instant::now();
        for i in 1..=5 {
            tx.send(protocol::request(
                i,
                "textDocument/didOpen",
                json!({ "n": i }),
            ))
            .expect("enqueue never blocks");
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "caller sends must not block on a stalled sink (took {elapsed:?})"
        );

        // Now release the stalled writer; it drains the FIFO queue.
        gate.wait();
        drop(tx); // signal shutdown
        handle.join().unwrap();
        assert!(
            !written.lock().unwrap().is_empty(),
            "the writer thread did eventually write once unblocked"
        );
    }

    #[test]
    fn writer_preserves_message_order_and_framing_end_to_end() {
        // Enqueue a sequence; once the (briefly-stalled) writer drains it, the
        // bytes on the sink must decode back to the SAME messages in the SAME
        // order, with intact Content-Length framing.
        let gate = Arc::new(Barrier::new(2));
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = StallingSink {
            gate: gate.clone(),
            gated: Mutex::new(false),
            written: written.clone(),
        };
        let (tx, handle) = spawn_writer(sink);

        let msgs = vec![
            protocol::request(1, "initialize", json!({ "rootUri": "file:///p" })),
            protocol::notification("initialized", json!({})),
            protocol::notification("textDocument/didOpen", json!({ "v": 1 })),
            protocol::notification("textDocument/didChange", json!({ "v": 2 })),
        ];
        for m in &msgs {
            tx.send(m.clone()).expect("enqueue");
        }
        gate.wait(); // release the writer
        drop(tx);
        handle.join().unwrap();

        // Decode the recorded bytes back into messages and compare order-faithfully.
        let bytes = written.lock().unwrap().clone();
        let mut reader = BufReader::new(&bytes[..]);
        let mut decoded = Vec::new();
        while let Some(m) = protocol::read_message(&mut reader).unwrap() {
            decoded.push(m);
        }
        assert_eq!(decoded, msgs, "FIFO order + framing preserved on the wire");
    }

    #[test]
    fn dropping_the_sender_flushes_and_exits_the_writer_thread() {
        // Graceful shutdown: with no stall, dropping the sender drains the queue,
        // flushes, and the writer thread terminates (the Drop-path contract).
        let gate = Arc::new(Barrier::new(2));
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = StallingSink {
            gate: gate.clone(),
            gated: Mutex::new(true), // pre-released: never stalls
            written: written.clone(),
        };
        let (tx, rx) = channel::<Value>();
        let handle = std::thread::spawn(move || run_writer_loop(sink, rx));
        tx.send(protocol::notification("exit", Value::Null))
            .unwrap();
        drop(tx);
        // join must return — the writer saw the Recv error and exited.
        handle.join().unwrap();
        assert!(!written.lock().unwrap().is_empty());
    }

    /// Spawn a benign, long-lived, stdin-piped child so `LspClient`'s
    /// enqueue/id/Drop machinery can be exercised without a real language server.
    /// The child reads/holds stdin and stays alive until killed (which Drop does).
    /// Returns `None` if no such helper binary exists on this host (CI-skip — a
    /// genuine absence, never a false pass; the asserts below only run when a real
    /// client was constructed).
    fn spawn_benign_lsp_client() -> Option<LspClient> {
        let cfg = if cfg!(windows) {
            LspServerConfig {
                command: "cmd".into(),
                args: vec!["/c".into(), "pause".into()],
                languages: vec!["rs".into()],
            }
        } else {
            LspServerConfig {
                command: "cat".into(),
                args: vec![],
                languages: vec!["rs".into()],
            }
        };
        LspClient::spawn(&cfg, "file:///proj").ok()
    }

    #[test]
    fn id_returns_then_increments_the_next_id_counter() {
        // `id()` must return the CURRENT next_id and post-increment it. A mutation
        // pinning it to a constant (0 / 1 / -1) would hand out a fixed, colliding
        // request id and never advance — breaking response correlation. `spawn`
        // consumes id 1 for `initialize`, so the next allocations are 2, 3, ….
        let Some(client) = spawn_benign_lsp_client() else {
            return; // no benign child available on this host; nothing to assert
        };
        let first = client.id();
        let second = client.id();
        assert_eq!(first, 2, "first post-handshake id is 2 (initialize took 1)");
        assert_eq!(second, 3, "id must strictly increment on each call");
        assert!(second > first, "id must advance, never return a constant");
    }

    #[test]
    fn did_open_and_enqueue_succeed_then_fail_after_shutdown() {
        // Two assertions in one client lifecycle:
        //   1. With a live writer, `did_open` (and thus `enqueue`) returns Ok — the
        //      message is actually handed to the channel (kills `did_open -> Ok(())`
        //      ONLY together with #2, since a bare Ok(()) passes #1 too).
        //   2. After the outgoing sender is dropped, `enqueue`/`did_open` MUST
        //      return Err (writer gone). The `-> Ok(())` mutations on both
        //      `did_open` and `enqueue` would WRONGLY report success here — this is
        //      the discriminating case that kills them.
        let Some(mut client) = spawn_benign_lsp_client() else {
            return;
        };
        // Live path: enqueue succeeds.
        client
            .did_open("file:///x.rs", "rust", "fn main(){}")
            .expect("did_open enqueues while the writer is live");
        client
            .enqueue(protocol::notification("textDocument/didChange", json!({})))
            .expect("enqueue succeeds while the writer is live");

        // Drop the sender to simulate the writer being gone, then both calls fail.
        drop(client.outgoing.take());
        let did_open_err = client.did_open("file:///x.rs", "rust", "x").unwrap_err();
        assert_eq!(
            did_open_err.kind(),
            std::io::ErrorKind::BrokenPipe,
            "did_open must surface a broken pipe once the writer is gone, not Ok(())"
        );
        let enqueue_err = client
            .enqueue(protocol::notification("textDocument/didChange", json!({})))
            .unwrap_err();
        assert_eq!(
            enqueue_err.kind(),
            std::io::ErrorKind::BrokenPipe,
            "enqueue must surface a broken pipe once the writer is gone, not Ok(())"
        );
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
