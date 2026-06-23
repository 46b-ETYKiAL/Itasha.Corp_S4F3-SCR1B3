//! Offline / no-network "telemetry-free" invariant gate (taxonomy #36 /
//! blueprint PART 2 §B item 10).
//!
//! SCR1B3's headline privacy claim is **telemetry-free**: the ONLY code path
//! that touches the network is the explicit, user-initiated self-updater. No
//! analytics, no phone-home, no crash-beacon, no usage ping. That claim has been
//! documented but, until this gate, was never PROVEN by a test.
//!
//! This is a STATIC source audit run as a normal `cargo test`: it walks every
//! crate's `src/` tree and asserts that the only network call-sites
//! (`ureq::`-family HTTP, raw `TcpStream`/`UdpSocket` connections) live under
//! `crates/scribe-core/src/update/`. A network call introduced ANYWHERE else
//! fails this test — making an accidental (or malicious) telemetry egress a
//! build-breaking regression rather than a silent privacy violation.
//!
//! Why a static audit (not a runtime socket-deny sandbox): the editor's net I/O
//! is reachable only behind an explicit user action (clicking "check for
//! updates"), so there is no always-on background request to intercept; the
//! honest, deterministic, CI-friendly proof is "no network call-site exists
//! outside the updater module". The updater's own request shapes + the
//! identifier-free `User-Agent` are separately asserted by the mock-HTTP tests
//! in `src/update/net.rs`.

use std::path::{Path, PathBuf};

/// Workspace root = this crate's manifest dir / `../..`.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("canonicalize workspace root")
}

/// The single directory permitted to contain network call-sites: the updater.
fn updater_dir() -> PathBuf {
    workspace_root()
        .join("crates")
        .join("scribe-core")
        .join("src")
        .join("update")
}

/// Recursively collect every `.rs` file under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip build output and any vendored target dirs.
            if path.file_name().and_then(|n| n.to_str()) == Some("target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Network-call-site needles. These match the ACTUAL invocation forms a network
/// egress would take, not mere mentions: `ureq::get(` / `ureq::post(` etc.,
/// and raw socket `connect(` / `bind(` constructors. We match call forms (with
/// the trailing `(` or `::`) so a doc-comment mentioning "ureq" by name does not
/// trip the gate — only real call-sites do.
const NETWORK_CALL_NEEDLES: &[&str] = &[
    "ureq::get(",
    "ureq::post(",
    "ureq::put(",
    "ureq::delete(",
    "ureq::request(",
    "ureq::Agent",
    "TcpStream::connect(",
    "UdpSocket::bind(",
    "reqwest::",
    "hyper::",
    "isahc::",
    "attohttpc::",
];

/// `true` if `path` is inside the permitted updater directory.
fn is_in_updater(path: &Path, updater: &Path) -> bool {
    path.starts_with(updater)
}

/// Strip the line-comment / doc-comment tail so a `// ureq::get(...)` mention in
/// prose is not counted as a call-site. We only need a coarse strip: cut at the
/// first `//` that is not inside an obvious string. For an audit gate this is
/// deliberately conservative — if a `//`-prefixed line still contains a call
/// needle we simply do not count THAT commented line.
fn code_part(line: &str) -> &str {
    match line.find("//") {
        Some(idx) => &line[..idx],
        None => line,
    }
}

#[test]
fn the_only_network_call_sites_live_in_the_updater_module() {
    let root = workspace_root();
    let updater = updater_dir();
    assert!(
        updater.is_dir(),
        "updater dir not found at {updater:?} — the audit anchor moved; update this test"
    );

    let mut files = Vec::new();
    for crate_dir in [
        "scribe-core",
        "scribe-render",
        "scribe-app",
        "scribe-win32-chrome",
    ] {
        rust_files(&root.join("crates").join(crate_dir).join("src"), &mut files);
    }
    assert!(
        files.len() > 20,
        "suspiciously few source files scanned ({}); the walk is broken",
        files.len()
    );

    let mut violations: Vec<String> = Vec::new();
    for file in &files {
        // The updater module is the ONE place network calls are allowed.
        if is_in_updater(file, &updater) {
            continue;
        }
        // `net.rs`'s `#[cfg(test)]` mock server uses `TcpListener` (a SERVER
        // bind, not an outbound call) — but it lives under update/ anyway, so the
        // skip above already covers it. Everything else must be call-site-free.
        let Ok(src) = std::fs::read_to_string(file) else {
            continue;
        };
        for (lineno, line) in src.lines().enumerate() {
            let code = code_part(line);
            for needle in NETWORK_CALL_NEEDLES {
                if code.contains(needle) {
                    violations.push(format!(
                        "{}:{} contains network call-site `{}`",
                        file.strip_prefix(&root).unwrap_or(file).display(),
                        lineno + 1,
                        needle
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "TELEMETRY-FREE INVARIANT VIOLATED — network call-site(s) found OUTSIDE \
         crates/scribe-core/src/update/. SCR1B3 must make zero network calls except \
         the user-initiated updater. Offenders:\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn updater_module_does_contain_the_network_calls_sanity_anchor() {
    // Positive control: the audit's anchor assumption — that the updater module
    // is WHERE the network calls live — must hold. If this fails, the updater
    // was refactored out from under the audit and the negative test above would
    // pass vacuously. `net.rs` is the only file expected to carry a `ureq::get(`.
    let net_rs = updater_dir().join("net.rs");
    let src =
        std::fs::read_to_string(&net_rs).unwrap_or_else(|e| panic!("cannot read {net_rs:?}: {e}"));
    assert!(
        src.contains("ureq::get("),
        "expected the updater's net.rs to hold the ureq::get call-site (audit anchor); \
         if the updater moved, re-point this test"
    );
}

#[test]
fn no_extra_network_client_crate_is_a_dependency() {
    // Supply-chain half of the invariant: assert no crate pulls in a SECOND HTTP
    // / network client. `ureq` (the updater's pure-Rust rustls client) is the
    // ONLY permitted network dependency, and only scribe-core may declare it. A
    // reqwest/hyper/isahc/etc. dependency anywhere is a telemetry-surface
    // regression even before a call-site is written.
    let root = workspace_root();
    // Forbidden network-client crate names as they appear in a `[dependencies]`
    // table (start-of-line `name =` or `name.workspace`). `ureq` is allowed.
    const FORBIDDEN_DEP_PREFIXES: &[&str] = &[
        "reqwest",
        "hyper",
        "isahc",
        "attohttpc",
        "surf",
        "curl",
        "actix-web",
        "axum",
        "warp",
        "tonic",
    ];
    let mut violations = Vec::new();
    for crate_dir in [
        "scribe-core",
        "scribe-render",
        "scribe-app",
        "scribe-win32-chrome",
    ] {
        let manifest = root.join("crates").join(crate_dir).join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        for (lineno, raw) in text.lines().enumerate() {
            let line = code_part_toml(raw).trim_start();
            for dep in FORBIDDEN_DEP_PREFIXES {
                // Match a dependency declaration: `<dep> = ...` or `<dep>.workspace`.
                let as_eq = format!("{dep} =");
                let as_ws = format!("{dep}.workspace");
                if line.starts_with(&as_eq) || line.starts_with(&as_ws) {
                    violations.push(format!(
                        "crates/{crate_dir}/Cargo.toml:{} declares forbidden network client `{dep}`",
                        lineno + 1
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "a second network-client dependency was added (only `ureq` in scribe-core is permitted):\n  {}",
        violations.join("\n  ")
    );
}

/// Strip a TOML `#` line comment so a commented mention of a crate name does not
/// trip the dependency audit.
fn code_part_toml(line: &str) -> &str {
    match line.find('#') {
        Some(idx) => &line[..idx],
        None => line,
    }
}
