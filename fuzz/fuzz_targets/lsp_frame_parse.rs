#![no_main]
//! Fuzz the LSP JSON-RPC frame reader. A hostile or buggy language server
//! sends arbitrary bytes over its stdout pipe; `protocol::read_message` parses
//! the `Content-Length` header + JSON body and MUST NOT panic or hang on
//! garbage. The header-size and message-size caps (`MAX_HEADER_BYTES`,
//! `MAX_MESSAGE_BYTES`) bound memory; a malformed frame returns `Err`, EOF
//! returns `Ok(None)`. We also drive `parse_publish_diagnostics` /
//! `response_id` over any successfully-decoded value so the downstream
//! extractors are exercised on attacker-shaped JSON.
use libfuzzer_sys::fuzz_target;
use std::io::BufReader;

fuzz_target!(|data: &[u8]| {
    // read_message takes a BufRead; feed the raw fuzz bytes as the "server"
    // output. Loop until EOF / error so multi-frame inputs are exhausted, but
    // bound the iterations so a stream of tiny valid frames can't spin forever.
    let mut reader = BufReader::new(data);
    for _ in 0..256 {
        match scribe_core::lsp::protocol::read_message(&mut reader) {
            Ok(Some(value)) => {
                // Downstream extractors over arbitrary decoded JSON — total.
                let _ = scribe_core::lsp::protocol::parse_publish_diagnostics(&value);
                let _ = scribe_core::lsp::protocol::response_id(&value);
            }
            Ok(None) => break, // EOF
            Err(_) => break,   // malformed frame — surfaced as Err, never panic
        }
    }
});
